// SPDX-License-Identifier: MPL-2.0

//! One session's presentation state.
//!
//! A window hosts exactly one surface today. The type exists so that the state
//! a session tab owns — its terminal view, its managed transcript, its
//! composer, its raw-attach state machine, its blink phase and its own
//! presentation metrics — is one value rather than seventeen fields
//! interleaved with the window's geometry and focus routing
//! ([ADR-263](../../../docs/decisions/ADR-251-275.md)).
//!
//! The window keeps every decision the surface cannot make on its own —
//! whether the pane is running, whether the selected session is managed,
//! whether the window has focus — and passes them in. That is deliberate: a
//! surface must never read the window's idea of "selected", because a second
//! surface would then present the first one's session.
//!
//! The host's shell adapter — koru.exe's `session_tab` — is what hosts this
//! type as a surface of its own shell; everything here is the
//! shell-independent half.

use std::time::{Duration, Instant};

use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_i18n::{Key as I18nKey, Lang, t};
use koru_session_observability::{ExperienceSample, SessionStats};
use koru_session_protocol::{GridSize, RenderUpdate, RowIndex, RowRange, SessionId};
use koru_session_view::{
    CellPoint, ManagedPresentationToken, ManagedSessionCopy, ManagedSessionView, PendingInputCopy,
    PendingInputView, RenderWork, TerminalCursorPreferences, TerminalView,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect as KurboRect};

use crate::interaction::SurfaceInteraction;
use crate::watching::{self, Watching};

const OVERSCAN_ROWS: u16 = 2;
pub const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// What one session's pane says when it has nothing of its own to show.
/// Per-surface, because two tabs are legitimately in two different states.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PaneStatus {
    #[default]
    Empty,
    Starting,
    Running,
    Detached,
    ControllerConflict,
    Exited,
    Failed,
    AgentUnavailable,
}

/// What [`SessionSurface::apply_render`] did with one stream update, so the
/// host can finish the bookkeeping only it can do — which tab holds the
/// controller, whether the frame is a reason to draw — without ever seeing the
/// gate's decision itself.
#[must_use]
pub enum RenderOutcome {
    /// The update was dropped: the sized-snapshot gate refused it, it belongs
    /// to a session this surface is not waiting for, or it arrived for a
    /// managed session. The last complete presentation stays on screen and
    /// nothing needs to be drawn.
    Dropped,
    /// A pending raw switch ended: the first snapshot at the requested grid
    /// was presented (or its apply failed, with the notice set — either way
    /// the wait is over and the controller is this surface's). The host
    /// completes its attach bookkeeping and redraws.
    SwitchPresented { session: SessionId },
    /// An ordinary update was applied to this surface's own model. Whether
    /// that is a reason to draw is the host's question — only the frame of a
    /// tab that is active in its own window is.
    Applied,
    /// The update could not be applied; the notice is set. A redraw shows it.
    ApplyFailed,
}

/// What one pane draw did, for the caller that owns the frame budget and the
/// status placeholder.
pub struct PaneFrame {
    /// Raw terminal render work, when the raw view drew this frame. A managed
    /// transcript reports none: its cost is not the grid's.
    pub raw_work: Option<RenderWork>,
    /// A pending raw switch that still has a previous frame to show. The
    /// window suppresses the status placeholder over it.
    pub pending_has_previous: bool,
}

pub struct SessionSurface<I: SurfaceInteraction> {
    pub view: TerminalView<I>,
    pub managed: ManagedSessionView<I>,
    pub pending: PendingInputView<I>,
    pub composer_visible: bool,
    pub active_epoch: u64,
    pub raw_switch_pending: bool,
    pub pending_raw_attachment: Option<(SessionId, u64)>,
    pub pending_raw_size: Option<GridSize>,
    pub raw_switch_previous_managed: bool,
    pub managed_presentation: Option<ManagedPresentationToken>,
    pub switch_started: Option<Instant>,
    pub delta_received: Option<Instant>,
    pub stream_gaps_since_present: u64,
    pub pending_experience: Option<ExperienceSample>,
    pub pending_frame_managed: bool,
    pub cursor_blink_visible: bool,
    pub cursor_blink_deadline: Option<Instant>,

    // ------------------------------------------------ hosted by the app shell
    //
    // The fields the window supplied per call before SP-10 and a shell-hosted
    // surface has to hold for itself. They are here rather than in a wrapper
    // because the host's shell `Surface` is an impl on *this* type: a second
    // struct around it would be the adapter ADR-221 forbids.
    /// The pane the shell last handed over, in window space
    /// (`Surface::set_bounds`). **The shell is now the only source**, which is
    /// what SP-9 said SP-10 would make true.
    pub bounds: Rect,
    /// Device pixels per logical pixel, from the shell's surface context.
    pub scale: f32,
    /// Whether **this** surface's session is managed.
    ///
    /// The window owned the equivalent question and passed it per call —
    /// deliberately, because a surface must never read the window's idea of
    /// "selected" or it would present the previous session's transcript. Under
    /// the shell a surface *is* one session, so the answer is its own, and
    /// [`attach`](Self::attach) is where it is set.
    pub session_is_managed: bool,

