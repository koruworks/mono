// SPDX-License-Identifier: MPL-2.0

//! **The run views** — Overview, Activity and Result for one delegated run,
//! and the handover a person takes the session through.
//!
//! You handed a piece of work to an agent and walked away. This surface is
//! how you come back to it: what you asked for, what it was given, where it
//! got to, every step it took, what it cost, what it produced, and — while
//! it is still working — the way to step in and the way to step back out
//! (ROADMAP §7.4, §10 P1.16).
//!
//! ## D36, which is this surface's to make visible
//!
//! There are two actions here that both look like stopping something, and
//! they are not the same thing at all:
//!
//! - **End the step and take the session.** The agent's current step stops;
//!   *the run does not*. You get the session, type in it, and give it back,
//!   and the agent carries on with what you asked for. It is recoverable,
//!   so it wears an ordinary chip and no confirmation stands in front of
//!   it — a confirmation on a recoverable action is what teaches people to
//!   click through the one that is not.
//! - **End the run.** The run is over. It cannot be resumed, and whatever
//!   it had not finished will not be produced. It wears the destructive
//!   face, it sits apart from the session controls, and it is the only
//!   control on this surface that opens a confirmation — one declaring no
//!   default, so Enter answers nothing until a button is deliberately
//!   chosen (ADR-265, row four).
//!
//! Both say what they would do **before** the click, in a line of ordinary
//! text under the control rather than in a tooltip: a tooltip is after —
//! you have to already be over the control, and on a touchpad you may never
//! be.
//!
//! ## Two rules about where work happens
//!
//! **Nothing on the window thread touches disk, the store or the network.**
//! The run is read through `koru_api_client::runs()` on a thread of this
//! surface's own ([`RunLinks`]), which is also the only way it is read: this
//! crate never opens a Run document, and `src/dependency.rs` fails the build
//! if it could.
//!
//! **A device that is not executing the run lays out no action at all**
//! (D15) — not a dimmed one with a tooltip: absent. It names the device
//! that is running the work, and says so in a sentence. A control that is
//! present but refuses is a promise the device cannot keep.
//!
//! ## What the surface may not do for itself
//!
//! Reading, ending, resuming and rating are calls this surface makes
//! through the typed client. The three handover verbs are not, and cannot
//! be: a controller lease belongs to the connection holding it, and in a
//! window that connection is the session backend's. A surface that opened
//! its own would take a second lease and the host would hold two — the
//! exact conflict §7.4 exists to prevent. So they are [`Request`]s, and the
//! host performs them on the connection that already owns the session.
//!
//! **Opening a result is a [`Request`] for the same kind of reason.** A
//! document in Koru opens as a tab, a tab belongs to the window, and which
//! strip it lands in was never a run view's to decide (ADR-368). So the
//! Result view names the version and the window opens it — into
//! `koru_artifacts_ui::ArtifactTab`, which reads the bytes and can save the
//! next version, and lives in the crate that owns the register path a save
//! goes through (ADR-369). **This crate has no second surface**: it never
//! asks for a result's bytes, and `src/dependency.rs` is why it could not
//! store one if it did.

mod action;
mod column;
mod dependency;
mod index;
mod links;
mod response;
mod state;
mod summary;
mod views;
mod words;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use koru_delegate_core::{
    ActorRef, DeviceId, ProfileId, Rating, RatingNote, RunId, SessionId, WorkspaceId,
};
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_i18n::time::{DateTimeParts, LocaleFormat};
use koru_i18n::{Key, Lang, t};
use koru_session_protocol::{HandoverState, TakeoverUrgency};
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, NamedKey};

pub use action::{OpenedResult, Operation, ReRun, Request};
pub use index::{DeletionAnswer, DeletionVerb, Motion, RunIndex, RunRow, TrashedRunRow, motion};
pub use koru_delegate_preamble::{Notice as PreambleNotice, Source as PreambleSource};
pub use koru_markdown_view::{Content as MarkdownContent, MarkdownView};
pub use links::{Answer, Command, MAX_HELD, RUN_SCOPES, RunLinks, Scripted, WINDOW};
pub use state::{
    Availability, PreambleView, RunFolder, SaveFailure, Saving, Snapshot, Standing, Tab, Takeover,
};
pub use summary::{Summary, failed_because, summary};

use action::Action;
use column::{Column, Control, Face, Laid, Line};
use views::Sections;

