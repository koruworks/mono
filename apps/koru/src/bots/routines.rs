// SPDX-License-Identifier: MPL-2.0
//! One visible owner overview. Retained observations never authorize opening.
use super::link::{Command, Failure, Said};
use koru_ai_core::{
    ChatId,
    bots::{BotChatLabel, BotId, BotResponse, routines::*},
};
use koru_i18n::Key as Words;
use std::rc::Rc;
pub(crate) mod detail;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadFailure {
    TransportOnly,
    Other(Failure),
}
pub(crate) fn connection_failure(code: koru_store_core::Code) -> ReadFailure {
    // api-client discovery maps only NotRunning to this typed code; handshake,
    // version and authentication uncertainty must clear the old page.
    if code == koru_store_core::Code::StoreUnreachable {
        ReadFailure::TransportOnly
    } else {
        ReadFailure::Other(Failure::Unanswered)
    }
}
pub(crate) fn listed(reply: Result<BotResponse, Said>) -> Result<BotRoutineListPage, ReadFailure> {
    match reply {
        Ok(BotResponse::Routines { page })
            if page.items.len() <= 16
                && page.bot_revision > 0
                && page.bot_revision <= i64::MAX as u64
                && page
                    .items
                    .iter()
                    .all(|item| match &item.local_conversations {
                        BotRoutineLocalConversations::Available { items, .. } => items.len() <= 4,
                        _ => true,
                    }) =>
        {
            Ok(page)
        }
        // Even gateway_unavailable can wrap a renewal error here. Only the
        // separately typed initial discovery failure is positive transport proof.
        Err(said) => Err(ReadFailure::Other(Failure::of(said))),
        _ => Err(ReadFailure::Other(Failure::Unanswered)),
    }
}

