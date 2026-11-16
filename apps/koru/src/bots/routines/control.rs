// SPDX-License-Identifier: MPL-2.0
//! **The owner controls of a Bot's Routines, as the pane holds them** (P4.3
//! piece 4): turning one on, pausing it, testing it, saving its timing and
//! name, and changing its access.
//!
//! **Nothing here is a state the owner has not confirmed.** A control is one
//! command to the owner and one answer. Until the answer arrives the control
//! is *pending* and cannot be pressed; the row keeps showing the state it
//! was read with. The answer's own state, revision and next time then
//! replace the row's, or the refusal is said in a sentence. An answer that
//! did not arrive is *Checking…*, and the list is read again.
//!
//! **One control at a time.** The owner holds one unresolved control per
//! Routine; the pane sends one at a time for the whole list, so no answer
//! can settle a control it was not sent for.
//!
//! **Hidden is cancelled.** Hiding the pane, or showing another Bot, drops
//! what was pending: its answer is ignored, and the list is read again when
//! the pane is next shown. No clock runs while it is hidden.
use super::*;
use koru_ai_core::bots::{BotAccess, BotCommand, BotRequestId};
use koru_delegate_core::{ScheduleId, SchedulePauseReason};
use koru_delegate_schedule::{ScheduleName, ScheduleState};
use koru_recurrence::RecurrenceRule;
use std::collections::HashMap;

/// One minute, the unit a Test claims.
const MINUTE_MS: i64 = 60_000;
/// How long after a Test's minute ended its history is read once more.
const TEST_SETTLE_MS: i64 = 2_000;

/// What the owner said instead of applying a control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub failure: Failure,
    /// `details.reasons` of `routine_not_ready`, in the owner's order.
    pub reasons: Vec<String>,
}

impl Refusal {
    /// No answer says what happened: the control may have been applied.
    pub(crate) fn unanswered() -> Self {
        Self {
            failure: Failure::Unanswered,
            reasons: Vec::new(),
        }
    }

    /// A refusal by its stable code, for a test standing in for the owner.
    #[cfg(test)]
    pub(crate) fn code(code: &str) -> Self {
        Self {
            failure: Failure::Refused(code.to_owned()),
            reasons: Vec::new(),
        }
    }

    /// `routine_not_ready` with its reasons.
    #[cfg(test)]
    pub(crate) fn not_ready(reasons: &[&str]) -> Self {
        Self {
            failure: Failure::Refused("routine_not_ready".to_owned()),
            reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
        }
    }
}

/// The control that is with the owner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Doing {
    TurningOn,
    Pausing,
    Testing,
    SavingTiming,
    SavingAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    seq: u64,
    schedule: ScheduleId,
    doing: Doing,
    /// The name an edit asked for: the reply does not repeat it.
    name: Option<ScheduleName>,
}

/// An accepted Test whose minute has not been seen in the history yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Testing {
    schedule: ScheduleId,
    minute_ms: i64,
    /// The one read after the minute ended has been asked for.
    asked: bool,
}

/// The access choices of one paused Routine, open in its detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccessForm {
    pub schedule: ScheduleId,
    pub access: BotAccess,
    /// The pages the current grant selects: saving sends none.
    pub selected_pages: u16,
    grant_revision: u64,
    /// One id for one attempt. Kept only to ask again after an answer that
    /// did not say the edit was refused.
    request: BotRequestId,
}

/// What a saved timing left for the dialog that asked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimingAnswer {
    Applied,
    /// The sentence to say in the dialog, which stays open with what the
    /// person typed.
    Problem(Words),
}

/// What the timing dialog is opened with: the selected Routine as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimingSeed {
    pub schedule: ScheduleId,
    pub name: ScheduleName,
    pub recurrence: RecurrenceRule,
    pub on: bool,
    pub zone: RoutineOwnerZoneObservation,
}

/// What a Restore or an Unarchive said of the Bot's Routines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Restored {
    /// Access can no longer be read: each stays paused until chosen again.
    pub needing_access: Vec<ScheduleId>,
    /// Not confirmed paused by an operation that stopped early.
    pub still_on: Vec<ScheduleId>,
}

impl Restored {
    pub(crate) fn of(progress: &koru_ai_core::bots::BotLifecycleProgress) -> Self {
        Self {
            needing_access: progress.routines_needing_access.clone(),
            still_on: progress.routines_still_on.clone(),
        }
    }