/// One delegated run, in three views.
pub struct RunsSurface {
    lang: Lang,
    /// The full locale convention for absolute scheduled-firing provenance.
    /// The host owns OS locale discovery; this surface only formats the wall
    /// clock parts it is handed through `koru-i18n`.
    locale: LocaleFormat,
    engine: TextEngine,
    links: RunLinks,
    run: Option<RunId>,
    workspace: Option<WorkspaceId>,
    snapshot: Option<Snapshot>,
    availability: Availability,
    /// The last thing the worker refused, in tokens.
    refusal: Option<state::Refusal>,
    tab: Tab,
    /// The delegated-agent instructions, once the worker has read them.
    /// Empty until the System prompt tab is opened: a person who never
    /// opens it never pays for the read.
    preamble: Option<PreambleView>,
    /// The instructions, drawn as the document they are, and the composer
    /// an edit to them is typed into.
    preamble_view: MarkdownView,
    /// Where a save of those instructions stands.
    saving: Saving,
    /// Whether the keyboard is in the composer rather than on the chips.
    editing_focused: bool,
    /// How many of the newest entries the Activity view holds.
    entries: u32,
    /// Where a takeover this surface asked for stands (§7.4).
    takeover: Takeover,
    /// Who is rating, when this window knows.
    profile: Option<ProfileId>,
    /// How a device is named at display time — the host resolves it; ids
    /// are what a run stores (PRINCIPLES, *a stable identifier never
    /// derives from a name*).
    device_names: BTreeMap<DeviceId, String>,
    /// The wall clock, set by the host. This crate reads no clock: a
    /// surface that read one would be a surface with a timer.
    now_ms: i64,
    /// The scheduled occurrence converted to this reader's local wall clock
    /// by the host. The epoch travels beside it so a stale conversion can
    /// never be shown for a newly read origin.
    occurrence_local: Option<(i64, DateTimeParts)>,
    note: TextInput,
    note_focused: bool,
    /// The note as the run holds it, so an edited one can be told from it.
    stored_note: String,
    confirm: Option<koru_ui::Confirm>,
    requests: Vec<Request>,
    viewport: (f32, f32),
    scroll: f32,
    laid: Laid,
    stale: bool,
    hover: Option<usize>,
    focus: Option<usize>,
    /// Whether this platform can show a folder in a file manager. The host
    /// answers; `false` until it does, so *Open location* is never on
    /// screen where pressing it would fail.
    can_reveal: bool,
    /// Whether this build lets a person start a delegation, which is what
    /// *Re-run* does. The host answers; `false` until it does.
    can_re_run: bool,
    /// The session tab this surface has already asked the host to open, so
    /// a repeated read does not ask twice.
    opened: Option<SessionId>,
    /// One explicit request to answer a provider question after a fresh read.
    response_requested: bool,
}

impl Default for RunsSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl RunsSurface {
    /// A surface with no worker behind it. What a test holds, and what a
    /// host with no gateway falls back to.
    pub fn new() -> Self {
        Self::with_links(RunLinks::inert())
    }

    pub fn with_links(links: RunLinks) -> Self {
        let mut engine = TextEngine::new();
        let note = TextInput::new(&mut engine, theme::role::row());
        Self {
            lang: Lang::En,
            locale: LocaleFormat::default(),
            engine,
            links,
            run: None,
            workspace: None,
            snapshot: None,
            availability: Availability::Loading,
            refusal: None,
            tab: Tab::default(),
            preamble: None,
            preamble_view: MarkdownView::new(),
            saving: Saving::default(),
            editing_focused: false,
            entries: WINDOW,
            takeover: Takeover::Idle,
            profile: None,
            device_names: BTreeMap::new(),
            now_ms: 0,
            occurrence_local: None,
            note,
            note_focused: false,
            stored_note: String::new(),
            confirm: None,
            requests: Vec::new(),
            viewport: (column::COLUMN_W, 600.0),
            scroll: 0.0,
            laid: Laid::default(),
            stale: true,
            hover: None,
            focus: None,
            can_reveal: false,
            can_re_run: false,
            opened: None,
            response_requested: false,
        }
    }

    /// A surface with a live worker for this build's gateway.
    pub fn for_host(segment: &str, root: Option<std::path::PathBuf>) -> Self {
        Self::with_links(RunLinks::open(segment, root))
    }

    // ------------------------------------------------------------- setters

    pub fn set_lang(&mut self, lang: Lang) {
        if self.lang != lang {
            self.lang = lang;
            self.locale = LocaleFormat::for_system(Some(lang.tag()), None);
            self.preamble_view.set_lang(lang);
            self.stale = true;
        }
    }

    /// The reader's complete locale convention. The host keeps OS discovery
    /// out of this leaf; this value supplies date order, clock preference and
    /// localized digits for a scheduled firing's absolute provenance.
    pub fn set_locale(&mut self, locale: &LocaleFormat) -> bool {
        let lang = locale.lang();
        if &self.locale == locale && self.lang == lang {
            return false;
        }
        self.locale.clone_from(locale);
        self.lang = lang;
        self.preamble_view.set_lang(lang);
        self.stale = true;
        true
    }

