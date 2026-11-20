// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::reader::{Command, Reader, Update};
use crate::{OwnedConversation, OwnedOffers, Rect, WorkspaceId};
use koru_ai_core::{
    AttemptStatus, ChatRole, InferenceAttemptId, MessageId, Timestamp, ToolActivityStatus,
};
use koru_ai_wire::{AI_STREAM_VERSION, PublicAiEvent, SendTurnAccepted, StreamEvent};
use std::sync::mpsc::Receiver;

mod restore;

const PREFIX: &str = "I'll look at the file.";
const ANSWER: &str = "Done: it works.";

struct Running {
    surface: ChatSurface,
    attempt: InferenceAttemptId,
    reply: MessageId,
    pending: ChatTranscript,
    complete: ChatTranscript,
    commands: Receiver<Command>,
}

impl Running {
    fn new(owned: bool) -> Self {
        let mut surface = ChatSurface::new();
        let (reply, _) = crate::work_view::tests::finished(&mut surface);
        let attempt = InferenceAttemptId::new();
        let mut complete = surface.transcript.clone().unwrap();
        for call in &mut complete.tool_activities {
            call.attempt_id = attempt;
        }
        let mut pending = complete.clone();
        pending.messages[1].state = MessageState::Pending;
        pending.messages[1].markdown = PREFIX.into();
        pending.messages[1].completed_at = None;
        pending.blocks = koru_ai_core::finalize_markdown(reply, PREFIX);
        for call in &mut pending.tool_activities {
            call.status = ToolActivityStatus::Running;
            call.completed_at = None;
        }
        let initial = surface.transcript.as_mut().unwrap();
        initial.messages.clear();
        initial.blocks.clear();
        initial.tool_activities.clear();
        if owned {
            surface.owned = Some(OwnedConversation {
                chat: complete.chat.id,
                destination: WorkspaceId::new(),
                provider: complete.chat.current_model.provider.clone(),
                offers: OwnedOffers::default(),
                refusals: &[],
            });
        }
        let (reader, commands, _) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.set_viewport(Rect::new(0.0, 0.0, 640.0, 600.0));
        surface.waiting = true;
        surface.pending_user = Some(complete.messages[0].markdown.clone());
        surface.apply_update(Update::Accepted(SendTurnAccepted {
            attempt_id: attempt,
            user_message_id: complete.messages[0].id,
            assistant_message_id: reply,
            replayed: false,
        }));
        let mut fixture = Self {
            surface,
            attempt,
            reply,
            pending,
            complete,
            commands,
        };
        fixture.say(PREFIX);
        fixture
    }

    fn event(&mut self, event: PublicAiEvent) {
        self.surface.apply_update(Update::Event(StreamEvent {
            version: AI_STREAM_VERSION,
            sequence: 1,
            event,
        }));
    }

    fn say(&mut self, text: &str) {
        self.event(PublicAiEvent::TextDelta {
            attempt_id: self.attempt,
            text: text.into(),
            at: Timestamp::from_millis(3_000),
        });
    }

    fn finish_calls(&mut self) {
        for activity in self.complete.tool_activities.clone() {
            self.event(PublicAiEvent::ToolActivity {
                attempt_id: self.attempt,
                activity,
            });
        }
    }

    fn recover(&mut self) {
        self.surface
            .apply_update(Update::RecoveryStarted(self.attempt));
        self.surface.apply_update(Update::Recovered {
            attempt: self.attempt,
            status: AttemptStatus::Complete,
            transcript: self.complete.clone(),
        });
    }

    fn assert_finished(&mut self) {
        self.surface.rebuild_messages(600.0);
        let replies: Vec<_> = self
            .surface
            .message_layouts
            .iter()
            .filter(|message| message.id == Some(self.reply))
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].state, MessageState::Complete);
        assert_eq!(replies[0].markdown, format!("{PREFIX}{ANSWER}"));
        assert!(self.surface.work_layouts.iter().all(|work| !work.live));
        assert!(self.surface.work_tick_deadline().is_none());
        assert!(!self.surface.is_working());
        assert!(!self.surface.waiting);
        assert!(self.surface.draft.is_empty());
        assert_eq!(self.surface.attempt, None);
        assert_eq!(self.surface.pending_assistant_message_id, None);
        assert_eq!(
            self.surface.transcript.as_ref().unwrap().tool_activities,
            self.complete.tool_activities
        );
    }
}