    /// Whether a person has something to look at before resuming the Bot.
    pub(crate) fn needs_a_look(&self) -> bool {
        !self.needing_access.is_empty() || !self.still_on.is_empty()
    }
}

/// What the pane draws of the controls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Data {
    pub pending: Option<(ScheduleId, Doing)>,
    pub checking: Option<ScheduleId>,
    pub notes: Vec<(ScheduleId, Vec<Words>)>,
    /// A Test that waits for the next minute: the Routine and that minute.
    pub test_wait: Option<(ScheduleId, i64)>,
    pub access: Option<AccessForm>,
    pub restored: Option<Restored>,
}

impl Data {
    pub(crate) fn doing(&self, schedule: ScheduleId) -> Option<Doing> {
        self.pending
            .filter(|(pending, _)| *pending == schedule)
            .map(|(_, doing)| doing)
    }
    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub(crate) fn notes(&self, schedule: ScheduleId) -> &[Words] {
        self.notes
            .iter()
            .find(|(noted, _)| *noted == schedule)
            .map_or(&[], |(_, notes)| notes.as_slice())
    }
}

#[derive(Default)]
pub(crate) struct Controls {
    seq: u64,
    pending: Option<Pending>,
    checking: Option<ScheduleId>,
    notes: HashMap<ScheduleId, Vec<Words>>,
    test_wait: Option<(ScheduleId, i64)>,
    testing: Option<Testing>,
    access: Option<AccessForm>,
    timing: Option<TimingAnswer>,
    restored: Option<Restored>,
    /// The conversation a row's *Open to decide* asked for.
    pub(super) deciding: Option<ChatId>,
    /// The list read on its way is the pane's own re-read.
    keep_open: bool,
}

impl Controls {
    /// The same pane shown for another Bot, or shown again: nothing pending
    /// survives, and no old answer can match a new control.
    pub(super) fn retired(&self, keep_restored: bool) -> Self {
        Self {
            seq: self.seq,
            restored: self.restored.clone().filter(|_| keep_restored),
            ..Self::default()
        }
    }
    pub(super) fn listed(&mut self) {
        self.checking = None;
    }
    /// The Routine a form was open on is no longer the selected one.
    pub(super) fn close_forms(&mut self) {
        self.access = None;
    }
    /// Whether the list read now answering was the pane's own, which keeps
    /// the open Routine open. Taken once.
    pub(super) fn take_keep_open(&mut self) -> bool {
        std::mem::take(&mut self.keep_open)
    }
    pub(super) fn refreshed(&mut self) {
        self.notes.clear();
        self.restored = None;
    }
    fn note(&mut self, schedule: ScheduleId, words: Vec<Words>) {
        if words.is_empty() {
            self.notes.remove(&schedule);
        } else {
            self.notes.insert(schedule, words);
        }
    }
}

/// Whether a state is the pause an owner control writes.
fn paused_by_person(state: &ScheduleState) -> bool {
    matches!(
        state,
        ScheduleState::Paused {
            reason: SchedulePauseReason::Manual,
            ..
        }
    )
}

/// The start of the minute after `now_ms`: when a Test can be asked again.
pub(crate) fn next_minute(now_ms: i64) -> i64 {
    (now_ms.div_euclid(MINUTE_MS) + 1) * MINUTE_MS
}

impl Routines {
    fn row(&self, schedule: ScheduleId) -> Option<&BotRoutineListItem> {
        self.held
            .as_ref()?
            .items
            .iter()
            .find(|item| item.schedule_id == schedule)
    }

    /// The row's Routine as last confirmed: its revision and state.
    fn confirmed(&self, schedule: ScheduleId) -> Option<(u32, &ScheduleState)> {
        match &self.row(schedule)?.live {
            BotRoutineLiveObservation::Current {
                definition_revision,
                state,
                ..
            } => Some((*definition_revision, state)),
            _ => None,
        }
    }

    /// Whether a new control may be sent now: the list is shown, fresh and
    /// not being read, and no other control is with the owner.
    fn may_control(&self) -> bool {
        self.visible && !self.stale && !self.loading() && self.controls.pending.is_none()
    }

    pub(crate) fn control_busy(&self) -> bool {
        self.controls.pending.is_some()
    }

