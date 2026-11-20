// SPDX-License-Identifier: MPL-2.0

//! Knowledge drafts and immutable save receipts. No I/O or document content.
use koru_ai_core::bots::library::{BOT_KNOWLEDGE_LIMIT, BotLibraryView, validate_knowledge};
use koru_ai_core::bots::{
    Bot, BotCommand, BotId, BotLifecycle, BotPage, BotRequestId, BotResponse,
};
use koru_i18n::Key as Words;

use super::link::{Command, Failure};

struct Save {
    request: BotRequestId,
    command: BotCommand,
    in_flight: bool,
}

#[derive(Default)]
pub(crate) struct Library {
    held: Option<BotLibraryView>,
    draft: Option<Vec<BotPage>>,
    save: Option<Save>,
    notice: Option<Words>,
    saved: bool,
    seq: u64,
    reading: bool,
    read_owed: bool,
    known_revision: u64,
}

impl Library {
    pub(crate) fn held(&self) -> Option<&BotLibraryView> {
        self.held.as_ref()
    }
    pub(crate) fn pages(&self) -> &[BotPage] {
        self.draft
            .as_deref()
            .unwrap_or_else(|| self.held.as_ref().map_or(&[], |v| v.knowledge.as_slice()))
    }
    pub(crate) fn notice(&self) -> Option<Words> {
        self.notice
    }
    pub(crate) fn loading(&self) -> bool {
        self.reading
    }
    pub(crate) fn awaiting(&self) -> bool {
        self.reading || self.read_owed || self.saving()
    }
    pub(crate) fn saving(&self) -> bool {
        self.save.as_ref().is_some_and(|s| s.in_flight)
    }
    pub(crate) fn retrying(&self) -> bool {
        self.save.as_ref().is_some_and(|s| !s.in_flight)
    }
    pub(crate) fn dirty(&self) -> bool {
        self.draft.is_some()
    }
    pub(crate) fn saved(&self) -> bool {
        self.saved && !self.dirty()
    }
    pub(crate) fn owns(&self, request: BotRequestId) -> bool {
        self.save.as_ref().is_some_and(|s| s.request == request)
    }
    pub(crate) fn owe_read(&mut self) {
        self.read_owed = true;
    }
    pub(crate) fn known_revision(&self) -> u64 {
        self.known_revision
    }