#[test]
fn pending_same_chat_read_preserves_stream_text_and_accepts_recovered_reply() {
    for owned in [true, false] {
        let mut f = Running::new(owned);
        for activity in f.pending.tool_activities.clone() {
            f.event(PublicAiEvent::ToolActivity {
                attempt_id: f.attempt,
                activity,
            });
        }
        assert!(
            !f.surface
                .apply_update(Update::Transcript(f.pending.clone()))
        );
        assert_eq!(f.surface.attempt, Some(f.attempt));
        assert_eq!(f.surface.pending_assistant_message_id, Some(f.reply));
        assert!(f.surface.waiting && f.surface.is_working());
        f.finish_calls();
        f.say(ANSWER);
        f.surface.rebuild_messages(600.0);
        assert_eq!(
            f.surface
                .message_layouts
                .iter()
                .filter(|m| m.role == ChatRole::User)
                .count(),
            1,
            "the read must not duplicate the optimistic question"
        );
        let replies: Vec<_> = f
            .surface
            .message_layouts
            .iter()
            .filter(|m| m.id == Some(f.reply))
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].markdown, format!("{PREFIX}{ANSWER}"));
        assert!(f.surface.work_layouts.iter().any(|work| work.live));
        f.recover();
        f.assert_finished();
        assert_eq!(f.surface.finished_replies(), 1);
        f.recover();
        assert_eq!(
            f.surface.finished_replies(),
            1,
            "one completion per attempt"
        );
    }
}

#[test]
fn pending_same_chat_read_also_preserves_normal_terminal_refresh() {
    let mut f = Running::new(true);
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    f.finish_calls();
    f.say(ANSWER);
    f.event(PublicAiEvent::Completed {
        attempt_id: f.attempt,
        stop_reason: None,
        at: Timestamp::from_millis(95_000),
    });
    assert!(
        f.commands.try_iter().any(|command| {
            matches!(command, Command::Read(chat) if chat == f.complete.chat.id)
        })
    );
    f.surface
        .apply_update(Update::Transcript(f.complete.clone()));
    f.assert_finished();
    assert_eq!(f.surface.finished_replies(), 1);
}

#[test]
fn a_read_started_before_acceptance_does_not_discard_the_accepted_stream() {
    let mut f = Running::new(true);
    let before_acceptance = f.surface.transcript.clone().unwrap();
    f.surface
        .apply_update(Update::Transcript(before_acceptance));
    assert_eq!(f.surface.attempt, Some(f.attempt));
    assert_eq!(f.surface.draft, PREFIX);
    f.recover();
    f.assert_finished();
}

#[test]
fn late_pending_snapshot_cannot_revive_a_completed_reply() {
    let mut f = Running::new(true);
    f.recover();
    assert!(
        !f.surface
            .apply_update(Update::Transcript(f.pending.clone()))
    );
    f.assert_finished();
    assert_eq!(f.surface.finished_replies(), 1);
}

#[test]
fn terminal_snapshot_can_settle_the_active_reply_without_a_terminal_stream_event() {
    let mut f = Running::new(true);
    assert!(
        f.surface
            .apply_update(Update::Transcript(f.complete.clone()))
    );
    f.assert_finished();
}

#[test]
fn recovery_from_an_older_attempt_cannot_replace_a_new_stream() {
    let mut f = Running::new(true);
    let newer = InferenceAttemptId::new();
    let reply = MessageId::new();
    f.surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id: newer,
        user_message_id: MessageId::new(),
        assistant_message_id: reply,
        replayed: false,
    }));
    f.recover();
    assert_eq!(f.surface.attempt, Some(newer));
    assert_eq!(f.surface.pending_assistant_message_id, Some(reply));
    assert_eq!(f.surface.draft, PREFIX);
    assert_eq!(f.surface.finished_replies(), 0);
}

#[test]
fn recovery_cannot_replace_a_newly_bound_bot_chat_before_its_read_arrives() {
    let mut f = Running::new(true);
    let mut next = f.surface.owned.clone().unwrap();
    next.chat = ChatId::new();
    f.surface.bind_owned(next.clone());
    f.recover();
    assert_eq!(f.surface.owned.as_ref().unwrap().chat, next.chat);
    assert_eq!(f.surface.draft, PREFIX);
    assert_eq!(f.surface.finished_replies(), 0);
    assert_eq!(f.surface.transcript.as_ref().unwrap().messages.len(), 0);
}

