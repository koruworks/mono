// SPDX-License-Identifier: MPL-2.0

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use koru_ai_provider::{CredentialStore, OsCredentialStore, SecretString};
use koru_ai_ui::{ChatSurface, DictationPress, DictationShown, SurfaceMode};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_settings_ui::{AiSlot, Sections, SettingsLinks, SettingsSurface};
use koru_ui::caption::{self, Action as CaptionAction};
use koru_ui::{Anchor, Menu, MenuItem};
use koru_window::{Appearance, ClosingWindowState, WindowApp, WindowRequest};
use vello::kurbo::Affine;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::MouseScrollDelta;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{
    CursorIcon, Icon, ResizeDirection, Theme, Window, WindowAttributes, WindowLevel,
};

use crate::activation::Activation;
use crate::geometry::ChatGeometry;

const MIN_WIDTH: f32 = 360.0;
const MIN_HEIGHT: f32 = 420.0;

/// Ask the Agent paired with this standalone window to start the shared voice
/// worker. The voice client retries its own root-qualified endpoint while this
/// bounded request runs on a helper thread.
fn ensure_voice_with_agent(channel_root: Option<&Path>) -> Result<(), String> {
    ask_agent(
        channel_root,
        koru_agent_wire::Request::EnsureService {
            service: koru_agent_wire::Service::Voice,
        },
    )
}

/// Ask the Agent to open a document a chip or a titled link names — the same
/// `OpenLink` a `koru://doc/…` URL from a browser becomes. The Agent resolves
/// where it lives and routes it to Koru; this window has no tabs of its own.
/// Bounded, and only ever called from a helper thread.
fn open_link_with_agent(
    channel_root: Option<&Path>,
    link: koru_link::KoruLink,
) -> Result<(), String> {
    let koru_link::KoruLink::Document { document, node } = link else {
        return Err("only a document link opens in Koru".into());
    };
    ask_agent(
        channel_root,
        koru_agent_wire::Request::OpenLink {
            document: document.to_string(),
            node: node.map(|node| node.to_string()),
        },
    )
}

/// One request to the Agent, answered `Ok` or not, with bounded timeouts.
fn ask_agent(channel_root: Option<&Path>, body: koru_agent_wire::Request) -> Result<(), String> {
    use koru_agent_wire::{
        AgentGreeting, FRAME_VERSION, FromAgent, FromShell, PROTOCOL_VERSION, Response, ShellHello,
    };

    let endpoint = match channel_root {
        Some(root) => {
            koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, koru_channel::segment(), root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::AGENT, koru_channel::segment()),
    }
    .map_err(|error| error.to_string())?;
    let mut stream = koru_ipc::connect(&endpoint).map_err(|error| error.to_string())?;
    koru_ipc::set_timeouts(&stream, Some(std::time::Duration::from_secs(12)));
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &ShellHello {
            product: "koru-ai-chat".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| error.to_string())?;
    let (version, greeting): (u8, AgentGreeting) =
        koru_ipc::frame::read_frame(&mut stream).map_err(|error| error.to_string())?;
    if version != FRAME_VERSION || !matches!(greeting, AgentGreeting::Welcome(_)) {
        return Err("the Agent protocol is incompatible".into());
    }
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &FromShell::Request { id: 1, body },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| error.to_string())?;
    loop {
        let (version, answer): (u8, FromAgent) =
            koru_ipc::frame::read_frame(&mut stream).map_err(|error| error.to_string())?;
        if version != FRAME_VERSION {
            return Err("the Agent framing version changed".into());
        }
        match answer {
            FromAgent::Response {
                id: 1,
                body: Response::Ok,
            } => return Ok(()),
            FromAgent::Response {
                id: 1,
                body: Response::Failed { message, .. },
            } => return Err(message),
            _ => {}
        }
    }
}

pub struct ChatApp {
    view: ChatView,
    activation: Option<Activation>,
    geometry: ChatGeometry,
    appearance: Appearance,
    /// **The settings authority, off the event thread.** This window reads the
    /// Global section and never writes it: its own Settings surface is SP-7's,
    /// and until then the shell is where a preference is changed. What it does
    /// need is to *hear* a change — two Koru windows disagreeing about the
    /// theme is exactly what the change fact exists to prevent.
    settings: koru_api_client::GlobalLink,
    dictation: koru_dictation::Dictation,
    dictation_key: Option<PhysicalKey>,
    channel_root: Option<PathBuf>,
    exit: bool,
    activation_requested: bool,
    /// Answers from the helper threads that asked the Agent to open a
    /// document a chip names, and the proxy that wakes the loop for them.
    opened: (
        std::sync::mpsc::Sender<Result<(), String>>,
        std::sync::mpsc::Receiver<Result<(), String>>,
    ),
    proxy: Option<EventLoopProxy<()>>,
}

impl ChatApp {
    pub fn new(
        link: Option<(koru_ai_core::ChatId, Option<koru_ai_core::MessageBlockId>)>,
        activation: Option<Activation>,
        store_root: Option<PathBuf>,
        channel_root: Option<PathBuf>,
        geometry_path: Option<PathBuf>,
        appearance: Appearance,
        settings: koru_api_client::GlobalLink,
    ) -> Self {
        let geometry = ChatGeometry::open(geometry_path);
        let size = geometry.size();
        let viewport = (size.0 / appearance.zoom, size.1 / appearance.zoom);
        let mut dictation = koru_dictation::Dictation::at(store_root.clone());
        if !cfg!(test) {
            koru_dictation::Preferences::from_section(settings.authoritative())
                .apply_to(&mut dictation);
            let consent_path = channel_root
                .as_ref()
                .map(|root| root.join("agent").join(koru_voice_consent::CONSENT_FILE))
                .or_else(koru_window::voice_consent_path);
            let consent = consent_path
                .map(|path| koru_voice_consent::read(&path))
                .and_then(|consent| consent.granted().cloned());
            dictation.set_consent(consent);
        }
        let mut view = ChatView::new(
            link,
            store_root,
            viewport,
            geometry.maximized(),
            geometry.pinned(),
        );
        // This window owns a dictation controller of its own: the composer's
        // microphone shows wherever voice is available (koru-ai D103).
        view.surface.offer_dictation(koru_dictation::available());
        Self {
            view,
            activation,
            geometry,
            appearance,
            settings,
            dictation,
            dictation_key: None,
            channel_root,
            exit: false,
            activation_requested: false,
            opened: std::sync::mpsc::channel(),
            proxy: None,
        }
    }