#[derive(Default)]
pub(crate) struct Routines {
    pub detail: detail::Detail,
    bot: Option<BotId>,
    pub held: Option<Rc<BotRoutineListPage>>,
    pub notice: Option<Words>,
    pub stale: bool,
    pub revision: u64,
    pub expanded: Option<koru_delegate_core::ScheduleId>,
    visible: bool,
    seq: u64,
    epoch: u64,
    asking: Option<(u64, u64)>,
    owed: bool,
    after: Option<koru_delegate_core::ScheduleId>,
    opening: Option<(u64, ChatId)>,
    opened: Option<ChatId>,
}
impl Routines {
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn show(&mut self, bot: Option<BotId>, visible: bool) -> bool {
        if self.bot != bot {
            let detail = self.detail.retired();
            *self = Self {
                detail,
                bot,
                revision: self.revision.wrapping_add(1),
                seq: self.seq,
                epoch: self.epoch.wrapping_add(1),
                ..Self::default()
            };
        }
        let visible = visible && bot.is_some();
        if self.visible == visible {
            return false;
        }
        self.visible = visible;
        self.detail.select(self.bot, self.expanded, visible);
        self.epoch = self.epoch.wrapping_add(1);
        self.stale = self.held.is_some();
        self.opening = None;
        self.opened = None;
        if visible {
            self.owed = true;
            self.after = None;
        }
        self.changed();
        true
    }
    pub fn invalidate(&mut self) {
        self.detail = self.detail.retired();
        self.held = None;
        self.expanded = None;
        self.opening = None;
        self.opened = None;
        self.notice = Some(Words::BotsRoutinesUnavailable);
        self.epoch = self.epoch.wrapping_add(1);
        self.changed();
    }
    pub fn loading(&self) -> bool {
        self.asking.is_some() || self.owed
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    pub fn navigable(&self) -> bool {
        self.visible && !self.stale && !self.loading() && self.notice.is_none()
    }
    pub fn read(&mut self) -> Option<Command> {
        if !self.visible || !self.owed || self.asking.is_some() {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        self.asking = Some((self.seq, self.epoch));
        self.owed = false;
        Some(Command::ReadRoutines {
            seq: self.seq,
            bot: self.bot?,
            after: self.after,
        })
    }
    pub fn unsent(&mut self) {
        self.asking = None;
        self.notice = Some(Words::BotsRoutinesUnavailable);
        self.held = None;
        self.changed();
    }
    pub fn refresh(&mut self, next: bool) -> bool {
        if self.loading() || !self.visible {
            return false;
        }
        self.after = if next {
            self.held.as_ref().and_then(|p| p.next_schedule)
        } else {
            None
        };
        if next && self.after.is_none() {
            return false;
        }
        self.owed = true;
        self.expanded = None;
        self.detail = self.detail.retired();
        self.stale = self.held.is_some();
        self.notice = None;
        self.opening = None;
        self.opened = None;
        self.changed();
        true
    }
    pub fn answer(
        &mut self,
        seq: u64,
        bot: BotId,
        result: Result<BotRoutineListPage, ReadFailure>,
    ) -> bool {
        if self.bot != Some(bot) || self.asking.map(|v| v.0) != Some(seq) {
            return false;
        }
        let (_, epoch) = self.asking.take().unwrap();
        if !self.visible || epoch != self.epoch {
            return true;
        }
        self.opening = None;
        self.opened = None;
        self.expanded = None;
        self.detail = self.detail.retired();
        match result {
            Ok(page) if page.bot_id == bot => {
                self.held = Some(Rc::new(page));
                self.stale = false;
                self.notice = None;
            }
            Err(ReadFailure::TransportOnly) => {
                self.stale = self.held.is_some();
                self.notice = Some(Words::BotsRoutinesStale);
            }
            Err(ReadFailure::Other(Failure::Refused(code))) => {
                self.held = None;
                self.stale = false;
                self.notice = Some(super::settings::general_refusal_words(&code));
            }
            _ => {
                self.held = None;
                self.stale = false;
                self.notice = Some(Words::BotsRoutinesUnavailable);
            }
        }
        self.changed();
        true
    }
    pub fn toggle(&mut self, schedule: koru_delegate_core::ScheduleId) -> bool {
        if self
            .held
            .as_ref()
            .is_none_or(|p| !p.items.iter().any(|i| i.schedule_id == schedule))
        {
            return false;
        }
        self.expanded = if self.expanded == Some(schedule) {
            None
        } else {
            Some(schedule)
        };
        self.detail.select(self.bot, self.expanded, self.visible);
        self.changed();
        true
    }
    pub fn can_open(&self, chat: ChatId) -> bool {
        self.visible && self.expanded.is_some() && self.detail.can_open(chat)
    }
    pub fn detail_answer(
        &mut self,
        seq: u64,
        bot: BotId,
        schedule: koru_delegate_core::ScheduleId,
        result: Result<BotRoutineDetail, ReadFailure>,
    ) -> bool {
        if !self.detail.answer(seq, bot, schedule, result) {
            return false;
        }
        self.opening = None;
        self.opened = None;
        self.changed();
        true
    }
    pub fn detail_refresh(&mut self, next: bool) -> bool {
        if !self.detail.refresh(next) {
            return false;
        }
        self.opening = None;
        self.opened = None;
        self.changed();
        true
    }
    pub fn detail_unsent(&mut self) {
        self.detail.unsent();
        self.changed();
    }
    pub fn open(&mut self, chat: ChatId) -> Option<Command> {
        if !self.can_open(chat) || self.opening.is_some() {
            return None;
        }
        let seq = self.revision;
        self.opening = Some((seq, chat));
        Some(Command::RoutineLabel {
            seq,
            bot: self.bot?,
            chat,
        })
    }
    pub fn labelled(
        &mut self,
        seq: u64,
        bot: BotId,
        chat: ChatId,
        result: Result<Option<BotChatLabel>, Failure>,
    ) -> bool {
        if self.bot != Some(bot) || self.opening != Some((seq, chat)) {
            return false;
        }
        self.opening = None;
        // A late waiter cannot revoke or reopen a newer projection. The
        // original command still completed physically, but its view is gone.
        if seq != self.revision {
            return true;
        }
        if self.can_open(chat)
            && result
                .as_ref()
                .ok()
                .and_then(|v| v.as_ref())
                .is_some_and(|label| label.bot_id == bot)
        {
            self.opened = Some(chat);
        } else {
            // The label lane's Failure erases transport/authority provenance.
            // Missing, foreign and unanswered are therefore all fail-closed.
            self.invalidate();
            self.notice = Some(Words::BotsConversationUnavailable);
        }
        true
    }
    pub fn take_open(&mut self) -> Option<(BotId, ChatId)> {
        let chat = self.opened.take()?;
        self.can_open(chat).then(|| (self.bot.unwrap(), chat))
    }
}