    pub(crate) fn controls_data(&self) -> Data {
        let mut notes: Vec<_> = self
            .controls
            .notes
            .iter()
            .map(|(schedule, notes)| (*schedule, notes.clone()))
            .collect();
        notes.sort_by_key(|(schedule, _)| schedule.as_uuid());
        Data {
            pending: self
                .controls
                .pending
                .as_ref()
                .map(|pending| (pending.schedule, pending.doing)),
            checking: self.controls.checking,
            notes,
            test_wait: self.controls.test_wait,
            access: self.controls.access.clone(),
            restored: self.controls.restored.clone(),
        }
    }

    fn send(
        &mut self,
        schedule: ScheduleId,
        doing: Doing,
        name: Option<ScheduleName>,
        command: BotCommand,
    ) -> Option<Command> {
        let bot = self.bot?;
        self.controls.seq = self.controls.seq.wrapping_add(1);
        self.controls.pending = Some(Pending {
            seq: self.controls.seq,
            schedule,
            doing,
            name,
        });
        self.controls.notes.remove(&schedule);
        self.changed();
        Some(Command::RoutineControl {
            seq: self.controls.seq,
            bot,
            schedule,
            command: Box::new(command),
        })
    }

    /// **The row's switch was pressed**: Pause a Routine that is on, turn on
    /// one that is paused. `None` when nothing may be sent now.
    pub(crate) fn toggle_routine(&mut self, schedule: ScheduleId) -> Option<Command> {
        if !self.may_control() {
            return None;
        }
        let (revision, state) = self.confirmed(schedule)?;
        let (action, doing) = match state {
            ScheduleState::Active { .. } => (BotRoutineLifecycleAction::Pause, Doing::Pausing),
            ScheduleState::Paused { .. } => (BotRoutineLifecycleAction::Enable, Doing::TurningOn),
            _ => return None,
        };
        let command = BotCommand::SetRoutineLifecycle {
            bot_id: self.bot?,
            schedule_id: schedule,
            expected_definition_revision: revision,
            action,
        };
        self.send(schedule, doing, None, command)
    }

    /// Whether Test is offered for the row: paused by a person, and nothing
    /// waiting. A Routine that is on is never offered one.
    pub(crate) fn testable(&self, schedule: ScheduleId) -> bool {
        self.confirmed(schedule)
            .is_some_and(|(_, state)| paused_by_person(state))
    }

    /// **Test was pressed**: one Run of a paused Routine now.
    pub(crate) fn test_routine(&mut self, schedule: ScheduleId) -> Option<Command> {
        if !self.may_control()
            || !self.testable(schedule)
            || self
                .controls
                .test_wait
                .is_some_and(|(waiting, _)| waiting == schedule)
        {
            return None;
        }
        let (revision, _) = self.confirmed(schedule)?;
        let command = BotCommand::TestRoutine {
            bot_id: self.bot?,
            schedule_id: schedule,
            expected_definition_revision: revision,
        };
        self.send(schedule, Doing::Testing, None, command)
    }

    /// The selected Routine as last read, when its timing can be edited: its
    /// detail is fresh and it is live.
    pub(crate) fn timing_seed(&self) -> Option<TimingSeed> {
        let detail = self.detail.data();
        let held = detail.held.as_ref()?;
        if !self.visible || detail.loading || detail.stale || detail.notice.is_some() {
            return None;
        }
        match &held.live {
            RoutineDetailLive::Current {
                name,
                recurrence,
                state,
                owner_zone,
                ..
            } if matches!(
                state,
                ScheduleState::Active { .. } | ScheduleState::Paused { .. }
            ) =>
            {
                Some(TimingSeed {
                    schedule: held.schedule_id,
                    name: name.clone(),
                    recurrence: recurrence.clone(),
                    on: matches!(state, ScheduleState::Active { .. }),
                    zone: owner_zone.clone(),
                })
            }
            _ => None,
        }
    }

    /// **Save in the timing dialog**: one command with the name and the
    /// timing, against the revision and name the detail was last read with.
    pub(crate) fn save_timing(
        &mut self,
        schedule: ScheduleId,
        name: ScheduleName,
        recurrence: RecurrenceRule,
    ) -> Result<Command, Words> {
        if self.controls.pending.is_some() || !self.visible {
            return Err(Words::BotsRoutineErrPending);
        }
        let held = self
            .detail
            .held
            .clone()
            .filter(|held| held.schedule_id == schedule && !self.detail.loading())
            .ok_or(Words::BotsRoutineErrPending)?;
        let RoutineDetailLive::Current {
            name: expected_name,
            definition_revision,
            ..
        } = &held.live
        else {
            return Err(Words::BotsRoutineErrDeleted);
        };
        let command = BotCommand::EditRoutine {
            bot_id: self.bot.ok_or(Words::BotsRoutineErrGone)?,
            schedule_id: schedule,
            expected_definition_revision: *definition_revision,
            expected_name: expected_name.clone(),
            name: name.clone(),
            recurrence,
        };
        self.send(schedule, Doing::SavingTiming, Some(name), command)
            .ok_or(Words::BotsRoutineErrGone)
    }

