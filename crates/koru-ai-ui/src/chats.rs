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

use koru_ai_core::{Chat, ChatId, MessageBlockId, ProfileId};
use koru_ai_wire::RenameChatRequest;
use winit::keyboard::{Key, ModifiersState};

use crate::ChatSurface;
use crate::reader::Command;

/// **A chat of another profile this surface was asked to show** (AI schema
/// 13), and the block it was to open at. A host opens it in a window of the
/// chat's profile ([`ChatSurface::take_wants_open_elsewhere`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChatElsewhere {
    pub chat: ChatId,
    pub block: Option<MessageBlockId>,
    pub profile: ProfileId,
}

impl ChatSurface {
    /// **The profile this surface's window is in** (AI schema 13, koru-ai
    /// D120). It lists only that profile's chats, makes a new chat in it,
    /// and shows no chat of another: one it is asked to show is handed back
    /// ([`take_wants_open_elsewhere`](Self::take_wants_open_elsewhere)).
    /// Said before the surface is activated, the first listing is already
    /// this profile's. A change of profile lists again, and a chat of the
    /// old profile on screen gives way to a new chat.
    pub fn set_profile(&mut self, profile: Option<ProfileId>) -> bool {
        if self.profile == profile {
            return false;
        }
        self.profile = profile;
        self.keep_profile_chats();
        if self
            .transcript
            .as_ref()
            .is_some_and(|transcript| self.belongs_elsewhere(&transcript.chat))
        {
            // Off the screen now, whether or not a new chat can be made yet.
            self.transcript = None;
            self.forget_input_walk();
            self.usage_report = None;
            self.question_navigation = None;
            self.question_navigation_anchor = None;
            self.messages_dirty = true;
            self.start_new_chat();
        }
        if self.reader.is_some() {
            self.send_command(Command::ListChats(profile));
        }
        true
    }

    /// The profile this surface's window is in, as its host last said.
    pub fn profile(&self) -> Option<ProfileId> {
        self.profile
    }

    /// A chat of another profile this surface was asked to show, taken once:
    /// the host opens it in a window of that profile.
    pub fn take_wants_open_elsewhere(&mut self) -> Option<ChatElsewhere> {
        self.wants_open_elsewhere.take()
    }

    /// Whether `chat` is another profile's than this surface's window. A
    /// chat the service has not placed yet, or a surface not told its
    /// profile, shows: never leaving a chat unreachable is the safer
    /// mistake, and the service holds what the chat may read either way.
    pub(crate) fn belongs_elsewhere(&self, chat: &Chat) -> bool {
        matches!(
            (self.profile, chat.profile_id),
            (Some(mine), Some(theirs)) if mine != theirs
        )
    }

    /// Keep only this profile's chats in the list a host reads (the side
    /// pane's *Recent chats*, `Ctrl+Shift+↑`/`↓`'s fallback).
    pub(crate) fn keep_profile_chats(&mut self) {
        if let Some(profile) = self.profile {
            self.chats
                .retain(|chat| chat.profile_id.is_none_or(|theirs| theirs == profile));
        }
    }

    /// Not shown here: hand `chat` back to the host with the block it was to
    /// open at, and leave the link. What was on screen stays; with nothing,
    /// the surface becomes a new chat.
    pub(crate) fn refuse_chat_elsewhere(&mut self, refused: &Chat) {
        let (chat, profile) = (refused.id, refused.profile_id);
        let block = self.highlight.take().map(|(block, _)| block);
        if self.pending_link_chat == Some(chat) {
            self.pending_link_chat = None;
        }
        self.chats.retain(|listed| listed.id != chat);
        if self.transcript.is_none() {
            self.fresh_chat_wanted = true;
        }
        if let Some(profile) = profile {
            self.wants_open_elsewhere = Some(ChatElsewhere {
                chat,
                block,
                profile,
            });
        }
    }

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

