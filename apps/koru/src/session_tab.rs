// SPDX-License-Identifier: MPL-2.0

//! A running shell or agent session, hosted as a main-window tab.
//!
//! The presentation is `koru_agents_ui::SessionSurface` — the surface Koru
//! Terminal hosted before it — and this module is koru.exe's adapter over it: the
//! [`View`] methods translated into surface calls, and [`SessionSender`], this
//! app's path from the surface to the per-window session backend
//! (`mainview/session_backend.rs` owns that backend and routes its events back
//! here).
//!
//! Follows the AI-chat-tab precedent (`ai_chat_view.rs`): the `&mut
//! WorkspaceDoc` argument is ignored on every method, because a session has no
//! document. The controller lease, the launch and every stream event are the
//! container's business — a tab only presents.
//!
//! ## What this adapter's boundary cannot carry (Koru Terminal's could)
//!
//! `View::key` speaks `(&Key, Option<&str>, ModifiersState)` with no physical
//! key, no release phase and no repeat flag, and the trait has no IME hook. So
//! a session tab in koru.exe sends every key as a non-repeating press, cannot
//! tell Ctrl+I from Ctrl+Tab on Windows (both arrive as logical Tab), and does
//! no preedit composition. Raw shells work fully on that vocabulary; the gaps
//! are recorded rather than papered over.

use std::time::Instant;

use koru_agents_ui::{PaneStatus, SessionSurface, SurfaceFocus};
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_i18n::t;
use koru_session_backend::InteractionSender;
use koru_session_profiles::LaunchProfile;
use koru_session_protocol::{
    GridSize, KeyModifiers, KeyStroke, MouseButton as ProtocolButton, ProjectId, SessionId,
    SessionProfileKey, TerminalKey,
};
use koru_session_view::{
    CellPoint, ClipboardAction, KeyPhase, ManagedKey, PointerModifiers, SessionSurfaceIntent,
    SessionSurfaceInteraction, TerminalKeyEvent, TerminalKeyInput, TerminalShortcut,
};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// Koru's path from the surface to the session backend — a newtype over the
/// backend crate's [`InteractionSender`], the same seam Koru Terminal's
/// `TerminalSender` is.
#[derive(Clone)]
pub struct SessionSender(pub InteractionSender);

impl SessionSurfaceInteraction for SessionSender {
    fn dispatch(&mut self, intent: SessionSurfaceIntent) {
        self.0.dispatch(intent);
    }
}

impl koru_agents_ui::SurfaceInteraction for SessionSender {
    fn resize(&self, size: GridSize) {
        self.0.resize(size);
    }

    fn clipboard(&self, action: ClipboardAction) {
        self.0.terminal_clipboard(action);
    }
}

/// A launch this tab was opened for, sent when the tab first becomes active —
/// only then has the surface been measured, and a grid asked for before that
/// is a resize the person watches happen.
#[derive(Clone)]
pub struct PendingLaunch {
    pub profile: LaunchProfile,
    pub raw_fallback: bool,
    /// The project this session starts in, riding the creation request
    /// (ADR-264: placement travels inside creation, never a follow-up
    /// assign). `None` is the Unassigned bucket — the `+` menu's case.
    pub project: Option<ProjectId>,
    /// Where the session's process starts, when the launch names one: a
    /// recovered tab starts where the lost session was. `None` keeps the
    /// project's folder, then the home folder.
    pub working_directory: Option<std::path::PathBuf>,
    /// Continue the agent's most recent conversation in that folder rather
    /// than start a fresh one (`launch-resume`). Raw agents only.
    pub resume: bool,
}

