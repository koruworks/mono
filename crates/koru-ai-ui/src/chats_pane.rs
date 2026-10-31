// SPDX-License-Identifier: MPL-2.0

//! **The Chats pane** — the standalone window's host for the Chats surface.
//!
//! The chat list is not the chat's own any more (AGENTUX R1): it is
//! [`koru_chats_ui::ChatsSurface`], the same component the workspace window's
//! navigator shows as its *Chats* view. In the standalone window the header's
//! *Chats* button opens it as a floating pane through `scene::floating_panel`;
//! a click outside or Escape closes it, and choosing a row reads that chat
//! **in place** — the window has one conversation and this is how it moves
//! between them. `Ctrl+Shift+E` opens it with the current chat selected.
//!
//! In the context pane and in a workspace tab the same button asks the host
//! to show its navigator's *Chats* view instead
//! ([`ChatSurface::take_wants_chats_view`]): the workspace has the navigator,
//! and a second list inside the chat would be the popover this replaced.
//!
//! **One interaction contract in both hosts** — focus enters on open and
//! returns to the composer on close; Up/Down, Home/End, Enter, Delete and
//! Escape — because both hosts forward the same keys to the same surface.
//! Delete asks through this surface's own confirmation and deletes through
//! its own `DeleteChat` path, which already clears a transcript that was
//! deleted from under it and says why a delete was refused.
//!
//! **`F2` renames the keyboard's chat in its row** — a field over the title,
//! as the workspace window's navigator renames a row: Enter renames, Escape
//! leaves the name as it was, an empty title is refused in place, and a
//! press anywhere else is *done*. It renames through
//! [`ChatSurface::rename_chat`], the rename command the chat's title row used
//! before the row was removed, so a chat open here takes its new name with
//! the list.

use std::time::{SystemTime, UNIX_EPOCH};

use koru_ai_core::ChatId;
use koru_ai_wire::RenameChatRequest;
use koru_chats_ui::Press;
use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::reader::Command;
use crate::{ChatSurface, DestructiveAction, Focus, HEADER_H, PAD, SurfaceMode};

/// The pane's widest: a list, not a page.
const PANE_MAX_W: f32 = 380.0;
/// The pane's tallest before its rows scroll.
const PANE_MAX_H: f32 = 460.0;
/// Air inside the panel's border.
const PANE_INSET: f32 = 4.0;
/// The rename field's height, centred in the row: the workspace window's
/// inline field's, so a row renamed in either window looks the same.
const RENAME_H: f32 = 26.0;

/// **A chat being renamed in its row.**
pub(crate) struct ChatsRename {
    chat: ChatId,
    /// The title as it was, so an Enter that changed nothing renames nothing.
    was: String,
    input: TextInput,
    /// Why the last Enter was refused. The next keystroke clears it: you are
    /// already fixing it.
    hint: Option<&'static str>,
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

impl ChatSurface {
    /// Whether the Chats surface is hosted here rather than by a navigator.
    pub(crate) fn hosts_chats_pane(&self) -> bool {
        self.mode == SurfaceMode::Standalone
    }

    /// The floating panel's rect, under the header's left edge.
    pub(crate) fn chats_panel(&self) -> Rect {
        let w = (self.viewport.w - PAD * 2.0).clamp(1.0, PANE_MAX_W);
        let h = (self.viewport.h - HEADER_H - PAD).clamp(1.0, PANE_MAX_H);
        Rect::new(self.viewport.x + PAD, self.viewport.y + HEADER_H, w, h)
    }

    /// The list's area and the line its rows start on, inside the panel.
    fn chats_area(&self) -> (Rect, f32) {
        let panel = self.chats_panel();
        let area = Rect::new(
            panel.x + PANE_INSET,
            panel.y + PANE_INSET,
            (panel.w - PANE_INSET * 2.0).max(0.0),
            (panel.h - PANE_INSET * 2.0).max(0.0),
        );
        (area, area.y + 2.0)
    }

    /// **A chat surface tells its host the chat list changed** — a chat was
    /// created, renamed, deleted, re-pointed at another model, made private,
    /// or answered (which moves it to the top). The workspace host drains this
    /// on its maintain pass and refreshes its Chats view; nothing polls.
    pub fn take_chat_list_changed(&mut self) -> bool {
        std::mem::take(&mut self.chat_list_changed)
    }

