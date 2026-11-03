// SPDX-License-Identifier: MPL-2.0

//! AI chat hosted as a main-window tab.
//!
//! A thin wrapper around [`koru_ai_ui::ChatSurface`] so the container can treat
//! it like any other [`TabView`] arm. The surface runs in
//! [`SurfaceMode::ContextPane`] to avoid wire-protocol changes; it starts a
//! fresh chat, attaches workspace context, and deliberately does not pre-attach
//! the active document.

use koru_ai_ui::{ChatId, ChatSurface, OpenAiCredentialRequest, SurfaceMode, WorkingFolderRequest};
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_store_core::DocumentId;
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

pub struct AiChatView {
    /// **Boxed.** A chat surface is about 41 KB, and a tab is moved by value
    /// through the restore path and the pane tree: unboxed, every [`TabView`]
    /// was the surface's size, and rebuilding a saved chat tab at startup
    /// stacked enough copies to overflow the window thread's 1 MB stack
    /// (2026-11-02). One pointer here keeps every tab small.
    surface: Box<ChatSurface>,
    /// What the tab strip last read off the chat (D22), kept so the
    /// container's pass is a comparison and nothing more until it changes.
    face: ChatFace,
    /// What the tab's mark says (D83), as the window's status pass last
    /// decided — the tab strip draws it without asking again.
    status: koru_chats_ui::ChatStatus,
    /// Reduced motion: the ring holds still.
    still: bool,
}

/// The few facts a chat tab is drawn from: whether the chat has started, its
/// own title and who answers it. Empty while the chat has not started, which
/// is the face of a new chat — *AI Chat* and the chats glyph.
#[derive(Default)]
struct ChatFace {
    made: bool,
    /// `None` until the chat has a title of its own: not started, or started
    /// and its automatic title not landed yet.
    title: Option<String>,
    provider: String,
}

impl AiChatView {
    fn over(mut surface: Box<ChatSurface>) -> Self {
        surface.set_hidden_agents(hidden_agents());
        surface.set_agent_defaults(agent_defaults());
        surface.set_agent_order(agent_order());
        surface.set_favorite_models(favorite_models());
        // Koru has a Settings tab: the picker's + opens *Agent providers*.
        surface.offer_agent_settings(true);
        // A changed file's path shows the file in Explorer or Finder (D62).
        surface.offer_reveal_paths(koru_workspace_host::reveal::is_supported());
        // Koru opens documents as tabs: a stored reply is followed by
        // *Open* (D78).
        surface.offer_open_stored_documents(true);
        Self {
            surface,
            face: ChatFace::default(),
            status: koru_chats_ui::ChatStatus::Idle,
            still: false,
        }
    }

    /// **Hear this chat's news while its tab is behind another** (D83):
    /// the service's answers, folded in without drawing — a background tab
    /// has to know its reply ended to stop its ring and mark it unread.
    pub fn drain_in_background(&mut self) -> bool {
        self.surface.drain_updates()
    }

    /// When a tab behind another should next hear its news: while a reply
    /// it sent is on its way or running, and never otherwise.
    pub fn background_deadline(&self) -> Option<std::time::Instant> {
        self.surface.listing_deadline()
    }

    /// A reply ended, or a question came, since the window last asked.
    pub fn take_arrival(&mut self) -> bool {
        self.surface.take_arrival()
    }

    /// A send was taken or a reply ended since the window last asked: the
    /// chat moves up its list (D85).
    pub fn take_activity(&mut self) -> bool {
        self.surface.take_activity()
    }

    /// The newest finished message on screen — what a look has seen.
    pub fn newest_seen_message(&self) -> Option<koru_ai_ui::MessageId> {
        self.surface.newest_seen_message()
    }

    pub fn is_working(&self) -> bool {
        self.surface.is_working()
    }

    pub fn needs_you(&self) -> bool {
        self.surface.needs_you()
    }

    /// Take the status the window's pass decided; `true` when the mark
    /// changed and the strip owes a repaint.
    pub fn set_status(&mut self, status: koru_chats_ui::ChatStatus, reduced_motion: bool) -> bool {
        self.surface.set_reduced_motion(reduced_motion);
        let changed = self.status != status || self.still != reduced_motion;
        self.status = status;
        self.still = reduced_motion;
        changed
    }