    /// **The session this surface shows**, or `None` while a launch is in
    /// flight and nothing has been named yet.
    ///
    /// The single most load-bearing field in the file: it is the *surface
    /// identity* half of ADR-263's late-frame guard. A snapshot is presented
    /// only when its session and its epoch both match, so a frame for a tab
    /// that has been switched away from cannot land under another tab's title.
    pub session: Option<SessionId>,
    /// What this session's pane says when it has nothing of its own to show.
    /// Per-surface, because two tabs are legitimately in two different states.
    pub status: PaneStatus,
    /// The provider's own state for a managed session, which outranks the pane
    /// status in the placeholder — "waiting for approval" is more useful than
    /// "running", and it is what the tab's status has to agree with.
    pub managed_state: Option<koru_session_protocol::ManagedState>,
    /// One exceptional sentence over this session's pane, already localized.
    pub notice: Option<String>,
    /// Whether this surface is the active tab. **Only the active one draws**,
    /// blinks, lays out or negotiates a viewport (R2) — and this is where that
    /// is enforced for the terminal half.
    pub active: bool,
    /// Whether the window has the keyboard focus, and whether the compositor is
    /// showing it. Both are window facts the shell reports; the caret's blink
    /// is the only thing that reads them.
    pub window_focused: bool,
    pub occluded: bool,
    /// Where input goes inside this surface. There are two places, and Tab
    /// moves between them.
    pub focus: SurfaceFocus,
    /// **Whether this surface is watching its session rather than driving
    /// it.** A delegated run's session belongs to the orchestrator for as
    /// long as the run works; a window can read it over a connection that
    /// took no lease, and this is what that reads like — no caret, no
    /// composer band, no transcript control, and a footer that says so with
    /// the one control that changes it.
    watching: Watching,
    /// Whether the footer's control is under the pointer, and whether it has
    /// the keyboard. Presentation only; the press itself is the host's.
    watch_hovered: bool,
    watch_focused: bool,
    /// A press on *Take the session*, waiting for the host to drain it. The
    /// surface cannot make the request itself — a controller lease belongs
    /// to the connection holding it, and this crate owns no connection.
    takeover_requested: bool,
    pub lang: Lang,
    /// The composer's own hover clock.
    pub tooltip: koru_ui::tooltip::Tooltip,
    /// The one path to the session client this surface's session runs on.
    ///
    /// The three views hold clones of it for their own intents; this one is for
    /// the two things the surface itself sends — a clipboard acquisition, which
    /// must happen off the loop, and a pending switch's retargeting resize.
    sender: I,
    /// What the last `draw` cost, for the frame diagnostic to drain. Held
    /// rather than reported because the diagnostic is the window's, and a
    /// surface that reached it would be a surface with a metrics dependency.
    pub last_render: Option<(Option<RenderWork>, Duration)>,
    /// When this surface was made, until the first frame reports it. **Taken
    /// once**: `first_present_ms` is a fact about opening, and a second one
    /// would be a fact about redrawing.
    created_at: Option<Instant>,
}

/// Where a key goes inside one session surface.
///
/// **Two, not four.** The window's old cycle had `Profiles` and `Sessions` in
/// it as well, because the left pane was part of the same focus ring; under the
/// shell the pane is a different surface's problem and this is what is left.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum SurfaceFocus {
    #[default]
    Terminal,
    Composer,
}

impl<I: SurfaceInteraction> SessionSurface<I> {
    /// **Send everything through `sender` from now on.** Each visible terminal
    /// has its own session connection, so a surface that moves to one — or
    /// back — repoints all four places that hold the sender, and keeps its
    /// model, scroll and selection.
    pub fn set_sender(&mut self, sender: &I) {
        *self.view.interaction_mut() = sender.clone();
        self.managed.set_interaction(sender.clone());
        self.pending.set_interaction(sender.clone());
        self.sender = sender.clone();
    }

    pub fn new(
        sender: &I,
        stats: std::sync::Arc<SessionStats>,
        lang: Lang,
        composer_visible: bool,
        cursor: TerminalCursorPreferences,
    ) -> Self {
        let mut view = TerminalView::with_stats(sender.clone(), stats);
        view.set_cursor_preferences(cursor);
        Self {
            view,
            managed: ManagedSessionView::new(sender.clone(), managed_copy(lang)),
            pending: PendingInputView::new(sender.clone(), pending_copy(lang)),
            composer_visible,
            active_epoch: 0,
            raw_switch_pending: false,
            pending_raw_attachment: None,
            pending_raw_size: None,
            raw_switch_previous_managed: false,
            managed_presentation: None,
            switch_started: None,
            delta_received: None,
            stream_gaps_since_present: 0,
            pending_experience: None,
            pending_frame_managed: false,
            cursor_blink_visible: true,
            cursor_blink_deadline: None,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            scale: 1.0,
            session_is_managed: false,
            session: None,
            status: PaneStatus::Empty,
            managed_state: None,
            notice: None,
            active: false,
            window_focused: false,
            occluded: false,
            focus: SurfaceFocus::Terminal,
            watching: Watching::default(),
            watch_hovered: false,
            watch_focused: false,
            takeover_requested: false,
            lang,
            tooltip: koru_ui::tooltip::Tooltip::default(),
            sender: sender.clone(),
            last_render: None,
            created_at: Some(Instant::now()),
        }
    }

    /// Whether this surface is watching its session rather than driving it.
    pub fn watching(&self) -> Watching {
        self.watching
    }

    /// **Enter or leave read-only.** Answers whether anything moved, so a
    /// host can decide to redraw and to renegotiate the grid the band was
    /// carved from.
    ///
    /// Entering takes the keyboard out of the composer, which is about to
    /// stop existing, and tells the transcript it may not be acted on — an
    /// approval answer and a turn cancellation are both lease-holders'
    /// verbs, and a control that would be refused is worse than no control
    /// (ADR-354).
    pub fn set_watching(&mut self, watching: Watching) -> bool {
        if self.watching == watching {
            return false;
        }
        let entering = watching.watching() && !self.watching.watching();
        self.watching = watching;
        self.managed.set_read_only(watching.watching());
        if entering {
            self.focus = SurfaceFocus::Terminal;
            self.pending.set_focused(false);
            self.watch_focused = false;
            self.watch_hovered = false;
            self.takeover_requested = false;
        }
        true
    }