    pub(crate) fn know_bot(&mut self, bot: &Bot) {
        self.known_revision = self.known_revision.max(bot.revision);
        if bot.lifecycle != BotLifecycle::Trashed
            && self
                .held
                .as_ref()
                .is_none_or(|v| v.revision != bot.revision)
        {
            self.owe_read();
        }
    }
    pub(crate) fn read(&mut self, bot: BotId) -> Option<Command> {
        if self.reading || !self.read_owed {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        self.reading = true;
        self.read_owed = false;
        Some(Command::ReadLibrary { bot, seq: self.seq })
    }
    pub(crate) fn read_unsent(&mut self) {
        self.reading = false;
        self.read_owed = false;
        self.notice = Some(Words::BotsErrUnreachable);
    }
    pub(crate) fn answer_read(
        &mut self,
        seq: u64,
        bot: &Bot,
        result: Result<BotLibraryView, Failure>,
    ) -> bool {
        if !self.reading || seq != self.seq {
            return false;
        }
        self.reading = false;
        match result {
            Ok(view)
                if view.bot_id == bot.id
                    && view.revision >= self.known_revision
                    && view.role.workspace == bot.home_workspace
                    && view
                        .memory
                        .as_ref()
                        .is_none_or(|p| p.workspace == bot.home_workspace)
                    && validate_knowledge(&view.knowledge, bot.home_workspace).is_ok() =>
            {
                if self.draft.is_some()
                    && self
                        .held
                        .as_ref()
                        .is_some_and(|old| old.revision != view.revision)
                {
                    self.notice = Some(Words::BotsChangedElsewhere);
                } else if self.save.is_none() {
                    self.notice = None;
                }
                if self.save.is_none() && self.draft.as_ref() == Some(&view.knowledge) {
                    self.draft = None;
                }
                self.known_revision = self.known_revision.max(view.revision);
                self.held = Some(view);
            }
            Err(Failure::Refused(code)) => {
                self.notice = Some(super::settings::refusal_words(&code))
            }
            _ => self.notice = Some(Words::BotsErrUnreachable),
        }
        true
    }
    fn mutable(bot: &Bot) -> bool {
        bot.lifecycle == BotLifecycle::Active && bot.pending_change.is_none()
    }
    pub(crate) fn can_edit(&self, bot: &Bot) -> bool {
        Self::mutable(bot)
            && self.save.is_none()
            && self
                .held
                .as_ref()
                .is_some_and(|v| v.bot_id == bot.id && v.revision == bot.revision)
    }
    pub(crate) fn can_save(&self, bot: &Bot) -> bool {
        if let Some(save) = &self.save {
            return Self::mutable(bot) && !save.in_flight;
        }
        self.can_edit(bot) && self.dirty()
    }
    pub(crate) fn add(&mut self, bot: &Bot, page: BotPage) -> bool {
        if !self.can_edit(bot)
            || self.pages().len() >= BOT_KNOWLEDGE_LIMIT
            || page.workspace != bot.home_workspace
            || self.pages().contains(&page)
            || self
                .held
                .as_ref()
                .is_some_and(|v| v.memory.as_ref() == Some(&page))
        {
            return false;
        }
        let mut pages = self.pages().to_vec();
        pages.push(page);
        self.edit(pages);
        true
    }
    pub(crate) fn remove(&mut self, bot: &Bot, page: &BotPage) -> bool {
        if !self.can_edit(bot) || !self.pages().contains(page) {
            return false;
        }
        let pages = self
            .pages()
            .iter()
            .filter(|p| *p != page)
            .cloned()
            .collect();
        self.edit(pages);
        true
    }
    pub(crate) fn move_page(&mut self, bot: &Bot, page: &BotPage, up: bool) -> bool {
        if !self.can_edit(bot) {
            return false;
        }
        let Some(index) = self.pages().iter().position(|p| p == page) else {
            return false;
        };
        let next = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1).filter(|i| *i < self.pages().len())
        };
        let Some(next) = next else {
            return false;
        };
        let mut pages = self.pages().to_vec();
        pages.swap(index, next);
        self.edit(pages);
        true
    }
    fn edit(&mut self, pages: Vec<BotPage>) {
        self.draft = self
            .held
            .as_ref()
            .filter(|v| v.knowledge != pages)
            .map(|_| pages);
        self.saved = false;
        self.notice = None;
    }
    pub(crate) fn discard(&mut self) -> bool {
        // An uncertain save must be reconciled, not silently forgotten.
        if self.save.is_some() || self.draft.is_none() {
            return false;
        }
        self.draft = None;
        self.notice = None;
        true
    }
    pub(crate) fn save(&mut self, bot: &Bot) -> Option<Command> {
        if !self.can_save(bot) {
            return None;
        }
        if self.save.is_none() {
            let request = BotRequestId::mint();
            self.save = Some(Save {
                request,
                command: BotCommand::SetKnowledge {
                    request_id: request,
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                    pages: self.pages().to_vec(),
                },
                in_flight: false,
            });
        }
        let save = self.save.as_mut()?;
        save.in_flight = true;
        self.notice = None;
        Some(Command::Write {
            bot: bot.id,
            request: save.request,
            command: Box::new(save.command.clone()),
        })
    }
    /// Bind an ordinary Memory page under the exact Library revision.
    pub(crate) fn set_memory(&mut self, bot: &Bot, memory: Option<BotPage>) -> Option<Command> {
        if !self.can_edit(bot)
            || memory.as_ref().is_some_and(|page| {
                page.workspace != bot.home_workspace
                    || self.pages().contains(page)
                    || self
                        .held
                        .as_ref()
                        .is_some_and(|held| &held.role == page || held.knowledge.contains(page))
            })
            || self.held.as_ref()?.memory == memory
        {
            return None;
        }
        let request = BotRequestId::mint();
        self.save = Some(Save {
            request,
            command: BotCommand::SetMemory {
                request_id: request,
                bot_id: bot.id,
                expected_revision: bot.revision,
                memory,
            },
            in_flight: false,
        });
        self.save(bot)
    }
    pub(crate) fn unsent(&mut self, request: BotRequestId) {
        if self.owns(request) {
            self.save.as_mut().expect("owned").in_flight = false;
            self.notice = Some(Words::BotsErrUnreachable);
        }
    }
    pub(crate) fn answer_save(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
    ) {
        if !self.owns(request) {
            return;
        }
        let save = self.save.as_ref().expect("owned");
        if let BotCommand::SetMemory {
            bot_id,
            expected_revision,
            memory,
            ..
        } = &save.command
        {
            match result {
                Ok(BotResponse::Bot { bot })
                    if bot.id == *bot_id && bot.revision == expected_revision.saturating_add(1) =>
                {
                    let memory = memory.clone();
                    self.save = None;
                    self.saved = true;
                    self.notice = None;
                    self.known_revision = self.known_revision.max(bot.revision);
                    if let Some(held) = &mut self.held
                        && held.revision <= bot.revision
                    {
                        held.revision = bot.revision;
                        held.memory = memory;
                    }
                    self.reading = false;
                    self.owe_read();
                }
                Err(Failure::Refused(code)) => {
                    self.save = None;
                    self.notice = Some(super::settings::refusal_words(&code));
                    self.owe_read();
                }
                _ => self.unsent(request),
            }
            return;
        }
        let BotCommand::SetKnowledge {
            bot_id,
            expected_revision,
            pages,
            ..
        } = &save.command
        else {
            return;
        };
        match result {
            Ok(BotResponse::KnowledgeSaved { receipt })
                if receipt.bot_id == *bot_id
                    && receipt.request_id == request
                    && receipt.revision == expected_revision.saturating_add(1)
                    && receipt.pages == *pages =>
            {
                if self.draft.as_ref() == Some(pages) {
                    self.draft = None;
                }
                self.save = None;
                self.saved = true;
                self.notice = None;
                self.known_revision = self.known_revision.max(receipt.revision);
                if let Some(held) = &mut self.held
                    && held.revision <= receipt.revision
                {
                    held.revision = receipt.revision;
                    held.knowledge = receipt.pages;
                }
                // A read sent before this acknowledgement cannot replace it.
                self.reading = false;
                self.owe_read();
            }
            Err(Failure::Refused(code)) => {
                self.save = None;
                self.notice = Some(super::settings::refusal_words(&code));
                self.owe_read();
            }
            _ => self.unsent(request),
        }
    }
}

#[cfg(test)]
mod tests;