pub struct SessionView {
    pub surface: SessionSurface<SessionSender>,
    /// The session this tab is for, or `None` while its launch has not come
    /// back named. The surface's own `session` field is presentation identity
    /// (set when a frame may land); this is the tab's.
    pub session: Option<SessionId>,
    /// The stable key of the profile this tab's session was launched from —
    /// identity, never a translated label. It is what capture writes beside
    /// the session id, so a restored tab can be labeled before the catalog
    /// answers.
    pub profile_key: Option<SessionProfileKey>,
    pub pending_launch: Option<PendingLaunch>,
    /// What this tab was when it was saved — its folder, its project, whether
    /// it was a raw terminal — kept from a restore until the Agent's first
    /// catalog says the session is still alive (then it is dropped) or is not
    /// (then the tab offers a replacement instead of closing; ADR-474).
    pub recovery: Option<koru_workspace_host::SessionRecovery>,
    /// The panel a recovered tab shows in place of a terminal.
    pub offer: Option<crate::session_offer::Offer>,
    /// A recovered shell answers its own offer: a fresh prompt in the same
    /// folder asks nothing of the person.
    pub offer_auto_start: bool,
    /// The Terminal settings' version this tab last applied its cursor from.
    cursor_version: u64,
    /// **This tab was opened to watch a delegated run's session.**
    ///
    /// Distinct from [`SessionSurface::watching`], which is where the tab is
    /// *now*: a person who takes the session is driving it and the surface
    /// says so, and when they give it back this is what says the tab goes
    /// back to reading rather than sitting on a session it no longer holds.
    ///
    /// [`SessionSurface::watching`]: koru_agents_ui::SessionSurface::watching
    pub watches_run: bool,
    /// The tab's own fonts. `View::draw` carries no text engine, so the view
    /// owns one — the same shape `ChatSurface` uses inside the AI chat tab.
    engine: TextEngine,
    /// Where the pointer last was, in this view's own space. `mouse_up` and
    /// `cursor_icon` arrive without coordinates.
    pointer: (f32, f32),
    /// Whether a press is held, so `mouse_move` can be a drag.
    pressed: bool,
}

impl SessionView {
    pub fn new(sender: &SessionSender, pending_launch: Option<PendingLaunch>) -> Self {
        let mut surface = SessionSurface::new(
            sender,
            std::sync::Arc::new(koru_session_observability::SessionStats::from_env()),
            crate::format::lang(),
            // Hidden by default (Q-E6 P1): a raw shell rarely wants the
            // pending-input band, so a session tab opens without it. The
            // toggle chord brings it up on demand, and a managed attach
            // reveals it — a managed agent's input *is* the composer. This is
            // koru.exe's choice only; Koru Terminal kept passing its own
            // `composer_default`.
            false,
            // The stored cursor, not the built-in one; `maintain` follows it
            // when Settings changes it.
            crate::terminal_prefs::cursor(),
        );
        // The workspace-host `View` boundary has no window-focus push, so the
        // surface treats the window as focused; `active` (only the shown tab)
        // still gates every blink deadline and every draw.
        surface.window_focused = true;
        surface
            .view
            .set_appearance(crate::terminal_prefs::appearance_for(
                pending_launch.as_ref().map(|launch| &launch.profile.id),
            ));
        Self {
            surface,
            session: None,
            cursor_version: crate::terminal_prefs::version(),
            profile_key: pending_launch
                .as_ref()
                .map(|launch| launch.profile.id.clone()),
            pending_launch,
            recovery: None,
            offer: None,
            offer_auto_start: false,
            watches_run: false,
            engine: TextEngine::new(),
            pointer: (0.0, 0.0),
            pressed: false,
        }
    }

    /// Show or hide the pending-input composer — koru.exe's spelling of
    /// Koru Terminal's window-menu "Input area" toggle, and the same path:
    /// flip the flag, pull focus out of a band that is no longer there, and
    /// renegotiate the grid the band was carved from (a switch in flight
    /// retargets — `renegotiate_grid` makes that choice).
    pub fn toggle_composer(&mut self) -> bool {
        self.surface.composer_visible = !self.surface.composer_visible;
        if !self.surface.composer_visible && self.surface.focus == SurfaceFocus::Composer {
            self.surface.focus = SurfaceFocus::Terminal;
            self.surface.pending.set_focused(false);
        }
        let _ = self.surface.renegotiate_grid();
        true
    }