    /// Open the chat on what the person has not seen, under a NEW divider,
    /// or put a divider away (`None`) — D84.
    pub fn show_new_from(&mut self, chat: ChatId, after: Option<koru_ai_ui::MessageId>) -> bool {
        self.surface.show_new_from(chat, after)
    }

    pub fn status(&self) -> koru_chats_ui::ChatStatus {
        self.status
    }

    /// The tab's mark, the ring turned to now unless motion is reduced.
    pub fn status_mark(&self) -> Option<koru_shell_chrome::status_mark::StatusMark> {
        self.status
            .mark((!self.still).then(std::time::Instant::now))
    }

    pub fn new() -> Self {
        let mut surface = Box::new(ChatSurface::new_at(
            SurfaceMode::ContextPane,
            koru_workspace_host::cli::root_from_args(),
        ));
        surface.activate();
        surface.start_new_chat();
        Self::over(surface)
    }

    /// Take the chat's face again if it moved — started, renamed, given its
    /// automatic title, reseated onto another chat. `true` when it did, and
    /// the tab's label is owed a refresh. Two string comparisons otherwise.
    pub fn refresh_face(&mut self) -> bool {
        let made = self.surface.current_chat_made();
        let (title, provider) = if made {
            (
                self.surface.current_chat_own_title(),
                self.surface.current_chat_provider().unwrap_or_default(),
            )
        } else {
            (None, "")
        };
        if made == self.face.made
            && title == self.face.title.as_deref()
            && provider == self.face.provider
        {
            return false;
        }
        self.face = ChatFace {
            made,
            title: title.map(str::to_owned),
            provider: provider.to_owned(),
        };
        true
    }

    /// The chat's full title once it has started and has one of its own;
    /// `None` before. The tab's label is this cut short; a screen reader is
    /// given all of it.
    pub fn started_title(&self) -> Option<&str> {
        self.face.title.as_deref()
    }

    /// The mark the tab wears once the chat has started: its agent's brand
    /// (Claude, Codex, …) through the one mapping the model picker draws
    /// with. `None` before the chat starts and for Koru's own providers,
    /// which keep the chats glyph.
    pub fn mark(&self) -> Option<koru_draw::brand::ProfileMark> {
        self.face
            .made
            .then(|| koru_ai_ui::provider_mark(&self.face.provider))
            .flatten()
    }

    pub fn chat_id(&self) -> Option<ChatId> {
        self.surface.current_chat_id()
    }

    /// Recent chats this surface has listed, most recent first. Empty until
    /// it has booted.
    pub fn recent_chats(&self, limit: usize) -> Vec<(ChatId, String)> {
        self.surface.recent_chats(limit)
    }

    /// A chat tab opened on its provider setup — where Settings' *Open Koru
    /// AI setup…* landed, as a tab of its own rather than the side panel.
    ///
    /// Dormant: nothing opens the setup for now (`ChatSurface::open_setup`).
    #[allow(dead_code)]
    pub fn setup() -> Self {
        let mut view = Self::new();
        view.surface.open_setup();
        view
    }

    /// A fresh chat that starts with `document` as a chip in the composer,
    /// as `@` would put it there: the page's `…` → *Open in chat*. The chip
    /// pins the document when the message is sent.
    pub fn on_document(document: DocumentId, title: String) -> Self {
        let mut view = Self::new();
        view.surface.attach_document(document, title);
        view
    }

    /// The message in the composer, chips in their `[[koru://doc/…]]`
    /// spelling.
    #[cfg(test)]
    pub fn composer_text(&mut self) -> String {
        self.surface.dictation_composer().text()
    }

    #[cfg(test)]
    pub fn setup_open(&self) -> bool {
        self.surface.setup_open()
    }

    /// Open an existing chat in this tab instead of starting a fresh one.
    pub fn existing(id: ChatId) -> Self {
        let mut surface = Box::new(ChatSurface::new_at(
            SurfaceMode::ContextPane,
            koru_workspace_host::cli::root_from_args(),
        ));
        surface.activate();
        surface.open_link(id, None);
        Self::over(surface)
    }

