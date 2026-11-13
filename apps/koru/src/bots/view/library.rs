// SPDX-License-Identifier: MPL-2.0

//! The Library card composes the settings' existing buttons, icons and scroll.
use koru_ai_core::bots::library::BOT_KNOWLEDGE_LIMIT;
use koru_ai_core::bots::{BotLifecycle, BotPage};
use koru_draw::el::{El, Key, Role, leaf, row, text};
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, roles};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::button::ActionStyle;

use super::controls::IconLeaf;
use super::region::{card, card_title, line};
use super::{BotView, SURFACE};

pub(super) const CARD: Key = Key::surface(SURFACE, "library");
pub(super) const ADD: Key = Key::surface(SURFACE, "knowledge-add");
pub(super) const SAVE: Key = Key::surface(SURFACE, "knowledge-save");
pub(super) const DISCARD: Key = Key::surface(SURFACE, "knowledge-discard");
pub(super) const RELOAD: Key = Key::surface(SURFACE, "library-reload");
const SUMMARY: Key = Key::surface(SURFACE, "knowledge-summary");
const NOTICE: Key = Key::surface(SURFACE, "library-notice");
const STATUS: Key = Key::surface(SURFACE, "library-status");
const EMPTY: Key = Key::surface(SURFACE, "knowledge-empty");
const TITLE: &str = "knowledge-title";
const OPEN: &str = "knowledge-open";
const UP: &str = "knowledge-up";
const DOWN: &str = "knowledge-down";
const REMOVE: &str = "knowledge-remove";

pub(super) fn page_key(page: &BotPage, part: &'static str) -> Key {
    Key::model(page.document.as_uuid().as_u128(), part)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Add,
    Save,
    Discard,
    Reload,
    Open(BotPage),
    Remove(BotPage),
    Move(BotPage, bool),
}

