// SPDX-License-Identifier: MPL-2.0

//! A transcript read and its response stream can arrive independently. A
//! pending snapshot has no stream-sequence boundary, so it cannot replace a
//! live draft without losing or duplicating text. Keep that stream until its
//! terminal event or recovery, and never turn a known terminal reply pending
//! again when an older read arrives late.

use crate::{ChatId, ChatSurface, ChatTranscript, MessageState};
use koru_ai_core::{ChatRole, InferenceAttempt, InferenceAttemptId};

fn pending_attempt(transcript: &ChatTranscript) -> Option<&InferenceAttempt> {
    transcript.active_attempt.as_ref().filter(|attempt| {
        !attempt.status.is_terminal()
            && attempt.chat_id == Some(transcript.chat.id)
            && transcript.messages.iter().any(|message| {
                Some(message.id) == attempt.assistant_message_id
                    && message.chat_id == transcript.chat.id
                    && message.role == ChatRole::Assistant
                    && message.state == MessageState::Pending
            })
    })
}

impl ChatSurface {
    /// Restore Stop and activity from an exact server binding. This starts no
    /// provider work and never constructs a second optimistic message pair.
    pub(crate) fn restore_transcript_attempt(&mut self) {
        if self.managed.is_some() || self.attempt.is_some() {
            return;
        }
        let Some(attempt) = self.transcript.as_ref().and_then(pending_attempt).cloned() else {
            return;
        };
        let chat = attempt.chat_id.unwrap();
        if self.ended_attempt == Some(attempt.id) || !self.recovery_targets_current_chat(chat) {
            return;
        }
        self.attempt = Some(attempt.id);
        self.bot_progress = None;
        self.pending_assistant_message_id = attempt.assistant_message_id;
        self.observed_attempt = Some(attempt.id);
        self.observation_generation = self.observation_generation.wrapping_add(1);
        self.live_turn_started_ms = Some(
            attempt
                .provider_started_at
                .unwrap_or(attempt.queued_at)
                .millis(),
        );
        self.waiting = true;
        self.observe_retry_at = Some(std::time::Instant::now());
        self.maintain_observation(std::time::Instant::now());
    }

