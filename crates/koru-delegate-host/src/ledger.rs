// SPDX-License-Identifier: MPL-2.0

//! One `runs.db`, shared by the three ports that read it.
//!
//! The orchestrator writes the ledger through its `RunStore` port; the
//! capture port reads a run's launch profile from the creation intent, and
//! the bus port reads a run's document, its asker and the identity its
//! `created` fact was first published under. The schedule evaluator also
//! consumes claims on its dedicated thread. They share **one connection**
//! behind a short mutex rather than opening a second handle: there is still
//! one answer to "what does the ledger say" and one SQLite writer.
//!
//! [`SharedLedger`] locks only for one SQLite statement. No path holds the
//! guard while it calls another port or waits on the actor rendezvous.

mod bots;
mod private;

use std::sync::{Arc, Mutex};

use koru_delegate_core::{
    AttemptGeneration, DelegationRequest, DocumentId, EventId, HostInstanceId, ProviderSessionKey,
    PublicationKind, RunId, WorkspaceId,
};
use koru_delegate_document::ScheduleClaimAuthority;
use koru_delegate_runtime::{RunStore, ScheduleLedger};
use koru_delegate_schedule::ScheduleFiringOutcome;
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, ClaimedSlot, IntentFacts, IntentRecord, LedgerError, OpenPublication,
    PendingIntent, Projection, PublicationRecord, RunsDb, SchedulePublicationCursor,
    SchedulePublicationFact, SchedulePublicationPreparation, SchedulePublicationRecord,
    SchedulePublicationRef, SkippedSlot, SlotClaim, SlotOutcome, WriteOutcome,
};

/// The ledger, held once and named by several ports.
#[derive(Clone)]
pub struct SharedLedger(Arc<Mutex<RunsDb>>);

impl SharedLedger {
    pub fn new(db: RunsDb) -> Self {
        Self(Arc::new(Mutex::new(db)))
    }

    /// The settled facts of a creation — the asker, the launch profile, the
    /// scope kind — read back from an intent that is written once and never
    /// rewritten.
    pub fn intent_facts(&self, run: RunId) -> Result<Option<IntentFacts>, LedgerError> {
        self.0.lock().unwrap().intent_facts(run)
    }

    /// Every publication of a run, oldest first, acknowledged included.
    pub fn publications(&self, run: RunId) -> Result<Vec<PublicationRecord>, LedgerError> {
        self.0.lock().unwrap().publications(run)
    }

    /// One publication by its id — what a re-publication reads to be
    /// stamped with the moment its first delivery carried.
    pub fn publication(&self, event: EventId) -> Result<Option<PublicationRecord>, LedgerError> {
        self.0.lock().unwrap().publication(event)
    }

    pub fn prepare_schedule_publication(
        &self,
        event: EventId,
        schedule: SchedulePublicationRef,
        fact: SchedulePublicationFact,
        body: &str,
    ) -> Result<SchedulePublicationPreparation, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_schedule_publication(event, schedule, fact, body)
    }

    #[cfg(test)]
    pub fn prepared_schedule_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.0.lock().unwrap().prepared_schedule_publications(limit)
    }

    pub fn prepared_schedule_publications_after(
        &self,
        cursor: Option<SchedulePublicationCursor>,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepared_schedule_publications_after(cursor, limit)
    }

    pub fn ready_schedule_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.0.lock().unwrap().ready_schedule_publications(limit)
    }

    #[cfg(test)]
    pub fn schedule_publications(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Vec<SchedulePublicationRecord>, LedgerError> {
        self.0.lock().unwrap().schedule_publications(schedule)
    }

    pub fn schedule_publication(
        &self,
        event: EventId,
    ) -> Result<Option<SchedulePublicationRecord>, LedgerError> {
        self.0.lock().unwrap().schedule_publication(event)
    }

    pub fn schedule_publication_for_fact(
        &self,
        schedule: koru_delegate_core::ScheduleId,
        fact: SchedulePublicationFact,
    ) -> Result<Option<SchedulePublicationRecord>, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .schedule_publication_for_fact(schedule, fact)
    }

    pub fn mark_schedule_publication_ready(
        &self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .mark_schedule_publication_ready(event)
    }

    pub fn abandon_schedule_publication(
        &self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().abandon_schedule_publication(event)
    }

    pub fn bump_schedule_publication_attempts(&self, event: EventId) -> Result<u32, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .bump_schedule_publication_attempts(event)
    }

    pub fn acknowledge_schedule_publication(
        &self,
        event: EventId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .acknowledge_schedule_publication(event)
    }

    pub fn schedule_slot(
        &self,
        schedule: koru_delegate_core::ScheduleId,
        occurrence_ms: i64,
    ) -> Result<Option<SlotClaim>, LedgerError> {
        self.0.lock().unwrap().slot(schedule, occurrence_ms)
    }

    pub fn validate_ordinary_schedule_run(&self, run: RunId) -> Result<(), LedgerError> {
        self.0.lock().unwrap().validate_ordinary_schedule_run(run)
    }
}