    /// The composer's reveal chord: primary+Shift+U. A letter chord because
    /// the `View` boundary cannot tell Ctrl+I from Ctrl+Tab (see the module
    /// docs), and one no shell or registry entry claims — checked before the
    /// composer's own keys so it can also put the band away while focused.
    fn composer_toggle_chord(key: &Key, mods: ModifiersState) -> bool {
        koru_keys::has_primary(mods)
            && mods.shift_key()
            && !mods.alt_key()
            && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("u"))
    }

    /// This tab became the one showing. Mirrors Koru Terminal's
    /// `Surface::activated`: the surface re-enters the sized-snapshot wait
    /// showing its own previous frame — it does not clear the view.
    pub fn activated(&mut self) {
        self.surface.active = true;
        self.surface.view.set_visible(true);
        let managed = self.surface.session_is_managed;
        self.surface.managed.set_visible(managed);
        let eligible = self.surface.blink_eligible();
        let _ = self.surface.reset_cursor_blink(eligible, Instant::now());
    }

    /// This tab stopped being the one showing. No caret, no deadline, no
    /// wakeups — the inactive-tab rule, enforced where the surface enforces it.
    pub fn deactivated(&mut self) {
        self.surface.active = false;
        self.surface.view.set_visible(false);
        self.surface.managed.set_visible(false);
        let _ = self.surface.sync_cursor_blink(false, Instant::now());
    }

    /// The cell under a point in this view's own coordinates.
    fn cell_at(&self, x: f32, y: f32) -> Option<CellPoint> {
        self.surface.terminal_point(self.surface.split().0, x, y)
    }

    /// A shortcut the terminal layer owns, resolved through the shared
    /// registry — after the container has declined the key, which is the
    /// precedence ADR-263 keeps.
    fn shortcut(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<TerminalShortcut> {
        koru_keys::REGISTRY
            .iter()
            .find(|entry| {
                entry.layer == koru_keys::Layer::Terminal && entry.chord.matches(key, text, mods)
            })
            .and_then(|entry| match entry.id {
                koru_keys::ShortcutId::TerminalCopyOrInterrupt => {
                    Some(TerminalShortcut::CopyOrInterrupt)
                }
                koru_keys::ShortcutId::TerminalCopySelection => {
                    Some(TerminalShortcut::CopySelection)
                }
                koru_keys::ShortcutId::TerminalPaste => Some(TerminalShortcut::Paste),
                koru_keys::ShortcutId::TerminalPasteAlternate => {
                    Some(TerminalShortcut::PasteAlternate)
                }
                _ => None,
            })
    }

    /// The composer's keys. `None` when the composer does not have the
    /// keyboard, so the caller falls through to the grid.
    fn composer_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if self.surface.focus != SurfaceFocus::Composer {
            return None;
        }
        if matches!(
            Self::shortcut(key, text, mods),
            Some(TerminalShortcut::Paste | TerminalShortcut::PasteAlternate)
        ) {
            return Some(self.surface.pending.paste_clipboard());
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.surface.focus = SurfaceFocus::Terminal;
                self.surface.pending.set_focused(false);
                return Some(true);
            }
            Key::Named(NamedKey::Backspace) => return Some(self.surface.pending.backspace()),
            Key::Named(NamedKey::Enter) if !mods.shift_key() => {
                let composer = self.surface.split().1;
                let (x, y) = self.surface.pending.layout(composer).send.centre();
                return Some(self.surface.pending.click(composer, x, y));
            }
            _ => {}
        }
        if !mods.control_key()
            && !mods.alt_key()
            && !mods.super_key()
            && let Some(text) = text
        {
            return Some(self.surface.pending.insert_text(text));
        }
        Some(false)
    }

    /// **The keys of a session this window is only reading.**
    ///
    /// There is nothing to type into, so nothing is sent: what is left is the
    /// one control at the foot of the chat and the way to reach it. Tab moves
    /// between the transcript and that control, Enter or Space presses it,
    /// Escape leaves it. Everything else is declined so the container's own
    /// shortcuts still work — a watched tab is still a tab.
    fn watch_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        match key {
            Key::Named(NamedKey::Tab) if mods.is_empty() || mods.shift_key() => {
                let focused = self.surface.watch_focused();
                self.surface.set_watch_focused(!focused)
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space)
                if self.surface.watch_focused() =>
            {
                self.surface.activate_watch_control()
            }
            Key::Named(NamedKey::Escape) if self.surface.watch_focused() => {
                self.surface.set_watch_focused(false)
            }
            _ => false,
        }
    }

    /// A managed session's keys — approvals and a cancel, never raw bytes.
    fn managed_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        let key = if koru_keys::has_primary(mods)
            && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("c"))
        {
            Some(ManagedKey::CancelTurn)
        } else {
            match key {
                Key::Named(NamedKey::Tab) if !mods.control_key() => Some(ManagedKey::Tab {
                    reverse: mods.shift_key(),
                }),
                Key::Named(NamedKey::Enter) => Some(ManagedKey::Enter),
                Key::Named(NamedKey::Space) => Some(ManagedKey::Space),
                Key::Named(NamedKey::Escape) => Some(ManagedKey::Escape),
                _ => None,
            }
        };
        key.is_some_and(|key| {
            let outcome = self.surface.managed.handle_key(key);
            outcome.handled || outcome.redraw
        })
    }

    /// The logical key as a terminal key. The `View` boundary carries no
    /// physical key, so the Ctrl+I/Ctrl+Tab distinction Koru Terminal repairs
    /// is not repairable here — Windows reports both as logical Tab.
    fn terminal_key(key: &Key, text: Option<&str>) -> Option<TerminalKey> {
        match key {
            Key::Named(NamedKey::Enter) => Some(TerminalKey::Enter),
            Key::Named(NamedKey::Tab) => Some(TerminalKey::Tab),
            Key::Named(NamedKey::Backspace) => Some(TerminalKey::Backspace),
            Key::Named(NamedKey::Escape) => Some(TerminalKey::Escape),
            Key::Named(NamedKey::ArrowUp) => Some(TerminalKey::ArrowUp),
            Key::Named(NamedKey::ArrowDown) => Some(TerminalKey::ArrowDown),
            Key::Named(NamedKey::ArrowLeft) => Some(TerminalKey::ArrowLeft),
            Key::Named(NamedKey::ArrowRight) => Some(TerminalKey::ArrowRight),
            Key::Character(character) => Some(TerminalKey::Character {
                text: character.to_string(),
            }),
            _ => text.map(|text| TerminalKey::Character {
                text: text.to_owned(),
            }),
        }
    }

    fn pointer_modifiers(&self, ctrl: bool) -> PointerModifiers {
        PointerModifiers {
            shift: false,
            control: ctrl,
            alt: false,
            primary: if cfg!(target_os = "macos") {
                false
            } else {
                ctrl
            },
        }
    }

    /// A press in the terminal grid at `click_count` clicks. Shared by
    /// `mouse_down` (one) and `mouse_double_click` (two).
    fn grid_press(&mut self, x: f32, y: f32, ctrl: bool, click_count: u8) -> bool {
        let terminal = self.surface.split().0;
        if !terminal.contains(x, y) {
            return false;
        }
        self.surface.focus = SurfaceFocus::Terminal;
        self.surface.pending.set_focused(false);
        if self.surface.session_is_managed {
            let outcome = self.surface.managed.pointer_press(x, y);
            return outcome.handled || outcome.redraw;
        }
        let Some(point) = self.cell_at(x, y) else {
            return true;
        };
        let modifiers = self.pointer_modifiers(ctrl);
        let outcome =
            self.surface
                .view
                .pointer_press(point, ProtocolButton::Left, modifiers, click_count);
        let eligible = self.surface.blink_eligible();
        let blink = outcome.handled && self.surface.reset_cursor_blink(eligible, Instant::now());
        outcome.handled || outcome.redraw || blink
    }
}

