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
    surface: ChatSurface,
}

impl AiChatView {
    pub fn new() -> Self {
        let mut surface = ChatSurface::new_at(
            SurfaceMode::ContextPane,
            koru_workspace_host::cli::root_from_args(),
        );
        surface.activate();
        surface.start_new_chat();
        Self { surface }
    }

    pub fn title(&self) -> Option<&str> {
        self.surface.current_chat_title()
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
        let mut surface = ChatSurface::new_at(
            SurfaceMode::ContextPane,
            koru_workspace_host::cli::root_from_args(),
        );
        surface.activate();
        surface.open_link(id, None);
        Self { surface }
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