    /// Take the *Take the session* press this surface has recorded.
    ///
    /// **The surface cannot make the request itself.** A controller lease
    /// belongs to the connection holding it, and this crate owns no
    /// connection; the host performs it on the one that already speaks to
    /// the session (§7.4, ADR-341) — the same reason `koru-runs-ui` reports
    /// its handover as a request rather than making it.
    pub fn take_takeover_request(&mut self) -> bool {
        std::mem::take(&mut self.takeover_requested)
    }

    /// A press inside the read-only band. Answers whether it landed on the
    /// control — the host redraws and drains the request.
    pub fn press_watch_band(&mut self, x: f32, y: f32) -> bool {
        if !matches!(self.watching, Watching::Yes) {
            return false;
        }
        let band = self.split().1;
        let mut engine = TextEngine::new();
        if !watching::layout(band, &mut engine, self.lang, self.watching).hits_action(x, y) {
            return false;
        }
        self.watch_focused = true;
        self.takeover_requested = true;
        true
    }

    /// The pointer moved over the band. Answers whether the control's hover
    /// changed, which is the only reason this costs a redraw.
    pub fn hover_watch_band(&mut self, x: f32, y: f32) -> bool {
        if !matches!(self.watching, Watching::Yes) {
            return std::mem::take(&mut self.watch_hovered);
        }
        let band = self.split().1;
        let mut engine = TextEngine::new();
        let over = watching::layout(band, &mut engine, self.lang, self.watching).hits_action(x, y);
        let changed = self.watch_hovered != over;
        self.watch_hovered = over;
        changed
    }

    /// Whether the band's control is under the pointer, so the host can ask
    /// for the press cursor without measuring the band a second time.
    pub fn watch_hovered(&self) -> bool {
        self.watch_hovered && matches!(self.watching, Watching::Yes)
    }

    /// Whether the band's control has the keyboard, so Tab reaches it and
    /// Enter presses it. The read-only surface's whole focus ring: there is
    /// one control and the transcript, and Tab moves between them.
    pub fn watch_focused(&self) -> bool {
        self.watch_focused && matches!(self.watching, Watching::Yes)
    }

    pub fn set_watch_focused(&mut self, focused: bool) -> bool {
        let next = focused && matches!(self.watching, Watching::Yes);
        if self.watch_focused == next {
            return false;
        }
        self.watch_focused = next;
        true
    }

    /// Press the band's control from the keyboard.
    pub fn activate_watch_control(&mut self) -> bool {
        if !matches!(self.watching, Watching::Yes) {
            return false;
        }
        self.takeover_requested = true;
        true
    }

    /// Draw the read-only band into the rect [`split`](Self::split) reserved
    /// for it. A surface that is not watching draws nothing.
    pub fn draw_watch_band(
        &mut self,
        canvas: &mut Scene,
        engine: &mut TextEngine,
        root: Affine,
        band: Rect,
    ) {
        if !self.watching.watching() {
            return;
        }
        watching::draw(
            canvas,
            root,
            engine,
            watching::Band {
                rect: band,
                lang: self.lang,
                watching: self.watching,
                hovered: self.watch_hovered,
                focused: self.watch_focused,
            },
        );
    }

    /// The clipboard is acquired **off the loop** — reading it can block on
    /// another process, and a terminal that stalled on a paste would be a
    /// terminal that stalled on somebody else's window.
    pub fn clipboard(&self, action: koru_session_view::ClipboardAction) {
        self.sender.clipboard(action);
    }

    /// **Point a switch in flight at the grid this surface now wants.**
    ///
    /// The window resized while a raw attach was pending. The snapshot that
    /// will be presented has to be the one that fits *this* size, or the
    /// keep-previous-frame wait would end on a frame that is already stale — so
    /// the target moves, and the resize goes out only once the attachment
    /// exists (before `Attached` the backend still controls the previous
    /// session, and resizing that process would be resizing what is on screen).
    ///
    /// Answers whether anything moved.
    pub fn retarget_switch(&mut self) -> bool {
        let size = self.running_grid_size();
        if self.pending_raw_size == Some(size) {
            return false;
        }
        self.pending_raw_size = Some(size);
        if self.pending_raw_attachment.is_some() {
            self.sender.resize(size);
        }
        true
    }

    /// How long this surface took to reach the screen, once.
    pub fn take_first_present_ms(&mut self) -> Option<u64> {
        self.created_at
            .take()
            .map(|at| at.elapsed().as_millis() as u64)
    }

    /// Whether a frame addressed to `session` belongs to **this** surface.
    ///
    /// ADR-263: a late snapshot for a deactivated surface is dropped by epoch
    /// *and* surface identity. This is the identity half, and it is a method
    /// rather than a comparison at four call sites so there is one answer.
    pub fn owns(&self, session: SessionId) -> bool {
        self.session == Some(session)
    }

    /// Where the grid draws, and where the composer draws, inside the bounds
    /// the shell handed over.
    ///
    /// **One arithmetic, shared by drawing and hit-testing** — the rule that
    /// used to live in `layout.rs` and now lives beside the only thing that
    /// still needs it. The composer takes a fixed band off the bottom; the grid
    /// takes what is left.
    pub fn split(&self) -> (Rect, Rect) {
        self.split_reserving(self.composer_visible && self.status == PaneStatus::Running)
    }

    /// **The same two rects, as they will be once this session is running.**
    ///
    /// The band is furniture the session's own state brings with it, so the
    /// pane a session is *created* in is measured from here rather than from
    /// [`split`](Self::split). See
    /// [`running_grid_size`](Self::running_grid_size) for what goes wrong
    /// otherwise.
    pub fn running_split(&self) -> (Rect, Rect) {
        self.split_reserving(self.composer_visible)
    }