    /// What the last save left for the timing dialog, taken once.
    pub(crate) fn take_timing_answer(&mut self) -> Option<TimingAnswer> {
        self.controls.timing.take()
    }

    /// Whether the selected Routine's access may be changed now: it is the
    /// one whose detail is read, and it is paused.
    pub(crate) fn access_changeable(&self) -> bool {
        let detail = self.detail.data();
        detail.held.as_ref().is_some_and(|held| {
            !detail.loading
                && !detail.stale
                && matches!(
                    &held.live,
                    RoutineDetailLive::Current {
                        state: ScheduleState::Paused { .. },
                        ..
                    }
                )
        })
    }

    /// **Change access was pressed**: open the choices on the access the
    /// Routine has now.
    pub(crate) fn open_access(&mut self) -> bool {
        if !self.access_changeable() || !self.may_control() {
            return false;
        }
        let Some(held) = self.detail.held.clone() else {
            return false;
        };
        self.controls.access = Some(AccessForm {
            schedule: held.schedule_id,
            access: held.access.access,
            selected_pages: held.access.selected_page_count,
            grant_revision: held.access.grant_revision,
            request: BotRequestId::mint(),
        });
        self.controls.notes.remove(&held.schedule_id);
        self.changed();
        true
    }

    pub(crate) fn choose_access(&mut self, access: BotAccess) -> bool {
        let Some(form) = self.controls.access.as_mut() else {
            return false;
        };
        if form.access == access {
            return false;
        }
        form.access = access;
        // Another choice is another attempt.
        form.request = BotRequestId::mint();
        self.changed();
        true
    }

    pub(crate) fn cancel_access(&mut self) -> bool {
        let closed = self.controls.access.take().is_some();
        if closed {
            self.changed();
        }
        closed
    }

    /// **Save in the access choices**: one command. The two approved choices
    /// select no page of their own.
    pub(crate) fn save_access(&mut self) -> Option<Command> {
        if !self.may_control() {
            return None;
        }
        let form = self.controls.access.clone()?;
        let command = BotCommand::UpdateRoutineAccess {
            request_id: form.request,
            bot_id: self.bot?,
            schedule_id: form.schedule,
            expected_grant_revision: form.grant_revision,
            access: form.access,
            selected_pages: Vec::new(),
        };
        self.send(form.schedule, Doing::SavingAccess, None, command)
    }

    /// The link took no command: nothing was sent, and nothing is pending.
    pub(crate) fn control_unsent(&mut self) {
        if let Some(pending) = self.controls.pending.take() {
            if pending.doing == Doing::SavingTiming {
                self.controls.timing =
                    Some(TimingAnswer::Problem(Words::BotsRoutineErrUnavailable));
            }
            self.controls
                .note(pending.schedule, vec![Words::BotsRoutineErrUnavailable]);
        }
        self.changed();
    }

    /// Read the list again, keeping what is shown until it answers.
    fn reread(&mut self) {
        if self.visible {
            self.owed = true;
            self.stale = self.held.is_some();
            self.controls.keep_open = true;
        }
    }