    /// The *Chats* button was pressed in a surface whose host has a navigator
    /// (the context pane, a workspace tab). The host shows its *Chats* view
    /// with this chat selected.
    pub fn take_wants_chats_view(&mut self) -> bool {
        std::mem::take(&mut self.wants_chats_view)
    }

    /// Open the pane, putting the keyboard on the current chat.
    pub(crate) fn open_chats_pane(&mut self) -> bool {
        if !self.chats_pane_started {
            self.chats_pane_started = true;
            // `new` builds no reader under this crate's own tests; the
            // `no-client` feature turns the pane's off for a host's.
            if !cfg!(any(test, feature = "no-client")) {
                self.chats_pane.start(self.root.clone());
            }
        }
        self.chat_model_open = false;
        self.tool_access_open = false;
        self.tool_mode_open = false;
        self.history_open = true;
        self.chats_rename = None;
        let current = self.current_chat_id();
        let (area, top) = self.chats_area();
        let pane = &mut self.chats_pane;
        pane.set_lang(self.lang);
        pane.set_now(epoch_ms());
        pane.set_current(current);
        // **Boots when shown.** Every opening asks again: nothing polls, so
        // what the pane shows is as old as the last time it was looked at.
        pane.refresh();
        pane.clear_focus();
        match current {
            Some(chat) => {
                pane.reveal(chat, area, top);
            }
            None => {
                pane.focus_first(area, top);
            }
        }
        true
    }

    /// Close the pane. **The keyboard returns** to the composer it came from.
    pub(crate) fn close_chats_pane(&mut self) -> bool {
        if !self.history_open {
            return false;
        }
        self.history_open = false;
        self.chats_rename = None;
        self.chats_pane.clear_focus();
        self.chats_pane.clear_hover();
        self.focus = Focus::Composer;
        true
    }

    /// The *Chats* button: the pane here, the navigator's view elsewhere.
    pub(crate) fn press_chats_button(&mut self) -> bool {
        if !self.hosts_chats_pane() {
            self.wants_chats_view = true;
            return true;
        }
        if self.history_open {
            self.close_chats_pane()
        } else {
            self.open_chats_pane()
        }
    }

    /// Whether `Ctrl+Shift+E` — the workspace's *show me where this lives* —
    /// was pressed.
    pub(crate) fn is_reveal_chord(key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        koru_keys::registry::get(koru_keys::ShortcutId::ToggleExplore)
            .chord
            .matches(key, text, mods)
    }

    /// A key, while the pane is open or when it is the chord that opens it.
    /// `None` means the pane has no say and the key goes on to the chat.
    pub(crate) fn chats_pane_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        // The rename field owns every key while it is open — Enter and
        // Escape included, which the list and the pane would otherwise take.
        if self.history_open && self.chats_rename.is_some() {
            return Some(self.chats_rename_key(key, text, mods));
        }
        if self.hosts_chats_pane() && Self::is_reveal_chord(key, text, mods) {
            return Some(self.open_chats_pane());
        }
        if !self.history_open {
            return None;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            return Some(self.close_chats_pane());
        }
        if koru_keys::registry::get(koru_keys::ShortcutId::RenameDocument)
            .chord
            .matches(key, text, mods)
        {
            return Some(self.start_chats_rename());
        }
        let press = Press::of(key)?;
        let (area, top) = self.chats_area();
        let handled = self.chats_pane.key(area, top, press);
        Some(self.drain_chats_pane() || handled)
    }