impl BotView {
    pub(super) fn library_pass(&mut self) -> bool {
        if !(self.settings_open || self.context_library_visible)
            || self.facts.as_ref().is_some_and(|v| v.trashed)
        {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        let Some(command) = self.library.read(bot.id) else {
            return false;
        };
        let sent = self.link.as_ref().is_some_and(|link| link.send(command));
        if !sent {
            self.library.read_unsent();
        }
        let mut work = self.work.get();
        work.commands += usize::from(sent);
        self.work.set(work);
        true
    }

    pub(super) fn library_card(&self, engine: &mut TextEngine) -> El {
        let bot = self.settings.bot().expect("settings have a Bot");
        let editable = self.library.can_edit(bot) && !self.settings.delivering();
        let mut content = card(CARD, Words::BotsLibrary)
            .child(
                row("Library standing role")
                    .gap(Space::S2)
                    .items_center()
                    .child(
                        text(self.words(Words::BotsRole))
                            .named("Library role label")
                            .role(roles::UI)
                            .fill()
                            .truncate(),
                    )
                    .child(self.button(
                        engine,
                        "Library open role",
                        super::region::OPEN_ROLE,
                        self.words(Words::BotsOpenRole),
                        ActionStyle::Secondary,
                        false,
                    )),
            )
            .child(card_title(t(self.lang, Words::BotsKnowledge)))
            .child(line(
                "Knowledge explanation",
                SUMMARY,
                self.words(Words::BotsKnowledgeHint),
                Ink::Secondary,
            ));
        if let Some(words) = self.library.notice() {
            content = content.child(line(
                "Library notice",
                NOTICE,
                self.words(words),
                Ink::Accent,
            ));
        }
        if (self.library.held().is_none() && self.library.notice().is_none())
            || self.library.loading()
            || self.library.saving()
            || self.library.saved()
        {
            let words = if self.library.saving() {
                Words::BotsSaving
            } else if self.library.loading() || self.library.held().is_none() {
                Words::BotsLoading
            } else {
                Words::BotsSaved
            };
            content = content.child(line(
                "Library status",
                STATUS,
                self.words(words),
                Ink::Secondary,
            ));
        }
        if self.library.held().is_some() && self.library.pages().is_empty() {
            content = content.child(line(
                "Knowledge empty",
                EMPTY,
                self.words(Words::BotsKnowledgeEmpty),
                Ink::Secondary,
            ));
        }
        for (index, page) in self.library.pages().iter().enumerate() {
            let title = self.page_title(page);
            let mut item = row("Knowledge page")
                .id(page_key(page, "knowledge-row"))
                .semantics(Role::Group)
                .name_text(title.clone())
                .gap(Space::S1)
                .items_center()
                .child(
                    text(title.clone())
                        .named("Knowledge page title")
                        .id(page_key(page, TITLE))
                        .role(roles::ROW)
                        .fill()
                        .truncate(),
                );
            for (part, glyph, words, disabled) in [
                (OPEN, icons::POP_OUT, Words::BotsOpenKnowledgePage, false),
                (
                    UP,
                    icons::CARET_UP,
                    Words::BotsKnowledgeUp,
                    !editable || index == 0,
                ),
                (
                    DOWN,
                    icons::CARET_DOWN,
                    Words::BotsKnowledgeDown,
                    !editable || index + 1 == self.library.pages().len(),
                ),
                (
                    REMOVE,
                    icons::WINDOW_CLOSE,
                    Words::BotsRemovePage,
                    !editable,
                ),
            ] {
                item = item.child(
                    leaf("Knowledge page action", IconLeaf { glyph, disabled })
                        .semantic()
                        .id(page_key(page, part))
                        .hit()
                        .focusable()
                        .disabled(disabled)
                        .states(&super::actions::ICON_STATES)
                        .semantics(Role::Button)
                        .name_text(fill(t(self.lang, words), &[title.as_str()])),
                );
            }
            content = content.child(item);
        }
        content = content.child(row("Knowledge add").child(self.button(
            engine,
            "Add Knowledge page",
            ADD,
            self.words(Words::BotsAddPage),
            ActionStyle::Secondary,
            !editable || self.library.pages().len() >= BOT_KNOWLEDGE_LIMIT,
        )));
        let actions = row("Knowledge actions")
            .gap(Space::S2)
            .items_center()
            .child(self.button(
                engine,
                "Save Knowledge",
                SAVE,
                self.words(if self.library.retrying() {
                    Words::BotsRetry
                } else {
                    Words::BotsSave
                }),
                ActionStyle::Primary,
                !self.library.can_save(bot) || self.settings.delivering(),
            ))
            .child(self.button(
                engine,
                "Discard Knowledge draft",
                DISCARD,
                self.words(Words::BotsDiscardKnowledge),
                ActionStyle::Ghost,
                !editable || !self.library.dirty(),
            ));
        content
            .child(actions)
            .child(row("Library refresh").child(self.button(
                engine,
                "Reload Library",
                RELOAD,
                self.words(Words::BotsRefreshLibrary),
                ActionStyle::Ghost,
                self.library.loading(),
            )))
    }

    pub(super) fn library_action(&self, key: Key) -> Option<Action> {
        Some(match key {
            ADD => Action::Add,
            SAVE => Action::Save,
            DISCARD => Action::Discard,
            RELOAD => Action::Reload,
            _ => {
                let page = self
                    .library
                    .pages()
                    .iter()
                    .find(|p| page_key(p, key.part) == key)?
                    .clone();
                match key.part {
                    OPEN => Action::Open(page),
                    UP => Action::Move(page, true),
                    DOWN => Action::Move(page, false),
                    REMOVE => Action::Remove(page),
                    _ => return None,
                }
            }
        })
    }
    pub(super) fn perform_library(&mut self, action: Action) -> bool {
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        if bot.lifecycle == BotLifecycle::Trashed {
            return false;
        }
        match action {
            Action::Reload => {
                self.library.owe_read();
                self.asked = false;
                true
            }
            Action::Open(page) => {
                self.role_request = Some(page);
                true
            }
            Action::Add => {
                if !self.library.can_edit(bot)
                    || self.settings.delivering()
                    || self.library.pages().len() >= BOT_KNOWLEDGE_LIMIT
                {
                    return false;
                }
                self.open_knowledge_picker();
                true
            }
            Action::Discard => {
                !self.settings.delivering() && self.library.can_edit(bot) && self.library.discard()
            }
            Action::Remove(page) => {
                if self.settings.delivering() {
                    return false;
                }
                let removed = self.library.remove(bot, &page);
                if removed
                    && self
                        .focus
                        .is_some_and(|key| key.object == page_key(&page, REMOVE).object)
                {
                    self.focus = Some(ADD);
                }
                removed
            }
            Action::Move(page, up) => {
                if self.settings.delivering() {
                    return false;
                }
                let moved = self.library.move_page(bot, &page, up);
                if moved && self.focus.is_some() {
                    self.focus = Some(page_key(&page, OPEN));
                }
                moved
            }
            Action::Save => {
                if self.settings.delivering() {
                    return false;
                }
                let Some(command) = self.library.save(bot) else {
                    return false;
                };
                self.send_write(command);
                true
            }
        }
    }
    pub(super) fn library_line(&self, key: Key) -> Option<String> {
        match key {
            SUMMARY => Some(self.words(Words::BotsKnowledgeHint)),
            NOTICE => self.library.notice().map(|words| self.words(words)),
            EMPTY => Some(self.words(Words::BotsKnowledgeEmpty)),
            STATUS => Some(self.words(if self.library.saving() {
                Words::BotsSaving
            } else if self.library.loading() || self.library.held().is_none() {
                Words::BotsLoading
            } else {
                Words::BotsSaved
            })),
            _ if key.part == TITLE => self
                .library
                .pages()
                .iter()
                .find(|p| page_key(p, TITLE) == key)
                .map(|p| self.page_title(p)),
            _ => None,
        }
    }
}