    /// Whether this tab's chat surface changed the chat list since the last
    /// ask — a chat made, renamed, deleted or answered. The window's Chats
    /// view refreshes on it (AGENTUX I5).
    pub fn take_chat_list_changed(&mut self) -> bool {
        self.surface.take_chat_list_changed()
    }

    /// Rename a chat through this tab's surface — the Chats view's rename,
    /// sent here when this tab shows the chat so its label follows.
    pub fn rename_chat(&mut self, chat: ChatId, title: &str) -> bool {
        self.surface.rename_chat(chat, title)
    }

    /// Pin or unpin a chat through this tab's surface — the Chats view's
    /// pin, sent here when this tab shows the chat (D86).
    pub fn pin_chat(&mut self, chat: ChatId, pinned: bool) -> bool {
        self.surface.pin_chat(chat, pinned)
    }

    /// `Ctrl+Shift+↑`/`↓` in this tab: the container moves to the chat
    /// above or below in its Chats list (D87).
    pub fn take_wants_chat_step(&mut self) -> Option<koru_chats_ui::ChatStep> {
        self.surface.take_wants_chat_step()
    }

    /// The chats this tab's surface last listed.
    pub fn listed_chats(&self) -> &[koru_chats_ui::Chat] {
        self.surface.listed_chats()
    }

    /// The *Chats* button in this tab was pressed: show the Chats view.
    pub fn take_wants_chats_view(&mut self) -> bool {
        self.surface.take_wants_chats_view()
    }

    pub fn set_context_document(&mut self, document: Option<(DocumentId, String)>) -> bool {
        self.surface.set_context_document(document)
    }

    pub fn take_openai_credential_request(&mut self) -> Option<OpenAiCredentialRequest> {
        self.surface.take_openai_credential_request()
    }

    pub fn openai_credential_saved(&mut self, result: Result<(), String>) -> bool {
        self.surface.openai_credential_saved(result)
    }

    pub fn take_working_folder_request(&mut self) -> Option<WorkingFolderRequest> {
        self.surface.take_working_folder_request()
    }

    pub fn working_folder_chosen(&mut self, chat: ChatId, folder: std::path::PathBuf) -> bool {
        self.surface.working_folder_chosen(chat, folder)
    }

    /// A document chip in this chat was clicked; the container opens it.
    pub fn take_open_document_request(&mut self) -> Option<koru_link::KoruLink> {
        self.surface.take_open_document_request()
    }

    /// The coding agents turned off on this machine (Agent providers).
    pub fn set_hidden_agents(&mut self, keys: Vec<String>) -> bool {
        self.surface.set_hidden_agents(keys)
    }

    /// Where a new chat starts (Agent providers).
    pub fn set_agent_defaults(&mut self, defaults: koru_ai_ui::AgentDefaults) -> bool {
        self.surface.set_agent_defaults(defaults)
    }

    /// The order the agents are listed in (Agent providers).
    pub fn set_agent_order(&mut self, keys: Vec<String>) -> bool {
        self.surface.set_agent_order(keys)
    }

    /// The picker's **+** was pressed: the container opens Settings on
    /// Agent providers.
    pub fn take_agent_settings_request(&mut self) -> bool {
        self.surface.take_agent_settings_request()
    }

    /// A changed file's path was pressed for a file not opened directly
    /// (D62): the container shows it in the file manager.
    pub fn take_reveal_path_request(&mut self) -> Option<std::path::PathBuf> {
        self.surface.take_reveal_path_request()
    }

    /// The models starred in the picker (D46), as the roaming setting holds
    /// them.
    pub fn set_favorite_models(&mut self, favorites: Vec<koru_ai_ui::FavoriteModel>) -> bool {
        self.surface.set_favorite_models(favorites)
    }

    /// A star was pressed in this chat's picker: the whole list to store.
    pub fn take_favorite_models_request(&mut self) -> Option<Vec<koru_ai_ui::FavoriteModel>> {
        self.surface.take_favorite_models_request()
    }
}