    /// The same arithmetic with the band's presence decided by the caller, so
    /// that the two questions above are one piece of geometry rather than two.
    fn split_reserving(&self, composer_band: bool) -> (Rect, Rect) {
        let bounds = Rect::new(0.0, 0.0, self.bounds.w.max(0.0), self.bounds.h.max(0.0));
        // **The two bands are exclusive**, and that is the point: a surface
        // that is watching has no composer to reserve room for, and the room
        // it does reserve is for the sentence saying why. One `if`, so there
        // is no state in which both are laid out or neither is measured.
        let composer_h = if self.watching.watching() {
            watching::FOOTER_H.min(bounds.h)
        } else if composer_band {
            koru_session_view::COMPOSER_HEIGHT.min(bounds.h)
        } else {
            0.0
        };
        let composer = Rect::new(
            bounds.x,
            bounds.y + bounds.h - composer_h,
            bounds.w,
            composer_h,
        );
        let terminal = Rect::new(
            bounds.x,
            bounds.y,
            bounds.w,
            (composer.y - bounds.y).max(0.0),
        );
        (terminal, composer)
    }

    /// **The rect the grid is measured, drawn and hit-tested in**, inside the
    /// pane the two splits above hand out.
    ///
    /// The pane's own rect is still the one that gets painted — the terminal
    /// background reaches the divider, so the padding is empty background
    /// rather than a strip of window showing through. What moves is *content*:
    /// [`CONTENT_INSET_X`](koru_session_view::CONTENT_INSET_X) off each side,
    /// and every question about the grid asks this rather than the pane, so a
    /// column the PTY writes into is a column the surface drew and a click on
    /// it lands on the cell under the pointer.
    pub fn content_rect(pane: Rect) -> Rect {
        let inset = koru_session_view::CONTENT_INSET_X;
        Rect::new(
            pane.x + inset,
            pane.y,
            (pane.w - inset * 2.0).max(0.0),
            pane.h,
        )
    }

    /// The grid this surface would ask for, given the bounds it was handed.
    pub fn grid_size(&self) -> GridSize {
        self.grid_size_for(self.split().0)
    }

    /// **The grid a session that is about to run will be created at.**
    ///
    /// [`split`](Self::split) keeps the composer band out of the pane until the
    /// session says `Running`, so a surface asked for its grid *before* the
    /// session exists measures a pane `COMPOSER_HEIGHT` taller than the one the
    /// session will render in — eight rows at an ordinary cell. Creating the
    /// PTY at that size and correcting it once the first prompt has printed is
    /// the startup jump ADR-263's sized-snapshot gate exists to remove: the
    /// shell repaints into the corrected grid, the rows it had already written
    /// go to scrollback, and a session that has printed one banner opens with a
    /// leading gap and a live scrollbar.
    ///
    /// So a launch, an attach and a retarget measure the pane as it will be,
    /// and every resize after the first snapshot goes through
    /// [`grid_size`](Self::grid_size) — which by then answers the same thing,
    /// because the surface is `Running`. This is the two-size rule the
    /// single-pane window kept as `running_raw_grid_size`, restored on the
    /// surface that now owns both halves.
    pub fn running_grid_size(&self) -> GridSize {
        self.grid_size_for(self.running_split().0)
    }

    /// What this pane says when it has nothing of its own to show — and what
    /// the tab's own status reads, so the two can never disagree.
    ///
    /// The provider's state wins where there is one: *waiting for approval* is
    /// the sentence somebody needs, and *running* would be true and useless.
    pub fn status_key(&self) -> I18nKey {
        use koru_session_protocol::ManagedState;

        if let Some(state) = self.managed_state {
            return match state {
                ManagedState::Idle => I18nKey::TerminalRunning,
                ManagedState::Running => I18nKey::TerminalManagedWorking,
                ManagedState::WaitingForInput => I18nKey::TerminalManagedWaitingForInput,
                ManagedState::WaitingForApproval => I18nKey::TerminalManagedWaitingForApproval,
                ManagedState::Completed => I18nKey::TerminalManagedCompleted,
                ManagedState::Cancelled => I18nKey::TerminalManagedCancelled,
                ManagedState::Failed => I18nKey::TerminalManagedProviderFailed,
            };
        }
        match self.status {
            PaneStatus::Empty => I18nKey::TerminalNoSelection,
            PaneStatus::Starting => I18nKey::TerminalStarting,
            PaneStatus::Running => I18nKey::TerminalRunning,
            PaneStatus::Detached => I18nKey::TerminalDetached,
            PaneStatus::ControllerConflict => I18nKey::TerminalControllerConflict,
            PaneStatus::Exited => I18nKey::TerminalExited,
            PaneStatus::Failed => I18nKey::TerminalFailed,
            PaneStatus::AgentUnavailable => I18nKey::TerminalAgentUnavailable,
        }
    }

    /// The window's half of "should the caret blink", now that the surface owns
    /// every part of it: focus, occlusion, whether this tab is the active one,
    /// where input is routed, and what this session's pane is doing.
    ///
    /// **`active` is in here, and that is R2's enforcement point**: an inactive
    /// tab has no eligible caret, so it sets no deadline and costs no timer
    /// wakeups at all.
    pub fn blink_eligible(&self) -> bool {
        // **No caret on a session this window is only reading.** A managed
        // transcript has no caret anyway; saying it here as well means a
        // watched raw session — which delegation never creates, but the
        // surface does not depend on that — could never grow one either.
        !self.watching.watching()
            && self.active
            && self.window_focused
            && !self.occluded
            && self.focus == SurfaceFocus::Terminal
            && self.status == PaneStatus::Running
            && !self.session_is_managed
    }

