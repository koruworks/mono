// SPDX-License-Identifier: MPL-2.0

//! The real in-memory ledger as the store port, without the worker counters.
//!
//! `RunsDb`'s own `RunStore` implementation also moves the process-global
//! counters (`crate::counters`) on five writes. Those counters are shared by
//! every test in a binary, and the counter tests assert deltas, so a
//! composition test must not move them. This wrapper forwards every method to
//! the same ledger — the five counted writes to `RunsDb`'s own methods, the
//! rest through its `RunStore`/`ScheduleLedger` implementations — and adds
//! nothing. The Bot and native-result domains are the ledger itself.

use koru_delegate_core::{
    ArtifactDisposition, AttemptGeneration, DelegationRequest, DocumentId, EventId, HostInstanceId,
    ProviderSessionKey, PublicationKind, RunId, ScheduleId, WorkspaceId, WorkspaceScope,
};
use koru_delegate_document::ScheduleClaimAuthority;
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, ClaimedSlot, DeletionRecord, IntentFacts, IntentRecord, LedgerError,
    OpenPublication, PendingIntent, PendingPublicationRunCursor, PendingPublicationRunSeek,
    Projection, PublicationRecord, RunsDb, SkippedSlot, SlotClaim, SlotOutcome, WriteOutcome,
};

use crate::{RunStore, ScheduleLedger};

pub(super) struct UncountedLedger(pub(super) RunsDb);

impl RunStore for UncountedLedger {
    fn decision_projection_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<RunId>, LedgerError> {
        <RunsDb as RunStore>::decision_projection_runs(&self.0, after, limit)
    }
    fn pending_publication_run_anchor(
        &self,
    ) -> Result<Option<PendingPublicationRunCursor>, LedgerError> {
        <RunsDb as RunStore>::pending_publication_run_anchor(&self.0)
    }
    fn next_pending_publication_run(
        &self,
        after: Option<PendingPublicationRunCursor>,
        through: PendingPublicationRunCursor,
        before_or_at: Option<PendingPublicationRunCursor>,
    ) -> Result<PendingPublicationRunSeek, LedgerError> {
        <RunsDb as RunStore>::next_pending_publication_run(&self.0, after, through, before_or_at)
    }
    fn decision_open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        <RunsDb as RunStore>::decision_open_publications_for(&self.0, run, limit)
    }
    fn native_results(&mut self) -> Option<&mut dyn koru_delegate_sqlite::NativeResultLedger> {
        Some(&mut self.0)
    }
    fn bots(&mut self) -> Option<&mut dyn koru_delegate_sqlite::BotLedger> {
        Some(&mut self.0)
    }
    fn private_effects(&mut self) -> Option<&mut dyn crate::PrivateEffectLedger> {
        Some(&mut self.0)
    }
    fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError> {
        <RunsDb as RunStore>::validate_intent(&self.0, request, workspace)
    }
    fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError> {
        RunsDb::record_intent(&mut self.0, request, workspace)
    }
    fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::satisfy_intent(&mut self.0, run, run_document)
    }
    fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::abandon_intent(&mut self.0, run)
    }
    fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError> {
        <RunsDb as RunStore>::pending_intents(&self.0)
    }
    fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError> {
        <RunsDb as RunStore>::intent(&self.0, run)
    }
    fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError> {
        <RunsDb as RunStore>::intents(&self.0)
    }
    fn intent_facts(&self, run: RunId) -> Result<Option<IntentFacts>, LedgerError> {
        <RunsDb as RunStore>::intent_facts(&self.0, run)
    }
    fn record_deletion(
        &mut self,
        run: RunId,
        artifacts: ArtifactDisposition,
    ) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::record_deletion(&mut self.0, run, artifacts)
    }
    fn clear_deletion(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::clear_deletion(&mut self.0, run)
    }
    fn deletion(&self, run: RunId) -> Result<Option<DeletionRecord>, LedgerError> {
        <RunsDb as RunStore>::deletion(&self.0, run)
    }
    fn forget_run(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::forget_run(&mut self.0, run)
    }
    fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError> {
        RunsDb::open_attempt(&mut self.0, run)
    }
    fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError> {
        <RunsDb as RunStore>::advance(&mut self.0, run, generation, phase)
    }
    fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError> {
        <RunsDb as RunStore>::bind_session(&mut self.0, run, generation, session, host)
    }
    fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError> {
        <RunsDb as RunStore>::retire(&mut self.0, run, generation)
    }
    fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError> {
        <RunsDb as RunStore>::current_attempt(&self.0, run)
    }
    fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError> {
        <RunsDb as RunStore>::live_attempts(&self.0)
    }
    fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError> {
        RunsDb::enqueue(&mut self.0, event, run, kind, body_json)
    }
    fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError> {
        RunsDb::acknowledge(&mut self.0, event)
    }
    fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError> {
        RunsDb::bump_attempts(&mut self.0, event)
    }
    fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError> {
        <RunsDb as RunStore>::open_publications(&self.0, limit)
    }
    fn open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        <RunsDb as RunStore>::open_publications_for(&self.0, run, limit)
    }
    fn publication(&self, event: EventId) -> Result<Option<PublicationRecord>, LedgerError> {
        <RunsDb as RunStore>::publication(&self.0, event)
    }
    fn advance_cursor(
        &mut self,
        run: RunId,
        projection: Projection,
        timeline_index: u64,
    ) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as RunStore>::advance_cursor(&mut self.0, run, projection, timeline_index)
    }
}

impl ScheduleLedger for UncountedLedger {
    fn claim_run(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<ClaimedSlot, LedgerError> {
        <RunsDb as ScheduleLedger>::claim_run(&mut self.0, authority, claimed_at_ms)
    }
    fn claim_run_with_missed(
        &mut self,
        run: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError> {
        <RunsDb as ScheduleLedger>::claim_run_with_missed(&mut self.0, run, missed, claimed_at_ms)
    }
    fn claim_skip(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<SkippedSlot, LedgerError> {
        <RunsDb as ScheduleLedger>::claim_skip(&mut self.0, authority, claimed_at_ms)
    }
    fn claim_skip_with_missed(
        &mut self,
        due: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError> {
        <RunsDb as ScheduleLedger>::claim_skip_with_missed(&mut self.0, due, missed, claimed_at_ms)
    }
    fn settle(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        outcome: SlotOutcome,
    ) -> Result<WriteOutcome, LedgerError> {
        <RunsDb as ScheduleLedger>::settle(&mut self.0, schedule, occurrence_ms, outcome)
    }
    fn unsettled(&self) -> Result<Vec<SlotClaim>, LedgerError> {
        <RunsDb as ScheduleLedger>::unsettled(&self.0)
    }
    fn by_run(&self, run: RunId) -> Result<Option<SlotClaim>, LedgerError> {
        <RunsDb as ScheduleLedger>::by_run(&self.0, run)
    }
    fn intent_scope(&self, run: RunId) -> Result<Option<WorkspaceScope>, LedgerError> {
        <RunsDb as ScheduleLedger>::intent_scope(&self.0, run)
    }
    fn observed_through_ms(&self, schedule: ScheduleId) -> Result<Option<i64>, LedgerError> {
        <RunsDb as ScheduleLedger>::observed_through_ms(&self.0, schedule)
    }
    fn claims_of(&self, schedule: ScheduleId) -> Result<Vec<SlotClaim>, LedgerError> {
        <RunsDb as ScheduleLedger>::claims_of(&self.0, schedule)
    }
}