    fn transcript_of(chat: Chat) -> koru_ai_wire::ChatTranscript {
        koru_ai_wire::ChatTranscript {
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id: chat.id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            chat,
            tool_activities: Vec::new(),
            notices: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            sources: Vec::new(),
            reasoning_control: None,
            attachments: Vec::new(),
        }
    }

    fn of(title: &str, profile: koru_ai_core::ProfileId) -> Chat {
        let mut chat = chat(title);
        chat.profile_id = Some(profile);
        chat
    }

    /// **One profile's chats** (AI schema 13): the surface lists the
    /// window's profile's, keeps no other's from a listing, and switching
    /// profile lists again and drops the old profile's at once.
    #[test]
    fn the_list_is_the_windows_profiles_and_follows_a_switch() {
        let (private, work) = (
            koru_ai_core::ProfileId::new(),
            koru_ai_core::ProfileId::new(),
        );
        let mut surface = ready();
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        assert!(surface.set_profile(Some(private)));
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ListChats(Some(profile)) if profile == private
        ));
        assert!(
            !surface.set_profile(Some(private)),
            "the same profile again"
        );

        let update = Update::Listed(vec![of("Mine", private), of("Theirs", work)]);
        assert!(update.changes_chat_list());
        surface.apply_update(update);
        let titles: Vec<&str> = surface.chats.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Mine"], "never another profile's");
        assert_eq!(surface.recent_chats(10).len(), 1);

        assert!(surface.set_profile(Some(work)));
        assert!(surface.chats.is_empty(), "the old profile's go at once");
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::ListChats(Some(profile)) if profile == work
        ));
    }

    /// **A chat of another profile is not shown** (AI schema 13): asked to
    /// open one — a link, a hit, a tab saved before chats had a profile —
    /// the surface hands it back with its block and profile, for a window of
    /// that profile, and shows what it showed before. A chat not placed yet
    /// shows.
    #[test]
    fn a_chat_of_another_profile_is_handed_back_not_shown() {
        let (private, work) = (
            koru_ai_core::ProfileId::new(),
            koru_ai_core::ProfileId::new(),
        );
        let mut surface = ready();
        surface.set_profile(Some(private));
        let theirs = of("Theirs", work);
        let block = koru_ai_core::MessageBlockId::new();
        surface.open_link(theirs.id, Some(block));
        surface.apply_update(Update::Transcript(transcript_of(theirs.clone())));
        assert!(surface.transcript.is_none(), "not shown here");
        assert_eq!(
            surface.take_wants_open_elsewhere(),
            Some(crate::ChatElsewhere {
                chat: theirs.id,
                block: Some(block),
                profile: work,
            })
        );
        assert_eq!(surface.take_wants_open_elsewhere(), None, "taken once");
        assert_eq!(surface.current_chat_id(), None);

        let mine = of("Mine", private);
        surface.apply_update(Update::Transcript(transcript_of(mine.clone())));
        assert_eq!(surface.current_chat_id(), Some(mine.id));
        let mut unplaced = chat("Not placed yet");
        unplaced.profile_id = None;
        surface.apply_update(Update::Transcript(transcript_of(unplaced.clone())));
        assert_eq!(surface.current_chat_id(), Some(unplaced.id));
        assert_eq!(surface.take_wants_open_elsewhere(), None);

        // A switch away from the shown chat's profile leaves it for a new
        // chat.
        surface.apply_update(Update::Transcript(transcript_of(mine.clone())));
        surface.set_profile(Some(work));
        assert_ne!(surface.current_chat_id(), Some(mine.id));
    }

    /// **A new chat is made in the window's profile** (AI schema 13).
    #[test]
    fn a_new_chat_is_made_in_the_windows_profile() {
        let work = koru_ai_core::ProfileId::new();
        let mut surface = ready();
        surface.set_profile(Some(work));
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.begin_unmade_chat(chat("New").current_model);
        assert!(surface.make_unmade_chat());
        match commands.recv_timeout(Duration::from_millis(50)).unwrap() {
            Command::Create(request) => assert_eq!(request.profile_id, Some(work)),
            _ => panic!("the chat is made first"),
        }
    }
}