impl koru_delegate_runtime::RoutineLedger for SharedLedger {
    fn routine_firing(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<Option<koru_delegate_sqlite::SlotClaim>, koru_delegate_sqlite::BotLedgerError> {
        self.0.lock().unwrap().routine_firing(slot)
    }
    fn routine_route(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<
        Option<koru_delegate_core::RoutineRouteIdentity>,
        koru_delegate_sqlite::BotLedgerError,
    > {
        self.0
            .lock()
            .unwrap()
            .routine_route(schedule)
            .map_err(Into::into)
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
        self.0.lock().unwrap().held_routine_page(after)
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
        self.0.lock().unwrap().queued_routine_page(after)
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
        self.0.lock().unwrap().routine_slots_page(route, after)
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
        koru_delegate_runtime::RoutineLedger::record_expired_routine(
            &mut *self.0.lock().unwrap(),
            authority,
        )
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
        // Acquire the writer before live authority is consumed, not afterward.
        koru_delegate_runtime::RoutineLedger::claim_bot_routine(
            &mut *self.0.lock().unwrap(),
            authority,
            admission,
            fresh,
        )
    }
    fn claim_bot_routine_test(
        &mut self,
        authority: koru_delegate_document::RoutineTestAuthority,
        admission: &koru_delegate_core::BotAdmissionBarrier,
        fresh: &koru_delegate_core::FreshBotPolicyProof,
    ) -> Result<
        (
            koru_delegate_core::RoutineClaimObservation,
            Option<koru_delegate_core::RoutineSkippedClaim>,
        ),
        koru_delegate_sqlite::BotLedgerError,
    > {
        // As above: the writer is held before the Test authority is consumed,
        // so nothing can change the ledger between its revalidation and the
        // claim transaction.
        koru_delegate_runtime::RoutineLedger::claim_bot_routine_test(
            &mut *self.0.lock().unwrap(),
            authority,
            admission,
            fresh,
        )
    }
    fn bot_admission_barrier(
        &self,
        owner: koru_delegate_core::BotOwner,
        bot: koru_delegate_core::BotId,
    ) -> Result<Option<koru_delegate_core::BotAdmissionBarrier>, koru_delegate_sqlite::BotLedgerError>
    {
        koru_delegate_runtime::RoutineLedger::bot_admission_barrier(
            &*self.0.lock().unwrap(),
            owner,
            bot,
        )
    }
    fn read_routine_claim(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<koru_delegate_core::RoutineClaimObservation, koru_delegate_sqlite::BotLedgerError>
    {
        self.0.lock().unwrap().read_routine_claim(slot)
    }
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &koru_delegate_core::RoutineLinkAcknowledgement,
    ) -> Result<koru_delegate_core::RoutineClaimRecord, koru_delegate_sqlite::BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .acknowledge_routine_link(acknowledgement)
    }
}

impl RunStore for SharedLedger {
    fn decision_projection_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<RunId>, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_projection_runs(after, limit)
    }
    fn pending_publication_run_anchor(
        &self,
    ) -> Result<Option<koru_delegate_sqlite::PendingPublicationRunCursor>, LedgerError> {
        self.0.lock().unwrap().pending_publication_run_anchor()
    }
    fn next_pending_publication_run(
        &self,
        after: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
        through: koru_delegate_sqlite::PendingPublicationRunCursor,
        before_or_at: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
    ) -> Result<koru_delegate_sqlite::PendingPublicationRunSeek, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .next_pending_publication_run(after, through, before_or_at)
    }
    fn decision_open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_open_publications_for(run, limit)
    }
    fn bots(&mut self) -> Option<&mut dyn koru_delegate_sqlite::BotLedger> {
        Some(self)
    }
    fn private_effects(&mut self) -> Option<&mut dyn koru_delegate_runtime::PrivateEffectLedger> {
        Some(self)
    }
    fn native_results(&mut self) -> Option<&mut dyn koru_delegate_sqlite::NativeResultLedger> {
        Some(self)
    }

    fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError> {
        self.0.lock().unwrap().validate_intent(request, workspace)
    }

    fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().record_intent(request, workspace)
    }

    fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().satisfy_intent(run, run_document)
    }

    fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().abandon_intent(run)
    }

    fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError> {
        self.0.lock().unwrap().pending_intents()
    }

    fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError> {
        self.0.lock().unwrap().intent(run)
    }

    fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError> {
        self.0.lock().unwrap().intents()
    }

    fn intent_facts(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::IntentFacts>, LedgerError> {
        self.0.lock().unwrap().intent_facts(run)
    }

    fn record_deletion(
        &mut self,
        run: RunId,
        artifacts: koru_delegate_core::ArtifactDisposition,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().record_deletion(run, artifacts)
    }

    fn clear_deletion(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().clear_deletion(run)
    }

    fn deletion(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::DeletionRecord>, LedgerError> {
        self.0.lock().unwrap().deletion(run)
    }

    fn forget_run(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().forget_run(run)
    }

    fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError> {
        self.0.lock().unwrap().open_attempt(run)
    }

    fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError> {
        self.0.lock().unwrap().advance(run, generation, phase)
    }

    fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .bind_session(run, generation, session, host)
    }

    fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError> {
        self.0.lock().unwrap().retire(run, generation)
    }

    fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError> {
        self.0.lock().unwrap().current_attempt(run)
    }

    fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError> {
        self.0.lock().unwrap().live_attempts()
    }

    fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().enqueue(event, run, kind, body_json)
    }

    fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError> {
        self.0.lock().unwrap().acknowledge(event)
    }

    fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError> {
        self.0.lock().unwrap().bump_attempts(event)
    }

    fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError> {
        self.0.lock().unwrap().open_publications(limit)
    }

    fn open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .open_publications_for(Some(run), limit)
    }

    fn publication(
        &self,
        event: EventId,
    ) -> Result<Option<koru_delegate_sqlite::PublicationRecord>, LedgerError> {
        self.0.lock().unwrap().publication(event)
    }

    fn advance_cursor(
        &mut self,
        run: RunId,
        projection: Projection,
        timeline_index: u64,
    ) -> Result<WriteOutcome, LedgerError> {
        self.0
            .lock()
            .unwrap()
            .advance_cursor(run, projection, timeline_index)
    }
}