/// **The agents turned off on this machine, for chat tabs opened later.**
/// The window pushes the list to every surface it holds when it changes;
/// a tab made afterwards takes it from here. One list per process, because
/// the setting is one per machine.
static HIDDEN_AGENTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn set_hidden_agents(keys: Vec<String>) {
    *HIDDEN_AGENTS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = keys;
}

fn hidden_agents() -> Vec<String> {
    HIDDEN_AGENTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// **Where a new chat starts, for chat tabs opened later** — the same
/// arrangement as [`HIDDEN_AGENTS`], for the same reason.
static AGENT_DEFAULTS: std::sync::Mutex<Option<koru_ai_ui::AgentDefaults>> =
    std::sync::Mutex::new(None);

pub(crate) fn set_agent_defaults(defaults: koru_ai_ui::AgentDefaults) {
    *AGENT_DEFAULTS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(defaults);
}

fn agent_defaults() -> koru_ai_ui::AgentDefaults {
    AGENT_DEFAULTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or_default()
}

/// **The agents' order, for chat tabs opened later** — the same
/// arrangement again.
static AGENT_ORDER: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn set_agent_order(keys: Vec<String>) {
    *AGENT_ORDER
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = keys;
}

fn agent_order() -> Vec<String> {
    AGENT_ORDER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// **The picker's favorites, for chat tabs opened later** — the same
/// arrangement again; one list per process, because the setting is one per
/// person.
static FAVORITE_MODELS: std::sync::Mutex<Vec<koru_ai_ui::FavoriteModel>> =
    std::sync::Mutex::new(Vec::new());

pub(crate) fn set_favorite_models(favorites: Vec<koru_ai_ui::FavoriteModel>) {
    *FAVORITE_MODELS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = favorites;
}

fn favorite_models() -> Vec<koru_ai_ui::FavoriteModel> {
    FAVORITE_MODELS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

impl AiChatView {
    /// Whether a picture is open in the chat's viewer, which the container
    /// lifts to the whole window like a document's diagram viewer.
    pub fn viewer_open(&self) -> bool {
        self.surface.viewer_open()
    }

    pub fn set_viewer_lift(&mut self, lift: Option<koru_ai_ui::ViewerLift>) {
        self.surface.set_viewer_lift(lift);
    }

    pub fn draw_lifted_viewer(&mut self, scene: &mut Scene, root: Affine) {
        self.surface.draw_lifted_viewer(scene, root);
    }

    pub fn viewer_semantics(
        &self,
        lang: koru_i18n::Lang,
    ) -> Option<koru_ui::media_viewer::ViewerSemantics> {
        self.surface.viewer_semantics(lang)
    }

    pub fn viewer_act(&mut self, action: koru_ui::media_viewer::Action) -> bool {
        self.surface.viewer_act(action)
    }

    pub fn viewer_zoom_key(&mut self, key: koru_ui::media_viewer::ZoomKey) -> bool {
        self.surface.viewer_zoom_key(key)
    }

    pub fn viewer_zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) -> bool {
        self.surface.viewer_zoom_wheel(delta, x, y)
    }
}

impl View for AiChatView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.surface.key(key, text, mods)
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        self.surface.mouse_down(x, y)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.surface.mouse_move(x, y)
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        self.surface.mouse_up()
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        self.surface.mouse_double_click(x, y)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        self.surface.scroll_by(delta);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.surface.set_viewport(Rect::new(0.0, 0.0, w, h));
    }

    fn cursor_icon(&self) -> CursorIcon {
        self.surface.cursor_icon()
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.surface.invalidate();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        _images: &koru_draw::ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        let workspace_id = workspace
            .current()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()));
        self.surface.set_context_workspace(workspace_id);
        self.surface.set_context_document(None);
        self.surface
            .submit_embedded_when_durable(workspace.owed() == 0);
        self.surface.draw(scene, root);
    }

    fn animating(&self) -> bool {
        self.surface.animating()
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        self.surface.maintenance_deadline()
    }

    fn maintain(&mut self, _ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        self.surface.maintain(now)
    }
}