    /// **A chip or a titled link was clicked.** This window has no tabs, so it
    /// asks the Agent — the same `OpenLink` a `koru://doc/…` URL from a browser
    /// becomes — on a helper thread, and says so on the status line only if
    /// that fails. Returns whether anything visible changed.
    fn service_open_requests(&mut self) -> bool {
        if let Some(link) = self.view.surface.take_open_document_request() {
            if cfg!(test) {
                return false;
            }
            let root = self.channel_root.clone();
            let answer = self.opened.0.clone();
            let proxy = self.proxy.clone();
            let _ = std::thread::Builder::new()
                .name("koru-ai open document".into())
                .spawn(move || {
                    let result = open_link_with_agent(root.as_deref(), link);
                    if let Err(error) = &result {
                        tracing::warn!(target: "koru_ai::ui", %error, "standalone AI could not open a document through the Agent");
                    }
                    let _ = answer.send(result);
                    if let Some(proxy) = proxy {
                        let _ = proxy.send_event(());
                    }
                });
        }
        let mut changed = false;
        while let Ok(result) = self.opened.1.try_recv() {
            if result.is_err() {
                changed |= self.view.surface.open_document_failed();
            }
        }
        changed
    }

    /// **Take an authoritative section, if one has arrived.**
    ///
    /// The startup read resolving and another window's change both land here,
    /// and both travel the same path: the appearance globals move, the OS
    /// titlebar follows, and the view is invalidated because every brush and
    /// every font is baked into a parley layout.
    ///
    /// Returns whether anything visible changed.
    fn adopt_settings(&mut self) -> bool {
        let Some(section) = self.settings.poll() else {
            return false;
        };
        // **Fed, before anything else is decided about it.** The Settings
        // surface holds no link of its own in this window; this is where its
        // Global section comes from, and it is the same fact the appearance is
        // derived from below, so the two can never disagree.
        let mut moved = false;
        if let Some(surface) = self.view.settings.as_mut() {
            moved = surface.adopt_global(&section);
        }
        let next = Appearance::from_section(&section);
        if next == self.appearance {
            return moved;
        }
        // Zoom moves the viewport rather than the palette, so it is re-derived
        // here rather than routed through `rezoom`: that path snaps to the 0.1
        // grid the keyboard steps in, and snapping a value the authority
        // actually holds would leave the viewport sized for one zoom and the
        // globals set to another.
        let previous = self.appearance.zoom;
        self.appearance = next;
        self.appearance.apply();
        if next.zoom != previous {
            let physical = (
                self.view.viewport.0 * previous,
                self.view.viewport.1 * previous,
            );
            self.view
                .set_viewport(physical.0 / next.zoom, physical.1 / next.zoom);
        }
        self.view.invalidate();
        true
    }

    fn request_exit(&mut self) {
        self.exit = true;
    }

    fn zoom(&self) -> f32 {
        self.appearance.zoom
    }

    fn view_point(&self, x: f32, y: f32) -> (f32, f32) {
        (x / self.zoom(), y / self.zoom())
    }

    fn rezoom(&mut self, next: f32) -> bool {
        // The bounds and the step grid come from beside the `theme::zoom()`
        // global, so this window and the shell agree about what 50% means
        // without either writing the numbers down.
        let next = theme::clamp_zoom(next);
        let old = self.zoom();
        if next == old {
            return false;
        }
        let physical_logical = (self.view.viewport.0 * old, self.view.viewport.1 * old);
        self.appearance.zoom = next;
        theme::set_zoom(next);
        self.view
            .set_viewport(physical_logical.0 / next, physical_logical.1 / next);
        self.view.invalidate();
        true
    }

    /// Refresh the roaming choices and this machine's consent before each hold.
    ///
    /// **The four come from the authority, not from a file.** This window is a
    /// separate process from the shell and cannot see its globals, so the
    /// authority is what the two agree through — and draining the link first
    /// means a hold begun right after somebody changed the microphone uses the
    /// one they chose.
    fn sync_dictation_preferences(&mut self) {
        if cfg!(test) {
            return;
        }
        let _ = self.adopt_settings();
        koru_dictation::Preferences::from_section(self.settings.authoritative())
            .apply_to(&mut self.dictation);
        let consent_path = self
            .channel_root
            .as_ref()
            .map(|root| root.join("agent").join(koru_voice_consent::CONSENT_FILE))
            .or_else(koru_window::voice_consent_path);
        let consent = consent_path
            .map(|path| koru_voice_consent::read(&path))
            .and_then(|consent| consent.granted().cloned());
        self.dictation.set_consent(consent);
    }

    fn route_dictation_action(&mut self, action: &koru_dictation::DictationAction) -> bool {
        // The chip composer owns its private document: its context is `()`.
        let composer = self.view.surface.dictation_composer();
        match action {
            koru_dictation::DictationAction::Begin { .. } => {
                koru_ui::DictationTarget::begin_composition(composer, ())
            }
            koru_dictation::DictationAction::Update { text, .. } => {
                koru_ui::DictationTarget::update_composition(composer, (), text.as_str());
                true
            }
            koru_dictation::DictationAction::Commit { text, .. } => {
                koru_ui::DictationTarget::commit_composition(composer, (), text.as_str());
                true
            }
            koru_dictation::DictationAction::Cancel { .. } => {
                koru_ui::DictationTarget::cancel_composition(composer, ());
                true
            }
        }
    }

    /// Begin a dictation into the composer: from the chord, held, or from
    /// the composer's microphone (`physical` is `None`), which has no key to
    /// release — ■, Escape, any key, Enter or the chord again ends that one.
    fn begin_dictation(&mut self, physical: Option<PhysicalKey>) -> bool {
        self.sync_dictation_preferences();
        let Some(action) = self.dictation.press(Instant::now()) else {
            return self.service_dictation();
        };
        let accepted = self.route_dictation_action(&action);
        self.dictation.began(accepted);
        self.dictation_key = physical.filter(|_| accepted);
        self.service_dictation() | accepted
    }

    /// **Stop, and keep the words** (koru-ai D103): ■, a press on the
    /// composer, or a key while the composer records. A finish, not an
    /// interruption — the composition stays provisional until the final
    /// transcript commits it.
    fn stop_dictation(&mut self) -> bool {
        if !self.dictation.stop(Instant::now()) {
            return false;
        }
        self.dictation_key = None;
        self.service_dictation() | true
    }

    /// **A press while the composer records**: ✕ throws the recording away
    /// and ■ — or any press on the composer — stops it, both spent there;
    /// anywhere else it stops and the press still lands. Answers
    /// `(spent, changed)`.
    fn dictation_press(&mut self, x: f32, y: f32) -> (bool, bool) {
        if !self.dictation.is_live() {
            return (false, false);
        }
        match self.view.dictation_press_at(x, y) {
            Some(DictationPress::Cancel) => (
                true,
                self.interrupt_dictation(koru_dictation::Interruption::LocalEvent),
            ),
            Some(DictationPress::Stop) => (true, self.stop_dictation() | true),
            None => (false, self.stop_dictation()),
        }
    }

    fn finish_dictation(&mut self, physical: PhysicalKey) -> bool {
        if self.dictation_key != Some(physical) {
            return false;
        }
        self.dictation_key = None;
        if self.dictation.release(Instant::now()) {
            self.service_dictation()
        } else {
            false
        }
    }

    fn interrupt_dictation(&mut self, why: koru_dictation::Interruption) -> bool {
        if !self.dictation.is_live() {
            return false;
        }
        self.dictation.interrupt(why);
        self.dictation_key = None;
        self.service_dictation()
    }

