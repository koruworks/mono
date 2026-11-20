// SPDX-License-Identifier: MPL-2.0
//! The quiet status between tool calls follows observed provider events.
//! The cache is attempt-scoped; changing a model or repainting cannot move it.
use super::{ChatSurface, Progress};
use koru_ai_core::MessageId;
use koru_ai_wire::PublicAiEvent;
use std::collections::HashMap;

impl ChatSurface {
    pub(crate) fn observe_bot_progress(&mut self, event: &PublicAiEvent) {
        let attempt = event.attempt_id();
        if self.attempt != Some(attempt) {
            return;
        }
        let progress = match event {
            PublicAiEvent::TextDelta { text, .. } if !text.is_empty() => Progress::Typing,
            PublicAiEvent::Started { .. }
            | PublicAiEvent::ReasoningDelta { .. }
            | PublicAiEvent::ToolActivity { .. }
            | PublicAiEvent::WebSearch { .. } => Progress::Thinking,
            _ if event.is_terminal() => {
                self.bot_progress = None;
                return;
            }
            _ => return,
        };
        self.bot_progress = Some((attempt, progress));
    }

    /// Called only when the transcript is rebuilt, never for animation frames.
    pub(crate) fn bot_reply_progress(&self) -> HashMap<MessageId, Progress> {
        if let Some(state) = &self.managed {
            return state
                .live
                .values()
                .map(|live| (live.reply, live.progress))
                .collect();
        }
        let (Some(reply), Some(attempt)) = (self.pending_assistant_message_id, self.attempt) else {
            return HashMap::new();
        };
        let progress = if !self.open_searches.is_empty() {
            Progress::Searching
        } else {
            self.bot_progress
                .filter(|(id, _)| *id == attempt)
                .map(|(_, progress)| progress)
                .unwrap_or_else(|| {
                    if self
                        .transcript
                        .as_ref()
                        .and_then(|t| t.active_attempt.as_ref())
                        .is_some_and(|active| {
                            active.id == attempt && active.provider_started_at.is_none()
                        })
                    {
                        Progress::Waiting
                    } else {
                        Progress::Thinking
                    }
                })
        };
        HashMap::from([(reply, progress)])
    }
}
