// SPDX-License-Identifier: MPL-2.0
//! History activation reaches the main composer's exact result, after it binds.
use super::*;

impl BotView {
    pub(crate) fn show_routine_result(&mut self, occurrence: uuid::Uuid) {
        self.pending_routine_result = Some(occurrence);
    }

    pub(super) fn routine_result_target(&mut self) -> Option<ConversationTarget> {
        let occurrence = self.pending_routine_result?;
        let facts = self.facts.as_ref()?;
        if self.chat != facts.main_chat {
            let target = ConversationTarget {
                bot: facts.id,
                name: self.label_name.clone().unwrap_or_default(),
                chat: facts.main_chat,
                result: Some(occurrence),
            };
            self.pending_routine_result = None;
            return Some(target);
        }
        if self.conversation.as_mut().is_some_and(|view| {
            view.dictation_surface_mut()
                .open_routine_result(facts.id, occurrence)
        }) {
            self.pending_routine_result = None;
        }
        None
    }
}