    fn service_dictation(&mut self) -> bool {
        let actions = self.dictation.pump(Instant::now());
        let mut changed = false;
        for action in &actions {
            changed |= self.route_dictation_action(action);
        }
        if let Some(refusal) = self.dictation.take_refusal() {
            tracing::info!(
                target: "koru::voice",
                code = refusal.code.as_str(),
                surface = refusal.surface,
                "standalone AI dictation refused"
            );
            // Said on the status line: this window has no voice set-up of its
            // own, and the sentence for a missing model or consent names the
            // workspace's (koru-ai D103).
            let sentence = if refusal.surface {
                koru_i18n::Key::VoiceErrorSurface
            } else {
                refusal.code.sentence_key()
            };
            changed |= self.view.surface.dictation_refused(sentence);
        }
        if !self.dictation.is_live() {
            self.dictation_key = None;
        }
        // What the composer's recording row shows (D103), and `None` the pass
        // the session ends.
        changed |= self
            .view
            .surface
            .show_dictation(self.dictation.status().map(shown), Instant::now());
        changed
    }

    /// The composer's microphone was pressed: begin, as the chord does.
    fn begin_asked_dictation(&mut self) -> bool {
        if !self.view.surface.take_wants_dictation() {
            return false;
        }
        self.begin_dictation(None) | true
    }
}

impl WindowApp for ChatApp {
    type Event = ();

    fn window(&self) -> WindowAttributes {
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes()
            .with_title("Koru AI")
            .with_inner_size(LogicalSize::new(
                self.geometry.size().0,
                self.geometry.size().1,
            ))
            .with_min_inner_size(LogicalSize::new(MIN_WIDTH, MIN_HEIGHT))
            .with_window_icon(ai_chat_icon())
            .with_maximized(self.geometry.maximized())
            .with_window_level(if self.geometry.pinned() {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            })
            .with_theme(Some(if self.appearance.effective_dark() {
                Theme::Dark
            } else {
                Theme::Light
            }));
        if let Some(position) = self.geometry.position() {
            attributes = attributes.with_position(position);
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes = attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
                .with_titlebar_buttons_hidden(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            attributes = attributes.with_decorations(false);
        }
        attributes
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn close_requested(&mut self) {
        self.interrupt_dictation(koru_dictation::Interruption::WindowClosed);
        self.request_exit();
    }

    fn exiting(&mut self, window: Option<ClosingWindowState>) {
        self.interrupt_dictation(koru_dictation::Interruption::Suspended);
        if let Some(window) = window {
            self.geometry.set_maximized(window.maximised);
            self.geometry.set_pinned(window.pinned);
            if !window.maximised {
                if let Some(position) = window.position {
                    self.geometry.moved(position);
                }
                if window.scale_factor > 0.0 {
                    self.geometry.resized(
                        window.inner_size.width as f32 / window.scale_factor as f32,
                        window.inner_size.height as f32 / window.scale_factor as f32,
                    );
                }
            }
        }
        self.geometry.persist();
    }

    fn attach(&mut self, proxy: EventLoopProxy<Self::Event>) {
        self.proxy = Some(proxy.clone());
        if let Some(activation) = &self.activation
            && let Ok(mut wake) = activation.wake.lock()
        {
            *wake = Some(proxy.clone());
        }
        let dictation_proxy = proxy.clone();
        self.dictation.client().install_wake(move || {
            let _ = dictation_proxy.send_event(());
        });
        // The settings link answers on its own thread. Without this the window
        // would sit on the registry's defaults until somebody moved the
        // pointer — the wake is what makes "correct later" actually arrive.
        let settings_proxy = proxy.clone();
        self.settings.on_update(move || {
            let _ = settings_proxy.send_event(());
        });
        let channel_root = self.channel_root.clone();
        self.dictation.client().install_ensure(move || {
            let root = channel_root.clone();
            let _ = std::thread::Builder::new()
                .name("koru-ai ensure dictation".into())
                .spawn(move || {
                    if let Err(error) = ensure_voice_with_agent(root.as_deref()) {
                        tracing::warn!(target: "koru::voice", %error, "standalone AI could not ensure the voice worker");
                    }
                });
        });
        // The endpoint can receive after bind and before the event loop starts.
        // An unconditional first drain makes that mailbox edge exact.
        let _ = proxy.send_event(());
    }

    fn wake(&mut self, (): Self::Event) -> bool {
        let mut changed =
            self.service_dictation() | self.adopt_settings() | self.service_open_requests();
        let Some(activation) = &self.activation else {
            return changed;
        };
        let exit = activation.take_exit();
        let focus = activation.take_focus();
        let links = activation.take_links();
        for (chat, block) in links {
            changed |= self.view.open_external_chat_link(chat, block);
        }
        self.activation_requested |= focus;
        if exit {
            self.request_exit();
        }
        changed
    }

    fn take_activation_request(&mut self) -> bool {
        std::mem::take(&mut self.activation_requested)
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        let (x, y) = self.view_point(x, y);
        self.view.window_menu.is_none() && self.view.caption_hit(x, y) == Some(CaptionHit::Caption)
    }

    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon {
        let (x, y) = self.view_point(x, y);
        self.view.cursor_icon_at(x, y)
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        koru_window::resize::is_ours()
            .then(|| koru_window::resize::edge(width, height, x, y, maximised))
            .flatten()
    }

    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        if size.width == 0 || size.height == 0 || scale_factor <= 0.0 {
            return;
        }
        let width = size.width as f32 / scale_factor as f32;
        let height = size.height as f32 / scale_factor as f32;
        self.geometry.resized(width, height);
        self.view
            .set_viewport(width / self.zoom(), height / self.zoom());
    }

    fn paint_before_showing(&self) -> bool {
        true
    }

    fn moved(&mut self, position: PhysicalPosition<i32>) {
        self.geometry.moved(position);
    }

    fn wants_drag(&self) -> bool {
        true
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        let (x, y) = self.view_point(x, y);
        self.view.mouse_move(x, y)
    }

    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, _ctrl: bool) -> bool {
        let (x, y) = self.view_point(x, y);
        let (spent, changed) = self.dictation_press(x, y);
        if spent {
            return changed;
        }
        changed | self.view.mouse_down(engine, x, y) | self.begin_asked_dictation()
    }

    fn double_click(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        let (x, y) = self.view_point(x, y);
        let (spent, changed) = self.dictation_press(x, y);
        if spent {
            return changed;
        }
        changed | self.view.mouse_double_click(x, y)
    }

    fn context_menu(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        let (x, y) = self.view_point(x, y);
        let (spent, changed) = self.dictation_press(x, y);
        if spent {
            return changed;
        }
        changed | self.view.context_menu(x, y)
    }

    fn drag(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        let (x, y) = self.view_point(x, y);
        self.view.mouse_move(x, y)
    }

    fn scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool {
        if mods.control_key() || mods.super_key() {
            // An open picture takes the zoom wheel; the window keeps its own.
            if self.view.viewer_zoom_wheel(delta) {
                return true;
            }
            return self.rezoom(self.zoom() + delta.signum() * 0.1);
        }
        self.view.scroll_by(delta)
    }

