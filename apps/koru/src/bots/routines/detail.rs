// SPDX-License-Identifier: MPL-2.0
//! One selected page; hidden observations and stale bytes never authorize open.
use super::*;
use koru_delegate_core::ScheduleId;

#[derive(Default)]
pub(crate) struct Detail {
    pub snapshot: Option<Rc<koru_api_client::bot_routines::Snapshot>>,
    native_before: Option<koru_api_client::bot_routines::Cursor>,
    pub selected: Option<(BotId, ScheduleId)>,
    pub held: Option<Rc<BotRoutineDetail>>,
    pub notice: Option<Words>,
    pub stale: bool,
    visible: bool,
    seq: u64,
    epoch: u64,
    asking: Option<(u64, u64)>,
    owed: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Data {
    pub snapshot: Option<Rc<koru_api_client::bot_routines::Snapshot>>,
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
            native_before: self.native_before.clone(),
        })
    }
    pub fn loading(&self) -> bool {
        self.asking.is_some() || self.owed
    }
    /// Only the visible first page follows actual unfinished work or its next
    /// due instant. No hidden polling or document work runs on this clock.
    pub fn follow_up_at(&self) -> Option<i64> {
        if !self.visible || self.loading() || self.stale || self.native_before.is_some() {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let earliest = snapshot.detail.observed_at_ms.saturating_add(2_000);
        if snapshot
            .occurrences
            .iter()
            .any(|o| matches!(o.state.as_str(), "queued" | "running" | "needs_you"))
        {
            return Some(earliest);
        }
        match &snapshot.detail.live {
            RoutineDetailLive::Current {
                next: RoutineNextObservation::Exact { at_ms },
                ..
            } => Some(earliest.max(at_ms.saturating_add(1_000))),
            _ => None,
        }
    }
    pub fn data(&self) -> Data {
        Data {
            snapshot: self.snapshot.clone(),
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
        self.snapshot = None;
        self.notice = Some(Words::BotsRoutinesUnavailable);
    }
    pub fn refresh(&mut self, next: bool) -> bool {
        if !self.visible || self.selected.is_none() || self.loading() {
            return false;
        }

        let cursor = if next {
            self.snapshot.as_ref().and_then(|s| s.next.clone())
        } else {
            None
        };
        if next && cursor.is_none() {
            return false;
        }
        self.native_before = cursor;
        self.owed = true;
        self.stale = self.held.is_some();
        self.notice = None;
        true
    }
    /// The page of history before the oldest line shown.
    pub fn older(&mut self) -> bool {
        self.refresh(true)
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
        self.data().navigable && self.snapshot.as_ref().is_some_and(|s| s.thread == chat)
    }
    pub fn native_answer(
        &mut self,
        seq: u64,
        bot: BotId,
        schedule: ScheduleId,
        result: Result<koru_api_client::bot_routines::Snapshot, ReadFailure>,
    ) -> bool {
        let result = result.and_then(|snapshot| {
            if snapshot.detail.bot_id != bot
                || snapshot.detail.schedule_id != schedule
                || snapshot.thread.as_uuid().is_nil()
                || snapshot.occurrences.len() > 16
                || snapshot.occurrences.iter().any(|o| {
                    o.routine != schedule.as_uuid()
                        || o.preview.as_ref().is_some_and(|p| {
                            Some(p.message.as_uuid()) != o.reply
                                || p.markdown.len() > koru_ai_core::bots::BotReplyPreview::MAX_BYTES
                        })
                })
            {
                return Err(ReadFailure::Other(Failure::Unanswered));
            }
            Ok(snapshot)
        });
        let snapshot = result.as_ref().ok().cloned();
        let matched = self.answer(seq, bot, schedule, result.map(|s| s.detail));
        if matched {
            self.snapshot = if self.held.is_some() {
                snapshot.map(Rc::new)
            } else {
                None
            };
        }
        matched
    }
}
