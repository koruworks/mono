// SPDX-License-Identifier: MPL-2.0
//! One selected page; hidden observations and stale bytes never authorize open.
use super::*;
use koru_ai_core::bots::routines::*;
use koru_delegate_core::ScheduleId;

#[derive(Default)]
pub(crate) struct Detail {
    pub selected: Option<(BotId, ScheduleId)>,
    pub held: Option<Rc<BotRoutineDetail>>,
    pub notice: Option<Words>,
    pub stale: bool,
    visible: bool,
    seq: u64,
    epoch: u64,
    asking: Option<(u64, u64)>,
    owed: bool,
    before: Option<i64>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Data {
    pub selected: Option<(BotId, ScheduleId)>,
    pub held: Option<Rc<BotRoutineDetail>>,
    pub notice: Option<Words>,
    pub stale: bool,
    pub loading: bool,
    pub navigable: bool,
}
impl Detail {
    pub fn retired(&self) -> Self {
        Self {
            seq: self.seq,
            epoch: self.epoch.wrapping_add(1),
            ..Self::default()
        }
    }
    pub fn select(&mut self, bot: Option<BotId>, schedule: Option<ScheduleId>, visible: bool) {
        let selected = bot.zip(schedule);
        if selected != self.selected {
            *self = Self {
                seq: self.seq,
                epoch: self.epoch.wrapping_add(1),
                selected,
                ..Self::default()
            };
        }
        if self.visible != visible {
            self.epoch = self.epoch.wrapping_add(1);
            self.visible = visible;
            self.asking = None;
            self.stale = self.held.is_some();
            if visible && self.selected.is_some() {
                self.owed = true;
            }
        }
        if selected.is_some() && self.held.is_none() && self.asking.is_none() && visible {
            self.owed = true;
        }
    }
    pub fn read(&mut self) -> Option<Command> {
        if !self.visible || !self.owed || self.asking.is_some() {
            return None;
        }
        let (bot, schedule) = self.selected?;
        self.seq = self.seq.wrapping_add(1);
        self.asking = Some((self.seq, self.epoch));
        self.owed = false;
        Some(Command::ReadRoutineDetail {
            seq: self.seq,
            bot,
            schedule,
            before: self.before,
        })
    }
    pub fn loading(&self) -> bool {
        self.asking.is_some() || self.owed
    }
    pub fn data(&self) -> Data {
        Data {
            selected: self.selected,
            held: self.held.clone(),
            notice: self.notice,
            stale: self.stale,
            loading: self.loading(),
            navigable: self.visible && !self.loading() && !self.stale && self.notice.is_none(),
        }
    }
    pub fn unsent(&mut self) {
        self.asking = None;
        self.held = None;
        self.notice = Some(Words::BotsRoutinesUnavailable);
    }
    pub fn refresh(&mut self, next: bool) -> bool {
        if !self.visible || self.selected.is_none() || self.loading() {
            return false;
        }
        let before = if next {
            match &self.held.as_ref().map(|d| &d.local_history) {
                Some(RoutineLocalHistoryObservation::Available {
                    next_before_occurrence_ms,
                    ..
                }) => *next_before_occurrence_ms,
                _ => None,
            }
        } else {
            None
        };
        if next && before.is_none() {
            return false;
        }
        self.before = before;
        self.owed = true;
        self.stale = self.held.is_some();
        self.notice = None;
        true
    }
    pub fn answer(
        &mut self,
        seq: u64,
        bot: BotId,
        schedule: ScheduleId,
        result: Result<BotRoutineDetail, ReadFailure>,
    ) -> bool {
        if self.selected != Some((bot, schedule)) || self.asking.map(|v| v.0) != Some(seq) {
            return false;
        }
        let (_, epoch) = self.asking.take().unwrap();
        if epoch != self.epoch || !self.visible {
            return false;
        }
        match result {
            Ok(detail) if detail.bot_id == bot && detail.schedule_id == schedule => {
                self.held = Some(Rc::new(detail));
                self.notice = None;
                self.stale = false;
            }
            Err(ReadFailure::TransportOnly) => {
                self.stale = self.held.is_some();
                self.notice = Some(Words::BotsRoutinesStale);
            }
            Err(ReadFailure::Other(Failure::Refused(code))) => {
                self.held = None;
                self.stale = false;
                self.notice = Some(super::super::settings::general_refusal_words(&code));
            }
            _ => {
                self.held = None;
                self.stale = false;
                self.notice = Some(Words::BotsRoutinesUnavailable);
            }
        }
        true
    }
    pub fn can_open(&self, chat: ChatId) -> bool {
        self.data().navigable
            && self
                .held
                .as_ref()
                .is_some_and(|detail| match &detail.local_history {
                    RoutineLocalHistoryObservation::Available { items, .. } => {
                        items.iter().any(|item| {
                            item.chat_id == chat.as_uuid()
                                && item.conversation == BotRoutineConversationVisibility::Available
                        })
                    }
                    _ => false,
                })
    }
}
pub(crate) fn detailed(reply: Result<BotResponse, Said>) -> Result<BotRoutineDetail, ReadFailure> {
    match reply {
        Ok(response @ BotResponse::RoutineDetail { .. }) => {
            validate_routine_detail_bytes(&response)
                .map_err(|_| ReadFailure::Other(Failure::Unanswered))?;
            let BotResponse::RoutineDetail { reply } = response else {
                unreachable!()
            };
            if reply.detail.bot_revision == 0
                || reply.detail.bot_revision > i64::MAX as u64
                || matches!(&reply.detail.local_history,RoutineLocalHistoryObservation::Available{items,..} if items.len()>16)
            {
                return Err(ReadFailure::Other(Failure::Unanswered));
            }
            Ok(reply.detail)
        }
        Err(said) => Err(ReadFailure::Other(Failure::of(said))),
        _ => Err(ReadFailure::Other(Failure::Unanswered)),
    }
}
