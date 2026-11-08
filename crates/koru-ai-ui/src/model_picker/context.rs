// SPDX-License-Identifier: MPL-2.0

//! **The picker's context toggle** (koru-ai D10, D76): a *1M context* pill
//! in the footer's second row, for a chat whose model has a longer context
//! window. On is the longer window; off is the model's own (*Auto*, before
//! D76).
//!
//! An agent may list a model twice, once per window (Claude's `opus` and
//! `opus[1m]`). The runtime folds the two into one model that carries the
//! longer window's id ([`AgentModelView::context`]), so the list shows one
//! *Opus* and this pill chooses the window. Choosing only chooses which id
//! the chat takes as its model, through the same update as a row: off is
//! the model's own id, on the window's. Nothing here knows how an agent
//! spells either.
//!
//! The window is a model, so it changes when the model may
//! (`ChatSurface::model_locked`): before an agent chat's first message, and
//! after it only where the agent's host takes a model with each message.
//! Elsewhere the agent started with it, and the pill only shows it.

use koru_ai_core::AiProviderId;
use koru_i18n::{Key, Lang, fill, number, t};

use crate::agents::{find_listed, is_agent_provider};

/// The chat's model's context choice.
pub(crate) struct ContextChoice {
    pub(super) agent: AiProviderId,
    /// The model's own id: off.
    pub(super) base: String,
    /// The id that runs it with the longer window.
    pub(super) long: String,
    pub(super) tokens: u32,
    /// The chat is on the longer window.
    pub(super) chosen_long: bool,
}

/// A window's size as the pill names it: *1M*, or *200k*.
pub(crate) fn window_label(lang: Lang, tokens: u32) -> String {
    let tokens = u64::from(tokens);
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        fill(
            t(lang, Key::AiModelContextMillions),
            &[&number::decimal(lang, tokens / 1_000_000)],
        )
    } else {
        fill(
            t(lang, Key::AiModelContextThousands),
            &[&number::decimal(lang, tokens / 1_000)],
        )
    }
}

/// The pill's words: *1M context*.
pub(crate) fn pill_label(lang: Lang, tokens: u32) -> String {
    fill(
        t(lang, Key::AiModelContextPill),
        &[&window_label(lang, tokens)],
    )
}

impl crate::ChatSurface {
    /// The chat's model's context choice: an agent chat on a listed model
    /// that has a longer window. `None` for every other chat and model.
    pub(crate) fn picker_context(&self) -> Option<ContextChoice> {
        let model = &self.transcript.as_ref()?.chat.current_model;
        if !is_agent_provider(&model.provider) {
            return None;
        }
        let (listed, chosen_long) = find_listed(
            self.listed_agent_models(&model.provider),
            &model.requested_model,
        )?;
        let context = listed.context.as_ref()?;
        Some(ContextChoice {
            agent: model.provider.clone(),
            base: listed.id.clone(),
            long: context.id.clone(),
            tokens: context.tokens,
            chosen_long,
        })
    }

    /// Choose the window: the longer one when `long`, else the model's own.
    /// Read only once the chat has begun on a host that keeps a live
    /// session's model.
    pub(crate) fn choose_context(&mut self, long: bool) -> bool {
        let Some(choice) = self.picker_context() else {
            return false;
        };
        if self.model_locked() {
            self.status = Some(t(self.lang, Key::AiModelContextLockedAfterStart).to_owned());
            return true;
        }
        if choice.chosen_long == long {
            return true;
        }
        let id = if long { choice.long } else { choice.base };
        self.choose_agent_model(choice.agent, id)
    }

    /// The pill pressed, or Space or Enter on it: the other window.
    pub(crate) fn toggle_context(&mut self) -> bool {
        match self.picker_context() {
            Some(choice) => self.choose_context(!choice.chosen_long),
            None => false,
        }
    }
}