    /// A press. `None` when the pane is shut. A press outside the panel
    /// closes it and goes on to whatever it landed on — the old popover's
    /// behaviour, kept — except on the *Chats* button, which toggles.
    pub(crate) fn chats_pane_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if !self.history_open {
            return None;
        }
        // The rename field is modal over the list: a press either moves its
        // caret or finishes it, and never also chooses a row.
        if self.chats_rename.is_some() {
            match self.chats_rename_rect() {
                Some(field) if field.contains(x, y) => {
                    if let Some(rename) = self.chats_rename.as_mut() {
                        rename.input.click(&mut self.engine, field, x, y);
                    }
                }
                _ => {
                    self.finish_chats_rename();
                }
            }
            return Some(true);
        }
        if self.chats_panel().contains(x, y) {
            let (area, top) = self.chats_area();
            let pressed = self.chats_pane.mouse_down(area, top, x, y);
            return Some(self.drain_chats_pane() || pressed);
        }
        if self.history_button().contains(x, y) {
            return Some(self.close_chats_pane());
        }
        self.close_chats_pane();
        None
    }

    /// A right-click on a row asks to delete it, through the same
    /// confirmation the Delete key reaches. `None` when the pane has no say.
    pub(crate) fn chats_pane_context_menu(&mut self, x: f32, y: f32) -> Option<bool> {
        if !self.history_open {
            return None;
        }
        if self.chats_rename.is_some() {
            self.finish_chats_rename();
            return Some(true);
        }
        if !self.chats_panel().contains(x, y) {
            self.close_chats_pane();
            return Some(true);
        }
        let (area, top) = self.chats_area();
        if let Some(chats) = self.chats_pane.context_menu(area, top, x, y) {
            self.open_confirmation(DestructiveAction::DeleteChats(chats));
        }
        Some(true)
    }

    pub(crate) fn chats_pane_mouse_move(&mut self, x: f32, y: f32) -> bool {
        if !self.history_open {
            return false;
        }
        let (area, top) = self.chats_area();
        if self.chats_panel().contains(x, y) {
            self.chats_pane.mouse_move(area, top, x, y)
        } else {
            self.chats_pane.clear_hover()
        }
    }

    /// A wheel over the open pane scrolls the pane, not the conversation.
    pub(crate) fn chats_pane_scroll(&mut self, dy: f32) -> Option<bool> {
        if !self.history_open {
            return None;
        }
        let (area, top) = self.chats_area();
        self.chats_pane.scroll_by(area, top, dy);
        Some(true)
    }

    /// Act on what the pane asked for. Reading a chat and starting one close
    /// the pane — the window has one conversation, and that is now it.
    fn drain_chats_pane(&mut self) -> bool {
        let mut changed = false;
        if let Some(chat) = self.chats_pane.take_wants_open() {
            self.close_chats_pane();
            // The old popover's read, unchanged: the chat opens at its end.
            self.scroll_loaded_chat_to_bottom = true;
            self.send_command(Command::Read(chat));
            changed = true;
        }
        if self.chats_pane.take_wants_new() {
            self.close_chats_pane();
            self.create_chat();
            changed = true;
        }
        let asked = self.chats_pane.take_wants_delete();
        if !asked.is_empty() {
            self.open_confirmation(DestructiveAction::DeleteChats(asked));
            changed = true;
        }
        changed
    }

    /// The host's wake path: fold in what the pane's reader answered, and
    /// ask again when this surface changed the list. Nothing here while the
    /// pane is shut — it asks on its next opening.
    pub(crate) fn maintain_chats_pane(&mut self) -> bool {
        if !self.chats_pane_started {
            return false;
        }
        let mut changed = self.chats_pane.poll();
        if self.history_open {
            if self.chat_list_changed_for_pane {
                self.chats_pane.refresh();
            }
            let current = self.current_chat_id();
            changed |= self.chats_pane.set_current(current);
        }
        self.chat_list_changed_for_pane = false;
        changed
    }

    /// Whether the pane owes a wake: open, and waiting on its reader.
    pub(crate) fn chats_pane_awaiting(&self) -> bool {
        self.history_open && self.chats_pane.awaiting()
    }

    pub(crate) fn draw_chats_pane(&mut self, canvas: &mut Scene, root: Affine) {
        let panel = self.chats_panel();
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let (area, top) = self.chats_area();
        self.chats_pane.set_lang(self.lang);
        self.chats_pane.set_now(epoch_ms());
        self.chats_pane
            .draw(canvas, root, &mut self.engine, area, top);
        self.draw_chats_rename(canvas, root);
    }

    /// **Rename a chat** through the reader's rename command — the one the
    /// chat's title row sent, and the one a host with a Chats list of its
    /// own (the workspace window's navigator) renames through, so the chat
    /// open in this surface takes the new name and the lists are told
    /// ([`take_chat_list_changed`](Self::take_chat_list_changed)). Nothing is
    /// sent for an empty title.
    pub fn rename_chat(&mut self, chat: ChatId, title: &str) -> bool {
        let title = title.trim();
        if title.is_empty() {
            return false;
        }
        self.send_command(Command::Rename(
            chat,
            RenameChatRequest {
                title: title.to_owned(),
            },
        ))
    }

    /// `F2` on the pane: open the field over the keyboard's chat, its title
    /// selected — renaming usually means replacing it.
    pub(crate) fn start_chats_rename(&mut self) -> bool {
        let Some(chat) = self.chats_pane.focused_chat() else {
            return false;
        };
        let Some(was) = self
            .chats_pane
            .chats()
            .iter()
            .find(|candidate| candidate.id == chat)
            .map(|candidate| candidate.title.clone())
        else {
            return false;
        };
        let mut input = TextInput::with_text(&mut self.engine, theme::role::row(), &was);
        input.select_all(&mut self.engine);
        self.chats_rename = Some(ChatsRename {
            chat,
            was,
            input,
            hint: None,
        });
        true
    }

    /// The field, centred over the row's title. `None` when the row is
    /// scrolled out of the list — a press then reads as *elsewhere*.
    fn chats_rename_rect(&self) -> Option<Rect> {
        let rename = self.chats_rename.as_ref()?;
        let (area, top) = self.chats_area();
        let title = self.chats_pane.title_rect(area, top, rename.chat)?;
        let h = RENAME_H.min(title.h);
        Some(Rect::new(
            title.x,
            title.y + (title.h - h) * 0.5,
            title.w,
            h,
        ))
    }

    fn chats_rename_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let Some(rename) = self.chats_rename.as_mut() else {
            return false;
        };
        match key {
            Key::Named(NamedKey::Enter) => {
                if rename.input.value().trim().is_empty() {
                    rename.hint = Some(koru_i18n::t(self.lang, koru_i18n::Key::ChatsNameNeeded));
                    return true;
                }
                self.finish_chats_rename()
            }
            // The name stays as it was, and the keyboard stays on its row.
            Key::Named(NamedKey::Escape) => {
                self.chats_rename = None;
                true
            }
            _ => {
                rename.hint = None;
                rename.input.key(&mut self.engine, key, text, mods);
                true
            }
        }
    }

    /// Close the field on what is typed: a new title renames, and an empty
    /// or unchanged one leaves the name as it was — a press elsewhere must
    /// not be a way round Enter's refusal.
    fn finish_chats_rename(&mut self) -> bool {
        let Some(rename) = self.chats_rename.take() else {
            return false;
        };
        let title = rename.input.value();
        let title = title.trim();
        if title.is_empty() || title == rename.was {
            return true;
        }
        self.rename_chat(rename.chat, title)
    }

    /// The field over the row, and the refusal under it in the red the
    /// window already uses for *this is the bad one*.
    fn draw_chats_rename(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(field) = self.chats_rename_rect() else {
            return;
        };
        let Some(rename) = self.chats_rename.as_mut() else {
            return;
        };
        let style = if rename.hint.is_some() {
            InputStyle::inline().invalid()
        } else {
            InputStyle::inline()
        };
        rename
            .input
            .draw(canvas, root, &mut self.engine, field, style, None, true);
        if let Some(hint) = rename.hint {
            let label = self.engine.simple_layout(hint, theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &label,
                field.x + 2.0,
                field.y + field.h + 2.0,
                Some(theme::p().close_hover),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, SyncSender};
    use std::time::Duration;

    use koru_ai_core::{AiProviderId, Chat, ChatId, ModelSelection, ReasoningChoice, Timestamp};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    use crate::reader::{Command, Reader, Update};
    use crate::{ChatSurface, Focus, SurfaceMode};

    fn chat(title: &str) -> Chat {
        Chat {
            id: ChatId::new(),
            title: title.into(),
            private: false,
            current_model: ModelSelection {
                provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                requested_model: "gpt-test".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: None,
            created_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(1),
        }
    }

    fn ready(mode: SurfaceMode) -> ChatSurface {
        let mut surface = ChatSurface::new(mode);
        surface.validated_ready = true;
        surface.setup_open = false;
        surface
    }

    fn press_chats(surface: &mut ChatSurface) -> bool {
        let button = surface.history_button();
        surface.mouse_down(button.x + 4.0, button.y + 4.0)
    }

    /// **The workspace has the navigator.** In the context pane and a tab
    /// the header carries no *Chats* button at all (the CTO's note): the
    /// rail's Chats square is the door, and a press where the button would
    /// be reaches nothing.
    #[test]
    fn a_workspace_hosted_header_has_no_chats_button() {
        let mut surface = ready(SurfaceMode::ContextPane);
        assert!(!surface.hosts_header_buttons());
        assert!(!press_chats(&mut surface) || !surface.history_open);
        assert!(
            !surface.take_wants_chats_view(),
            "nothing asked for the view"
        );
        assert!(!surface.history_open, "no popover inside the chat");
    }

    /// In the standalone window the same button opens the floating pane, and
    /// Escape closes it with the keyboard back in the composer.
    #[test]
    fn the_standalone_window_opens_the_pane_and_escape_closes_it() {
        let mut surface = ready(SurfaceMode::Standalone);
        // A list with a row to land on: *New chat* left the list for the
        // heading, so an empty one has nowhere for the keyboard to enter.
        surface.chats_pane.take_listing(Ok(koru_chats_ui::Listing {
            chats: vec![chat("One")],
            agents: Vec::new(),
        }));
        assert!(press_chats(&mut surface));
        assert!(surface.history_open);
        assert!(!surface.take_wants_chats_view());
        assert!(surface.chats_pane.has_keyboard(), "focus enters on open");
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
        assert!(!surface.history_open);
        assert_eq!(surface.focus, Focus::Composer, "focus returns on close");
    }

    /// `Ctrl+Shift+E` in the standalone window opens the pane; the context
    /// pane leaves the chord to its host.
    #[test]
    fn the_reveal_chord_opens_the_standalone_pane() {
        let chord = koru_keys::registry::get(koru_keys::ShortcutId::ToggleExplore).chord;
        let mods = if cfg!(target_os = "macos") {
            ModifiersState::SUPER | ModifiersState::SHIFT
        } else {
            ModifiersState::CONTROL | ModifiersState::SHIFT
        };
        let key = Key::Character("E".into());
        assert!(chord.matches(&key, Some("E"), mods));
        let mut surface = ready(SurfaceMode::Standalone);
        assert!(surface.key(&key, Some("E"), mods));
        assert!(surface.history_open);
        let mut pane = ready(SurfaceMode::ContextPane);
        pane.key(&key, Some("E"), mods);
        assert!(!pane.history_open);
    }

    /// A click outside the open pane closes it.
    #[test]
    fn a_click_outside_closes_the_pane() {
        let mut surface = ready(SurfaceMode::Standalone);
        surface.set_viewport(koru_draw::geometry::Rect::new(0.0, 0.0, 900.0, 800.0));
        press_chats(&mut surface);
        assert!(surface.history_open);
        let panel = surface.chats_panel();
        surface.mouse_down(panel.x + panel.w + 40.0, panel.y + panel.h + 40.0);
        assert!(!surface.history_open);
    }

    /// **Told, never polled.** A chat made, renamed or deleted — or answered,
    /// which moves it — is news the host drains; a failure is not.
    #[test]
    fn the_host_is_told_when_the_chat_list_changes() {
        let mut surface = ready(SurfaceMode::ContextPane);
        assert!(!surface.take_chat_list_changed());
        surface.apply_update(Update::Renamed(chat("Renamed")));
        assert!(surface.take_chat_list_changed());
        assert!(!surface.take_chat_list_changed(), "a one-shot drain");
        surface.apply_update(Update::HistoryChanged {
            chats: Vec::new(),
            transcript: None,
        });
        assert!(surface.take_chat_list_changed());
        surface.apply_update(Update::Failure("nope".into()));
        assert!(!surface.take_chat_list_changed());
    }

    /// The standalone window's pane, open on `chats` with the keyboard on
    /// the newest, and a reader whose commands the test reads.
    fn pane_on(chats: Vec<Chat>) -> (ChatSurface, Receiver<Command>, SyncSender<Update>) {
        let mut surface = ready(SurfaceMode::Standalone);
        let (reader, commands, updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.chats_pane.take_listing(Ok(koru_chats_ui::Listing {
            chats,
            agents: Vec::new(),
        }));
        assert!(press_chats(&mut surface));
        assert!(surface.history_open);
        (surface, commands, updates)
    }

    fn press(surface: &mut ChatSurface, key: NamedKey) -> bool {
        surface.key(&Key::Named(key), None, ModifiersState::empty())
    }

    fn type_text(surface: &mut ChatSurface, text: &str) {
        for ch in text.chars() {
            let ch = ch.to_string();
            surface.key(
                &Key::Character(ch.as_str().into()),
                Some(&ch),
                ModifiersState::empty(),
            );
        }
    }

    /// **Rename lives in the Chats list now**, where the composer's
    /// `/rename` was a stopgap: `F2` opens a field over the keyboard's row
    /// with its title, and Enter renames through the reader's rename command
    /// — the one the title row sent — with the pane still open.
    #[test]
    fn f2_renames_the_keyboards_chat_through_the_rename_command() {
        let one = chat("One");
        let (mut surface, commands, _updates) = pane_on(vec![one.clone()]);
        assert!(press(&mut surface, NamedKey::F2));
        assert!(surface.chats_rename.is_some(), "the field is offered");
        assert!(
            surface.chats_rename_rect().is_some(),
            "over the row it renames"
        );
        // The title is selected, so typing replaces it.
        type_text(&mut surface, " Two ");
        assert!(press(&mut surface, NamedKey::Enter));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Rename(chat, request) => {
                assert_eq!(chat, one.id);
                assert_eq!(request.title, "Two");
            }
            _ => panic!("the rename goes through the rename command"),
        }
        assert!(commands.try_recv().is_err(), "and nothing else");
        assert!(surface.chats_rename.is_none());
        assert!(
            surface.history_open,
            "Enter renamed; it did not close the pane"
        );
        assert!(surface.composer.is_blank(), "and never became a turn");
    }

    /// Escape leaves the name as it was — and the pane open, the keyboard on
    /// the row; an empty title is refused in place and an unchanged one
    /// renames nothing.
    #[test]
    fn escape_an_empty_or_an_unchanged_title_renames_nothing() {
        let (mut surface, commands, _updates) = pane_on(vec![chat("One")]);
        press(&mut surface, NamedKey::F2);
        type_text(&mut surface, "Typed");
        assert!(press(&mut surface, NamedKey::Escape));
        assert!(surface.chats_rename.is_none());
        assert!(
            surface.history_open,
            "Escape closed the field, not the pane"
        );
        assert!(surface.chats_pane.has_keyboard());

        press(&mut surface, NamedKey::F2);
        press(&mut surface, NamedKey::Backspace);
        assert!(press(&mut surface, NamedKey::Enter));
        assert!(
            surface.chats_rename.is_some(),
            "an empty title keeps the field"
        );
        assert_eq!(
            surface.chats_rename.as_ref().and_then(|rename| rename.hint),
            Some(koru_i18n::t(surface.lang, koru_i18n::Key::ChatsNameNeeded))
        );
        type_text(&mut surface, "x");
        assert!(
            surface
                .chats_rename
                .as_ref()
                .is_some_and(|rename| rename.hint.is_none()),
            "the next keystroke clears the complaint"
        );
        press(&mut surface, NamedKey::Escape);

        press(&mut surface, NamedKey::F2);
        assert!(press(&mut surface, NamedKey::Enter));
        assert!(surface.chats_rename.is_none(), "an unchanged title is done");
        assert!(
            commands.try_recv().is_err(),
            "and none of it renamed anything"
        );
    }

    /// A press away from the field is *done*: it renames what was typed, and
    /// does not also choose the row it landed on.
    #[test]
    fn a_press_elsewhere_finishes_the_rename() {
        let older = chat("Older");
        let mut newer = chat("Newer");
        newer.updated_at = Timestamp::from_millis(2);
        let (mut surface, commands, _updates) = pane_on(vec![older.clone(), newer]);
        // The keyboard enters on the newest; step down to the older.
        press(&mut surface, NamedKey::ArrowDown);
        press(&mut surface, NamedKey::F2);
        type_text(&mut surface, "Renamed");
        let field = surface.chats_rename_rect().expect("the field is showing");
        let panel = surface.chats_panel();
        assert!(surface.mouse_down(panel.x + 20.0, field.y - field.h));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Rename(chat, request) => {
                assert_eq!(chat, older.id);
                assert_eq!(request.title, "Renamed");
            }
            _ => panic!("the press renames"),
        }
        assert!(commands.try_recv().is_err(), "and reads no other chat");
        assert!(surface.chats_rename.is_none());
        assert!(surface.history_open);
    }

    /// `F2` with the keyboard on no chat offers nothing.
    #[test]
    fn f2_on_an_empty_list_offers_no_field() {
        let (mut surface, _commands, _updates) = pane_on(Vec::new());
        assert!(!press(&mut surface, NamedKey::F2));
        assert!(surface.chats_rename.is_none());
    }

    /// The command a host with its own Chats list renames through: trimmed,
    /// and never sent for an empty title.
    #[test]
    fn rename_chat_sends_the_rename_command_for_a_title() {
        let mut surface = ready(SurfaceMode::ContextPane);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        let chat = ChatId::new();
        assert!(!surface.rename_chat(chat, "   "));
        assert!(commands.try_recv().is_err());
        assert!(surface.rename_chat(chat, "  Hello "));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Rename(renamed, request) => {
                assert_eq!(renamed, chat);
                assert_eq!(request.title, "Hello");
            }
            _ => panic!("expected the rename"),
        }
    }
}
