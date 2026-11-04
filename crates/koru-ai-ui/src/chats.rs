// SPDX-License-Identifier: MPL-2.0

//! **What the surface says to its host about the Chats list.**
//!
//! The chat list is not the chat's own (AGENTUX R1): it is
//! [`koru_chats_ui::ChatsSurface`], which the workspace window's navigator
//! shows as its *Chats* view. This surface keeps no list of its own on
//! screen; it tells the host when the list changed
//! ([`ChatSurface::take_chat_list_changed`]), renames and pins through its
//! reader for a host whose navigator asked, and hands `Ctrl+Shift+↑`/`↓` to
//! the host, whose list it is ([`ChatSurface::take_wants_chat_step`]).

use koru_ai_core::ChatId;
use koru_ai_wire::RenameChatRequest;
use winit::keyboard::{Key, ModifiersState};

use crate::ChatSurface;
use crate::reader::Command;

impl ChatSurface {
    /// **A chat surface tells its host the chat list changed** — a chat was
    /// created, renamed, deleted, re-pointed at another model, made private,
    /// or answered (which moves it to the top). The workspace host drains this
    /// on its maintain pass and refreshes its Chats view; nothing polls.
    pub fn take_chat_list_changed(&mut self) -> bool {
        std::mem::take(&mut self.chat_list_changed)
    }

    /// **Rename a chat** through the reader's rename command — the one a
    /// host with a Chats list of its own (the workspace window's navigator)
    /// renames through, so the chat open in this surface takes the new name
    /// and the lists are told
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

    /// **Pin a chat, or unpin it** (AI schema 12): the service keeps it on
    /// the chat and the list takes the answer in place, as a rename's. The
    /// navigator's Pinned group and its menu read `Chat::pinned_at`.
    pub fn pin_chat(&mut self, chat: ChatId, pinned: bool) -> bool {
        self.send_command(Command::Pin(chat, koru_ai_wire::PinChatRequest { pinned }))
    }

    /// Which step through the Chats list a key is, if it is one (D87).
    pub(crate) fn chat_step_chord(
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<koru_chats_ui::ChatStep> {
        let is = |id| koru_keys::registry::get(id).chord.matches(key, text, mods);
        if is(koru_keys::ShortcutId::PreviousChat) {
            Some(koru_chats_ui::ChatStep::Previous)
        } else if is(koru_keys::ShortcutId::NextChat) {
            Some(koru_chats_ui::ChatStep::Next)
        } else {
            None
        }
    }

    /// **Move to the chat above or below in the Chats list** (D87), as
    /// choosing its row would. The host is asked
    /// ([`take_wants_chat_step`](Self::take_wants_chat_step)), because the
    /// list it shows is its own. The key is taken either way: it means *next
    /// chat* here, not *move this line*.
    pub(crate) fn step_chat(&mut self, step: koru_chats_ui::ChatStep) -> bool {
        self.wants_chat_step = Some(step);
        true
    }

    /// `Ctrl+Shift+↑`/`↓` in this surface: the host moves to the chat above
    /// or below in its list (D87).
    pub fn take_wants_chat_step(&mut self) -> Option<koru_chats_ui::ChatStep> {
        self.wants_chat_step.take()
    }

    /// The chats this surface last listed, for a host whose own list has
    /// not been asked yet.
    pub fn listed_chats(&self) -> &[koru_ai_core::Chat] {
        &self.chats
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use koru_ai_core::{AiProviderId, Chat, ChatId, ModelSelection, ReasoningChoice, Timestamp};

    use crate::ChatSurface;
    use crate::reader::{Command, Reader, Update};

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
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            created_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(1),
        }
    }

    fn ready() -> ChatSurface {
        let mut surface = ChatSurface::new();
        surface.validated_ready = true;
        surface.setup_open = false;
        surface
    }

    /// **Told, never polled.** A chat made, renamed or deleted — or answered,
    /// which moves it — is news the host drains; a failure is not.
    #[test]
    fn the_host_is_told_when_the_chat_list_changes() {
        let mut surface = ready();
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

    /// **A renamed chat keeps its place** in the list: a rename is not
    /// activity.
    #[test]
    fn a_renamed_chat_keeps_its_place() {
        let mut surface = ready();
        let (one, two) = (chat("One"), chat("Two"));
        surface.chats = vec![one.clone(), two.clone()];
        let mut renamed = two.clone();
        renamed.title = "Second".into();
        surface.apply_update(Update::Renamed(renamed));
        let titles: Vec<&str> = surface.chats.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["One", "Second"]);
    }

    /// **Only messages are activity** (koru-ai D85, the user's call
    /// 2026-11-03): a chat whose model, privacy or folder changed keeps its
    /// place too.
    #[test]
    fn a_chats_settings_changing_keeps_its_place() {
        let mut surface = ready();
        let (one, two) = (chat("One"), chat("Two"));
        surface.chats = vec![one.clone(), two.clone()];
        let titles = |surface: &ChatSurface| {
            surface
                .chats
                .iter()
                .map(|c| c.title.clone())
                .collect::<Vec<_>>()
        };
        let mut changed = two.clone();
        changed.current_model.requested_model = "gpt-other".into();
        surface.apply_update(Update::ModelUpdated(changed.clone(), None));
        assert_eq!(titles(&surface), ["One", "Two"], "a model is not activity");
        changed.private = true;
        surface.apply_update(Update::PrivacyUpdated(changed.clone()));
        assert_eq!(titles(&surface), ["One", "Two"], "nor is privacy");
        changed.agent_working_folder = Some("D:/work".into());
        surface.apply_update(Update::FolderUpdated(changed));
        assert_eq!(titles(&surface), ["One", "Two"], "nor a folder");
        assert!(surface.chats[1].private, "the row still takes the change");
    }

    /// **A pin goes through the pin command and lands in place** (AI
    /// schema 12): the chat keeps its place, and the list reads its pin
    /// time; unpinning clears it.
    #[test]
    fn a_pinned_chat_is_sent_and_kept_in_place() {
        let (one, two) = (chat("One"), chat("Two"));
        let mut surface = ready();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.chats = vec![one.clone(), two.clone()];
        assert!(surface.pin_chat(two.id, true));
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Pin(chat, request) => {
                assert_eq!(chat, two.id);
                assert!(request.pinned);
            }
            _ => panic!("the pin goes through the pin command"),
        }
        let mut pinned = two.clone();
        pinned.pinned_at = Some(koru_ai_core::Timestamp::from_millis(9));
        let update = Update::Pinned(pinned);
        assert!(update.changes_chat_list(), "other views hear of it");
        surface.apply_update(update);
        let listed: Vec<_> = surface
            .chats
            .iter()
            .map(|chat| (chat.title.as_str(), chat.pinned_at))
            .collect();
        assert_eq!(
            listed,
            [
                ("One", None),
                ("Two", Some(koru_ai_core::Timestamp::from_millis(9)))
            ]
        );
        assert!(surface.pin_chat(two.id, false));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Pin(_, koru_ai_wire::PinChatRequest { pinned: false })
        ));
    }

    /// The command a host with its own Chats list renames through: trimmed,
    /// and never sent for an empty title.
    #[test]
    fn rename_chat_sends_the_rename_command_for_a_title() {
        let mut surface = ready();
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