    /// The retained occurrence converted to local wall-clock fields by the
    /// host. `None` clears a prior run's conversion; no clock or timezone is
    /// read while this surface lays out or draws.
    pub fn set_occurrence_local(&mut self, local: Option<(i64, DateTimeParts)>) -> bool {
        if self.occurrence_local != local {
            self.occurrence_local = local;
            self.stale = true;
            true
        } else {
            false
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The wall clock, in epoch milliseconds. Every time this surface shows
    /// is relative to it, and it changes nothing else.
    pub fn set_now_ms(&mut self, now_ms: i64) {
        if self.now_ms != now_ms {
            self.now_ms = now_ms;
            self.stale = true;
        }
    }

    /// Who a rating is recorded as.
    pub fn set_profile(&mut self, profile: Option<ProfileId>) {
        self.profile = profile;
    }

    /// **Whether this platform can show a folder in a file manager at all.**
    ///
    /// The host answers, because the seam is the shell's
    /// (`koru_workspace_host::reveal`) and this crate is chartered not to
    /// know it exists. A platform that cannot must not put *Open location*
    /// on screen — a row that is there and fails when pressed is the outcome
    /// that policy exists to prevent, and it is the same rule the shell's own
    /// menu row follows.
    ///
    /// Defaults to `false`: a surface nobody has told is a surface that
    /// promises nothing.
    pub fn set_can_reveal(&mut self, can: bool) {
        if self.can_reveal != can {
            self.can_reveal = can;
            self.stale = true;
        }
    }

    /// **Whether this build lets a person start a delegation**, which is
    /// all *Re-run* does: it hands a seed to the window's one delegation
    /// surface. The host answers (`koru_features::delegate`, which this
    /// crate is chartered not to link), and a build that hides Delegate
    /// must not lay out a control the window would refuse. The run itself —
    /// its status, activity, result and every other control — is shown
    /// either way.
    ///
    /// Defaults to `false`, as [`Self::set_can_reveal`] does: a surface
    /// nobody has told promises nothing.
    pub fn set_can_re_run(&mut self, can: bool) {
        if self.can_re_run != can {
            self.can_re_run = can;
            self.stale = true;
        }
    }

    /// What a device is called today. The host resolves it; this surface
    /// stores none of it and shows the id to nobody.
    pub fn set_device_name(&mut self, device: DeviceId, name: String) {
        if self.device_names.get(&device) == Some(&name) {
            return;
        }
        self.device_names.insert(device, name);
        self.stale = true;
    }

    /// Where the session's handover stands, as the host reads it from the
    /// session catalog. **The surface never decides this** — §7.4 records a
    /// takeover only when the lease is granted, and the orchestrator is
    /// what records it; this is the surface being told.
    pub fn set_handover(&mut self, handover: HandoverState) {
        let next = Takeover::of(handover);
        if self.takeover != next {
            self.takeover = next;
            self.stale = true;
        }
        if next == Takeover::Released
            && let Some(session) = self.snapshot.as_ref().and_then(|s| s.session)
            && self.opened != Some(session)
        {
            // The holder let go: the session is this person's to open.
            self.opened = Some(session);
            self.requests.push(Request::OpenSession { session });
        }
        if next == Takeover::Idle {
            self.opened = None;
        }
    }

    // -------------------------------------------------------------- opening

    /// Follow a run. `workspace` is needed only for a run this device did
    /// not create — one that arrived by sync — and naming it always is
    /// harmless.
    pub fn open(&mut self, run: RunId, workspace: Option<WorkspaceId>) {
        if self.run == Some(run) && self.workspace == workspace {
            return;
        }
        self.run = Some(run);
        self.workspace = workspace;
        self.snapshot = None;
        self.availability = Availability::Loading;
        self.refusal = None;
        self.occurrence_local = None;
        self.entries = WINDOW;
        self.takeover = Takeover::Idle;
        self.opened = None;
        self.tab = Tab::Overview;
        self.response_requested = false;
        self.scroll = 0.0;
        self.stale = true;
        self.links.send(Command::Open { run, workspace });
    }

    /// Stop following anything.
    pub fn close(&mut self) {
        self.response_requested = false;
        self.run = None;
        self.snapshot = None;
        self.occurrence_local = None;
        self.availability = Availability::Loading;
        self.stale = true;
        self.links.send(Command::Close);
    }

    pub fn run(&self) -> Option<RunId> {
        self.run
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.scroll = 0.0;
            self.focus = None;
            self.stale = true;
            if tab == Tab::SystemPrompt {
                // **Read on the way in, never in a frame.** Opening the
                // tab is an input event; what it does is ask the worker
                // thread, which is where the file is read. Asked on every
                // opening rather than once, so what is on screen is what a
                // run started now would be given — the whole point of the
                // sentence at the top of the tab. Whatever was read last
                // stays up until the answer arrives, so there is no blink.
                self.links.send(Command::ReadPreamble);
            }
        }
    }

    /// The delegated-agent instructions this surface holds, if it has read
    /// them yet.
    pub fn preamble(&self) -> Option<&PreambleView> {
        self.preamble.as_ref()
    }

    /// Where a save of the instructions stands.
    pub fn saving(&self) -> &Saving {
        &self.saving
    }

    /// The instructions as a document — what a test reads the System prompt
    /// tab out of.
    pub fn preamble_view(&self) -> &MarkdownView {
        &self.preamble_view
    }

    /// What this device may do about the run it is showing (D15).
    pub fn standing(&self) -> Option<Standing> {
        self.snapshot.as_ref().map(Snapshot::standing)
    }

    pub fn takeover(&self) -> Takeover {
        self.takeover
    }

    /// The last refusal the worker named, as a stable code.
    pub fn refusal(&self) -> Option<(Operation, &str)> {
        self.refusal
            .as_ref()
            .map(|refusal| (refusal.operation, refusal.code.as_str()))
    }

    /// Drain what only the host can do.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Draw everything again: the palette, the density or the font moved
    /// under this surface.
    pub fn invalidate(&mut self) {
        self.engine = TextEngine::new();
        self.note = TextInput::new(&mut self.engine, theme::role::row());
        let stored = self.stored_note.clone();
        self.note.set_text(&mut self.engine, &stored);
        self.stale = true;
    }

    /// The pointer shape over this surface.
    pub fn cursor_icon(&self) -> winit::window::CursorIcon {
        self.cursor()
    }

    /// Pick up whatever the worker has said. Answers whether to repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for answer in self.links.drain() {
            changed = true;
            match answer {
                Answer::Reachable(true) => {
                    if self.snapshot.is_some() {
                        self.availability = Availability::Live;
                    }
                }
                Answer::Reachable(false) => self.availability = Availability::Down,
                Answer::Read {
                    run,
                    workspace,
                    view,
                    folder,
                } => {
                    if self.run != Some(run) {
                        continue;
                    }
                    let snapshot = Snapshot::from_view(run, workspace, *view, folder);
                    self.stored_note = snapshot
                        .header
                        .rating
                        .as_ref()
                        .and_then(|rating| rating.note.as_ref())
                        .map(|note| note.as_str().to_owned())
                        .unwrap_or_default();
                    if !self.note_focused {
                        let stored = self.stored_note.clone();
                        self.note.set_text(&mut self.engine, &stored);
                    }
                    self.snapshot = Some(snapshot);
                    self.availability = Availability::Live;
                    self.refusal = None;
                    self.respond_after_read();
                }
                Answer::Refused { operation, code } => {
                    self.response_requested = false;
                    self.refusal = Some(state::Refusal { operation, code });
                }
                Answer::Preamble(preamble) => self.take_preamble(preamble),
                Answer::PreambleSaved(answer) => match *answer {
                    Ok(preamble) => {
                        self.saving = Saving::Saved;
                        self.take_preamble(preamble);
                    }
                    Err(failure) => self.saving = Saving::Failed(failure),
                },
            }
        }
        if changed {
            self.stale = true;
        }
        changed
    }

    /// Adopt instructions the worker resolved. **The composer goes with
    /// them**: the file on disk is what a run would be given, and an edit
    /// of what it used to say is not something to keep offering to save.
    fn take_preamble(&mut self, preamble: PreambleView) {
        self.preamble_view.set_content(preamble.content.clone());
        self.preamble_view.set_lang(self.lang);
        self.editing_focused = false;
        self.preamble = Some(preamble);
    }

    // ------------------------------------------------------------- geometry

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        if self.viewport != (w, h) {
            self.viewport = (w, h);
            self.stale = true;
        }
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let step = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 48.0,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        let max = (self.laid.bottom - self.viewport.1 + column::TOP).max(0.0);
        self.scroll = (self.scroll - step).clamp(0.0, max);
    }

    pub fn content_bottom(&self) -> f32 {
        self.laid.bottom
    }

    /// The column the views are laid out in: centred, and never wider than
    /// text stays readable in.
    fn column_rect(&self) -> (f32, f32) {
        let width = column::COLUMN_W.min(self.viewport.0 - column::PAD_X * 2.0);
        let x = ((self.viewport.0 - width) * 0.5).max(column::PAD_X);
        (x, width.max(120.0))
    }

    // ------------------------------------------------------------- the pass

    /// Lay the whole surface out once, if anything has moved.
    pub fn relayout(&mut self) {
        if !self.stale {
            return;
        }
        self.stale = false;
        let (x, w) = self.column_rect();
        let lang = self.lang;
        let now = self.now_ms;
        let mut engine = std::mem::replace(&mut self.engine, TextEngine::new());
        // The document surface comes out of `self` for the pass, because
        // laying it out is `&mut` and so is every measurement beside it.
        let mut preamble_view = std::mem::take(&mut self.preamble_view);
        let mut column = Column::new(&mut engine, x, w, column::TOP);
        // **What this run is called**, above the views rather than inside
        // one: it is the same in all three, and it is what a person says
        // when they mean this run. The objective — what was *asked for* —
        // is the Overview's own section, and stays there.
        if let Some(run) = self.run {
            column.title(&koru_delegate_core::RunName::of(run).to_string());
        }
        column.tabs(Tab::ALL.map(|tab| {
            (
                Action::Show(tab),
                t(
                    lang,
                    match tab {
                        Tab::Overview => Key::RunTabOverview,
                        Tab::Activity => Key::RunTabActivity,
                        Tab::Result => Key::RunTabResult,
                        Tab::SystemPrompt => Key::RunTabSystemPrompt,
                    },
                )
                .to_owned(),
                tab == self.tab,
            )
        }));
        column.gap(column::SECTION_GAP);
        // **The instructions are not about this run**, so they draw before
        // the run is read and on a device that will never read it.
        if self.tab == Tab::SystemPrompt {
            views::system_prompt(
                &mut column,
                lang,
                self.preamble.as_ref(),
                &mut preamble_view,
                &self.saving,
            );
        } else {
            match (&self.snapshot, self.availability) {
                (None, Availability::Down) => {
                    column.wrapped(Line::Body, t(lang, Key::RunActivityUnavailable))
                }
                (None, _) => column.wrapped(Line::Body, t(lang, Key::RunActivityLoading)),
                (Some(snapshot), _) => {
                    let context = Sections {
                        lang,
                        locale: &self.locale,
                        now,
                        snapshot,
                        occurrence_local: self.occurrence_local,
                        takeover: self.takeover,
                        device_names: &self.device_names,
                        availability: self.availability,
                        note_differs: self.note.value() != self.stored_note,
                        can_reveal: self.can_reveal,
                        can_re_run: self.can_re_run,
                    };
                    match self.tab {
                        Tab::Overview => context.overview(&mut column),
                        Tab::Activity => context.activity(&mut column, self.entries),
                        Tab::Result => context.result(&mut column),
                        // Drawn above; the run is not consulted for it.
                        Tab::SystemPrompt => {}
                    }
                }
            }
        }
        self.laid = column.finish();
        self.engine = engine;
        self.preamble_view = preamble_view;
        if self
            .focus
            .is_some_and(|index| index >= self.laid.controls.len())
        {
            self.focus = None;
        }
    }

    // --------------------------------------------------------------- input

    /// Whether a confirmation is standing in front of everything.
    pub fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// What Enter would do in the confirmation, as a value a test can
    /// assert on. `None` is ADR-265 row four: nothing, until a button is
    /// deliberately chosen.
    pub fn confirmation_default(&self) -> Option<koru_ui::confirm::Outcome> {
        self.confirm.as_ref().and_then(|c| c.default_action())
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        self.relayout();
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.mouse_down(&mut self.engine, self.viewport, x, y);
            return self.answer_confirmation(outcome);
        }
        let hit = self.control_at(x, y);
        if let Some(index) = hit {
            self.focus = Some(index);
            let control = &self.laid.controls[index];
            if !control.enabled {
                return true;
            }
            if matches!(control.face, Face::Field) {
                self.note_focused = true;
                let rect = control.rect;
                // The rect is in the column's own space and the pointer is
                // in the viewport's: the caret lands where the letter is
                // only if the scroll is added back, exactly as the hit test
                // added it to find this control at all.
                self.note.click(&mut self.engine, rect, x, y + self.scroll);
                return true;
            }
            let action = control.action.clone();
            self.note_focused = false;
            self.press(action);
            return true;
        }
        if let Some((mx, my)) = self.laid.markdown {
            let mut engine = std::mem::replace(&mut self.engine, TextEngine::new());
            let mut view = std::mem::take(self.document_mut());
            // The column's own space, so the scroll goes back on exactly as
            // the control hit test above put it on.
            let landed = view.mouse_down(&mut engine, (mx, my), x, y + self.scroll);
            *self.document_mut() = view;
            self.engine = engine;
            if landed {
                self.editing_focused = true;
                self.focus = None;
                self.stale = true;
                return true;
            }
        }
        if self.note_focused {
            self.note_focused = false;
            self.note.blur(&mut self.engine);
            return true;
        }
        false
    }

    pub fn mouse_up(&mut self) -> bool {
        self.note.release();
        self.document_mut().mouse_up();
        false
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some(confirm) = &mut self.confirm {
            return confirm.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        let was = self.hover;
        self.hover = self.control_at(x, y);
        was != self.hover
    }

    pub fn cursor(&self) -> winit::window::CursorIcon {
        if let Some(confirm) = &self.confirm {
            return confirm.cursor();
        }
        match self.hover {
            Some(_) => koru_ui::cursor::PRESS,
            None => koru_ui::cursor::IDLE,
        }
    }

    /// **Every control is reachable from the keyboard.**
    ///
    /// One ordered list of enabled targets — the same list the pointer hits
    /// — walked by Tab and Shift+Tab, activated by Enter or Space, with
    /// Escape letting go. There is no shared focus model in Koru's chrome
    /// to join (DESIGN), so this surface owns its own and keeps it to the
    /// list it already has.
    pub fn key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        self.relayout();
        if let Some(confirm) = &mut self.confirm {
            let outcome = confirm.key(key);
            return self.answer_confirmation(outcome);
        }
        // **The composer holds the keys while it holds the keyboard**, with
        // two ways out that every other surface in Koru also has: Tab moves
        // to the controls, Escape lets go.
        if self.editing_focused && self.document_mut().is_editing() {
            match key {
                WinitKey::Named(NamedKey::Escape) | WinitKey::Named(NamedKey::Tab) => {
                    self.editing_focused = false;
                    if matches!(key, WinitKey::Named(NamedKey::Tab)) {
                        self.step_focus(!mods.shift_key());
                    }
                    self.stale = true;
                    return true;
                }
                _ => {
                    let mut engine = std::mem::replace(&mut self.engine, TextEngine::new());
                    let mut view = std::mem::take(self.document_mut());
                    let handled = view.key(&mut engine, key, text, mods);
                    *self.document_mut() = view;
                    self.engine = engine;
                    self.stale = true;
                    return handled;
                }
            }
        }
        if self.note_focused {
            match key {
                WinitKey::Named(NamedKey::Escape) => {
                    self.note_focused = false;
                    self.note.blur(&mut self.engine);
                    return true;
                }
                WinitKey::Named(NamedKey::Enter) => {
                    self.press(Action::SaveNote);
                    return true;
                }
                _ => {
                    let typed = self.note.key(&mut self.engine, key, text, mods);
                    self.stale = true;
                    return matches!(typed, koru_ui::text_field::Typed::Handled);
                }
            }
        }
        match key {
            WinitKey::Named(NamedKey::Tab) => {
                self.step_focus(!mods.shift_key());
                true
            }
            WinitKey::Named(NamedKey::ArrowRight) | WinitKey::Named(NamedKey::ArrowDown) => {
                self.step_focus(true);
                true
            }
            WinitKey::Named(NamedKey::ArrowLeft) | WinitKey::Named(NamedKey::ArrowUp) => {
                self.step_focus(false);
                true
            }
            WinitKey::Named(NamedKey::Enter) | WinitKey::Named(NamedKey::Space) => {
                let Some(index) = self.focus else {
                    return false;
                };
                let Some(control) = self.laid.controls.get(index) else {
                    return false;
                };
                if !control.enabled {
                    return true;
                }
                if matches!(control.face, Face::Field) {
                    self.note_focused = true;
                    return true;
                }
                let action = control.action.clone();
                self.press(action);
                true
            }
            WinitKey::Named(NamedKey::Escape) if self.focus.is_some() => {
                self.focus = None;
                true
            }
            _ => false,
        }
    }

    fn step_focus(&mut self, forward: bool) {
        self.focus = self.laid.step(self.focus, forward);
        self.stale = true;
    }

    /// The word on the control the ring is sitting on, for a keyboard test.
    #[cfg(test)]
    fn focused_label(&self) -> Option<String> {
        let index = self.focus?;
        match &self.laid.controls.get(index)?.face {
            column::Face::Tab(label, _)
            | column::Face::Chip(label)
            | column::Face::Danger(label)
            | column::Face::Rating(label, _, _) => Some(label.clone()),
            column::Face::Field => None,
        }
    }

    fn control_at(&self, x: f32, y: f32) -> Option<usize> {
        // The column's own space: the scroll goes back on exactly as the
        // layout pass took it off.
        self.laid.control_at(x, y + self.scroll)
    }

    /// Press one control by the word on it — how a test presses one, in the
    /// language the surface is drawing in.
    pub fn press_label(&mut self, label: &str) -> bool {
        self.relayout();
        let found = self
            .laid
            .controls
            .iter()
            .find(|control| control.enabled && control.label() == label)
            .map(|control| control.action.clone());
        match found {
            Some(action) => {
                self.press(action);
                true
            }
            None => false,
        }
    }

    /// Whether a control with this word is laid out at all. **What D15's
    /// test asks**: on a device that is not executing, the answer is no,
    /// because there is nothing there to press.
    pub fn has_control(&mut self, label: &str) -> bool {
        self.relayout();
        self.laid
            .controls
            .iter()
            .any(|control| control.label() == label)
    }

    /// Every line of text this surface is showing, in layout order — what a
    /// test reads a run out of (UC3). A rendered document's own lines are
    /// part of it, in the place the column put them.
    pub fn lines(&mut self) -> Vec<String> {
        self.relayout();
        let mut lines: Vec<String> = self
            .laid
            .paints
            .iter()
            .map(|paint| paint.text.clone())
            .collect();
        if self.laid.markdown.is_some() {
            lines.extend(self.preamble_view.lines());
        }
        lines
    }

    /// Every control's word, in layout order.
    pub fn control_labels(&mut self) -> Vec<String> {
        self.relayout();
        self.laid
            .controls
            .iter()
            .map(|control| control.label().to_owned())
            .collect()
    }

    fn press(&mut self, action: Action) {
        let lang = self.lang;
        match action {
            Action::Show(tab) => self.set_tab(tab),
            Action::ShowEarlier => {
                self.entries = (self.entries + WINDOW).min(MAX_HELD);
                self.links.send(Command::Window {
                    entries: self.entries,
                });
                self.stale = true;
            }
            Action::TakeControl => self.ask_for_session(TakeoverUrgency::AtTurnBoundary),
            Action::TakeControlNow => self.ask_for_session(TakeoverUrgency::Now),
            Action::GiveBackControl => {
                if let Some(session) = self.snapshot.as_ref().and_then(|s| s.session) {
                    self.opened = None;
                    self.requests.push(Request::GiveBackControl { session });
                }
            }
            Action::OpenSession => {
                if let Some(session) = self.snapshot.as_ref().and_then(|s| s.session) {
                    self.opened = Some(session);
                    self.requests.push(Request::OpenSession { session });
                }
            }
            Action::AskToEndRun => {
                // **The one confirmation on this surface**, and the only
                // irreversible action behind it. Its body says what ending
                // the run costs *and* names the recoverable action beside
                // it, because the mistake this guards against is somebody
                // reaching for it when they meant to take the session.
                self.confirm = Some(
                    koru_ui::Confirm::localized(
                        t(lang, Key::RunEndRunTitle),
                        t(lang, Key::RunEndRunBody),
                        t(lang, Key::RunEndRun),
                        t(lang, Key::RunCancelAction),
                    )
                    .declaring_no_default(),
                );
                self.stale = true;
            }
            Action::Resume => {
                self.links.send(Command::Resume);
                self.stale = true;
            }
            Action::OpenResult(index) => self.ask_for_result(index),
            Action::ShowMoreMarkdown => {
                self.document_mut().reveal_more();
                self.stale = true;
            }
            Action::EditPreamble => {
                let mut engine = std::mem::replace(&mut self.engine, TextEngine::new());
                self.preamble_view.begin_edit(&mut engine);
                self.engine = engine;
                self.editing_focused = true;
                self.saving = Saving::Idle;
                self.stale = true;
            }
            Action::StopEditingPreamble => {
                self.preamble_view.cancel_edit();
                self.editing_focused = false;
                self.stale = true;
            }
            Action::SavePreamble => {
                if let Some(text) = self.preamble_view.edited() {
                    self.saving = Saving::Sending;
                    self.links.send(Command::SavePreamble { text });
                    self.stale = true;
                }
            }
            // **Nothing here reaches the run.** The seed is read out of what
            // is already on screen and handed to the window, which owns the
            // one delegation surface; this run is not touched, not read again
            // and not told about it.
            Action::ReRun => {
                if let Some(seed) = self.re_run_seed() {
                    self.requests.push(Request::ReRun(Box::new(seed)));
                }
            }
            // **Nothing is spawned here.** The shell owns the one platform
            // seam that shows a path in a file manager, and it performs it
            // off the window thread; the control is not laid out at all
            // unless the folder was there when the run was last read.
            Action::RevealFolder => {
                if let Some(folder) = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.folder.as_ref())
                    .filter(|folder| folder.present())
                {
                    self.requests
                        .push(Request::RevealFolder(folder.path().into()));
                }
            }
            Action::Rate(rating) => self.rate(rating),
            Action::SaveNote => {
                if let Some(rating) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.header.rating.as_ref())
                    .map(|rating| rating.rating)
                {
                    self.rate(rating);
                }
                self.note_focused = false;
            }
        }
    }

    /// The one document this surface draws: the instructions on the System
    /// prompt tab. A run's *result* is a tab of its own (ADR-368).
    fn document_mut(&mut self) -> &mut MarkdownView {
        &mut self.preamble_view
    }

    /// **Ask the window to open one of the run's results as a tab**
    /// (ADR-368).
    ///
    /// Nothing is read here and nothing on this surface changes: the Result
    /// view stays exactly as the person left it, and the document opens
    /// where every other document in Koru opens. A result whose bytes are
    /// on another device is named from what the run already says (D27) —
    /// the run names the device and this surface knows what it is called,
    /// so the tab never has to ask a gateway a question it would refuse.
    fn ask_for_result(&mut self, index: usize) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let Some(result) = snapshot.header.artifacts.get(index) else {
            return;
        };
        let here = result.held_by == snapshot.device;
        let held_elsewhere = (!here).then(|| self.device_name(result.held_by));
        let opened = OpenedResult {
            run: snapshot.run,
            version: result.artifact,
            title: result.title.as_str().to_owned(),
            size_bytes: result.size_bytes,
            held_elsewhere,
        };
        self.requests.push(Request::OpenResult(Box::new(opened)));
    }

    /// What a device is called today, as the host told this surface.
    fn device_name(&self, device: DeviceId) -> String {
        self.device_names
            .get(&device)
            .cloned()
            .unwrap_or_else(|| t(self.lang, Key::RunDeviceUnknown).to_owned())
    }

    /// **What this run was asked for**, for a person to ask for again.
    ///
    /// The pins are filtered here because this is the only place that knows
    /// what each of them points at now: a source in the trash or gone would
    /// be refused by admission (D48), and putting a person in front of a run
    /// that cannot start and telling them so afterwards is the failure that
    /// refusal exists to prevent. An empty state list means this device could
    /// not resolve the sources at all, and everything is carried unchanged --
    /// not knowing is not the same as gone (D46).
    fn re_run_seed(&self) -> Option<crate::ReRun> {
        let snapshot = self.snapshot.as_ref()?;
        let workspace = snapshot.workspace?;
        let header = &snapshot.header;
        let known = !snapshot.context.is_empty();
        let context: Vec<_> = header
            .context
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                !known
                    || snapshot.context.get(*index).copied()
                        == Some(koru_delegate_core::ContextState::Live)
            })
            .map(|(_, item)| item.clone())
            .collect();
        Some(crate::ReRun {
            run: snapshot.run,
            workspace,
            objective: header.objective.clone(),
            agent: header.agent.clone(),
            requested_model: header.requested_model.clone(),
            scope: header.scope.clone(),
            dropped: known && context.len() != header.context.len(),
            context,
            grants: header.grants.clone(),
        })
    }

    /// §7.4's request, in the two urgencies D36 distinguishes. Neither ends
    /// the run, and the surface says so under each of them before the
    /// click; what it does **not** do is claim the takeover — that is the
    /// orchestrator's to record when the lease is granted.
    fn ask_for_session(&mut self, urgency: TakeoverUrgency) {
        let Some(session) = self.snapshot.as_ref().and_then(|s| s.session) else {
            return;
        };
        self.takeover = Takeover::Asked;
        self.stale = true;
        self.requests
            .push(Request::TakeControl { session, urgency });
    }

    fn rate(&mut self, rating: Rating) {
        let Some(profile) = self.profile else {
            return;
        };
        let typed = self.note.value();
        let note = RatingNote::new(typed.trim())
            .ok()
            .filter(|_| !typed.trim().is_empty());
        self.links.send(Command::Rate {
            rating,
            note,
            rated_by: ActorRef::Person { profile },
        });
        self.stale = true;
    }

    fn answer_confirmation(&mut self, outcome: koru_ui::confirm::Outcome) -> bool {
        match outcome {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            koru_ui::confirm::Outcome::Keep | koru_ui::confirm::Outcome::Alternative => true,
            koru_ui::confirm::Outcome::Cancel => {
                self.confirm = None;
                self.stale = true;
                true
            }
            koru_ui::confirm::Outcome::Confirm => {
                self.confirm = None;
                self.links.send(Command::EndRun);
                self.stale = true;
                true
            }
        }
    }

    // ---------------------------------------------------------------- paint

    pub fn draw(&mut self, scene_out: &mut Scene, root: Affine) {
        self.relayout();
        let (vw, vh) = self.viewport;
        let max = (self.laid.bottom - vh + column::TOP).max(0.0);
        self.scroll = self.scroll.min(max);
        let p = theme::p();
        scene::fill(scene_out, root, p.bg, &scene::rect(0.0, 0.0, vw, vh));
        let shifted = root * Affine::translate((0.0, -(self.scroll as f64)));

        let paints = std::mem::take(&mut self.laid.paints);
        for paint in &paints {
            let laid = match paint.max_width {
                Some(width) => self
                    .engine
                    .simple_layout_wrapped(&paint.text, paint.style(), width),
                None => self.engine.simple_layout(&paint.text, paint.style()),
            };
            scene::draw_layout(
                scene_out,
                shifted,
                &laid,
                paint.x,
                paint.y,
                Some(paint.tone()),
            );
        }
        self.laid.paints = paints;

        // The document, if this pass laid one out. It measured itself in
        // `relayout`; here it only draws what it already measured.
        if let Some((mx, my)) = self.laid.markdown {
            let mut engine = std::mem::replace(&mut self.engine, TextEngine::new());
            let mut view = std::mem::take(self.document_mut());
            view.draw(scene_out, shifted, &mut engine, mx, my);
            *self.document_mut() = view;
            self.engine = engine;
        }

        let controls = std::mem::take(&mut self.laid.controls);
        for (index, control) in controls.iter().enumerate() {
            self.draw_control(
                scene_out,
                shifted,
                control,
                self.hover == Some(index),
                self.focus == Some(index),
            );
        }
        self.laid.controls = controls;

        if let Some(confirm) = &self.confirm {
            confirm.draw(scene_out, root, &mut self.engine, self.viewport);
        }
    }

    /// The note field is this surface's own — its text lives here, not in
    /// the control — so it is drawn here and everything else goes through
    /// the one shared painter both surfaces in this crate use.
    fn draw_control(
        &mut self,
        scene_out: &mut Scene,
        root: Affine,
        control: &Control,
        hovered: bool,
        focused: bool,
    ) {
        if matches!(control.face, Face::Field) {
            let r = control.rect;
            let mut note = std::mem::replace(
                &mut self.note,
                TextInput::new(&mut self.engine, theme::role::row()),
            );
            note.draw(
                scene_out,
                root,
                &mut self.engine,
                r,
                InputStyle::inline(),
                Some(t(self.lang, Key::RunRateNote)),
                self.note_focused,
            );
            self.note = note;
            return;
        }
        column::draw_control(scene_out, root, &mut self.engine, control, hovered, focused);
    }
}

/// Re-exported so a host can name what this surface refused without
/// reaching into the module.
pub use state::Refusal;