    fn key(
        &mut self,
        _engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // **A key while the composer records stops it and keeps its words**
        // (koru-ai D103), and is spent on that: the composer cannot take a
        // key mid-composition, and Escape must not reach the chat and arm
        // the stop of a running reply. A modifier alone is not typing.
        if self.dictation.is_live() && !is_modifier(key) {
            return self.stop_dictation() | true;
        }
        if (mods.control_key() || mods.super_key()) && !mods.alt_key() && !self.view.viewer_open() {
            let zoom = match key {
                Key::Character(value) if value.as_str() == "+" || value.as_str() == "=" => {
                    Some(self.zoom() + 0.1)
                }
                Key::Character(value) if value.as_str() == "-" => Some(self.zoom() - 0.1),
                Key::Character(value) if value.as_str() == "0" => Some(1.0),
                _ => None,
            };
            if let Some(zoom) = zoom {
                return self.rezoom(zoom);
            }
        }
        self.view.key(key, text, mods)
    }

    fn physical_key_pressed(
        &mut self,
        _engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        let chord = koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord;
        if !chord.matches(key, text, mods) {
            return false;
        }
        if !repeat && self.dictation_key.is_none() {
            // A recording the microphone began has no key to release: the
            // chord ends it, keeping its words (D103).
            if self.dictation.is_live() {
                self.stop_dictation();
            } else {
                self.begin_dictation(Some(physical));
            }
        }
        true
    }

    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.finish_dictation(physical)
    }

    fn focus_changed(&mut self, focused: bool) -> bool {
        if let Some(activation) = &self.activation {
            activation.set_focused(focused);
        }
        if focused {
            false
        } else {
            self.interrupt_dictation(koru_dictation::Interruption::FocusLost)
        }
    }

    fn system_theme_changed(&mut self, dark: bool) -> bool {
        if !self.appearance.follows_system() || theme::is_dark() == dark {
            return false;
        }
        theme::set_dark(dark);
        self.view.invalidate();
        true
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        [
            self.view.surface.maintenance_deadline(),
            self.dictation.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn maintain(&mut self, now: Instant) -> bool {
        // Drained here as well as on the wake: `maintain` runs after every
        // event, and a section that landed during a keystroke should not wait
        // for the proxy round trip.
        self.view.surface.maintain(now)
            | self.begin_asked_dictation()
            | self.service_dictation()
            | self.adopt_settings()
            | self.service_open_requests()
            | self.view.pick_pending_folder()
    }

    fn animating(&self) -> bool {
        self.view.surface.animating()
    }

    fn mouse_up(&mut self) -> bool {
        self.view.surface.mouse_up()
    }

    /// The mouse's thumb buttons: back and forward over the chats this
    /// window read in place (D102).
    fn navigate_history(&mut self, delta: i32) -> bool {
        self.view.navigate_history(delta)
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        let changed = self.view.maximized != maximised;
        self.view.maximized = maximised;
        self.geometry.set_maximized(maximised);
        changed
    }

    fn set_pinned(&mut self, pinned: bool) -> bool {
        let changed = self.view.pinned != pinned;
        self.view.pinned = pinned;
        self.geometry.set_pinned(pinned);
        changed
    }

    fn pinned(&self) -> bool {
        self.geometry.pinned()
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        let request = self.view.window_request.take();
        if request == Some(WindowRequest::Close) {
            self.request_exit();
        }
        request
    }

    fn draw(&mut self, canvas: &mut vello::Scene, engine: &mut TextEngine, _w: f32, _h: f32) {
        self.view
            .draw(canvas, engine, Affine::scale(self.zoom() as f64));
    }
}

/// **What the composer draws about a dictation** (koru-ai D103): its level,
/// whether the microphone is open, and whether the last words are being
/// finished — never the words. The workspace shell answers the same way
/// (`apps/koru`'s `mainview::dictation::chat::shown`).
fn shown(status: koru_dictation::Status) -> DictationShown {
    use koru_dictation::{Stage, State};
    DictationShown {
        level: status.level.step(),
        listening: status.stage == Stage::Live && status.worker == State::Listening,
        finishing: status.stage == Stage::Finalising || status.worker == State::Finalising,
    }
}

/// A modifier pressed on its own: the start of a chord, not typing.
fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Control
                | NamedKey::Shift
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
        )
    )
}