    /// **The owner's answer to the pending control.** `false` when it is not
    /// this pane's, or not the control that is pending: nothing changes.
    pub(crate) fn control_answer(
        &mut self,
        seq: u64,
        bot: BotId,
        schedule: ScheduleId,
        result: Result<BotResponse, Refusal>,
        now_ms: i64,
    ) -> bool {
        if self.bot != Some(bot)
            || self
                .controls
                .pending
                .as_ref()
                .is_none_or(|pending| pending.seq != seq || pending.schedule != schedule)
        {
            return false;
        }
        let pending = self.controls.pending.take().expect("checked above");
        match result {
            Ok(BotResponse::RoutineControl { control })
                if control.schedule_id == schedule
                    && matches!(
                        (pending.doing, control.action),
                        (Doing::TurningOn, BotRoutineControlAction::Enable)
                            | (Doing::Pausing, BotRoutineControlAction::Pause)
                            | (Doing::SavingTiming, BotRoutineControlAction::Edit)
                    ) =>
            {
                self.confirm(schedule, &control, pending.name);
                if pending.doing == Doing::SavingTiming {
                    self.controls.timing = Some(TimingAnswer::Applied);
                }
            }
            Ok(BotResponse::RoutineTest { test })
                if pending.doing == Doing::Testing && test.schedule_id == schedule =>
            {
                self.controls.testing = Some(Testing {
                    schedule,
                    minute_ms: test.occurrence_ms,
                    asked: false,
                });
                // Its history is where the result is: show it.
                if self.expanded == Some(schedule) {
                    self.detail.refresh(false);
                } else {
                    self.expanded = Some(schedule);
                    self.detail.select(self.bot, self.expanded, self.visible);
                }
            }
            Ok(BotResponse::RoutineAccess { access })
                if pending.doing == Doing::SavingAccess && access.schedule_id == schedule =>
            {
                self.controls.access = None;
                if let Some(restored) = self.controls.restored.as_mut() {
                    restored.needing_access.retain(|id| *id != schedule);
                }
                if let Some(page) = self.held.as_mut() {
                    if let Some(item) = Rc::make_mut(page)
                        .items
                        .iter_mut()
                        .find(|item| item.schedule_id == schedule)
                    {
                        item.grant.grant_revision = access.grant_revision;
                        item.grant.access = access.access;
                        item.grant.selected_page_count = access.selected_page_count;
                    }
                }
                self.detail.refresh(false);
            }
            // An answer to some other question says nothing of this one.
            Ok(_) => self.unknown(schedule, pending.doing),
            Err(refusal) => self.refused(schedule, pending.doing, refusal, now_ms),
        }
        self.changed();
        true
    }

    /// The owner confirmed a state: the row shows exactly it.
    fn confirm(
        &mut self,
        schedule: ScheduleId,
        control: &BotRoutineControl,
        name: Option<ScheduleName>,
    ) {
        if let Some(page) = self.held.as_mut() {
            if let Some(item) = Rc::make_mut(page)
                .items
                .iter_mut()
                .find(|item| item.schedule_id == schedule)
            {
                let name = name.or_else(|| match &item.live {
                    BotRoutineLiveObservation::Current { name, .. } => Some(name.clone()),
                    _ => None,
                });
                if let Some(name) = name {
                    item.live = BotRoutineLiveObservation::Current {
                        name,
                        definition_revision: control.definition_revision,
                        state: control.state.clone(),
                        next: control.next.clone(),
                    };
                }
            }
        }
        // The selected detail holds the older revision: read it again.
        if self.expanded == Some(schedule) {
            self.detail.refresh(false);
        }
        // A Routine that was turned on has no Test and no access form.
        if matches!(control.state, ScheduleState::Active { .. }) {
            if self
                .controls
                .access
                .as_ref()
                .is_some_and(|form| form.schedule == schedule)
            {
                self.controls.access = None;
            }
            if self
                .controls
                .test_wait
                .is_some_and(|(waiting, _)| waiting == schedule)
            {
                self.controls.test_wait = None;
            }
        }
    }

    /// No answer says what happened: say so, and read what is true.
    fn unknown(&mut self, schedule: ScheduleId, doing: Doing) {
        self.controls.checking = Some(schedule);
        if doing == Doing::SavingTiming {
            self.controls.timing = Some(TimingAnswer::Problem(Words::BotsRoutineChecking));
        }
        self.reread();
    }