impl ScheduleLedger for SharedLedger {
    fn claim_run(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<ClaimedSlot, LedgerError> {
        let claim = authority
            .into_run()
            .map_err(|_| LedgerError::InvalidScheduleAuthority)?;
        self.0.lock().unwrap().claim_scheduled_run_at(
            claim.schedule,
            claim.occurrence_ms,
            claim.revision,
            &claim.request,
            claim.workspace,
            claimed_at_ms,
        )
    }
    fn claim_run_with_missed(
        &mut self,
        run: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError> {
        ScheduleLedger::claim_run_with_missed(
            &mut *self.0.lock().unwrap(),
            run,
            missed,
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
        self.0.lock().unwrap().claim_skip_at(
            claim.schedule,
            claim.occurrence_ms,
            claim.revision,
            claim.run,
            outcome,
            claim.busy_holder,
            claimed_at_ms,
        )
    }
    fn claim_skip_with_missed(
        &mut self,
        due: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError> {
        ScheduleLedger::claim_skip_with_missed(
            &mut *self.0.lock().unwrap(),
            due,
            missed,
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
        self.0
            .lock()
            .unwrap()
            .record_outcome(schedule, occurrence_ms, outcome)
    }
    fn unsettled(&self) -> Result<Vec<SlotClaim>, LedgerError> {
        self.0.lock().unwrap().ordinary_unsettled_claims()
    }
    fn by_run(&self, run: RunId) -> Result<Option<SlotClaim>, LedgerError> {
        self.0.lock().unwrap().slot_for_run(run)
    }
    fn intent_scope(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_core::WorkspaceScope>, LedgerError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .intent_facts(run)?
            .map(|facts| facts.scope))
    }
    fn observed_through_ms(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Option<i64>, LedgerError> {
        self.0.lock().unwrap().observed_through_ms(schedule)
    }
    fn claims_of(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Vec<SlotClaim>, LedgerError> {
        self.0.lock().unwrap().slots_of(schedule)
    }
}
