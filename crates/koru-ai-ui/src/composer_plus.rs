// SPDX-License-Identifier: MPL-2.0

//! The composer's plus menu. Its recent rows come from the containing window;
//! selecting one writes the same document pill as the `@` picker.

use std::borrow::Cow;
use std::path::PathBuf;

use koru_ai_core::{AttachmentKind, ChatAttachmentId, ChatId, DocumentId, WorkspaceId};
use koru_i18n::{Key as TextKey, t};
use koru_ui::menu::MenuItem;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::{ChatSurface, Choice, ChoiceOutcome, Focus, Menu, attachments, icons, reader::Command};

/// Which native picker the containing window should show for This computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputerPickerKind {
    File,
    Folder,
}

/// One file import tied to the chat and attachment id selected before the
/// first network step, so failures can retry the same operation.
#[derive(Clone, Debug)]
pub struct WorkspaceImportRequest {
    pub op: uuid::Uuid,
    pub chat: ChatId,
    pub workspace: WorkspaceId,
    pub attachment: ChatAttachmentId,
    pub path: PathBuf,
}

#[derive(Clone)]
pub(super) enum PlusItem {
    Recent(&'static str),
    KoruContext(&'static str),
    ThisComputer(&'static str),
    Commands(&'static str),
    Skills(&'static str),
    ComputerFile(&'static str),
    ComputerFolder(&'static str),
    SearchWeb(&'static str, bool),
    Back(&'static str),
    Document(DocumentId, String),
    Empty(&'static str),
    Hint(&'static str),
}

impl MenuItem for PlusItem {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Recent(label)
            | Self::KoruContext(label)
            | Self::ThisComputer(label)
            | Self::Commands(label)
            | Self::Skills(label)
            | Self::ComputerFile(label)
            | Self::ComputerFolder(label)
            | Self::SearchWeb(label, _)
            | Self::Back(label)
            | Self::Empty(label)
            | Self::Hint(label) => Cow::Borrowed(label),
            Self::Document(_, title) => Cow::Borrowed(title),
        }
    }

    fn icon(&self) -> Option<char> {
        match self {
            Self::Recent(_) => Some(icons::CLOCK),
            Self::KoruContext(_) | Self::Document(_, _) | Self::ComputerFile(_) => {
                Some(icons::DOCUMENT)
            }
            Self::ThisComputer(_) | Self::ComputerFolder(_) => Some(icons::FOLDER),
            Self::Commands(_) | Self::Skills(_) => Some(icons::SPARKLE),
            Self::SearchWeb(_, on) => Some(if *on { icons::CHECK } else { icons::GLOBE }),
            Self::Back(_) | Self::Empty(_) | Self::Hint(_) => None,
        }
    }

    fn has_submenu(&self) -> bool {
        matches!(self, Self::Recent(_) | Self::ThisComputer(_))
    }

    fn selectable(&self) -> bool {
        !matches!(self, Self::Empty(_) | Self::Hint(_))
    }
}

impl ChatSurface {
    /// The containing window's resolved recents, newest first. A changed
    /// title updates the next open of the menu; a menu already under the
    /// pointer keeps the rows it was opened with.
    pub fn set_recent_documents(&mut self, documents: Vec<(DocumentId, String)>) -> bool {
        let picker_documents = documents
            .iter()
            .map(|(document, title)| {
                (
                    koru_core::DocumentId::from_uuid(document.as_uuid()),
                    title.clone(),
                )
            })
            .collect();
        let picker_changed = self.composer.set_recent_documents(picker_documents);
        if self.recent_documents == documents {
            return picker_changed;
        }
        self.recent_documents = documents;
        true
    }

    fn plus_menu_at(&mut self, items: Vec<PlusItem>) {
        let button = self.plus_button();
        self.plus_menu = Some(Menu::new(&mut self.engine, (button.x, button.y), items));
    }

    pub(super) fn open_plus_menu(&mut self) {
        self.dismiss_popovers();
        let mut items = vec![
            PlusItem::Recent(t(self.lang, TextKey::HomeRecent)),
            PlusItem::KoruContext(t(self.lang, TextKey::AiComposerKoruContext)),
        ];
        if self.transcript.as_ref().is_some_and(|transcript| {
            crate::agents::is_agent_provider(&transcript.chat.current_model.provider)
        }) {
            items.push(PlusItem::ThisComputer(t(
                self.lang,
                TextKey::AiComposerThisComputer,
            )));
            items.push(PlusItem::Commands(t(
                self.lang,
                TextKey::AiComposerCommands,
            )));
            items.push(PlusItem::Skills(t(self.lang, TextKey::AiComposerSkills)));
        }
        if self.web_search_available() {
            items.push(PlusItem::SearchWeb(
                t(self.lang, TextKey::AiComposerSearchWeb),
                self.web_search_armed,
            ));
        }
        items.push(PlusItem::Hint(t(self.lang, TextKey::AiComposerPlusHint)));
        self.plus_menu_at(items);
    }

    fn open_computer_plus_menu(&mut self) {
        self.plus_menu_at(vec![
            PlusItem::Back(t(self.lang, TextKey::Back)),
            PlusItem::ComputerFile(t(self.lang, TextKey::AiComposerComputerFile)),
            PlusItem::ComputerFolder(t(self.lang, TextKey::AiComposerComputerFolder)),
        ]);
    }

    fn open_recent_plus_menu(&mut self) {
        let mut items = vec![PlusItem::Back(t(self.lang, TextKey::Back))];
        if self.recent_documents.is_empty() {
            items.push(PlusItem::Empty(t(self.lang, TextKey::AiComposerNoRecent)));
        } else {
            items.extend(
                self.recent_documents
                    .iter()
                    .map(|(id, title)| PlusItem::Document(*id, title.clone())),
            );
        }
        self.plus_menu_at(items);
    }

    pub(super) fn choose_plus_item(&mut self, index: usize) -> bool {
        let Some(item) = self
            .plus_menu
            .as_ref()
            .and_then(|menu| menu.items().get(index))
            .cloned()
        else {
            return false;
        };
        match item {
            PlusItem::Recent(_) => self.open_recent_plus_menu(),
            PlusItem::ThisComputer(_) => self.open_computer_plus_menu(),
            PlusItem::Back(_) => self.open_plus_menu(),
            PlusItem::ComputerFile(_) => {
                self.plus_menu = None;
                self.computer_picker_request = Some(ComputerPickerKind::File);
            }
            PlusItem::ComputerFolder(_) => {
                self.plus_menu = None;
                self.computer_picker_request = Some(ComputerPickerKind::Folder);
            }
            PlusItem::SearchWeb(_, _) => {
                self.web_search_armed = !self.web_search_armed;
                self.open_plus_menu();
            }
            PlusItem::Commands(_) | PlusItem::Skills(_) => {
                let marker = if matches!(item, PlusItem::Commands(_)) {
                    "/"
                } else {
                    "$"
                };
                self.plus_menu = None;
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                self.composer.key(
                    &Key::Character(marker.into()),
                    Some(marker),
                    ModifiersState::empty(),
                );
                self.sync_skill_list();
            }
            PlusItem::KoruContext(_) => {
                self.plus_menu = None;
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                self.composer.key(
                    &Key::Character("@".into()),
                    Some("@"),
                    ModifiersState::empty(),
                );
            }
            PlusItem::Document(id, title) => {
                self.plus_menu = None;
                self.focus = Focus::Composer;
                self.composer.insert_document(id, &title);
            }
            PlusItem::Empty(_) | PlusItem::Hint(_) => return false,
        }
        true
    }

    pub(super) fn plus_menu_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self.plus_button().contains(x, y) {
            if self.plus_menu.take().is_none() {
                self.open_plus_menu();
            }
            return Some(true);
        }
        let menu = self.plus_menu.as_ref()?;
        if let Some(index) = menu.item_at(self.menu_viewport(), x, y) {
            return Some(self.choose_plus_item(index));
        }
        self.plus_menu = None;
        Some(true)
    }

    pub(super) fn plus_menu_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let viewport = self.menu_viewport();
        let menu = self.plus_menu.as_mut()?;
        let next = menu.item_at(viewport, x, y);
        let changed = menu.hover != next;
        menu.hover = next;
        Some(changed)
    }

    pub(super) fn plus_menu_key(&mut self, key: &Key) -> Option<bool> {
        let menu = self.plus_menu.as_mut()?;
        match key {
            Key::Named(NamedKey::Escape) => {
                self.plus_menu = None;
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let enabled: Vec<_> = menu.items().iter().map(MenuItem::selectable).collect();
                menu.step(
                    &enabled,
                    if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                        -1
                    } else {
                        1
                    },
                );
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let selected = menu.hover.unwrap_or(0);
                self.choose_plus_item(selected);
            }
            _ => return Some(false),
        }
        Some(true)
    }

    /// The host shows the native picker and answers through
    /// [`computer_path_chosen`](Self::computer_path_chosen).
    pub fn take_computer_picker_request(&mut self) -> Option<ComputerPickerKind> {
        self.computer_picker_request.take()
    }

    pub fn computer_path_chosen(&mut self, kind: ComputerPickerKind, path: PathBuf) -> bool {
        if !path.is_absolute() {
            return false;
        }
        match kind {
            ComputerPickerKind::Folder => self.attach_reference_path(path, AttachmentKind::Folder),
            ComputerPickerKind::File => {
                let dialog = Choice::new(
                    t(self.lang, TextKey::AiComputerFileChoiceTitle),
                    t(self.lang, TextKey::AiComputerFileChoiceBody),
                )
                .answer(t(self.lang, TextKey::AiComputerAddToWorkspace))
                .answer(t(self.lang, TextKey::AiComputerAttachReference))
                .defaulting_to(0);
                self.computer_file_choice = Some((path, dialog));
                true
            }
        }
    }

    pub fn take_workspace_import_request(&mut self) -> Option<WorkspaceImportRequest> {
        self.workspace_import_request.take()
    }

    /// The Files host answered the exact import operation it was given.
    /// A successful import starts the owned chat copy; a failure keeps the
    /// provisional chip and operation id for retry.
    pub fn workspace_file_imported(
        &mut self,
        op: uuid::Uuid,
        result: Result<DocumentId, String>,
    ) -> bool {
        let Some(request) = self
            .workspace_imports
            .values()
            .find(|request| request.op == op)
            .cloned()
        else {
            return false;
        };
        match result {
            Ok(document) => {
                self.workspace_import_documents
                    .insert(request.attachment, document);
                self.copy_imported_workspace_file(&request, document);
            }
            Err(message) => {
                tracing::debug!(target: "koru_ai::ui", op = %op, reason = %message, "Files import for chat failed");
                self.fail_workspace_import(&request, TextKey::AiComputerImportFailed);
            }
        }
        true
    }

    pub(super) fn copy_imported_workspace_file(
        &mut self,
        request: &WorkspaceImportRequest,
        document: DocumentId,
    ) {
        if !self.queue_command(Command::CopyWorkspaceFile(
            request.chat,
            koru_ai_wire::FromWorkspaceAttachmentRequest {
                id: request.attachment,
                workspace: request.workspace,
                document,
            },
        )) {
            self.fail_workspace_import(request, TextKey::AiComputerCopyFailed);
        }
    }

    pub(super) fn fail_workspace_import(&mut self, request: &WorkspaceImportRequest, key: TextKey) {
        if let Some(draft) = self.strips.draft_mut(request.chat, request.attachment) {
            draft.state = attachments::DraftState::Failed { retryable: true };
        }
        if self.shown_chat_id() == Some(request.chat) {
            self.status = Some(t(self.lang, key).to_owned());
        }
    }

    pub(super) fn retry_workspace_import(&mut self, chat: ChatId, id: ChatAttachmentId) -> bool {
        let Some(request) = self.workspace_imports.get(&id).cloned() else {
            return false;
        };
        if request.chat != chat {
            return false;
        }
        if let Some(draft) = self.strips.draft_mut(chat, id) {
            draft.state = attachments::DraftState::Storing;
        }
        if self.queue_command(Command::AddGrants(
            chat,
            Some(request.workspace),
            Vec::new(),
            request.op,
        )) {
            self.pending_workspace_import_grant = Some((request.op, request));
        } else {
            self.fail_workspace_import(&request, TextKey::AiComputerGrantFailed);
        }
        true
    }

    fn attach_reference_path(&mut self, path: PathBuf, kind: AttachmentKind) -> bool {
        let Some(chat) = self.shown_chat_id() else {
            return false;
        };
        if self.chat_unmade() {
            if !self.make_unmade_chat() {
                return false;
            }
            self.pending_computer_reference = Some((chat, path, kind));
        } else {
            self.add_draft(chat, attachments::Draft::reference(path, kind));
        }
        true
    }

    pub(super) fn finish_computer_file_choice(&mut self, outcome: ChoiceOutcome) -> bool {
        let Some((path, _)) = self.computer_file_choice.take() else {
            return false;
        };
        match outcome {
            ChoiceOutcome::Keep => {
                // The caller has already updated the dialog's focus/hover.
                // Keep its state by returning before taking it.
                unreachable!("Keep is handled by the input wrappers")
            }
            ChoiceOutcome::Dismiss => true,
            ChoiceOutcome::Chosen(0) => self.begin_workspace_import(path),
            ChoiceOutcome::Chosen(1) => self.attach_reference_path(path, AttachmentKind::File),
            ChoiceOutcome::Chosen(_) => true,
        }
    }

    fn begin_workspace_import(&mut self, path: PathBuf) -> bool {
        let (Some(chat), Some(workspace)) = (self.shown_chat_id(), self.context_workspace) else {
            self.status = Some(t(self.lang, TextKey::AiComputerChooseWorkspace).to_owned());
            return true;
        };
        if !self.make_unmade_chat() {
            return true;
        }
        let op = uuid::Uuid::new_v4();
        let request = WorkspaceImportRequest {
            op,
            chat,
            workspace,
            attachment: ChatAttachmentId::new(),
            path,
        };
        self.add_draft(
            chat,
            attachments::Draft::workspace_pending(request.attachment, &request.path),
        );
        if !self
            .strips
            .get(chat)
            .iter()
            .any(|draft| draft.id == request.attachment)
        {
            return true;
        }
        self.workspace_imports
            .insert(request.attachment, request.clone());
        if self.queue_command(Command::AddGrants(chat, Some(workspace), Vec::new(), op)) {
            self.pending_workspace_import_grant = Some((op, request));
        } else {
            self.status = Some(t(self.lang, TextKey::AiComputerGrantFailed).to_owned());
            if let Some(draft) = self.strips.draft_mut(chat, request.attachment) {
                draft.state = attachments::DraftState::Failed { retryable: true };
            }
        }
        true
    }

    pub(super) fn computer_choice_key(&mut self, key: &Key) -> Option<bool> {
        let (_, choice) = self.computer_file_choice.as_mut()?;
        let outcome = choice.key(key);
        Some(match outcome {
            ChoiceOutcome::Keep => true,
            choice => self.finish_computer_file_choice(choice),
        })
    }

    pub(super) fn computer_choice_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (_, choice) = self.computer_file_choice.as_mut()?;
        let outcome = choice.mouse_down(
            &mut self.engine,
            (self.viewport.w, self.viewport.h),
            x - self.viewport.x,
            y - self.viewport.y,
        );
        Some(match outcome {
            ChoiceOutcome::Keep => true,
            choice => self.finish_computer_file_choice(choice),
        })
    }
}

#[cfg(test)]
mod tests;
