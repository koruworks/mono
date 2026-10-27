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

use std::time::{SystemTime, UNIX_EPOCH};

use koru_chats_ui::Press;
use koru_draw::geometry::Rect;
use koru_draw::scene;
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
        if self.hosts_chats_pane() && Self::is_reveal_chord(key, text, mods) {
            return Some(self.open_chats_pane());
        }
        if !self.history_open {
            return None;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            return Some(self.close_chats_pane());
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
    }
}

#[cfg(test)]
mod tests {
    use koru_ai_core::{AiProviderId, Chat, ChatId, ModelSelection, ReasoningChoice, Timestamp};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    use crate::reader::Update;
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

    /// **The workspace has the navigator.** In the context pane and a tab, the
    /// *Chats* button asks the host for its Chats view — no list inside the
    /// chat (R1).
    #[test]
    fn the_chats_button_asks_a_workspace_host_for_its_view() {
        let mut surface = ready(SurfaceMode::ContextPane);
        assert!(press_chats(&mut surface));
        assert!(!surface.history_open, "no popover inside the chat");
        assert!(surface.take_wants_chats_view());
        assert!(!surface.take_wants_chats_view(), "a one-shot drain");
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
}