#[test]
fn recovery_cannot_replace_another_chat_or_accept_a_mismatched_transcript() {
    for replace_shown_chat in [false, true] {
        let mut f = Running::new(false);
        let mut other = f.complete.clone();
        other.chat.id = ChatId::new();
        other.tool_policy.chat_id = other.chat.id;
        other.messages.clear();
        other.blocks.clear();
        other.tool_activities.clear();
        let recovered = if replace_shown_chat {
            f.surface.apply_update(Update::Transcript(other.clone()));
            f.complete.clone()
        } else {
            other.clone()
        };
        f.surface.apply_update(Update::Recovered {
            attempt: f.attempt,
            status: AttemptStatus::Complete,
            transcript: recovered,
        });
        assert_eq!(f.surface.finished_replies(), 0);
        assert_eq!(
            f.surface.current_chat_id(),
            Some(if replace_shown_chat {
                other.chat.id
            } else {
                f.complete.chat.id
            })
        );
        assert!(f.surface.transcript.as_ref().unwrap().messages.is_empty());
    }
}

#[test]
fn opening_another_chat_fences_old_recovery_before_the_read_and_allows_the_next_turn() {
    let mut f = Running::new(false);
    let next = Running::new(false);
    let mut opening = next.complete.clone();
    opening.messages.clear();
    opening.blocks.clear();
    opening.tool_activities.clear();
    assert!(f.surface.open_link(opening.chat.id, None));
    f.recover();
    assert_eq!(f.surface.pending_link_chat, Some(opening.chat.id));
    assert_eq!(f.surface.finished_replies(), 0);
    assert!(f.surface.transcript.as_ref().unwrap().messages.is_empty());

    // The requested read is still allowed to land, consuming the link intent.
    f.surface.apply_update(Update::Transcript(opening));
    assert_eq!(f.surface.pending_link_chat, None);
    assert_eq!(f.surface.current_chat_id(), Some(next.complete.chat.id));
    f.surface.pending_user = Some(next.complete.messages[0].markdown.clone());
    f.surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id: next.attempt,
        user_message_id: next.complete.messages[0].id,
        assistant_message_id: next.reply,
        replayed: false,
    }));
    f.surface.apply_update(Update::Recovered {
        attempt: next.attempt,
        status: AttemptStatus::Complete,
        transcript: next.complete.clone(),
    });
    assert_eq!(f.surface.finished_replies(), 1);
    assert_eq!(f.surface.attempt, None);
    assert_eq!(
        f.surface.transcript.as_ref().unwrap().messages,
        next.complete.messages
    );
}

#[test]
fn new_chat_intent_rejects_old_recovery_until_its_own_created_and_accepted_turn() {
    let mut f = Running::new(false);
    f.surface.pending_link_chat = Some(f.complete.chat.id);
    assert!(f.surface.start_new_chat());
    assert!(f.surface.fresh_chat_wanted);
    assert_eq!(f.surface.pending_link_chat, None);
    f.recover();
    assert_eq!(f.surface.finished_replies(), 0);
    assert!(f.surface.transcript.as_ref().unwrap().messages.is_empty());

    // Choosing a model gives the new intent its own chat identity.
    f.surface
        .begin_unmade_chat(f.complete.chat.current_model.clone());
    let created = f.surface.transcript.clone().unwrap();
    let chat = created.chat.id;
    assert_ne!(chat, f.complete.chat.id);
    assert!(f.surface.make_unmade_chat());
    assert!(
        f.commands.try_iter().any(|command| {
            matches!(command, Command::Create(request) if request.chat_id == chat)
        })
    );
    f.surface.apply_update(Update::Created(created));
    let mut next = Running::new(false);
    next.complete.chat.id = chat;
    next.complete.tool_policy.chat_id = chat;
    for message in &mut next.complete.messages {
        message.chat_id = chat;
    }
    for call in &mut next.complete.tool_activities {
        call.chat_id = chat;
    }
    f.surface.pending_user = Some(next.complete.messages[0].markdown.clone());
    f.surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id: next.attempt,
        user_message_id: next.complete.messages[0].id,
        assistant_message_id: next.reply,
        replayed: false,
    }));
    f.surface.apply_update(Update::Recovered {
        attempt: next.attempt,
        status: AttemptStatus::Complete,
        transcript: next.complete.clone(),
    });
    assert_eq!(f.surface.current_chat_id(), Some(chat));
    assert_eq!(f.surface.finished_replies(), 1);
    assert_eq!(f.surface.attempt, None);
    assert_eq!(f.surface.pending_link_chat, None);
    assert_eq!(
        f.surface.transcript.as_ref().unwrap().messages,
        next.complete.messages
    );
}