impl View for SessionView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // A recovered tab has no terminal behind it to type into.
        if let Some(offer) = self.offer.as_mut() {
            return offer.key(key);
        }
        // **Before every other key.** A watched session has no composer to
        // toggle, no grid to type into and no approval to answer; the only
        // keys it has are the ones that reach the control at its foot.
        if self.surface.watching().watching() {
            return self.watch_key(key, mods);
        }
        // Before the composer's own keys, so the chord can put the band away
        // even while it holds the keyboard.
        if Self::composer_toggle_chord(key, mods) && self.surface.status == PaneStatus::Running {
            return self.toggle_composer();
        }
        if let Some(handled) = self.composer_key(key, text, mods) {
            return handled;
        }
        if mods.is_empty()
            && matches!(key, Key::Named(NamedKey::Tab))
            && self.surface.composer_visible
            && self.surface.status == PaneStatus::Running
        {
            self.surface.focus = SurfaceFocus::Composer;
            self.surface.pending.set_focused(true);
            return true;
        }
        if self.surface.status != PaneStatus::Running {
            return false;
        }
        if self.surface.session_is_managed {
            return self.managed_key(key, mods);
        }
        if self.surface.raw_switch_pending {
            return false;
        }
        let input = if let Some(shortcut) = Self::shortcut(key, text, mods) {
            TerminalKeyInput::Shortcut(shortcut)
        } else {
            let Some(key) = Self::terminal_key(key, text) else {
                return false;
            };
            TerminalKeyInput::Stroke(KeyStroke {
                key,
                modifiers: KeyModifiers {
                    shift: mods.shift_key(),
                    control: mods.control_key(),
                    alt: mods.alt_key(),
                    primary: koru_keys::has_primary(mods),
                },
            })
        };
        let outcome = self.surface.view.handle_key(TerminalKeyEvent {
            input,
            // The `View` boundary reports presses only; a release never
            // reaches this adapter.
            phase: KeyPhase::Pressed,
            repeat: false,
        });
        if let Some(action) = outcome.clipboard {
            self.surface.clipboard(action);
        }
        let eligible = self.surface.blink_eligible();
        let blink = outcome.handled && self.surface.reset_cursor_blink(eligible, Instant::now());
        outcome.handled || outcome.redraw || blink
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        self.pointer = (x, y);
        if let Some(offer) = self.offer.as_mut() {
            return offer.mouse_down(
                &mut self.engine,
                self.surface.lang,
                self.surface.bounds,
                x,
                y,
            );
        }
        self.pressed = true;
        let composer = self.surface.split().1;
        // The same band, read two ways: the composer when this window drives
        // the session, the read-only footer when it is watching one.
        if self.surface.watching().watching() {
            self.pressed = false;
            if composer.h > 0.0 && composer.contains(x, y) {
                return self.surface.press_watch_band(x, y);
            }
            return self.surface.set_watch_focused(false);
        }
        if composer.h > 0.0 && composer.contains(x, y) {
            let handled = self.surface.pending.click(composer, x, y);
            if handled {
                self.surface.focus = if self.surface.pending.focused() {
                    SurfaceFocus::Composer
                } else {
                    SurfaceFocus::Terminal
                };
            }
            return handled;
        }
        self.grid_press(x, y, ctrl, 1)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(offer) = self.offer.as_mut() {
            return offer.mouse_move(
                &mut self.engine,
                self.surface.lang,
                self.surface.bounds,
                x,
                y,
            );
        }
        let composer = self.surface.split().1;
        if self.surface.watching().watching() {
            return self.surface.hover_watch_band(x, y);
        }
        if composer.h > 0.0 && composer.contains(x, y) {
            return self.surface.pending.pointer_moved(composer, x, y);
        }
        let left_composer = self.surface.pending.pointer_moved(composer, -1.0, -1.0);
        if !self.pressed || self.surface.session_is_managed {
            return left_composer;
        }
        let Some(point) = self.cell_at(x, y) else {
            return left_composer;
        };
        let outcome = self
            .surface
            .view
            .pointer_drag(point, self.pointer_modifiers(false));
        outcome.handled || outcome.redraw || left_composer
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        if !std::mem::take(&mut self.pressed) {
            return false;
        }
        if self.surface.watching().watching() {
            return false;
        }
        if self.surface.session_is_managed {
            return false;
        }
        let (x, y) = self.pointer;
        let Some(point) = self.cell_at(x, y) else {
            return false;
        };
        let outcome = self.surface.view.pointer_release(
            point,
            ProtocolButton::Left,
            self.pointer_modifiers(false),
        );
        outcome.handled || outcome.redraw
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        // A double click selects a word; the click count is the container's
        // timing, and the surface must never do it twice.
        self.grid_press(x, y, false, 2)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if self.offer.is_some() {
            return;
        }
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        if lines == 0.0 {
            return;
        }
        if self.surface.session_is_managed {
            let line = theme::role::ui().metrics.line_height.max(1.0);
            let next = self.surface.managed.scroll_offset()
                + if lines > 0.0 { -line * 3.0 } else { line * 3.0 };
            self.surface.managed.set_scroll_offset(next);
            return;
        }
        let (x, y) = self.pointer;
        let Some(point) = self.cell_at(x, y) else {
            return;
        };
        let step = if lines > 0.0 { 3 } else { -3 };
        let _ = self
            .surface
            .view
            .wheel(point, step, self.pointer_modifiers(false));
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.surface.bounds = Rect::new(0.0, 0.0, w, h);
        // A resize mid-switch retargets the switch rather than resizing the
        // process still on screen; a live grid renegotiates its viewport. The
        // surface makes that choice — see `SessionSurface::renegotiate_grid`.
        let _ = self.surface.renegotiate_grid();
    }

    fn cursor_icon(&self) -> CursorIcon {
        if let Some(offer) = self.offer.as_ref() {
            return offer.cursor();
        }
        let composer = self.surface.split().1;
        let (x, y) = self.pointer;
        if composer.h <= 0.0 || !composer.contains(x, y) {
            return CursorIcon::Default;
        }
        if self.surface.watching().watching() {
            // The band holds one control and a sentence; the hover the
            // surface already tracks is the same question this is.
            return match self.surface.watch_hovered() {
                true => koru_ui::cursor::PRESS,
                false => CursorIcon::Default,
            };
        }
        match self.surface.pending.layout(composer).hit(
            x,
            y,
            self.surface.pending.model().has_retry(),
        ) {
            Some(koru_session_view::PendingInputHit::Draft) => CursorIcon::Text,
            Some(_) => koru_ui::cursor::PRESS,
            None => CursorIcon::Default,
        }
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        // A theme or font change invalidates every shaped row; the style is
        // re-prepared on the next draw, which is the first thing with an
        // engine in hand.
        let active = self.surface.active;
        self.surface.view.set_visible(active);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &Workspace,
        _images: &koru_draw::ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        // **`scale` is the factor the window appends *after* the scene — and
        // this host appends none.** The surface API's `scale` exists because
        // Koru Terminal's window renders the drawn scene under one more DPI
        // transform (`cx.scale`), and `DeviceGrid` — the thing that snaps every
        // cell edge onto a device pixel — multiplies `root` by it to reach the
        // glass. koru-workspace-host composes the *whole* glass transform into
        // `root` (DPI × display zoom × the tab's origin) and renders the scene
        // 1:1, so the append factor here is exactly 1.
        //
        // This used to pass `root.as_coeffs()[0]` — the DPI×zoom already inside
        // `root` — which made the snapping grid `(dpi·zoom)²` while the frame
        // was drawn under `dpi·zoom`. At 100%/1.0 the square is the same number
        // and every edge landed; at 90%/110% every snapped edge missed the
        // device pixel and the ground antialiased through between cells — the
        // seams in the maintainers' OpenCode block-art screenshot (Q-E8 P10). Proven
        // by rasterization in `koru-session-view/tests/raster.rs`
        // (`the_workspace_host_chain_has_no_seams_at_fractional_zoom`).
        let scale = 1.0;
        self.surface.scale = scale;
        // A recovered tab paints its offer over the terminal's own ground and
        // nothing else: there is no grid, no composer and no status.
        if let Some(offer) = self.offer.as_ref() {
            let bounds = self.surface.bounds;
            koru_draw::scene::fill(
                scene,
                root,
                self.surface.view.background(),
                &koru_draw::scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
            );
            let dark = crate::terminal_prefs::appearance_for(self.profile_key.as_ref()).dark;
            offer.draw(
                scene,
                root,
                &mut self.engine,
                self.surface.lang,
                bounds,
                dark,
            );
            return;
        }
        self.surface
            .view
            .set_appearance(crate::terminal_prefs::appearance_for(
                self.profile_key.as_ref(),
            ));
        if self.surface.view.prepare_style(&mut self.engine, scale) {
            // Font/terminal text zoom changes the PTY's cell count as well as
            // its paint. Reuse the same resize path as viewport changes.
            let _ = self.surface.renegotiate_grid();
        }
        // The grid paints whole cells, so what is left over at the right and
        // bottom edge (up to a cell wide or tall, and the content inset) would
        // show the page behind it. The ground goes under the whole pane first.
        let bounds = self.surface.bounds;
        koru_draw::scene::fill(
            scene,
            root,
            self.surface.view.background(),
            &koru_draw::scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        );
        let now = Instant::now();
        let eligible = self.surface.blink_eligible();
        let _ = self.surface.sync_cursor_blink(eligible, now);
        let (terminal, composer) = self.surface.split();
        let managed = self.surface.session_is_managed;
        let frame = self
            .surface
            .draw_pane(scene, &mut self.engine, root, terminal, scale, managed);
        let p = theme::p();
        if self.surface.shows_status(&frame, managed) {
            draw_label(
                scene,
                root,
                &mut self.engine,
                t(self.surface.lang, self.surface.status_key()),
                terminal,
                theme::role::ui(),
                p.placeholder,
            );
        }
        if composer.h > 0.0 {
            if self.surface.watching().watching() {
                self.surface
                    .draw_watch_band(scene, &mut self.engine, root, composer);
            } else {
                self.surface
                    .pending
                    .draw(scene, root, &mut self.engine, composer);
            }
        }
        if let Some(notice) = self.surface.notice.as_deref() {
            draw_label(
                scene,
                root,
                &mut self.engine,
                notice,
                Rect::new(terminal.x, terminal.y + 8.0, terminal.w, 28.0),
                theme::role::meta(),
                p.text_dim,
            );
        }
        // The `View` boundary has no presented hook; a drawn frame is the
        // closest moment to acknowledge the transcript the person saw.
        self.surface.presented_managed();
    }

    fn animating(&self) -> bool {
        false
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        // The caret's quiet interval, and nothing else — and only while this
        // surface is the active tab, which `blink_eligible` enforces. An
        // inactive session tab costs no timer wake at all.
        self.surface
            .cursor_blink_active(self.surface.blink_eligible())
            .then_some(self.surface.cursor_blink_deadline)
            .flatten()
    }

    fn maintain(&mut self, _ws: &mut WorkspaceDoc, now: Instant) -> bool {
        let version = crate::terminal_prefs::version();
        let restyled = version != self.cursor_version;
        if restyled {
            self.cursor_version = version;
            self.surface
                .view
                .set_cursor_preferences(crate::terminal_prefs::cursor());
        }
        let eligible = self.surface.blink_eligible();
        self.surface.maintain_cursor_blink(eligible, now) || restyled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A color chosen in Settings reaches an open terminal.** The Terminal
    /// section was patched and never read back in Koru, so every tab drew the
    /// built-in cursor whatever was picked.
    #[test]
    fn a_cursor_chosen_in_settings_reaches_an_open_terminal() {
        let mut tab = view();
        let mut ws =
            koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").unwrap();
        // Its own shape, so its own colour: a cursor matching the editor
        // caret takes the caret's colour instead.
        crate::terminal_prefs::adopt(&koru_settings::TerminalSettings {
            cursor_color: koru_settings::CaretColorChoice::Amber,
            cursor_width: koru_settings::TerminalCursorWidth::Thin,
            ..koru_settings::TerminalSettings::default()
        });
        assert!(View::maintain(&mut tab, &mut ws, Instant::now()));
        assert_eq!(
            tab.surface.view.cursor_preferences().color,
            koru_draw::theme::CaretColor::Amber
        );
        assert!(
            !View::maintain(&mut tab, &mut ws, Instant::now()) || tab.surface.blink_eligible(),
            "applied once, not every pass"
        );
    }

    fn view() -> SessionView {
        // A dead channel: the receiver is dropped, so every command the
        // surface sends is discarded — the same affordance the surface crate's
        // own tests use.
        let (sender, _commands) = koru_session_backend::channel();
        SessionView::new(&SessionSender(sender), None)
    }

    /// The inactive-tab rule at the adapter's boundary: a session tab that is
    /// not the one showing is ineligible to blink and sets no timer deadline;
    /// activation makes it eligible again.
    #[test]
    fn activation_gates_blinking_and_the_maintenance_deadline() {
        let mut v = view();
        v.surface.status = PaneStatus::Running;
        assert!(!v.surface.blink_eligible(), "not shown, not eligible");
        assert_eq!(View::maintenance_deadline(&v), None);

        v.activated();
        assert!(v.surface.blink_eligible(), "the shown running tab blinks");

        v.deactivated();
        assert!(!v.surface.blink_eligible());
        assert_eq!(
            View::maintenance_deadline(&v),
            None,
            "deactivation clears every timer wake"
        );
    }

    /// The composer opens hidden (Q-E6 P1) — a raw shell rarely wants the
    /// band — and the reveal chord brings it up and puts it away again,
    /// returning focus to the grid when it takes the band out from under it.
    #[test]
    fn the_composer_is_hidden_by_default_and_the_chord_toggles_it() {
        let mut v = view();
        assert!(!v.surface.composer_visible, "hidden on a fresh session tab");
        v.surface.status = PaneStatus::Running;
        let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
            .expect("create");
        assert!(View::key(
            &mut v,
            &mut ws,
            &Key::Character("u".into()),
            Some("U"),
            koru_keys::PRIMARY_SHIFT,
        ));
        assert!(v.surface.composer_visible, "the chord revealed it");

        // Hide it again while it holds the keyboard: focus must come back to
        // the grid rather than pointing at a band that is no longer there.
        v.surface.focus = SurfaceFocus::Composer;
        v.surface.pending.set_focused(true);
        assert!(View::key(
            &mut v,
            &mut ws,
            &Key::Character("u".into()),
            Some("U"),
            koru_keys::PRIMARY_SHIFT,
        ));
        assert!(!v.surface.composer_visible, "and away again");
        assert_eq!(v.surface.focus, SurfaceFocus::Terminal);
    }

    /// **A watched session's keys reach the one control it has, and nothing
    /// else** (ADR-361). Tab moves to the button at the foot of the chat,
    /// Enter presses it, Escape leaves it — and no key is ever sent to a
    /// session this window is only reading.
    #[test]
    fn a_watched_session_routes_its_keys_to_the_one_control_it_has() {
        let mut v = view();
        v.surface.bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        v.surface.status = PaneStatus::Running;
        v.surface.set_watching(koru_agents_ui::Watching::Yes);
        let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
            .expect("create");

        // Ordinary typing reaches nothing: there is nowhere for it to go.
        assert!(!View::key(
            &mut v,
            &mut ws,
            &Key::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        ));
        assert!(!v.surface.take_takeover_request());

        // Tab reaches the control, Enter presses it.
        assert!(View::key(
            &mut v,
            &mut ws,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        ));
        assert!(v.surface.watch_focused());
        assert!(View::key(
            &mut v,
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));
        assert!(
            v.surface.take_takeover_request(),
            "the host drains the ask; the surface claims nothing"
        );

        // Escape gives the keyboard back to the transcript.
        assert!(View::key(
            &mut v,
            &mut ws,
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty(),
        ));
        assert!(!v.surface.watch_focused());

        // And the composer's reveal chord does nothing: there is no composer
        // on a session this window cannot type into.
        assert!(!View::key(
            &mut v,
            &mut ws,
            &Key::Character("u".into()),
            Some("U"),
            koru_keys::PRIMARY_SHIFT,
        ));
        assert!(!v.surface.composer_visible);
    }

    /// The viewport reaches the surface's bounds — what every grid measurement
    /// is made from.
    #[test]
    fn the_viewport_reaches_the_surface_bounds() {
        let mut v = view();
        let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
            .expect("create");
        View::set_viewport(&mut v, 640.0, 480.0);
        assert_eq!((v.surface.bounds.w, v.surface.bounds.h), (640.0, 480.0));
        // And the document argument is ignored, the AI-chat precedent: a
        // session has no document, so a key changes nothing in the workspace.
        let _ = View::key(
            &mut v,
            &mut ws,
            &Key::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        );
    }
}

/// A label, vertically centred and inset by the pane's own content inset — the
/// same placement Koru Terminal's shell adapter gives the status placeholder
/// and the notice.
fn draw_label(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    text: &str,
    rect: Rect,
    style: theme::TextStyle,
    colour: vello::peniko::Color,
) {
    let layout = engine.simple_layout(text, style);
    koru_draw::scene::draw_layout_first_line(
        scene,
        root,
        &layout,
        rect.x + koru_session_view::CONTENT_INSET_X,
        rect.y + (rect.h - layout.height()) * 0.5,
        Some(colour),
    );
}