    /// The grid this surface would ask for, given the pane it was handed.
    ///
    /// **Measured from the padded content, not from the pane.** The inset is
    /// applied here rather than at each caller so that a launch, an attach, a
    /// retarget and every later resize all measure the same width — a session
    /// created from the pane's full width would be told it has two more columns
    /// than the surface ever draws, and would wrap into pixels behind the
    /// padding.
    pub fn grid_size_for(&self, terminal: Rect) -> GridSize {
        let content = Self::content_rect(terminal);
        let (cell_w, cell_h) = self.view.cell_size().unwrap_or((8.0, 18.0));
        GridSize {
            columns: ((content.w / cell_w).floor() as u16).max(1),
            rows: ((content.h / cell_h).floor() as u16).max(1),
        }
    }

    fn visible_rows(&self, size: GridSize) -> RowRange {
        self.view.rows_for_resize(size.rows).unwrap_or(RowRange {
            first: RowIndex(0),
            count: size.rows,
        })
    }

    /// Negotiate a new viewport for the raw grid. A pending raw switch owns
    /// the resize itself — retargeting it here would resize the process the
    /// window is still showing.
    pub fn resize_view(&mut self, selected_is_managed: bool, size: GridSize) -> bool {
        if self.raw_switch_pending || selected_is_managed {
            return false;
        }
        if self.view.model().is_none() {
            return false;
        }
        self.view
            .resize_terminal(size, self.visible_rows(size), OVERSCAN_ROWS)
            .unwrap_or(false)
    }

    /// **Re-negotiate the grid after something moved this surface's pane** —
    /// new bounds from the shell, or the composer band being toggled under it.
    ///
    /// A switch in flight retargets instead of resizing: before `Attached` the
    /// backend still controls the previous session, and the sized-snapshot gate
    /// has to expect the grid the snapshot will actually arrive at. One method
    /// because both callers have to make the same choice, and a caller that
    /// resized a pending switch would strand the tab waiting for a frame at a
    /// size nobody asked for.
    pub fn renegotiate_grid(&mut self) -> bool {
        if self.raw_switch_pending {
            return self.retarget_switch();
        }
        let size = self.grid_size();
        self.resize_view(self.session_is_managed, size)
    }

    /// Drop every presented frame. The three views are reset together or an
    /// old transcript outlives the grid that belonged with it.
    pub fn reset_presentation(&mut self) {
        self.view.reset();
        self.managed.reset();
        self.pending.detach();
    }

    /// Enter the keep-previous-frame wait. `raw_size` is the grid the first
    /// acceptable snapshot must arrive at; `previous_managed` records what is
    /// still on screen behind the wait, because a managed transcript keeps
    /// drawing where a raw grid keeps its last model.
    pub fn begin_switch(
        &mut self,
        opens_raw: bool,
        previous_managed: bool,
        raw_size: Option<GridSize>,
    ) {
        self.raw_switch_pending = opens_raw;
        self.raw_switch_previous_managed = opens_raw && previous_managed;
        self.pending_raw_attachment = None;
        self.pending_raw_size = raw_size;
        if !opens_raw {
            self.reset_presentation();
        }
    }

    /// Abandon a raw switch without presenting it. Every field of the machine
    /// clears together: a stale target size or attachment would gate the next
    /// snapshot against a grid nobody asked for.
    pub fn clear_raw_switch(&mut self) {
        self.raw_switch_pending = false;
        self.pending_raw_attachment = None;
        self.pending_raw_size = None;
        self.raw_switch_previous_managed = false;
    }

    /// Adopt an attachment: the epoch every later stream event is checked
    /// against, the transcript's visibility, and a composer bound to this
    /// session.
    pub fn attach(&mut self, session_id: SessionId, epoch: u64, managed: bool) {
        self.active_epoch = epoch;
        self.session_is_managed = managed;
        self.managed.set_visible(managed);
        self.pending.attach(session_id, Vec::new());
    }

    /// A raw controller was granted (`Attached`): arm the sized-snapshot wait
    /// with the identity and epoch every later stream event is checked
    /// against, and answer the grid the host must now ask the backend to
    /// resize to. Keep the last complete frame on screen until the first
    /// snapshot for this controller is ready — render commits identity, model
    /// and pixels together.
    ///
    /// Relocated from the host's `Attached` arm at the W3 extraction, so the
    /// gate's arming and its decision ([`apply_render`](Self::apply_render))
    /// live behind one boundary.
    pub fn begin_raw_attachment(&mut self, session_id: SessionId, epoch: u64) -> GridSize {
        self.raw_switch_pending = true;
        self.pending_raw_attachment = Some((session_id, epoch));
        if self.pending_raw_size.is_none() {
            self.pending_raw_size = Some(self.running_grid_size());
        }
        self.pending_raw_size
            .expect("raw attachment established its requested size")
    }