    fn refused(&mut self, schedule: ScheduleId, doing: Doing, refusal: Refusal, now_ms: i64) {
        let code = match &refusal.failure {
            Failure::Unanswered => return self.unknown(schedule, doing),
            Failure::Refused(code) => code.as_str(),
        };
        let words = match code {
            "routine_control_unknown" => return self.unknown(schedule, doing),
            // Already in the asked state: no sentence, the read state shows.
            "routine_state_conflict" => {
                self.reread();
                Vec::new()
            }
            "revision_conflict" => {
                self.reread();
                if self.expanded == Some(schedule) {
                    self.detail.refresh(false);
                }
                vec![Words::BotsRoutineErrChanged]
            }
            "routine_not_ready" => not_ready_words(&refusal.reasons),
            // A wait the pane knows the length of, not an error.
            "routine_test_minute_ending" | "routine_test_too_soon" if doing == Doing::Testing => {
                self.controls.test_wait = Some((schedule, next_minute(now_ms)));
                Vec::new()
            }
            "routine_must_be_paused" if doing == Doing::SavingAccess => {
                self.reread();
                vec![Words::BotsRoutineErrPauseForAccess]
            }
            "wrong_owner" if doing == Doing::SavingAccess => vec![Words::BotsRoutineErrPageRights],
            "invalid_request" if doing == Doing::SavingTiming => {
                vec![Words::BotsRoutineInvalidTime]
            }
            code => vec![refusal_words(code)],
        };
        // A new attempt of an access edit is a new request, unless the
        // answer left the same one open to ask again.
        if doing == Doing::SavingAccess
            && !matches!(code, "routine_unavailable" | "routine_control_pending")
            && let Some(form) = self.controls.access.as_mut()
        {
            form.request = BotRequestId::mint();
        }
        if doing == Doing::SavingTiming {
            self.controls.timing = Some(TimingAnswer::Problem(
                words
                    .first()
                    .copied()
                    .unwrap_or(Words::BotsRoutineErrChanged),
            ));
        }
        self.controls.note(schedule, words);
    }

    /// The moment the pane next has something to do by the clock, in epoch
    /// milliseconds: a Test's wait ending, or its minute ending. `None`
    /// while hidden, and while nothing waits.
    pub(crate) fn control_deadline(&self) -> Option<i64> {
        if !self.visible {
            return None;
        }
        let wait = self.controls.test_wait.map(|(_, at)| at);
        let test = self
            .controls
            .testing
            .filter(|test| !test.asked)
            .map(|test| test.minute_ms + MINUTE_MS + TEST_SETTLE_MS);
        [wait, test].into_iter().flatten().min()
    }

    /// The clock reached `now_ms`. `true` when anything the pane draws
    /// changed. Starts nothing by itself: a Test is pressed by a person.
    pub(crate) fn control_tick(&mut self, now_ms: i64) -> bool {
        if !self.visible {
            return false;
        }
        let mut changed = false;
        if self.controls.test_wait.is_some_and(|(_, at)| now_ms >= at) {
            self.controls.test_wait = None;
            changed = true;
        }
        if let Some(test) = self.controls.testing
            && !test.asked
            && now_ms >= test.minute_ms + MINUTE_MS + TEST_SETTLE_MS
        {
            if self.detail.selected.map(|(_, selected)| selected) == Some(test.schedule) {
                // One read after the minute: its line is there, or the Test
                // never started.
                self.controls.testing = Some(Testing {
                    asked: true,
                    ..test
                });
                if !self.detail.refresh(false) && !self.detail.loading() {
                    self.settle_test(false);
                }
            } else {
                // The person looked elsewhere: nothing is claimed either way.
                self.controls.testing = None;
            }
            changed = true;
        }
        if changed {
            self.changed();
        }
        changed
    }

    /// The selected detail answered: a Test's minute is in its history, or,
    /// after the minute ended, the Test did not start.
    pub(super) fn settle_test(&mut self, read: bool) {
        let Some(test) = self.controls.testing else {
            return;
        };
        if self.detail.selected.map(|(_, selected)| selected) != Some(test.schedule) {
            return;
        }
        let seen = self.detail.held.as_ref().is_some_and(|held| {
            matches!(&held.occurrences, RoutineOccurrenceHistoryObservation::Available { items, .. }
                if items.iter().any(|line| line.occurrence_ms == test.minute_ms))
        });
        if seen {
            self.controls.testing = None;
        } else if test.asked {
            self.controls.testing = None;
            // A history that could not be read says nothing of the Test.
            if read || self.detail.held.is_some() {
                self.controls
                    .note(test.schedule, vec![Words::BotsRoutineTestNotStarted]);
            }
        }
    }

    /// **A Restore or an Unarchive of this Bot finished**: what it said of
    /// the Routines is shown until the person refreshes the list.
    pub(crate) fn note_restored(&mut self, restored: Restored) {
        self.controls.restored = Some(restored);
        if self.visible {
            self.reread();
        }
        self.changed();
    }

