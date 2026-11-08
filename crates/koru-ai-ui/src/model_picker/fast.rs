// SPDX-License-Identifier: MPL-2.0

//! **The picker's Fast toggle** (koru-ai D11, D76): a *⚡ Fast* pill in the
//! footer's second row, for an agent chat whose model offers its agent's
//! Fast mode — Claude Code's `fast` option, Codex's *Fast* service tier. A
//! model without one has no pill. Koru's icon catalog has no lightning, so
//! the pill's ⚡ is the bundled Phosphor face's `ph-lightning`
//! ([`super::elements::LIGHTNING`]).
//!
//! Unlike the level and the context window, Fast mode can change after the
//! chat has begun: the agent takes it with the next message (Claude's
//! wrapper sets it live; Codex takes a tier per turn). The choice is the
//! chat's (`Chat::agent_fast`); one never made reads as the model's own
//! default.

use koru_ai_core::AiProviderId;
use koru_ai_wire::UpdateChatAgentSettingsRequest;

use crate::agents::{find_listed, is_agent_provider};
use crate::reader::Command;

/// The chat's model's Fast mode.
pub(crate) struct FastChoice {
    pub(super) agent: AiProviderId,
    /// Fast mode is on for the chat's next message.
    pub(super) on: bool,
}

impl crate::ChatSurface {
    /// The chat's model's Fast mode: an agent chat on a listed model that
    /// offers one. `None` for every other chat and model.
    pub(crate) fn picker_fast(&self) -> Option<FastChoice> {
        let chat = &self.transcript.as_ref()?.chat;
        let model = &chat.current_model;
        if !is_agent_provider(&model.provider) {
            return None;
        }
        let (listed, _) = find_listed(
            self.listed_agent_models(&model.provider),
            &model.requested_model,
        )?;
        let fast = listed.fast?;
        Some(FastChoice {
            agent: model.provider.clone(),
            on: chat.agent_fast.unwrap_or(fast.on_by_default),
        })
    }

    /// Turn Fast mode on or off for the chat's next message.
    pub(crate) fn choose_fast(&mut self, on: bool) -> bool {
        let Some(choice) = self.picker_fast() else {
            return false;
        };
        if choice.on == on {
            return true;
        }
        let Some(transcript) = self.transcript.as_mut() else {
            return false;
        };
        transcript.chat.agent_fast = Some(on);
        let chat = transcript.chat.id;
        tracing::debug!(target: "koru_ai::ui", agent = %choice.agent, on, "fast mode chosen");
        self.send_command(Command::UpdateAgentSettings(
            chat,
            UpdateChatAgentSettingsRequest {
                mode: None,
                fast: Some(on),
            },
        ))
    }

    /// The pill pressed, or Space or Enter on it: Fast mode the other way.
    pub(crate) fn toggle_fast(&mut self) -> bool {
        match self.picker_fast() {
            Some(choice) => self.choose_fast(!choice.on),
            None => false,
        }
    }
}