    /// **The sized-snapshot gate** (ADR-263), and what follows it: apply one
    /// render update to this surface's own model, or refuse it.
    ///
    /// The host has already answered the *surface identity* half of the guard
    /// — this update's session is one this surface owns or waits for. What is
    /// decided here is the rest: a pending raw switch presents nothing until a
    /// snapshot arrives whose size equals the grid it asked for and whose
    /// session matches the pending attachment, and a delta or a gap can never
    /// end the wait (only a `Snapshot` carries a size). A refused update is
    /// dropped, not queued.
    ///
    /// Relocated from the host's render arm at the W3 extraction — the same
    /// checks, in the same order, behind the surface boundary.
    pub fn apply_render(&mut self, update: RenderUpdate) -> RenderOutcome {
        let session = match &update {
            RenderUpdate::Snapshot { snapshot } => snapshot.session_id,
            RenderUpdate::Delta { delta } => delta.session_id,
            RenderUpdate::Gap { gap } => gap.session_id,
        };
        let incoming = match &update {
            RenderUpdate::Snapshot { snapshot } => Some(snapshot.size),
            RenderUpdate::Delta { .. } | RenderUpdate::Gap { .. } => None,
        };
        if self.raw_switch_pending {
            let Some((pending, epoch)) = self.pending_raw_attachment else {
                // Attached is deliberately published before the render reader
                // starts. Ignore a defensive out-of-order frame rather than
                // exposing it under the previous identity.
                return RenderOutcome::Dropped;
            };
            if pending != session {
                return RenderOutcome::Dropped;
            }
            let desired = self
                .pending_raw_size
                .unwrap_or_else(|| self.running_grid_size());
            if incoming != Some(desired) {
                // Keep the last complete presentation visible. Resize was
                // requested when Attached arrived; it is host-atomic and
                // answers with an authoritative snapshot.
                return RenderOutcome::Dropped;
            }
            self.reset_presentation();
            self.session = Some(session);
            self.attach(session, epoch, false);
            self.status = PaneStatus::Running;
            if self.view.apply(update).is_err() {
                self.notice = Some(t(self.lang, I18nKey::TerminalFailed).into());
            } else {
                let eligible = self.blink_eligible();
                let _ = self.reset_cursor_blink(eligible, Instant::now());
                self.delta_received = Some(Instant::now());
                self.clear_raw_switch();
            }
            return RenderOutcome::SwitchPresented { session };
        }
        if self.session_is_managed {
            // Managed provider events are never interpreted as terminal cells.
            return RenderOutcome::Dropped;
        }
        if self.view.apply(update).is_err() {
            self.notice = Some(t(self.lang, I18nKey::TerminalFailed).into());
            return RenderOutcome::ApplyFailed;
        }
        let eligible = self.blink_eligible();
        let _ = self.reset_cursor_blink(eligible, Instant::now());
        self.delta_received = Some(Instant::now());
        let size = self.grid_size();
        let managed = self.session_is_managed;
        let _ = self.resize_view(managed, size);
        RenderOutcome::Applied
    }

    /// Whether the caret should be blinking. `host_eligible` carries the
    /// window's half of the question — focus, occlusion, the pane's state —
    /// and the surface adds its own: a switch in flight has no caret to blink,
    /// and the authored preference wins.
    pub fn cursor_blink_active(&self, host_eligible: bool) -> bool {
        host_eligible && !self.raw_switch_pending && self.view.cursor_blinks()
    }

    /// Put the cursor back in its authored visible phase and restart the
    /// quiet interval. Input and fresh host output both use this path: a caret
    /// must not disappear immediately after the person or the PTY moved it.
    pub fn reset_cursor_blink(&mut self, host_eligible: bool, now: Instant) -> bool {
        self.cursor_blink_visible = true;
        let changed = self.view.set_cursor_visible(true);
        self.cursor_blink_deadline = self
            .cursor_blink_active(host_eligible)
            .then_some(now + CURSOR_BLINK_INTERVAL);
        changed
    }

    /// Reconcile focus/lifecycle state without advancing the phase. Inactive
    /// surfaces keep a visible cursor and no deadline, so an unfocused or
    /// covered Terminal costs no timer wakeups.
    pub fn sync_cursor_blink(&mut self, host_eligible: bool, now: Instant) -> bool {
        if !self.cursor_blink_active(host_eligible) {
            self.cursor_blink_deadline = None;
            self.cursor_blink_visible = true;
            return self.view.set_cursor_visible(true);
        }
        if self.cursor_blink_deadline.is_none() {
            self.cursor_blink_deadline = Some(now + CURSOR_BLINK_INTERVAL);
            self.cursor_blink_visible = true;
            return self.view.set_cursor_visible(true);
        }
        false
    }

    pub fn maintain_cursor_blink(&mut self, host_eligible: bool, now: Instant) -> bool {
        let changed = self.sync_cursor_blink(host_eligible, now);
        let Some(deadline) = self.cursor_blink_deadline else {
            return changed;
        };
        if now < deadline {
            return changed;
        }
        self.cursor_blink_visible = !self.cursor_blink_visible;
        self.cursor_blink_deadline = Some(now + CURSOR_BLINK_INTERVAL);
        self.view.set_cursor_visible(self.cursor_blink_visible) || changed
    }

    /// The cell under a window point, in the pane the window handed over. A
    /// switch in flight answers `None`: the pixels on screen belong to the
    /// session that is leaving.
    ///
    /// **The point is made relative to the padded content**, which is the same
    /// origin `draw_pane` hands the view — a hit test measured from the pane
    /// would put every selection one column right of the glyph clicked, and a
    /// press in the padding would select column 0 instead of nothing.
    pub fn terminal_point(&self, terminal: Rect, x: f32, y: f32) -> Option<CellPoint> {
        if self.raw_switch_pending {
            return None;
        }
        let content = Self::content_rect(terminal);
        let geometry = self.view.geometry()?;
        geometry.hit_test(Point::new(
            f64::from(x - content.x),
            f64::from(y - content.y),
        ))
    }

