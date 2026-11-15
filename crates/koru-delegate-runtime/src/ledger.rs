// SPDX-License-Identifier: MPL-2.0

//! `runs.db` as the [`RunStore`] port, so a host composes without an
//! adapter of its own. Every method is the ledger's own; nothing is added
//! or reinterpreted here.
//!
//! **Five of them also move a counter** ([`crate::counters`]). This is the
//! seam every consequential thing a run does passes through, because §7.1
//! makes it durable before it is anything else, so counting here counts
//! what happened rather than what was attempted — the number rises with
//! the committed write and not before it.

use koru_delegate_core::{
    ArtifactDisposition, AttemptGeneration, DelegationRequest, DocumentId, EventId, HostInstanceId,
    ProviderSessionKey, PublicationKind, RunId, WorkspaceId,
};
use koru_delegate_schedule::ScheduleFiringOutcome;
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, ClaimedSlot, DeletionRecord, IntentFacts, IntentRecord, LedgerError,
    OpenPublication, PendingIntent, Projection, RunsDb, SkippedSlot, SlotClaim, SlotOutcome,
    WriteOutcome,
};

use crate::{RunStore, ScheduleLedger, counters};
use koru_delegate_document::ScheduleClaimAuthority;

impl crate::RoutineLedger for RunsDb {
    fn routine_firing(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<Option<koru_delegate_sqlite::SlotClaim>, koru_delegate_sqlite::BotLedgerError> {
        RunsDb::routine_firing(self, slot)
    }
    fn routine_route(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<
        Option<koru_delegate_core::RoutineRouteIdentity>,
        koru_delegate_sqlite::BotLedgerError,
    > {
        RunsDb::routine_route(self, schedule).map_err(Into::into)
    }

    fn held_routine_page(
        &self,
        after: Option<koru_delegate_sqlite::RoutineHeldCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineHeldCursor,
            koru_delegate_sqlite::RoutineRecoveryEntry,
        >,
        koru_delegate_sqlite::BotLedgerError,
    > {
        RunsDb::held_routine_page(self, after)
    }

    fn queued_routine_page(
        &self,
        after: Option<koru_delegate_sqlite::RoutineQueuedCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineQueuedCursor,
            Option<koru_delegate_sqlite::RoutineRecoveryEntry>,
        >,
        koru_delegate_sqlite::BotLedgerError,
    > {
        RunsDb::queued_routine_page(self, after)
    }

    fn routine_slots_page(
        &self,
        route: &koru_delegate_core::RoutineRouteIdentity,
        after: Option<koru_delegate_sqlite::RoutineSlotCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineSlotCursor,
            koru_delegate_sqlite::RoutineOccurrenceFact,
        >,
        koru_delegate_sqlite::BotLedgerError,
    > {
        RunsDb::routine_slots_page(self, route, after)
    }
    fn record_expired_routine(
        &mut self,
        authority: koru_delegate_document::RoutineExpiryAuthority,
    ) -> Result<
        (
            koru_delegate_core::RoutineClaimObservation,
            Option<koru_delegate_core::RoutineSkippedClaim>,
        ),
        koru_delegate_sqlite::BotLedgerError,
    > {
        // Sole production conversion; the host holds its writer mutex, and no
        // external call lies between live consumption and the transaction.
        let facts = authority
            .into_expired_bot_routine()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        RunsDb::record_expired_routine(self, facts)
    }
    fn claim_bot_routine(
        &mut self,
        authority: ScheduleClaimAuthority,
        admission: &koru_delegate_core::BotAdmissionBarrier,
        fresh: &koru_delegate_core::FreshBotPolicyProof,
    ) -> Result<
        (
            koru_delegate_core::RoutineClaimObservation,
            Option<koru_delegate_core::RoutineSkippedClaim>,
        ),
        koru_delegate_sqlite::BotLedgerError,
    > {
        // This is the sole production conversion into neutral claim facts. The
        // host already holds its writer mutex. No await or external port lies
        // between consuming the actual live token and the ledger transaction.
        let facts = authority
            .into_bot_routine()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        RunsDb::claim_bot_routine(self, facts, admission, fresh)
    }
    fn read_routine_claim(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<koru_delegate_core::RoutineClaimObservation, koru_delegate_sqlite::BotLedgerError>
    {
        RunsDb::read_routine_claim(self, slot)
    }
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &koru_delegate_core::RoutineLinkAcknowledgement,
    ) -> Result<koru_delegate_core::RoutineClaimRecord, koru_delegate_sqlite::BotLedgerError> {
        RunsDb::acknowledge_routine_link(self, acknowledgement)
    }
}

impl RunStore for RunsDb {
    fn decision_projection_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<RunId>, LedgerError> {
        RunsDb::decision_projection_runs(self, after, limit)
    }
    fn pending_publication_run_anchor(
        &self,
    ) -> Result<Option<koru_delegate_sqlite::PendingPublicationRunCursor>, LedgerError> {
        RunsDb::pending_publication_run_anchor(self)
    }
    fn next_pending_publication_run(
        &self,
        after: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
        through: koru_delegate_sqlite::PendingPublicationRunCursor,
        before_or_at: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
    ) -> Result<koru_delegate_sqlite::PendingPublicationRunSeek, LedgerError> {
        RunsDb::next_pending_publication_run(self, after, through, before_or_at)
    }
    fn decision_open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        RunsDb::decision_open_publications_for(self, run, limit)
    }
    fn native_results(&mut self) -> Option<&mut dyn koru_delegate_sqlite::NativeResultLedger> {
        Some(self)
    }
    fn bots(&mut self) -> Option<&mut dyn koru_delegate_sqlite::BotLedger> {
        Some(self)
    }

    fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError> {
        RunsDb::validate_intent(self, request, workspace)
    }

    fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError> {
        let outcome = RunsDb::record_intent(self, request, workspace)?;
        if outcome == WriteOutcome::Applied {
            counters::intent_recorded();
        }
        Ok(outcome)
    }

    fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError> {
        RunsDb::satisfy_intent(self, run, run_document)
    }

    fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        RunsDb::abandon_intent(self, run)
    }

    fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError> {
        RunsDb::pending_intents(self)
    }

    fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError> {
        RunsDb::intent(self, run)
    }

    fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError> {
        RunsDb::intents(self)
    }

    fn intent_facts(&self, run: RunId) -> Result<Option<IntentFacts>, LedgerError> {
        RunsDb::intent_facts(self, run)
    }

    fn record_deletion(
        &mut self,
        run: RunId,
        artifacts: ArtifactDisposition,
    ) -> Result<WriteOutcome, LedgerError> {
        RunsDb::record_deletion(self, run, artifacts)
    }

    fn clear_deletion(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        RunsDb::clear_deletion(self, run)
    }

    fn deletion(&self, run: RunId) -> Result<Option<DeletionRecord>, LedgerError> {
        RunsDb::deletion(self, run)
    }

    fn forget_run(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        RunsDb::forget_run(self, run)
    }

    fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError> {
        let row = RunsDb::open_attempt(self, run)?;
        counters::attempt_opened();
        Ok(row)
    }

    fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError> {
        RunsDb::advance(self, run, generation, phase)
    }

    fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError> {
        RunsDb::bind_session(self, run, generation, session, host)
    }

    fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError> {
        RunsDb::retire(self, run, generation)
    }

    fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError> {
        RunsDb::current_attempt(self, run)
    }

    fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError> {
        RunsDb::live_attempts(self)
    }

    fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError> {
        let outcome = RunsDb::enqueue(self, event, run, kind, body_json)?;
        if outcome == WriteOutcome::Applied {
            counters::publication_queued(kind);
        }
        Ok(outcome)
    }

    fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError> {
        let outcome = RunsDb::acknowledge(self, event)?;
        if outcome == WriteOutcome::Applied {
            counters::publication_acknowledged();
        }
        Ok(outcome)
    }

    fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError> {
        let attempts = RunsDb::bump_attempts(self, event)?;
        counters::publication_retried();
        Ok(attempts)
    }

    fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError> {
        RunsDb::open_publications(self, limit)
    }

    fn open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        RunsDb::open_publications_for(self, Some(run), limit)
    }

    fn publication(
        &self,
        event: EventId,
    ) -> Result<Option<koru_delegate_sqlite::PublicationRecord>, LedgerError> {
        RunsDb::publication(self, event)
    }

    fn advance_cursor(
        &mut self,
        run: RunId,
        projection: Projection,
        timeline_index: u64,
    ) -> Result<WriteOutcome, LedgerError> {
        RunsDb::advance_cursor(self, run, projection, timeline_index)
    }
}

impl ScheduleLedger for RunsDb {
    fn claim_run(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<ClaimedSlot, LedgerError> {
        let claim = authority
            .into_run()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        self.claim_scheduled_run_at(
            claim.schedule,
            claim.occurrence_ms,
            claim.revision,
            &claim.request,
            claim.workspace,
            claimed_at_ms,
        )
    }

    fn claim_skip(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<SkippedSlot, LedgerError> {
        let claim = authority
            .into_skip()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        let outcome = match claim.outcome {
            ScheduleFiringOutcome::SkippedMissed => SlotOutcome::SkippedMissed,
            ScheduleFiringOutcome::SkippedBusy => SlotOutcome::SkippedBusy,
            ScheduleFiringOutcome::Fired => return Err(LedgerError::InvalidSkipOutcome),
        };
        RunsDb::claim_skip_at(
            self,
            claim.schedule,
            claim.occurrence_ms,
            claim.revision,
            claim.run,
            outcome,
            claim.busy_holder,
            claimed_at_ms,
        )
    }

    fn claim_run_with_missed(
        &mut self,
        run: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError> {
        let run = run
            .into_run()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        let missed = missed
            .into_skip()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        if run.schedule != missed.schedule
            || run.revision != missed.revision
            || missed.outcome != ScheduleFiringOutcome::SkippedMissed
            || missed.busy_holder.is_some()
        {
            return Err(LedgerError::InvalidScheduleAuthority);
        }
        self.claim_scheduled_run_with_missed_at(
            run.schedule,
            run.occurrence_ms,
            run.revision,
            &run.request,
            run.workspace,
            missed.occurrence_ms,
            missed.run,
            claimed_at_ms,
        )
    }

    fn claim_skip_with_missed(
        &mut self,
        due: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError> {
        let due = due
            .into_skip()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        let missed = missed
            .into_skip()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        if due.schedule != missed.schedule
            || due.revision != missed.revision
            || missed.outcome != ScheduleFiringOutcome::SkippedMissed
            || missed.busy_holder.is_some()
        {
            return Err(LedgerError::InvalidScheduleAuthority);
        }
        let outcome = match due.outcome {
            ScheduleFiringOutcome::SkippedMissed => SlotOutcome::SkippedMissed,
            ScheduleFiringOutcome::SkippedBusy => SlotOutcome::SkippedBusy,
            ScheduleFiringOutcome::Fired => return Err(LedgerError::InvalidSkipOutcome),
        };
        self.claim_skip_with_missed_at(
            due.schedule,
            due.occurrence_ms,
            due.revision,
            due.run,
            outcome,
            due.busy_holder,
            missed.occurrence_ms,
            missed.run,
            claimed_at_ms,
        )
    }

    fn settle(
        &mut self,
        schedule: koru_delegate_core::ScheduleId,
        occurrence_ms: i64,
        outcome: SlotOutcome,
    ) -> Result<WriteOutcome, LedgerError> {
        if outcome != SlotOutcome::Fired {
            return Err(LedgerError::InvalidSettlementOutcome);
        }
        self.record_outcome(schedule, occurrence_ms, outcome)
    }

    fn unsettled(&self) -> Result<Vec<SlotClaim>, LedgerError> {
        self.ordinary_unsettled_claims()
    }
    fn by_run(&self, run: RunId) -> Result<Option<SlotClaim>, LedgerError> {
        self.slot_for_run(run)
    }
    fn intent_scope(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_core::WorkspaceScope>, LedgerError> {
        Ok(self.intent_facts(run)?.map(|facts| facts.scope))
    }
    fn observed_through_ms(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Option<i64>, LedgerError> {
        RunsDb::observed_through_ms(self, schedule)
    }
    fn claims_of(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Vec<SlotClaim>, LedgerError> {
        self.slots_of(schedule)
    }
}