    pub(crate) fn maintain_observation(&mut self, now: std::time::Instant) -> bool {
        let Some(at) = self.observe_retry_at else {
            return false;
        };
        if now < at {
            return false;
        }
        let mut changed = false;
        if let Some((chat, attempt, generation)) = self.pending_observation_stop {
            if !self.reader.as_ref().is_some_and(|reader| {
                reader
                    .send(crate::reader::Command::StopObserving(
                        chat, attempt, generation,
                    ))
                    .is_ok()
            }) {
                self.observe_retry_at = Some(now + std::time::Duration::from_millis(500));
                return false;
            }
            self.pending_observation_stop = None;
            changed = true;
        }
        let Some(transcript) = &self.transcript else {
            self.observe_retry_at = None;
            return false;
        };
        if !self.observes_transcript(transcript) {
            self.observe_retry_at = None;
            return changed;
        }
        let command = crate::reader::Command::ObserveAttempt(
            transcript.chat.id,
            self.observed_attempt.unwrap(),
            self.observation_generation,
        );
        // A full local command queue is not a refusal of the running reply.
        // Keep one bounded retry on the existing active-surface maintenance lane.
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.send(command).is_ok())
        {
            self.observe_retry_at = None;
            true
        } else {
            self.observe_retry_at = Some(now + std::time::Duration::from_millis(500));
            false
        }
    }

    pub(crate) fn retire_transcript_observation(&mut self) {
        let generation = self.observation_generation;
        self.observation_generation = self.observation_generation.wrapping_add(1);
        if let Some(attempt) = self.observed_attempt.take() {
            // An observation not yet enqueued has no reader worker to retire.
            if self.observe_retry_at.is_none()
                && let Some(chat) = self.shown_chat_id()
            {
                self.pending_observation_stop = Some((chat, attempt, generation));
            }
            if self.attempt == Some(attempt) {
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.waiting = false;
            }
        }
        self.observe_retry_at = self
            .pending_observation_stop
            .map(|_| std::time::Instant::now());
        self.maintain_observation(std::time::Instant::now());
    }

    pub(crate) fn observes_transcript(&self, transcript: &ChatTranscript) -> bool {
        self.observed_attempt.is_some_and(|id| {
            self.attempt == Some(id)
                && self.ended_attempt != Some(id)
                && self.recovery_targets_current_chat(transcript.chat.id)
                && pending_attempt(transcript).is_some_and(|attempt| {
                    attempt.id == id
                        && attempt.assistant_message_id == self.pending_assistant_message_id
                })
        })
    }

    pub(crate) fn apply_observed_transcript(
        &mut self,
        attempt: InferenceAttemptId,
        mut transcript: ChatTranscript,
    ) -> bool {
        if self.observed_attempt != Some(attempt) || !self.observes_transcript(&transcript) {
            return false;
        }
        if let Some(current) = &self.transcript {
            // This channel observes the running reply. A delayed snapshot
            // cannot undo a newer model-selection acknowledgement.
            transcript.chat.current_model = current.chat.current_model.clone();
            transcript.reasoning_control = current.reasoning_control.clone();
            if current == &transcript {
                return false;
            }
            let reply = self.pending_assistant_message_id;
            let changed_text = transcript.messages.iter().any(|message| {
                Some(message.id) == reply
                    && current
                        .messages
                        .iter()
                        .any(|old| old.id == message.id && old.markdown != message.markdown)
            });
            if changed_text {
                self.bot_progress = Some((attempt, crate::bot_presentation::Progress::Typing));
            } else if current.tool_activities != transcript.tool_activities {
                self.bot_progress = Some((attempt, crate::bot_presentation::Progress::Thinking));
            }
        }
        self.transcript = Some(transcript);
        self.messages_dirty = true;
        self.waiting = true;
        true
    }
    pub(crate) fn recovery_targets_current_chat(&self, chat: ChatId) -> bool {
        !self.fresh_chat_wanted
            && self.shown_chat_id() == Some(chat)
            && self.owned.as_ref().is_none_or(|owned| owned.chat == chat)
            && self.pending_link_chat.is_none_or(|target| target == chat)
    }

    /// The link is an outstanding navigation intent, not the chat's permanent
    /// provenance. Only its own successful read consumes it.
    pub(crate) fn settle_chat_link(&mut self, chat: ChatId) {
        if self.pending_link_chat == Some(chat) {
            self.pending_link_chat = None;
        }
    }

    /// A read of the current legacy conversation that would replace newer
    /// response state. Engine-owned conversations reconcile their own feed.
    pub(crate) fn keep_streamed_transcript(&self, incoming: &ChatTranscript) -> bool {
        if self.managed.is_some() || !self.recovery_targets_current_chat(incoming.chat.id) {
            return false;
        }
        let Some(current) = &self.transcript else {
            return false;
        };
        let terminal: std::collections::HashSet<_> = current
            .messages
            .iter()
            .filter(|message| message.state.is_terminal())
            .map(|message| message.id)
            .collect();
        if let Some(reply) = incoming.messages.iter().find(|message| {
            message.state == MessageState::Pending && terminal.contains(&message.id)
        }) {
            tracing::debug!(
                target: "koru_ai::ui_stream",
                chat = %incoming.chat.id,
                reply = %reply.id,
                "AI UI ignored a pending snapshot of a terminal reply"
            );
            return true;
        }
        if self.reply_streaming()
            && self.observed_attempt.is_none()
            && let Some(reply) = self.pending_assistant_message_id
            && !incoming
                .messages
                .iter()
                .any(|message| message.id == reply && message.state.is_terminal())
        {
            tracing::debug!(
                target: "koru_ai::ui_stream",
                chat = %incoming.chat.id,
                attempt = ?self.attempt,
                reply = %reply,
                "AI UI kept its response stream across a pending transcript read"
            );
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests;