    /// Draw whichever of the three presentations this surface currently owns.
    /// The order is the invariant: while a raw switch is pending the previous
    /// frame stays on screen — the managed transcript that was there, or the
    /// raw model that was there — and only a bare surface falls through to the
    /// terminal background.
    pub fn draw_pane(
        &mut self,
        canvas: &mut Scene,
        engine: &mut TextEngine,
        root: Affine,
        terminal: Rect,
        scale: f32,
        selected_is_managed: bool,
    ) -> PaneFrame {
        let bounds = KurboRect::new(
            f64::from(terminal.x),
            f64::from(terminal.y),
            f64::from(terminal.x + terminal.w),
            f64::from(terminal.y + terminal.h),
        );
        // The managed transcript is handed the whole pane — it insets its own
        // items by the same constant, and its background is the panel's rather
        // than the terminal's. The raw grid is handed the padded content, so the
        // pane is filled first: `TerminalView::draw` paints only what it is
        // given, and the two side strips would otherwise show the window
        // through them.
        let content = Self::content_rect(terminal);
        let content_bounds = KurboRect::new(
            f64::from(content.x),
            f64::from(content.y),
            f64::from(content.x + content.w),
            f64::from(content.y + content.h),
        );
        // **The pane fill and the grid's default-background cells are the same
        // colour by construction**, not by two call sites happening to read the
        // same palette field. A default-background cell paints nothing of its
        // own and shows the ground under it, so a difference between them would
        // be a box of the wrong shade behind every run of ordinary text.
        let background = self.view.background();
        let fill_pane = |canvas: &mut Scene| {
            scene::fill(
                canvas,
                root,
                background,
                &scene::rect(terminal.x, terminal.y, terminal.w, terminal.h),
            );
        };
        let pending_has_previous = self.raw_switch_pending
            && if self.raw_switch_previous_managed {
                true
            } else {
                self.view.model().is_some()
            };
        let mut raw_work = None;
        if self.raw_switch_pending && self.raw_switch_previous_managed {
            let outcome = self.managed.draw(engine, canvas, root, bounds, scale);
            self.managed_presentation = outcome.presentation;
        } else if self.raw_switch_pending && self.view.model().is_some() {
            fill_pane(canvas);
            raw_work = Some(self.view.draw(engine, canvas, root, content_bounds, scale));
            self.managed_presentation = None;
        } else if self.raw_switch_pending {
            fill_pane(canvas);
            self.managed_presentation = None;
        } else if selected_is_managed {
            let outcome = self.managed.draw(engine, canvas, root, bounds, scale);
            self.managed_presentation = outcome.presentation;
        } else {
            fill_pane(canvas);
            raw_work = Some(self.view.draw(engine, canvas, root, content_bounds, scale));
            self.managed_presentation = None;
        }
        PaneFrame {
            raw_work,
            pending_has_previous,
        }
    }

    /// Whether this surface has nothing of its own to show, and the window
    /// should name the pane's state instead.
    pub fn shows_status(&self, frame: &PaneFrame, selected_is_managed: bool) -> bool {
        (self.raw_switch_pending && !frame.pending_has_previous)
            || (!selected_is_managed && self.view.model().is_none())
    }

    /// Stage this frame's experience sample. A switch that has not presented
    /// keeps its timer running: the sample belongs to the frame that finally
    /// shows the new session, not to the ones that kept the old one.
    pub fn record_frame(
        &mut self,
        first_present_ms: Option<u64>,
        render_cpu_ms: u64,
        selected_is_managed: bool,
    ) {
        self.pending_frame_managed = selected_is_managed;
        let switch_present_ms = if self.raw_switch_pending {
            None
        } else {
            self.switch_started
                .take()
                .map(|started| started.elapsed().as_millis() as u64)
        };
        self.pending_experience = Some(ExperienceSample {
            first_present_ms,
            switch_present_ms,
            delta_present_ms: self
                .delta_received
                .take()
                .map(|received| received.elapsed().as_millis() as u64),
            render_cpu_ms: Some(render_cpu_ms),
            ..ExperienceSample::default()
        });
    }

    /// Acknowledge the transcript the compositor actually showed. Attention is
    /// only ever acknowledged from here.
    pub fn presented_managed(&mut self) {
        if let Some(token) = self.managed_presentation.take() {
            self.managed.presented(token);
        }
    }

    /// The sample for the frame that just reached the screen, with the stream
    /// gaps that a managed frame accumulated behind it.
    pub fn take_experience_sample(&mut self) -> Option<ExperienceSample> {
        let mut sample = self.pending_experience.take()?;
        if self.pending_frame_managed {
            sample.stream_gaps = Some(std::mem::take(&mut self.stream_gaps_since_present));
        }
        Some(sample)
    }

    pub fn record_stream_gap(&mut self) {
        self.stream_gaps_since_present = self.stream_gaps_since_present.saturating_add(1);
    }
}

fn pending_copy(lang: Lang) -> PendingInputCopy {
    PendingInputCopy {
        heading: t(lang, I18nKey::TerminalPendingInput).into(),
        placeholder: t(lang, I18nKey::TerminalInputPlaceholder).into(),
        add_attachment: t(lang, I18nKey::TerminalAddImage).into(),
        send: t(lang, I18nKey::TerminalSend).into(),
        retry: t(lang, I18nKey::TerminalRetry).into(),
        remove: t(lang, I18nKey::TerminalRemoveImage).into(),
        ingesting: t(lang, I18nKey::TerminalImageAdding).into(),
        rejected: t(lang, I18nKey::TerminalImageRejected).into(),
        delivering: t(lang, I18nKey::TerminalImageSending).into(),
        delivered: t(lang, I18nKey::TerminalImageSent).into(),
        delivery_failed: t(lang, I18nKey::TerminalImageDeliveryFailed).into(),
        delivery_native: t(lang, I18nKey::TerminalImageDeliveryNative).into(),
        delivery_path: t(lang, I18nKey::TerminalImageDeliveryPath).into(),
        delivery_unsupported: t(lang, I18nKey::TerminalImageDeliveryUnsupported).into(),
        native_short: t(lang, I18nKey::TerminalImageNativeShort).into(),
        path_short: t(lang, I18nKey::TerminalImagePathShort).into(),
        unsupported_short: t(lang, I18nKey::TerminalImageUnsupportedShort).into(),
        limit_error: t(lang, I18nKey::TerminalAttachmentLimit).into(),
        invalid_image: t(lang, I18nKey::TerminalInvalidImage).into(),
        host_unavailable: t(lang, I18nKey::TerminalAgentUnavailable).into(),
        generic_error: t(lang, I18nKey::TerminalInputFailed).into(),
    }
}