/// The step `HistoryBack` or `HistoryForward` asks for, if the key is one.
fn history_chord(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<i32> {
    let is = |id| koru_keys::registry::get(id).chord.matches(key, text, mods);
    if is(koru_keys::ShortcutId::HistoryBack) {
        Some(-1)
    } else if is(koru_keys::ShortcutId::HistoryForward) {
        Some(1)
    } else {
        None
    }
}

struct ChatView {
    surface: ChatSurface,
    /// **Settings, when it is open.** Koru AI is a one-surface window, so it
    /// has no tab to put this in: the window menu opens it, it draws over the
    /// content band instead of the chat, and Escape closes it. Global and AI
    /// (R6) — it shows no Terminal section because it hosts no terminal.
    settings: Option<SettingsSurface>,
    viewport: (f32, f32),
    pointer: (f32, f32),
    maximized: bool,
    pinned: bool,
    window_request: Option<WindowRequest>,
    window_menu: Option<Menu<WindowMenuItem>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptionHit {
    Control(CaptionAction),
    Caption,
}

#[derive(Clone, Copy)]
enum WindowMenuItem {
    Settings(bool),
    Pin(bool),
    Minimise,
    Maximise(bool),
    #[cfg(windows)]
    SnapLeft,
    #[cfg(windows)]
    SnapRight,
    Close,
}

impl MenuItem for WindowMenuItem {
    fn label(&self) -> Cow<'_, str> {
        Cow::Borrowed(match self {
            Self::Settings(_) => "Settings",
            Self::Pin(true) => "Turn Off Always on Top",
            Self::Pin(false) => "Always on Top",
            Self::Minimise => "Minimize",
            Self::Maximise(true) => "Restore",
            Self::Maximise(false) => "Maximize",
            #[cfg(windows)]
            Self::SnapLeft => "Snap Left",
            #[cfg(windows)]
            Self::SnapRight => "Snap Right",
            Self::Close => "Close Window",
        })
    }

    fn icon(&self) -> Option<char> {
        Some(match self {
            Self::Settings(_) => icons::WRENCH,
            Self::Pin(_) => icons::PUSH_PIN,
            Self::Minimise => icons::WINDOW_MINIMISE,
            Self::Maximise(true) => icons::WINDOW_RESTORE,
            Self::Maximise(false) => icons::SQUARE,
            #[cfg(windows)]
            Self::SnapLeft => icons::ALIGN_LEFT,
            #[cfg(windows)]
            Self::SnapRight => icons::ALIGN_RIGHT,
            Self::Close => icons::WINDOW_CLOSE,
        })
    }

    fn filled_icon(&self) -> bool {
        matches!(self, Self::Pin(true))
    }

    fn checked(&self) -> Option<bool> {
        match self {
            Self::Settings(open) => Some(*open),
            Self::Pin(pinned) => Some(*pinned),
            _ => None,
        }
    }

    fn destructive(&self) -> bool {
        matches!(self, Self::Close)
    }
}

impl ChatView {
    fn new(
        link: Option<(koru_ai_core::ChatId, Option<koru_ai_core::MessageBlockId>)>,
        root: Option<PathBuf>,
        viewport: (f32, f32),
        maximized: bool,
        pinned: bool,
    ) -> Self {
        let mut surface = ChatSurface::new_at(SurfaceMode::Standalone, root);
        if let Some((chat, block)) = link {
            surface.open_link(chat, block);
        }
        surface.activate();
        surface.set_viewport(Rect::new(
            0.0,
            theme::CAPTION_H,
            viewport.0,
            (viewport.1 - theme::CAPTION_H).max(0.0),
        ));
        Self {
            surface,
            settings: None,
            viewport,
            pointer: (0.0, 0.0),
            maximized,
            pinned,
            window_request: None,
            window_menu: None,
        }
    }

    /// An agent chat asked for its working folder: the native picker is the
    /// window's. Cancelling answers nothing.
    fn pick_pending_folder(&mut self) -> bool {
        let Some(request) = self.surface.take_working_folder_request() else {
            return false;
        };
        let mut dialog = rfd::FileDialog::new().set_title(koru_i18n::t(
            koru_i18n::current(),
            koru_i18n::Key::AiAgentChooseFolder,
        ));
        if let Some(start) = request.start.as_deref() {
            dialog = dialog.set_directory(start);
        }
        match dialog.pick_folder() {
            Some(folder) => self.surface.working_folder_chosen(request.chat, folder),
            None => false,
        }
    }

    fn save_pending_credential(&mut self) -> bool {
        let Some(request) = self.surface.take_openai_credential_request() else {
            return false;
        };
        let account = format!("koru-{}", koru_channel::segment());
        let result = match request.into_secret() {
            Some(secret) => SecretString::new(secret)
                .and_then(|secret| OsCredentialStore.save_openai(&account, secret)),
            None => OsCredentialStore.remove_openai(&account),
        }
        .map_err(|error| error.public_message);
        self.surface.openai_credential_saved(result)
    }

    fn caption_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.viewport.0, theme::CAPTION_H)
    }

    fn caption_hit(&self, x: f32, y: f32) -> Option<CaptionHit> {
        let cap = self.caption_rect();
        if !cap.contains(x, y) {
            return None;
        }
        caption::hit(cap, x, y)
            .map(CaptionHit::Control)
            .or(Some(CaptionHit::Caption))
    }

    fn menu_items(&self) -> Vec<WindowMenuItem> {
        vec![
            WindowMenuItem::Settings(self.settings.is_some()),
            WindowMenuItem::Pin(self.pinned),
            WindowMenuItem::Minimise,
            WindowMenuItem::Maximise(self.maximized),
            #[cfg(windows)]
            WindowMenuItem::SnapLeft,
            #[cfg(windows)]
            WindowMenuItem::SnapRight,
            WindowMenuItem::Close,
        ]
    }

    fn open_window_menu(&mut self, engine: &mut TextEngine) {
        let Some(button) = caption::controls(self.caption_rect())
            .into_iter()
            .find(|control| control.action == CaptionAction::WindowMenu)
            .map(|control| control.rect)
        else {
            return;
        };
        self.window_menu = Some(Menu::aligned(
            engine,
            (button.x + button.w, button.y + button.h),
            self.menu_items(),
            Anchor::ButtonRight,
        ));
    }

    fn request_for_item(item: WindowMenuItem) -> Option<WindowRequest> {
        Some(match item {
            // Opening Settings is this window's own business; the shell is not
            // told, because nothing about the window changes.
            WindowMenuItem::Settings(_) => return None,
            WindowMenuItem::Pin(_) => WindowRequest::TogglePin,
            WindowMenuItem::Minimise => WindowRequest::Minimise,
            WindowMenuItem::Maximise(_) => WindowRequest::ToggleMaximise,
            #[cfg(windows)]
            WindowMenuItem::SnapLeft => WindowRequest::Place(koru_window::snap::Placement::Half(
                koru_window::snap::Side::Left,
            )),
            #[cfg(windows)]
            WindowMenuItem::SnapRight => WindowRequest::Place(koru_window::snap::Placement::Half(
                koru_window::snap::Side::Right,
            )),
            WindowMenuItem::Close => WindowRequest::Close,
        })
    }

    fn caption_mouse_down(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if let Some(menu) = &self.window_menu {
            if let Some(index) = menu.item_at(self.viewport, x, y) {
                let item = menu.items()[index];
                self.window_menu = None;
                match item {
                    WindowMenuItem::Settings(_) => self.toggle_settings(),
                    other => self.window_request = Self::request_for_item(other),
                }
                return true;
            }
            self.window_menu = None;
        }
        match self.caption_hit(x, y) {
            Some(CaptionHit::Control(CaptionAction::WindowMenu)) => {
                self.open_window_menu(engine);
                true
            }
            Some(CaptionHit::Control(CaptionAction::Minimise)) => {
                self.window_request = Some(WindowRequest::Minimise);
                true
            }
            Some(CaptionHit::Control(CaptionAction::ToggleMaximise)) => {
                self.window_request = Some(WindowRequest::ToggleMaximise);
                true
            }
            Some(CaptionHit::Control(CaptionAction::Close)) => {
                self.window_request = Some(WindowRequest::Close);
                true
            }
            Some(CaptionHit::Caption) => {
                self.window_request = Some(WindowRequest::Drag);
                true
            }
            _ => false,
        }
    }

    /// **Open or close Settings.** The surface is built on demand and dropped
    /// when it closes, which is also when its link's worker stops — a window
    /// that is not showing settings holds no connection for them.
    fn toggle_settings(&mut self) {
        if self.settings.take().is_some() {
            return;
        }
        let mut settings = SettingsSurface::with_links(
            // **Fed, not linked.** This window has held a `GlobalLink` since
            // SP-6b and hears every change fact on it already; a second link to
            // the same section would be a second connection answering the same
            // question. `ChatApp::adopt_settings` hands the section down.
            Sections::koru_ai(),
            SettingsLinks::disabled(),
        );
        settings.set_lang(koru_i18n::Lang::for_system(
            koru_window::system_locale::system_locale().as_deref(),
        ));
        settings.set_ai_slot(AiSlot {
            note: "Providers, models and reasoning are set up in the chat's own \
                   setup panel."
                .to_owned(),
            action: None,
        });
        settings.set_viewport(self.viewport.0, self.viewport.1);
        self.settings = Some(settings);
    }

    fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        if matches!(key, Key::Named(NamedKey::Escape)) && self.window_menu.take().is_some() {
            return true;
        }
        if let Some(settings) = self.settings.as_mut() {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.settings = None;
                return true;
            }
            return settings.key(key, text, mods);
        }
        // **Back and forward, before the chat sees the key** (D102) — from
        // the composer too. The registry's chord is the workspace window's:
        // `Alt`+←/→, which types nothing in a text field off macOS, and
        // `⌘[`/`⌘]` on macOS, where `⌥`+←/→ is word motion and stays the
        // composer's. Taken even with nowhere to go, as the workspace
        // window takes it, so it never reaches the text as something else.
        if let Some(delta) = history_chord(key, text, mods) {
            self.navigate_history(delta);
            return true;
        }
        let changed = self.surface.key(key, text, mods);
        self.save_pending_credential() || changed
    }

    /// Back (`-1`) or forward (`+1`) through the chats this window read
    /// into its place. Nothing while Settings or a picture covers the chat:
    /// the chat underneath must not change unseen.
    fn navigate_history(&mut self, delta: i32) -> bool {
        if self.settings.is_some() || self.surface.viewer_open() {
            return false;
        }
        self.window_menu = None;
        self.surface.step_chat_history(delta)
    }

    /// Whether the chat's picture viewer is up. It covers the whole window,
    /// caption included, so while it is up every press is its own.
    fn viewer_open(&self) -> bool {
        self.settings.is_none() && self.surface.viewer_open()
    }

    /// `Ctrl`+wheel over an open picture zooms the picture.
    fn viewer_zoom_wheel(&mut self, delta: f32) -> bool {
        if !self.viewer_open() {
            return false;
        }
        let wheel = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, delta as f64));
        let (x, y) = self.pointer;
        self.surface.viewer_zoom_wheel(wheel, x, y)
    }

    /// What a press means to the composer's recording (D103), where the
    /// composer is what the press reaches: not over the caption, the window
    /// menu, the Settings surface or an open picture.
    fn dictation_press_at(&self, x: f32, y: f32) -> Option<DictationPress> {
        if self.viewer_open()
            || self.window_menu.is_some()
            || self.settings.is_some()
            || y <= theme::CAPTION_H
        {
            return None;
        }
        self.surface.dictation_press_at(x, y)
    }

    fn mouse_down(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if self.viewer_open() {
            return self.surface.mouse_down(x, y);
        }
        let changed = if self.window_menu.is_some() || y <= theme::CAPTION_H {
            self.caption_mouse_down(engine, x, y)
        } else if let Some(settings) = self.settings.as_mut() {
            let pressed = settings.mouse_down(x, y);
            self.drain_settings_requests();
            pressed
        } else {
            self.surface.mouse_down(x, y)
        };
        self.save_pending_credential() || changed
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let old_pointer = self.pointer;
        self.pointer = (x, y);
        let surface_changed = match self.settings.as_mut() {
            Some(settings) => settings.mouse_move(x, y),
            None => self.surface.mouse_move(x, y),
        };
        if let Some(menu) = &mut self.window_menu {
            let next = menu.item_at(self.viewport, x, y);
            let changed = next != menu.hover;
            menu.hover = next;
            surface_changed || changed
        } else if y <= theme::CAPTION_H || old_pointer.1 <= theme::CAPTION_H {
            true
        } else {
            surface_changed
        }
    }

    fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        if self.viewer_open() {
            return self.surface.mouse_double_click(x, y);
        }
        if self.caption_hit(x, y) == Some(CaptionHit::Caption) {
            self.window_request = Some(WindowRequest::ToggleMaximise);
            true
        } else {
            self.surface.mouse_double_click(x, y)
        }
    }

    fn context_menu(&mut self, x: f32, y: f32) -> bool {
        if y <= theme::CAPTION_H {
            false
        } else {
            self.surface.context_menu(x, y)
        }
    }

    fn scroll_by(&mut self, delta: f32) -> bool {
        let wheel = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, delta as f64));
        match self.settings.as_mut() {
            Some(settings) => {
                settings.scroll_by(wheel);
                true
            }
            None => self.surface.scroll_by(wheel),
        }
    }

    fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = (width, height);
        if let Some(settings) = self.settings.as_mut() {
            settings.set_viewport(width, height);
        }
        self.surface.set_viewport(Rect::new(
            0.0,
            theme::CAPTION_H,
            width,
            (height - theme::CAPTION_H).max(0.0),
        ));
        // The surface is laid out in the window's coordinates, so a picture
        // it opens is lifted over the whole window, caption included.
        self.surface.set_viewer_lift(Some(koru_ai_ui::ViewerLift {
            origin: (0.0, 0.0),
            size: (width, height),
        }));
    }

    fn cursor_icon_at(&self, x: f32, y: f32) -> CursorIcon {
        if self.viewer_open() {
            return self.surface.cursor_icon();
        }
        if self
            .window_menu
            .as_ref()
            .is_some_and(|menu| menu.item_at(self.viewport, x, y).is_some())
            || matches!(self.caption_hit(x, y), Some(CaptionHit::Control(_)))
        {
            CursorIcon::Default
        } else if let Some(settings) = self.settings.as_ref() {
            settings.cursor_icon()
        } else {
            self.surface.cursor_icon()
        }
    }

    fn open_external_chat_link(
        &mut self,
        chat: koru_ai_core::ChatId,
        block: Option<koru_ai_core::MessageBlockId>,
    ) -> bool {
        self.surface.open_link(chat, block)
    }

    fn invalidate(&mut self) {
        self.surface.invalidate();
        if let Some(settings) = self.settings.as_mut() {
            settings.invalidate();
        }
    }

    /// The Settings surface asked for something. Koru AI can perform almost
    /// none of it — it has no document tabs, no `renderer.conf` of its own to
    /// write and no voice model cache to walk — so the rows that would raise
    /// those are the rows this host does not fill.
    fn drain_settings_requests(&mut self) {
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        // Every other request belongs to a host that has the thing it acts on.
        // This window fills none of those rows, so none of them can arrive; if
        // one ever does, doing nothing is the honest answer rather than a
        // half-performed effect.
        if settings
            .take_requests()
            .contains(&koru_settings_ui::Request::OpenAiSetup)
        {
            self.settings = None;
        }
    }

    fn draw(&mut self, canvas: &mut vello::Scene, engine: &mut TextEngine, root: Affine) {
        match self.settings.as_mut() {
            // Settings replaces the chat rather than floating over it: this
            // window has one content band, and a settings page you can see the
            // transcript through is two pages at once.
            Some(settings) => settings.draw(canvas, root),
            None => self.surface.draw(canvas, root),
        }
        self.draw_caption(canvas, engine, root);
        if let Some(menu) = &self.window_menu {
            #[cfg(windows)]
            let enabled = [true; 7];
            #[cfg(not(windows))]
            let enabled = [true; 5];
            menu.draw(canvas, root, engine, self.viewport, &enabled);
        }
        // The chat's picture viewer, last, over the caption too.
        if self.settings.is_none() {
            self.surface.draw_lifted_viewer(canvas, root);
        }
    }

    fn draw_caption(&mut self, canvas: &mut vello::Scene, engine: &mut TextEngine, root: Affine) {
        let palette = theme::p();
        let cap = self.caption_rect();
        scene::fill(
            canvas,
            root,
            palette.bg,
            &scene::rect(cap.x, cap.y, cap.w, cap.h),
        );
        scene::fill(
            canvas,
            root,
            palette.panel_border,
            &scene::rect(cap.x, cap.y + cap.h - 1.0, cap.w, 1.0),
        );
        let buttons_left = caption::controls(cap)
            .first()
            .map_or(cap.x + cap.w, |control| control.rect.x);
        let title = engine.simple_layout_wrapped(
            "Koru AI",
            theme::role::row(),
            (buttons_left - theme::CAPTION_PAD_X * 2.0).max(1.0),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            theme::CAPTION_PAD_X,
            (cap.h - title.height()) * 0.5,
            Some(palette.text),
        );
        caption::draw(
            canvas,
            root,
            engine,
            cap,
            caption::State {
                hovered: match self.caption_hit(self.pointer.0, self.pointer.1) {
                    Some(CaptionHit::Control(action)) => Some(action),
                    _ => None,
                },
                maximized: self.maximized,
                menu: caption::MenuVisual::Static,
            },
        );
    }
}

