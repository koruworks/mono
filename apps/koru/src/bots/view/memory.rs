// SPDX-License-Identifier: MPL-2.0

//! Memory is an ordinary page bound by the engine's Library revision.
use super::*;
use koru_ai_core::bots::BotPage;
use koru_draw::el::{El, Key, col, row, text};
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, roles};
use koru_ui::button::ActionStyle;

pub(super) const CHOOSE: Key = Key::surface(SURFACE, "memory-choose");
const OPEN: Key = Key::surface(SURFACE, "memory-open");
const REMOVE: Key = Key::surface(SURFACE, "memory-remove");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Choose,
    Open,
    Remove,
}

impl BotView {
    pub(super) fn memory_section(&self, engine: &mut TextEngine) -> El {
        let page = self.library.held().and_then(|held| held.memory.as_ref());
        let editable = !self.stale
            && !self.settings.delivering()
            && self
                .settings
                .bot()
                .is_some_and(|bot| self.library.can_edit(bot));
        let mut section = col("Memory page")
            .id(Key::surface(SURFACE, "memory-section"))
            .gap(Space::S2)
            .child(super::region::card_title(t(self.lang, Words::BotsMemory)));
        if let Some(page) = page {
            section = section.child(
                text(self.page_title(page))
                    .named("Memory page title")
                    .role(roles::ROW)
                    .ink(Ink::Primary)
                    .wrap(),
            );
        }
        let mut actions = row("Memory page actions")
            .gap(Space::S2)
            .items_center()
            .child(self.button(
                engine,
                "Choose Memory page",
                CHOOSE,
                self.words(Words::BotsChooseRolePage),
                ActionStyle::Secondary,
                !editable,
            ));
        if let Some(page) = page {
            actions = actions
                .child(self.button(
                    engine,
                    "Open Memory page",
                    OPEN,
                    self.words(Words::BotsMemoryOpen),
                    ActionStyle::Secondary,
                    false,
                ))
                .child(self.button(
                    engine,
                    "Remove Memory page",
                    REMOVE,
                    koru_i18n::fill(
                        t(self.lang, Words::BotsRemovePage),
                        &[&self.page_title(page)],
                    ),
                    ActionStyle::Ghost,
                    !editable,
                ));
        }
        section.child(actions)
    }

    pub(super) fn memory_action(&self, key: Key) -> Option<Action> {
        if !self.settings_open {
            return None;
        }
        match key {
            CHOOSE => Some(Action::Choose),
            OPEN => Some(Action::Open),
            REMOVE => Some(Action::Remove),
            _ => None,
        }
    }

    pub(super) fn choose_memory_page(&mut self, page: BotPage) -> bool {
        self.save_memory_page(Some(page))
    }

    fn save_memory_page(&mut self, page: Option<BotPage>) -> bool {
        if self.stale || !self.settings_open || self.settings.delivering() {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        let Some(command) = self.library.set_memory(bot, page) else {
            return false;
        };
        self.send_write(command);
        true
    }

    pub(super) fn perform_memory(&mut self, action: Action) -> bool {
        if self.stale || !self.settings_open || self.create.is_some() {
            return false;
        }
        match action {
            Action::Choose => {
                if self.settings.delivering()
                    || self
                        .settings
                        .bot()
                        .is_none_or(|bot| !self.library.can_edit(bot))
                {
                    return false;
                }
                self.open_memory_picker();
                true
            }
            Action::Open => {
                let Some(page) = self.library.held().and_then(|held| held.memory.clone()) else {
                    return false;
                };
                self.role_request = Some(page);
                true
            }
            Action::Remove => self.save_memory_page(None),
        }
    }

    pub(crate) fn take_document_request(&mut self) -> Option<koru_editor::trigger::DocumentTarget> {
        self.take_role_request()
            .map(|page| koru_editor::trigger::DocumentTarget {
                document: koru_core::DocumentId::from_uuid(page.document.as_uuid()),
                node: None,
            })
    }
}