fn managed_copy(lang: Lang) -> ManagedSessionCopy {
    ManagedSessionCopy {
        tool_running: t(lang, I18nKey::TerminalManagedWorking).into(),
        tool_completed: t(lang, I18nKey::TerminalManagedToolSucceeded).into(),
        tool_failed: t(lang, I18nKey::TerminalManagedToolFailed).into(),
        tool_command: t(lang, I18nKey::TerminalManagedToolCommand).into(),
        tool_file_change: t(lang, I18nKey::TerminalManagedToolFileChange).into(),
        tool_permissions: t(lang, I18nKey::TerminalManagedToolPermissions).into(),
        tool_mcp: t(lang, I18nKey::TerminalManagedToolMcp).into(),
        tool_dynamic: t(lang, I18nKey::TerminalManagedToolDynamic).into(),
        tool_web_search: t(lang, I18nKey::TerminalManagedToolWebSearch).into(),
        tool_image_generation: t(lang, I18nKey::TerminalManagedToolImageGeneration).into(),
        tool_action: t(lang, I18nKey::TerminalManagedToolAction).into(),
        approval_required: t(lang, I18nKey::TerminalManagedApprovalTitle).into(),
        approval_too_large: t(lang, I18nKey::TerminalManagedApprovalTooLarge).into(),
        approve: t(lang, I18nKey::TerminalManagedApprovalAllow).into(),
        reject: t(lang, I18nKey::TerminalManagedApprovalDeny).into(),
        allow_remembered: t(lang, I18nKey::TerminalManagedApprovalAllowRemembered).into(),
        reject_remembered: t(lang, I18nKey::TerminalManagedApprovalDenyRemembered).into(),
        approval_submitting: t(lang, I18nKey::TerminalManagedApprovalSubmitting).into(),
        approval_unrepresentable: t(lang, I18nKey::TerminalManagedApprovalUnrepresentable).into(),
        attachment_accepted: t(lang, I18nKey::TerminalImageSent).into(),
        attachment_rejected: t(lang, I18nKey::TerminalImageRejected).into(),
        usage: t(lang, I18nKey::TerminalManagedUsage).into(),
        input_tokens: t(lang, I18nKey::TerminalManagedInputTokens).into(),
        output_tokens: t(lang, I18nKey::TerminalManagedOutputTokens).into(),
        cached_tokens: t(lang, I18nKey::TerminalManagedCachedInputTokens).into(),
        turn_running: t(lang, I18nKey::TerminalManagedWorking).into(),
        waiting_for_input: t(lang, I18nKey::TerminalManagedWaitingForInput).into(),
        turn_completed: t(lang, I18nKey::TerminalManagedCompleted).into(),
        turn_cancelled: t(lang, I18nKey::TerminalManagedCancelled).into(),
        turn_failed: t(lang, I18nKey::TerminalManagedProviderFailed).into(),
        provider_idle: t(lang, I18nKey::TerminalRunning).into(),
        provider_running: t(lang, I18nKey::TerminalManagedWorking).into(),
        provider_waiting_for_input: t(lang, I18nKey::TerminalManagedWaitingForInput).into(),
        provider_waiting_for_approval: t(lang, I18nKey::TerminalManagedWaitingForApproval).into(),
        provider_completed: t(lang, I18nKey::TerminalManagedCompleted).into(),
        provider_cancelled: t(lang, I18nKey::TerminalManagedCancelled).into(),
        provider_failed: t(lang, I18nKey::TerminalManagedProviderFailed).into(),
        stream_gap: t(lang, I18nKey::TerminalManagedTranscriptGap).into(),
        transcript_truncated: t(lang, I18nKey::TerminalManagedTranscriptTruncated).into(),
        provider_unavailable: t(lang, I18nKey::TerminalManagedProviderUnavailable).into(),
        provider_incompatible: t(lang, I18nKey::TerminalManagedProviderIncompatible).into(),
        model_unavailable: t(lang, I18nKey::DelegateModelUnavailable).into(),
        generic_error: t(lang, I18nKey::TerminalManagedProviderFailed).into(),
        cancel_turn: t(lang, I18nKey::TerminalCancel).into(),
        number: managed_number(lang),
        usage_value: if matches!(lang, Lang::ZhHans | Lang::Ja) {
            managed_usage_value_cjk
        } else {
            managed_usage_value
        },
        usage_list: if lang == Lang::Ar {
            managed_usage_list_ar
        } else {
            managed_usage_list
        },
    }
}

fn managed_usage_value(label: &str, value: &str) -> String {
    format!("{label}: {value}")
}

fn managed_usage_value_cjk(label: &str, value: &str) -> String {
    format!("{label}\u{ff1a}{value}")
}

fn managed_usage_list(values: &[String]) -> String {
    values.join(" \u{00b7} ")
}

fn managed_usage_list_ar(values: &[String]) -> String {
    values.join("\u{060c} ")
}

fn managed_number(lang: Lang) -> fn(u64) -> String {
    match lang {
        Lang::En => |value| koru_i18n::number::decimal(Lang::En, value),
        Lang::Es => |value| koru_i18n::number::decimal(Lang::Es, value),
        Lang::PtBr => |value| koru_i18n::number::decimal(Lang::PtBr, value),
        Lang::De => |value| koru_i18n::number::decimal(Lang::De, value),
        Lang::Fr => |value| koru_i18n::number::decimal(Lang::Fr, value),
        Lang::Ar => |value| koru_i18n::number::decimal(Lang::Ar, value),
        Lang::ZhHans => |value| koru_i18n::number::decimal(Lang::ZhHans, value),
        Lang::Ja => |value| koru_i18n::number::decimal(Lang::Ja, value),
    }
}

#[cfg(test)]
mod tests;