pub fn geometry_path(channel_root: Option<&Path>) -> Option<PathBuf> {
    match channel_root {
        Some(root) => Some(root.join("agent").join("windows.conf")),
        None => koru_window::window_geometry_path(None),
    }
}

/// Koru's mark with the sparkle badge that distinguishes this auxiliary window
/// in the taskbar and app switcher.
fn ai_chat_icon() -> Option<Icon> {
    let bytes: &[u8] = match koru_channel::segment() {
        "stable" => include_bytes!("../../../assets/koru-ai-chat.png"),
        "beta" => include_bytes!("../../../assets/koru-ai-chat-beta.png"),
        "nightly" => include_bytes!("../../../assets/koru-ai-chat-nightly.png"),
        "internal-release" => include_bytes!("../../../assets/koru-ai-chat-internal-release.png"),
        _ => include_bytes!("../../../assets/koru-ai-chat-internal-debug.png"),
    };
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_caption_uses_shared_window_button_geometry() {
        let view = ChatView::new(
            None,
            None,
            (
                crate::geometry::DEFAULT_WIDTH,
                crate::geometry::DEFAULT_HEIGHT,
            ),
            false,
            false,
        );
        for control in caption::controls(view.caption_rect()) {
            let (x, y) = control.rect.centre();
            assert_eq!(
                view.caption_hit(x, y),
                Some(CaptionHit::Control(control.action))
            );
        }
    }

    #[test]
    fn standalone_window_menu_maps_every_row_to_a_window_request() {
        assert_eq!(
            ChatView::request_for_item(WindowMenuItem::Pin(false)),
            Some(WindowRequest::TogglePin)
        );
        assert_eq!(
            ChatView::request_for_item(WindowMenuItem::Minimise),
            Some(WindowRequest::Minimise)
        );
        assert_eq!(
            ChatView::request_for_item(WindowMenuItem::Maximise(false)),
            Some(WindowRequest::ToggleMaximise)
        );
        assert_eq!(
            ChatView::request_for_item(WindowMenuItem::Close),
            Some(WindowRequest::Close)
        );
        // **The one row the shell is not told about.** Opening Settings changes
        // nothing about the window, so there is no request to make; answering
        //  is how the menu says a row is this window's own business.
        assert_eq!(
            ChatView::request_for_item(WindowMenuItem::Settings(false)),
            None
        );
    }

    /// **Koru AI shows Global and AI, and no Terminal section** (R6): it hosts
    /// no terminal, so a Terminal row would be a control with nothing to
    /// control.
    #[test]
    fn the_settings_surface_shows_the_two_sections_this_window_owns() {
        let mut view = ChatView::new(None, None, (900.0, 700.0), false, false);
        assert!(view.settings.is_none(), "the window opens on the chat");
        view.toggle_settings();
        let settings = view.settings.as_ref().expect("Settings opened");
        assert_eq!(settings.sections(), Sections::koru_ai());
        // ...and it holds no link of its own: this window already has one, and
        // two connections answering the same question is one too many.
        assert!(!settings.links().holds_global());
        assert!(!settings.links().holds_terminal());
        view.toggle_settings();
        assert!(view.settings.is_none(), "and the same row closes it");
    }

    /// **The history chord is the window's, from the composer too** (D102):
    /// `Alt`+←/→ (`⌘[`/`⌘]` on macOS) is taken before the chat, so it never
    /// moves the caret or types, and a plain arrow still reaches the text.
    #[test]
    fn the_history_chord_is_taken_before_the_composer() {
        let mut view = ChatView::new(None, None, (900.0, 700.0), false, false);
        for letter in ["h", "i"] {
            view.key(
                &Key::Character(letter.into()),
                Some(letter),
                ModifiersState::empty(),
            );
        }
        let typed = view.surface.dictation_composer().text();
        assert_eq!(typed, "hi", "the composer has the keyboard");
        for id in [
            koru_keys::ShortcutId::HistoryBack,
            koru_keys::ShortcutId::HistoryForward,
        ] {
            let chord = koru_keys::registry::get(id).chord;
            let (key, text) = match chord.on {
                koru_keys::On::Named(named) => (Key::Named(named), None),
                koru_keys::On::Letter(letter) => (Key::Character(letter.to_string().into()), None),
            };
            assert!(view.key(&key, text, chord.mods), "{id:?} is taken");
            assert!(history_chord(&key, text, chord.mods).is_some());
        }
        assert_eq!(
            view.surface.dictation_composer().text(),
            typed,
            "nothing moved into the message"
        );
        assert_eq!(
            history_chord(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::empty()
            ),
            None,
            "a plain arrow is the composer's"
        );
    }

    /// Escape closes Settings rather than reaching the chat underneath it.
    #[test]
    fn escape_closes_settings() {
        let mut view = ChatView::new(None, None, (900.0, 700.0), false, false);
        view.toggle_settings();
        assert!(view.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
        assert!(view.settings.is_none());
    }

    /// The window feeds the Global section it already hears about down to the
    /// surface, rather than the surface opening a second link for it.
    #[test]
    fn the_window_feeds_the_section_it_already_hears_about() {
        let mut view = ChatView::new(None, None, (900.0, 700.0), false, false);
        view.toggle_settings();
        let settings = view.settings.as_mut().expect("Settings opened");
        assert!(settings.adopt_global(&koru_settings::GlobalSettings {
            start_on_login: true,
            ..koru_settings::GlobalSettings::default()
        }));
    }

    #[test]
    fn an_explicit_root_keeps_geometry_under_the_same_agent_root() {
        assert_eq!(
            geometry_path(Some(Path::new("C:/isolated"))),
            Some(PathBuf::from("C:/isolated/agent/windows.conf"))
        );
    }

    #[test]
    fn standalone_chat_hosts_the_physical_dictation_hold() {
        let mut app = ChatApp::new(
            None,
            None,
            None,
            None,
            None,
            Appearance::default(),
            koru_api_client::GlobalLink::disabled(),
        );
        app.dictation
            .set_consent(Some(koru_voice_consent::Granted::recorded(
                koru_voice_consent::Terms {
                    model: "whisper-base".into(),
                    version: "q5_1".into(),
                    licence: "MIT".into(),
                    attribution: "OpenAI".into(),
                    download_bytes: 1,
                },
                1,
            )));
        let physical = PhysicalKey::Code(winit::keyboard::KeyCode::KeyD);
        let key = Key::Character("d".into());
        let chord = koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord;
        let mut engine = TextEngine::new();

        assert!(app.physical_key_pressed(
            &mut engine,
            physical,
            &key,
            Some("d"),
            chord.mods,
            false,
        ));
        assert!(app.dictation.is_live());
        assert_eq!(app.dictation_key, Some(physical));

        app.physical_key_released(physical);
        assert_eq!(app.dictation.stage(), koru_dictation::Stage::Finalising);
        assert_eq!(app.dictation_key, None);
    }

    /// A window whose controller has been agreed with, its composer offering
    /// the microphone, laid out once.
    fn recording_app(engine: &mut TextEngine) -> ChatApp {
        let mut app = ChatApp::new(
            None,
            None,
            None,
            None,
            None,
            Appearance::default(),
            koru_api_client::GlobalLink::disabled(),
        );
        app.dictation
            .set_consent(Some(koru_voice_consent::Granted::recorded(
                koru_voice_consent::Terms {
                    model: "nemotron".into(),
                    version: "1".into(),
                    licence: "NVIDIA".into(),
                    attribution: "NVIDIA".into(),
                    download_bytes: 1,
                },
                1,
            )));
        app.view.surface.offer_dictation(true);
        app.draw(&mut vello::Scene::new(), engine, 900.0, 700.0);
        app
    }

    fn centre(rect: Rect) -> (f32, f32) {
        (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
    }

    /// What the worker would say, for the session the controller runs.
    fn say(
        app: &mut ChatApp,
        event: impl FnOnce(koru_dictation::SessionId, u64) -> koru_dictation::Event,
    ) {
        let session = app.dictation.session().expect("a session");
        let generation = app.dictation.generation().expect("a generation");
        app.dictation.observe(event(session, generation));
        app.service_dictation();
    }

    /// **The microphone records into the composer, and Escape stops it**
    /// (koru-ai D103): the press begins a dictation with no key to release,
    /// the footer shows the recording, partial words are not the message,
    /// Escape finishes them — spent, never reaching the chat — and the final
    /// words land after what was typed.
    #[test]
    fn the_microphone_records_and_escape_stops_keeping_the_words() {
        let mut engine = TextEngine::new();
        let mut app = recording_app(&mut engine);
        for letter in ["h", "i", " "] {
            app.key(
                &mut engine,
                &Key::Character(letter.into()),
                Some(letter),
                ModifiersState::empty(),
            );
        }
        let mic = app
            .view
            .surface
            .microphone_for_host_test()
            .expect("the footer has a microphone");
        let (x, y) = centre(mic);
        assert!(app.click(&mut engine, x, y, false));
        assert!(app.dictation.is_live(), "the press began a dictation");
        assert_eq!(app.dictation_key, None, "with no key to release");
        say(&mut app, |session, generation| {
            koru_dictation::Event::State {
                session,
                generation,
                state: koru_dictation::State::Listening,
            }
        });
        assert!(app.view.surface.dictating(), "the footer records");
        say(&mut app, |session, generation| {
            koru_dictation::Event::Partial {
                session,
                generation,
                text: koru_dictation::Transcript::new("hello"),
            }
        });
        assert_eq!(app.view.surface.dictation_composer().text(), "hi ");

        assert!(app.key(
            &mut engine,
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty()
        ));
        assert_eq!(
            app.dictation.stage(),
            koru_dictation::Stage::Finalising,
            "Escape stops, and keeps the words"
        );
        say(&mut app, |session, generation| {
            koru_dictation::Event::Final {
                session,
                generation,
                text: koru_dictation::Transcript::new("hello there"),
            }
        });
        say(&mut app, |session, generation| {
            koru_dictation::Event::Ended {
                session,
                generation,
                reason: koru_dictation::EndReason::Completed,
            }
        });
        assert_eq!(
            app.view.surface.dictation_composer().text(),
            "hi hello there"
        );
        assert!(!app.view.surface.dictating(), "the footer is back");
    }

    /// **✕ throws away only the recording's words**, and the chord ends a
    /// recording the microphone began.
    #[test]
    fn cancel_discards_the_recording_and_the_chord_stops_one() {
        let mut engine = TextEngine::new();
        let mut app = recording_app(&mut engine);
        app.key(
            &mut engine,
            &Key::Character("a".into()),
            Some("a"),
            ModifiersState::empty(),
        );
        let (x, y) = centre(app.view.surface.microphone_for_host_test().unwrap());
        app.click(&mut engine, x, y, false);
        say(&mut app, |session, generation| {
            koru_dictation::Event::Partial {
                session,
                generation,
                text: koru_dictation::Transcript::new("spoken"),
            }
        });
        app.draw(&mut vello::Scene::new(), &mut engine, 900.0, 700.0);
        let (cancel, _) = app
            .view
            .surface
            .dictation_buttons_for_host_test()
            .expect("✕ and ■");
        let (x, y) = centre(cancel);
        assert!(app.click(&mut engine, x, y, false));
        assert!(!app.dictation.is_live(), "✕ ended it");
        assert_eq!(app.view.surface.dictation_composer().text(), "a");
        assert!(!app.view.surface.dictating());

        // Begun again from the microphone, the chord stops it.
        app.draw(&mut vello::Scene::new(), &mut engine, 900.0, 700.0);
        let (x, y) = centre(app.view.surface.microphone_for_host_test().unwrap());
        app.click(&mut engine, x, y, false);
        assert!(app.dictation.is_live());
        let chord = koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord;
        assert!(app.physical_key_pressed(
            &mut engine,
            PhysicalKey::Code(winit::keyboard::KeyCode::KeyD),
            &Key::Character("d".into()),
            Some("d"),
            chord.mods,
            false,
        ));
        assert_eq!(app.dictation.stage(), koru_dictation::Stage::Finalising);
    }

    #[test]
    fn exit_snapshot_wins_over_queued_geometry_events() {
        let root = std::env::temp_dir().join(format!(
            "koru-ai-chat-exit-geometry-{}",
            uuid::Uuid::new_v4()
        ));
        let path = root.join("windows.conf");
        let mut app = ChatApp::new(
            None,
            None,
            None,
            None,
            Some(path.clone()),
            Appearance::default(),
            koru_api_client::GlobalLink::disabled(),
        );
        app.exiting(Some(ClosingWindowState {
            position: Some(PhysicalPosition::new(30, 40)),
            inner_size: PhysicalSize::new(1_200, 1_400),
            scale_factor: 2.0,
            maximised: false,
            pinned: true,
        }));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("ai-chat = x=30;y=40;w=600;h=700;maximized=0;pinned=1"),
            "{body}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