    /// **A row's *Open to decide***: the newest conversation of the Routine
    /// that the list read says is here, through the same checked open a
    /// history line uses.
    pub(crate) fn open_decision(&mut self, chat: ChatId) -> Option<Command> {
        if !self.visible || self.stale || self.loading() || !self.row_holds(chat) {
            return None;
        }
        self.controls.deciding = Some(chat);
        let command = self.open(chat);
        if command.is_none() {
            self.controls.deciding = None;
        }
        command
    }

    /// Whether a fresh row lists `chat` as a conversation held here.
    pub(super) fn row_holds(&self, chat: ChatId) -> bool {
        self.held.as_ref().is_some_and(|page| {
            page.items
                .iter()
                .any(|item| match &item.local_conversations {
                    BotRoutineLocalConversations::Available { items, .. } => {
                        items.iter().any(|row| {
                            row.chat_id == chat.as_uuid()
                                && row.conversation == BotRoutineConversationVisibility::Available
                        })
                    }
                    _ => false,
                })
        })
    }
}

/// **A Routine control's refusal, in a sentence a person can act on**, by
/// its stable code. No code is ever shown.
pub(crate) fn refusal_words(code: &str) -> Words {
    match code {
        "not_found" => Words::BotsRoutineErrGone,
        "wrong_owner" | "forbidden" => Words::BotsRoutineErrWrongOwner,
        "workspace_unavailable" => Words::BotsRoutineErrWorkspace,
        "bot_trashed" => Words::BotsRoutineErrBotTrashed,
        "bot_lifecycle_pending" => Words::BotsRoutineErrLifecyclePending,
        "bot_archived" => Words::BotsRoutineErrBotArchived,
        "bot_operation_pending" => Words::BotsRoutineErrOperationPending,
        "revision_conflict" => Words::BotsRoutineErrChanged,
        "routine_not_bound" => Words::BotsRoutineErrNotBound,
        "routine_not_live" => Words::BotsRoutineErrDeleted,
        "routine_owner_elsewhere" => Words::BotsRoutineErrElsewhere,
        "routine_control_pending" => Words::BotsRoutineErrPending,
        "routine_control_unknown" => Words::BotsRoutineChecking,
        "routine_state_conflict" => Words::BotsRoutineErrChanged,
        "routine_unavailable" | "ai_worker_saturated" | "storage_error" => {
            Words::BotsRoutineErrUnavailable
        }
        "routine_not_ready" => Words::BotsRoutineErrNotReady,
        "routine_must_be_paused" => Words::BotsRoutineErrPauseToTest,
        // Shown as a wait on the button; said only where there is no button.
        "routine_test_minute_ending" | "routine_test_too_soon" | "routine_test_busy" => {
            Words::BotsRoutineErrTestBusy
        }
        "routine_test_minute_taken" => Words::BotsRoutineErrMinuteTaken,
        "routine_run_in_progress" => Words::BotsRoutineErrRunInProgress,
        "routine_access_pending" => Words::BotsRoutineErrAccessPending,
        _ => Words::BotsRoutineErrOther,
    }
}

/// Every Routine code the owner can answer (`koru_ai_wire::bot_routine_codes`).
#[cfg(test)]
pub(crate) const ROUTINE_CODES: &[&str] = &[
    "routine_not_bound",
    "routine_not_live",
    "routine_owner_elsewhere",
    "routine_control_pending",
    "routine_control_unknown",
    "routine_state_conflict",
    "routine_unavailable",
    "routine_not_ready",
    "routine_must_be_paused",
    "routine_test_minute_ending",
    "routine_test_too_soon",
    "routine_test_minute_taken",
    "routine_test_busy",
    "routine_run_in_progress",
    "routine_access_pending",
];

/// One sentence per reason of `routine_not_ready`, in the owner's order. A
/// reason this build does not know, or none, is one plain sentence.
pub(crate) fn not_ready_words(reasons: &[String]) -> Vec<Words> {
    let mut words: Vec<Words> = reasons
        .iter()
        .filter_map(|reason| {
            Some(match reason.as_str() {
                "bot_paused" => Words::BotsRoutineErrBotPaused,
                "bot_archived" => Words::BotsRoutineErrBotIsArchived,
                "creation_not_bound" => Words::BotsRoutineErrNotBound,
                "access_unavailable" => Words::BotsRoutineErrAccessUnreadable,
                "access_pending" => Words::BotsRoutineErrAccessApplying,
                _ => return None,
            })
        })
        .collect();
    words.dedup();
    if words.is_empty() {
        words.push(Words::BotsRoutineErrNotReady);
    }
    words
}

#[cfg(test)]
mod tests;
