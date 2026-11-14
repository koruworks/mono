// SPDX-License-Identifier: MPL-2.0

//! Commit-first schedule activity publication (ROADMAP P2.6, ADR-402).
//!
//! This host adapter is the only layer that sees both typed schedule facts and
//! event envelopes. It persists the exact envelope before authority changes,
//! never holds the ledger mutex over Store or gateway I/O, and reconciles a
//! stored envelope with its original origin/session rather than regenerating
//! it under a new process context.

use std::sync::{Arc, Condvar, Mutex};

use koru_api_client::Client;
use koru_delegate_core::ScheduleId;
use koru_delegate_events::{
    EmitContext, Envelope, ScheduleChain, ScheduleCreated, ScheduleFired, SchedulePaused,
    ScheduleRef, ScheduleSkipReason, ScheduleSkipped, ScheduleUpdated, Stamp,
};
use koru_delegate_runtime::{
    Port, PortError, PortResult, PreparedScheduleEvent, ScheduleDefinitionEvent,
    ScheduleEventMaintenance, ScheduleEventRef, ScheduleEvents, ScheduleOccurrenceEvent,
    schedule_definition_event,
};
use koru_delegate_schedule::{ScheduleDefinition, ScheduleFiringOutcome, ScheduleHeader};
use koru_delegate_sqlite::{
    SchedulePublicationCursor, SchedulePublicationFact, SchedulePublicationRecord,
    SchedulePublicationRef, SlotClaim, SlotOutcome, SlotState,
};
use koru_events::validate;
use koru_events::wire::{AppendReceipt, EventBatch};

use crate::ledger::SharedLedger;

const COMPONENT: &str = "delegate";
const BATCH: usize = 32;
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Default)]
struct AuthorityGateState {
    held: bool,
    stopped: bool,
}

/// Serializes live prepare-to-authority sequences with restart comparison.
///
/// Unlike a plain mutex, shutdown can close this gate and wake a scheduler
/// waiting behind a publisher's bounded Store request. The current holder keeps
/// its ordinary RAII ownership; new entrants receive `None` and exit.
#[derive(Clone, Default)]
pub(crate) struct ScheduleEventAuthorityGate {
    state: Arc<Mutex<AuthorityGateState>>,
    released: Arc<Condvar>,
}

impl ScheduleEventAuthorityGate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn enter(&self) -> Option<ScheduleEventAuthorityGuard<'_>> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.held && !state.stopped {
            state = self
                .released
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if state.stopped {
            None
        } else {
            state.held = true;
            Some(ScheduleEventAuthorityGuard { gate: self })
        }
    }

    pub(crate) fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        self.released.notify_all();
    }
}

pub(crate) struct ScheduleEventAuthorityGuard<'a> {
    gate: &'a ScheduleEventAuthorityGate,
}

impl Drop for ScheduleEventAuthorityGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.held = false;
        self.gate.released.notify_one();
    }
}

// Owned phase guards clone the same state/Condvar; ordinary enter() above keeps
// its existing lifetime and wait behavior. No Routine coordinator is wired yet.
struct OwnedSchedulePhaseGuard {
    gate: ScheduleEventAuthorityGate,
}
impl koru_delegate_runtime::SchedulePhaseGuard for OwnedSchedulePhaseGuard {}
impl Drop for OwnedSchedulePhaseGuard {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        state.held = false;
        self.gate.released.notify_one();
    }
}
impl koru_delegate_runtime::SchedulePhaseGate for ScheduleEventAuthorityGate {
    fn enter_until(
        &self,
        deadline: std::time::Instant,
    ) -> PortResult<Box<dyn koru_delegate_runtime::SchedulePhaseGuard>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if state.stopped || remaining.is_zero() {
                return Err(PortError::new(
                    Port::Schedules,
                    "routine_phase_unavailable",
                    "Schedule phase stopped or expired",
                ));
            }
            if !state.held {
                state.held = true;
                return Ok(Box::new(OwnedSchedulePhaseGuard { gate: self.clone() }));
            }
            (state, _) = self
                .released
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReconcileDisposition {
    /// The row was made ready or conclusively abandoned.
    Settled,
    /// Authority was temporarily unavailable. Retry with bounded backoff.
    Retry,
    /// Current authority cannot prove this historical fact either way. A
    /// later Store/schedule wake may provide new evidence; do not poll.
    AwaitAuthorityWake,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeliveryDisposition {
    Acknowledge,
    Retry,
}

fn rejected_delivery_disposition(
    available: bool,
    code: Option<koru_store_core::Code>,
) -> DeliveryDisposition {
    if !available || code == Some(koru_store_core::Code::InvalidRequest) {
        DeliveryDisposition::Acknowledge
    } else {
        DeliveryDisposition::Retry
    }
}

fn definition_disposition(prepared_revision: u32, current_revision: u32) -> ReconcileDisposition {
    match current_revision.cmp(&prepared_revision) {
        std::cmp::Ordering::Less => ReconcileDisposition::Settled,
        std::cmp::Ordering::Equal => ReconcileDisposition::Settled,
        std::cmp::Ordering::Greater => ReconcileDisposition::AwaitAuthorityWake,
    }
}

fn page_retry_needed(more: bool, dispositions: &[ReconcileDisposition]) -> bool {
    dispositions.contains(&ReconcileDisposition::Retry)
        || (more
            && !dispositions
                .iter()
                .all(|item| *item == ReconcileDisposition::AwaitAuthorityWake))
}

pub struct BusScheduleEvents {
    client: Client,
    event_store: koru_store_client::Client,
    ledger: SharedLedger,
    ctx: EmitContext,
    prepared_cursor: Option<SchedulePublicationCursor>,
    reconcile_gate: Option<Arc<ScheduleEventAuthorityGate>>,
}

impl BusScheduleEvents {
    pub fn new(client: Client, ledger: SharedLedger) -> Self {
        // Schedule-event authority reads and journal sends are the only Store
        // traffic on this clone. Its timeout is deliberately shorter than the
        // delegate supervisor's grace period; every other Store client keeps
        // the ordinary 30-second request budget.
        let event_store = client.store().with_request_timeout(REQUEST_TIMEOUT);
        Self {
            client,
            event_store,
            ledger,
            ctx: EmitContext::system(COMPONENT),
            prepared_cursor: None,
            reconcile_gate: None,
        }
    }

    pub fn publisher(
        client: Client,
        ledger: SharedLedger,
        gate: Arc<ScheduleEventAuthorityGate>,
    ) -> Self {
        let mut events = Self::new(client, ledger);
        events.reconcile_gate = Some(gate);
        events
    }

    fn error(code: &'static str, error: impl std::fmt::Display) -> PortError {
        PortError::new(Port::Events, code, error.to_string())
    }

    fn persisted_ref(schedule: ScheduleEventRef) -> SchedulePublicationRef {
        SchedulePublicationRef {
            schedule: schedule.schedule,
            workspace: schedule.workspace,
            document: schedule.document,
        }
    }

    fn event_ref(schedule: SchedulePublicationRef) -> ScheduleEventRef {
        ScheduleEventRef {
            schedule: schedule.schedule,
            workspace: schedule.workspace,
            document: schedule.document,
        }
    }

    fn chain_ref(schedule: ScheduleEventRef) -> ScheduleRef {
        ScheduleRef {
            schedule: schedule.schedule,
            workspace: schedule.workspace,
            document: schedule.document,
        }
    }

    fn context_of(envelope: &Envelope) -> EmitContext {
        EmitContext {
            session_id: envelope.origin.session_id,
            device_id: envelope.origin.device_id,
            profile_id: envelope.scope.profile_id,
            actor: envelope.actor.clone(),
        }
    }

    fn parse(record: &SchedulePublicationRecord) -> PortResult<Envelope> {
        let envelope: Envelope = serde_json::from_str(record.body.as_str())
            .map_err(|error| Self::error("schedule_event_body", error))?;
        if envelope.event_id != record.event {
            return Err(Self::error(
                "schedule_event_identity",
                "the stored body does not carry its row EventId",
            ));
        }
        validate(&envelope).map_err(|error| Self::error("schedule_event_invalid", error))?;
        Ok(envelope)
    }

    fn serialize(envelope: &Envelope) -> PortResult<String> {
        serde_json::to_string(envelope).map_err(|error| Self::error("schedule_event_encode", error))
    }

    fn root_record(&self, schedule: ScheduleId) -> PortResult<Option<SchedulePublicationRecord>> {
        self.ledger
            .schedule_publication_for_fact(
                schedule,
                SchedulePublicationFact::Created { revision: 1 },
            )
            .map_err(|error| Self::error("schedule_event_ledger", error))
    }

    fn prepared_page(
        &mut self,
        limit: usize,
    ) -> PortResult<(Vec<SchedulePublicationRecord>, bool)> {
        if limit == 0 {
            return Ok((Vec::new(), false));
        }
        let mut prepared = self
            .ledger
            .prepared_schedule_publications_after(self.prepared_cursor, limit.saturating_add(1))
            .map_err(|error| Self::error("schedule_event_ledger", error))?;
        if prepared.is_empty() && self.prepared_cursor.take().is_some() {
            prepared = self
                .ledger
                .prepared_schedule_publications_after(None, limit.saturating_add(1))
                .map_err(|error| Self::error("schedule_event_ledger", error))?;
        }
        let more = prepared.len() > limit;
        prepared.truncate(limit);
        Ok((prepared, more))
    }

    fn stored_matches_created(
        record: &SchedulePublicationRecord,
        header: &ScheduleHeader,
    ) -> PortResult<bool> {
        let stored = Self::parse(record)?;
        let (expected, _) = ScheduleChain::created(
            &Self::context_of(&stored),
            Self::chain_ref(Self::event_ref(record.schedule)),
            Stamp {
                event_id: record.event,
                occurred_at_ms: header.created_at_ms,
            },
            ScheduleCreated {
                requested_by: header.definition.template.requested_by,
                revision: 1,
            },
        );
        Ok(stored == expected && header.schedule == record.schedule.schedule)
    }

    fn stored_matches_definition(
        record: &SchedulePublicationRecord,
        root: koru_delegate_core::EventId,
        definition: &ScheduleDefinition,
    ) -> PortResult<bool> {
        let stored = Self::parse(record)?;
        let chain = ScheduleChain::rejoin(
            &Self::context_of(&stored),
            Self::chain_ref(Self::event_ref(record.schedule)),
            root,
        );
        let stamp = Stamp {
            event_id: record.event,
            occurred_at_ms: definition.effective_at_ms,
        };
        let expected = match schedule_definition_event(definition) {
            ScheduleDefinitionEvent::Updated => chain.updated(
                stamp,
                ScheduleUpdated {
                    revision: definition.revision,
                },
            ),
            ScheduleDefinitionEvent::Paused(reason) => chain.paused(
                stamp,
                SchedulePaused {
                    revision: definition.revision,
                    reason,
                },
            ),
        };
        Ok(stored == expected)
    }

    fn occurrence_from_claim(
        schedule: ScheduleEventRef,
        claim: &SlotClaim,
    ) -> Option<ScheduleOccurrenceEvent> {
        let outcome = match claim.state {
            SlotState::Claimed => return None,
            SlotState::Settled(SlotOutcome::Fired) => ScheduleFiringOutcome::Fired,
            SlotState::Settled(SlotOutcome::SkippedMissed) => ScheduleFiringOutcome::SkippedMissed,
            SlotState::Settled(SlotOutcome::SkippedBusy) => ScheduleFiringOutcome::SkippedBusy,
            SlotState::Settled(SlotOutcome::SkippedRetired) => return None,
        };
        Some(ScheduleOccurrenceEvent {
            schedule,
            revision: claim.revision,
            occurrence_ms: claim.occurrence_ms,
            run: claim.run,
            outcome,
            busy_holder: claim.busy_holder,
            claimed_at_ms: claim.claimed_at_ms,
        })
    }

    fn pending_fired_from_claim(
        schedule: ScheduleEventRef,
        claim: &SlotClaim,
    ) -> Option<ScheduleOccurrenceEvent> {
        matches!(claim.state, SlotState::Claimed).then_some(ScheduleOccurrenceEvent {
            schedule,
            revision: claim.revision,
            occurrence_ms: claim.occurrence_ms,
            run: claim.run,
            outcome: ScheduleFiringOutcome::Fired,
            busy_holder: None,
            claimed_at_ms: claim.claimed_at_ms,
        })
    }

    fn stored_matches_occurrence(
        record: &SchedulePublicationRecord,
        root: koru_delegate_core::EventId,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<bool> {
        let stored = Self::parse(record)?;
        let chain = ScheduleChain::rejoin(
            &Self::context_of(&stored),
            Self::chain_ref(fact.schedule),
            root,
        );
        let stamp = Stamp {
            event_id: record.event,
            occurred_at_ms: fact.claimed_at_ms,
        };
        let expected = match fact.outcome {
            ScheduleFiringOutcome::Fired if fact.busy_holder.is_none() => chain.fired(
                stamp,
                ScheduleFired {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                    run: fact.run,
                },
            ),
            ScheduleFiringOutcome::SkippedMissed if fact.busy_holder.is_none() => chain.skipped(
                stamp,
                ScheduleSkipped {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                    reason: ScheduleSkipReason::missed(),
                },
            ),
            ScheduleFiringOutcome::SkippedBusy => {
                let Some(holder) = fact.busy_holder else {
                    return Ok(false);
                };
                chain.skipped(
                    stamp,
                    ScheduleSkipped {
                        revision: fact.revision,
                        occurrence_ms: fact.occurrence_ms,
                        reason: ScheduleSkipReason::busy(holder),
                    },
                )
            }
            _ => return Ok(false),
        };
        Ok(stored == expected)
    }

    fn prepare_envelope(
        &self,
        schedule: ScheduleEventRef,
        fact: SchedulePublicationFact,
        envelope: Envelope,
    ) -> PortResult<PreparedScheduleEvent> {
        let body = Self::serialize(&envelope)?;
        let prepared = self
            .ledger
            .prepare_schedule_publication(
                envelope.event_id,
                Self::persisted_ref(schedule),
                fact,
                &body,
            )
            .map_err(|error| Self::error("schedule_event_prepare", error))?;
        Ok(PreparedScheduleEvent {
            event: prepared.publication.event,
        })
    }

    fn fresh_created(
        &self,
        schedule: ScheduleEventRef,
        header: &ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent> {
        let stamp = Stamp::fresh(header.created_at_ms);
        let (envelope, _) = ScheduleChain::created(
            &self.ctx,
            Self::chain_ref(schedule),
            stamp,
            ScheduleCreated {
                requested_by: header.definition.template.requested_by,
                revision: 1,
            },
        );
        self.prepare_envelope(
            schedule,
            SchedulePublicationFact::Created { revision: 1 },
            envelope,
        )
    }

    fn prepare_claim(
        &self,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        let Some(root) = self.root_record(fact.schedule.schedule)? else {
            return Err(Self::error(
                "schedule_event_root",
                "the schedule has no durable created root",
            ));
        };
        let stored_fact = match fact.outcome {
            ScheduleFiringOutcome::Fired if fact.busy_holder.is_none() => {
                SchedulePublicationFact::Fired {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                }
            }
            ScheduleFiringOutcome::SkippedMissed if fact.busy_holder.is_none() => {
                SchedulePublicationFact::Skipped {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                }
            }
            ScheduleFiringOutcome::SkippedBusy => {
                if fact.busy_holder.is_none() {
                    return Err(Self::error(
                        "schedule_event_fact",
                        "a busy skip has no holder",
                    ));
                }
                SchedulePublicationFact::Skipped {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                }
            }
            _ => {
                return Err(Self::error(
                    "schedule_event_fact",
                    "an occurrence has an invalid outcome/holder pair",
                ));
            }
        };
        if let Some(existing) = self
            .ledger
            .schedule_publication_for_fact(fact.schedule.schedule, stored_fact)
            .map_err(|error| Self::error("schedule_event_ledger", error))?
        {
            if Self::stored_matches_occurrence(&existing, root.event, fact)? {
                return Ok(existing
                    .ready_at_ms
                    .is_none()
                    .then_some(PreparedScheduleEvent {
                        event: existing.event,
                    }));
            }
            return Err(Self::error(
                "schedule_event_conflict",
                "the occurrence already has another event fact",
            ));
        }
        let chain = ScheduleChain::rejoin(&self.ctx, Self::chain_ref(fact.schedule), root.event);
        let stamp = Stamp::fresh(fact.claimed_at_ms);
        let envelope = match fact.outcome {
            ScheduleFiringOutcome::Fired => chain.fired(
                stamp,
                ScheduleFired {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                    run: fact.run,
                },
            ),
            ScheduleFiringOutcome::SkippedMissed => chain.skipped(
                stamp,
                ScheduleSkipped {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                    reason: ScheduleSkipReason::missed(),
                },
            ),
            ScheduleFiringOutcome::SkippedBusy => chain.skipped(
                stamp,
                ScheduleSkipped {
                    revision: fact.revision,
                    occurrence_ms: fact.occurrence_ms,
                    reason: ScheduleSkipReason::busy(
                        fact.busy_holder.expect("validated busy holder"),
                    ),
                },
            ),
        };
        self.prepare_envelope(fact.schedule, stored_fact, envelope)
            .map(Some)
    }

    fn authoritative_document(
        &self,
        schedule: SchedulePublicationRef,
    ) -> Result<koru_delegate_document::ScheduleDocument, koru_store_core::Error> {
        self.event_store
            .snapshot(schedule.workspace, schedule.document)
            .and_then(|bytes| {
                koru_delegate_document::ScheduleDocument::from_snapshot(&bytes).map_err(|error| {
                    koru_store_core::Error::new(
                        koru_store_core::Code::CorruptState,
                        error.to_string(),
                    )
                })
            })
    }

    fn reconcile_created_authority(
        &mut self,
        record: &SchedulePublicationRecord,
        header: &ScheduleHeader,
    ) -> PortResult<ReconcileDisposition> {
        if Self::stored_matches_created(record, header)? {
            self.ready(PreparedScheduleEvent {
                event: record.event,
            })?;
        } else {
            self.abandon(PreparedScheduleEvent {
                event: record.event,
            })?;
        }
        Ok(ReconcileDisposition::Settled)
    }

    fn reconcile_definition_authority(
        &mut self,
        record: &SchedulePublicationRecord,
        revision: u32,
        definition: &ScheduleDefinition,
    ) -> PortResult<ReconcileDisposition> {
        let Some(root) = self.root_record(record.schedule.schedule)? else {
            self.abandon(PreparedScheduleEvent {
                event: record.event,
            })?;
            return Ok(ReconcileDisposition::Settled);
        };
        let exact = definition.revision == revision
            && Self::stored_matches_definition(record, root.event, definition)?;
        let disposition = definition_disposition(revision, definition.revision);
        if exact {
            self.ready(PreparedScheduleEvent {
                event: record.event,
            })?;
        } else if definition.revision <= revision {
            // A lower revision proves the mutation did not land; the same
            // revision with another lifecycle or body proves this exact fact
            // did not land.
            self.abandon(PreparedScheduleEvent {
                event: record.event,
            })?;
        }
        Ok(disposition)
    }

    fn reconcile_one(
        &mut self,
        record: SchedulePublicationRecord,
    ) -> PortResult<ReconcileDisposition> {
        // Parse and validate before consulting authority. An invalid prepared
        // body can never be made publishable; it is still safe to abandon.
        if Self::parse(&record).is_err() {
            self.ledger
                .abandon_schedule_publication(record.event)
                .map_err(|error| Self::error("schedule_event_abandon", error))?;
            return Ok(ReconcileDisposition::Settled);
        }
        match record.fact {
            SchedulePublicationFact::Created { .. } => {
                match self.authoritative_document(record.schedule) {
                    Ok(document) => {
                        let header = document
                            .header()
                            .map_err(|error| Self::error("schedule_event_authority", error))?;
                        self.reconcile_created_authority(&record, &header)
                    }
                    Err(error)
                        if matches!(
                            error.code(),
                            koru_store_core::Code::DocumentNotFound
                                | koru_store_core::Code::WorkspaceNotFound
                        ) =>
                    {
                        self.abandon(PreparedScheduleEvent {
                            event: record.event,
                        })?;
                        Ok(ReconcileDisposition::Settled)
                    }
                    Err(_) => Ok(ReconcileDisposition::Retry),
                }
            }
            SchedulePublicationFact::Updated { revision }
            | SchedulePublicationFact::Paused { revision } => {
                match self.authoritative_document(record.schedule) {
                    Ok(document) => {
                        let definition = document
                            .header()
                            .map_err(|error| Self::error("schedule_event_authority", error))?
                            .definition;
                        self.reconcile_definition_authority(&record, revision, &definition)
                    }
                    Err(error)
                        if matches!(
                            error.code(),
                            koru_store_core::Code::DocumentNotFound
                                | koru_store_core::Code::WorkspaceNotFound
                        ) =>
                    {
                        self.abandon(PreparedScheduleEvent {
                            event: record.event,
                        })?;
                        Ok(ReconcileDisposition::Settled)
                    }
                    Err(_) => Ok(ReconcileDisposition::Retry),
                }
            }
            SchedulePublicationFact::Fired { occurrence_ms, .. }
            | SchedulePublicationFact::Skipped { occurrence_ms, .. } => {
                let claim = self
                    .ledger
                    .schedule_slot(record.schedule.schedule, occurrence_ms)
                    .map_err(|error| Self::error("schedule_event_ledger", error))?;
                let Some(claim) = claim else {
                    self.abandon(PreparedScheduleEvent {
                        event: record.event,
                    })?;
                    return Ok(ReconcileDisposition::Settled);
                };
                let schedule = Self::event_ref(record.schedule);
                if let Some(pending) = Self::pending_fired_from_claim(schedule, &claim) {
                    let Some(root) = self.root_record(record.schedule.schedule)? else {
                        self.abandon(PreparedScheduleEvent {
                            event: record.event,
                        })?;
                        return Ok(ReconcileDisposition::Settled);
                    };
                    if !Self::stored_matches_occurrence(&record, root.event, pending)? {
                        self.abandon(PreparedScheduleEvent {
                            event: record.event,
                        })?;
                        return Ok(ReconcileDisposition::Settled);
                    }
                    return Ok(ReconcileDisposition::AwaitAuthorityWake);
                }
                let Some(fact) = Self::occurrence_from_claim(schedule, &claim) else {
                    self.abandon(PreparedScheduleEvent {
                        event: record.event,
                    })?;
                    return Ok(ReconcileDisposition::Settled);
                };
                let Some(root) = self.root_record(record.schedule.schedule)? else {
                    self.abandon(PreparedScheduleEvent {
                        event: record.event,
                    })?;
                    return Ok(ReconcileDisposition::Settled);
                };
                if !Self::stored_matches_occurrence(&record, root.event, fact)? {
                    self.abandon(PreparedScheduleEvent {
                        event: record.event,
                    })?;
                    return Ok(ReconcileDisposition::Settled);
                }
                // The claim is the authoritative one-slot/one-outcome
                // mutation. The Schedule document's firing list is its
                // user-facing projection and can be repaired independently.
                self.ready(PreparedScheduleEvent {
                    event: record.event,
                })?;
                Ok(ReconcileDisposition::Settled)
            }
        }
    }

    fn reconcile_prepared_record(
        &mut self,
        record: SchedulePublicationRecord,
    ) -> PortResult<ReconcileDisposition> {
        let gate = self.reconcile_gate.clone();
        let _authority = match gate.as_ref() {
            Some(gate) => match gate.enter() {
                Some(authority) => Some(authority),
                None => return Ok(ReconcileDisposition::AwaitAuthorityWake),
            },
            None => None,
        };
        self.reconcile_one(record)
    }

    fn drain_one(&mut self, record: SchedulePublicationRecord) -> PortResult<bool> {
        let envelope = match Self::parse(&record) {
            Ok(envelope) => envelope,
            Err(_) => {
                self.ledger
                    .acknowledge_schedule_publication(record.event)
                    .map_err(|error| Self::error("schedule_event_ack", error))?;
                return Ok(true);
            }
        };
        self.ledger
            .bump_schedule_publication_attempts(record.event)
            .map_err(|error| Self::error("schedule_event_attempt", error))?;
        if !self.client.events().available() {
            self.ledger
                .acknowledge_schedule_publication(record.event)
                .map_err(|error| Self::error("schedule_event_ack", error))?;
            return Ok(true);
        }
        let delivery: Result<AppendReceipt, koru_store_core::Error> = self.event_store.post_json(
            "/v1/events",
            &EventBatch {
                events: vec![envelope],
            },
        );
        match delivery.and_then(|receipt| {
            if let Some(rejected) = receipt.rejected.first() {
                Err(koru_store_core::Error::new(
                    koru_store_core::Code::InvalidRequest,
                    format!("{}: {}", rejected.code, rejected.message),
                ))
            } else {
                Ok(())
            }
        }) {
            Ok(()) => {
                self.ledger
                    .acknowledge_schedule_publication(record.event)
                    .map_err(|error| Self::error("schedule_event_ack", error))?;
                Ok(true)
            }
            Err(error) => match rejected_delivery_disposition(true, Some(error.code())) {
                DeliveryDisposition::Acknowledge => {
                    self.ledger
                        .acknowledge_schedule_publication(record.event)
                        .map_err(|error| Self::error("schedule_event_ack", error))?;
                    Ok(true)
                }
                DeliveryDisposition::Retry => Ok(false),
            },
        }
    }
}

impl ScheduleEvents for BusScheduleEvents {
    fn maintain(&mut self) -> PortResult<ScheduleEventMaintenance> {
        let mut report = self.reconcile_prepared(BATCH)?;
        report.retry_needed |= self.drain_ready(BATCH)?.retry_needed;
        Ok(report)
    }

    fn drain_ready(&mut self, limit: usize) -> PortResult<ScheduleEventMaintenance> {
        for _ in 0..limit {
            let Some(record) = self
                .ledger
                .ready_schedule_publications(1)
                .map_err(|error| Self::error("schedule_event_ledger", error))?
                .pop()
            else {
                break;
            };
            if !self.drain_one(record)? {
                break;
            }
        }
        let retry_needed = !self
            .ledger
            .ready_schedule_publications(1)
            .map_err(|error| Self::error("schedule_event_ledger", error))?
            .is_empty();
        Ok(ScheduleEventMaintenance { retry_needed })
    }

    fn reconcile_prepared(&mut self, limit: usize) -> PortResult<ScheduleEventMaintenance> {
        let (prepared, more_prepared) = self.prepared_page(limit)?;
        let mut dispositions = Vec::with_capacity(prepared.len());
        for record in prepared {
            let next_cursor = SchedulePublicationCursor::from(&record);
            let disposition = self.reconcile_prepared_record(record)?;
            dispositions.push(disposition);
            // Retryable authority failures arm the bounded backoff but still
            // advance through this bounded page. Otherwise one permanently
            // unreadable document would pin the cursor ahead of every other
            // schedule. A lone failed row is reached again after cursor wrap.
            self.prepared_cursor = Some(next_cursor);
        }
        // Keep walking a recovered backlog after this batch made progress.
        // A prefix made entirely of authority-ambiguous historical definitions
        // must not become an idle polling loop.
        Ok(ScheduleEventMaintenance {
            retry_needed: page_retry_needed(more_prepared, &dispositions),
        })
    }

    fn reconcile_created(&mut self, schedule: ScheduleId) -> PortResult<()> {
        let Some(record) = self.root_record(schedule)? else {
            return Ok(());
        };
        if record.ready_at_ms.is_none() {
            let _ = self.reconcile_one(record)?;
        }
        Ok(())
    }

    fn existing_created(
        &mut self,
        schedule: ScheduleEventRef,
        header: &ScheduleHeader,
    ) -> PortResult<()> {
        if let Some(existing) = self.root_record(schedule.schedule)? {
            if Self::stored_matches_created(&existing, header)? {
                if existing.ready_at_ms.is_none() {
                    self.ready(PreparedScheduleEvent {
                        event: existing.event,
                    })?;
                }
                return Ok(());
            }
            if existing.ready_at_ms.is_none() {
                self.abandon(PreparedScheduleEvent {
                    event: existing.event,
                })?;
            } else {
                return Err(Self::error(
                    "schedule_event_conflict",
                    "the durable created root disagrees with schedule authority",
                ));
            }
        }
        let prepared = self.fresh_created(schedule, header)?;
        self.ready(prepared)
    }

    fn prepare_created(
        &mut self,
        schedule: ScheduleEventRef,
        header: &ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent> {
        if let Some(existing) = self.root_record(schedule.schedule)? {
            if Self::stored_matches_created(&existing, header)? {
                return Ok(PreparedScheduleEvent {
                    event: existing.event,
                });
            }
            return Err(Self::error(
                "schedule_event_conflict",
                "the prepared created root belongs to another authoritative fact",
            ));
        }
        self.fresh_created(schedule, header)
    }

    fn prepare_definition(
        &mut self,
        schedule: ScheduleEventRef,
        definition: &ScheduleDefinition,
    ) -> PortResult<PreparedScheduleEvent> {
        let fact = match schedule_definition_event(definition) {
            ScheduleDefinitionEvent::Updated => SchedulePublicationFact::Updated {
                revision: definition.revision,
            },
            ScheduleDefinitionEvent::Paused(_) => SchedulePublicationFact::Paused {
                revision: definition.revision,
            },
        };
        if let Some(existing) = self
            .ledger
            .schedule_publication_for_fact(schedule.schedule, fact)
            .map_err(|error| Self::error("schedule_event_ledger", error))?
        {
            let root = self.root_record(schedule.schedule)?.ok_or_else(|| {
                Self::error(
                    "schedule_event_root",
                    "the schedule has no durable created root",
                )
            })?;
            if Self::stored_matches_definition(&existing, root.event, definition)? {
                return Ok(PreparedScheduleEvent {
                    event: existing.event,
                });
            }
            return Err(Self::error(
                "schedule_event_conflict",
                "the definition revision already has another event fact",
            ));
        }
        let root = self.root_record(schedule.schedule)?.ok_or_else(|| {
            Self::error(
                "schedule_event_root",
                "the schedule has no durable created root",
            )
        })?;
        let chain = ScheduleChain::rejoin(&self.ctx, Self::chain_ref(schedule), root.event);
        let stamp = Stamp::fresh(definition.effective_at_ms);
        let envelope = match schedule_definition_event(definition) {
            ScheduleDefinitionEvent::Updated => chain.updated(
                stamp,
                ScheduleUpdated {
                    revision: definition.revision,
                },
            ),
            ScheduleDefinitionEvent::Paused(reason) => chain.paused(
                stamp,
                SchedulePaused {
                    revision: definition.revision,
                    reason,
                },
            ),
        };
        self.prepare_envelope(schedule, fact, envelope)
    }

    fn prepare_occurrence(
        &mut self,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        self.prepare_claim(fact)
    }

    fn reconcile_occurrence(
        &mut self,
        schedule: ScheduleEventRef,
        prepared: Option<PreparedScheduleEvent>,
        claim: &SlotClaim,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        let actual = Self::occurrence_from_claim(schedule, claim)
            .or_else(|| Self::pending_fired_from_claim(schedule, claim));
        let Some(actual) = actual else {
            if let Some(prepared) = prepared {
                self.abandon(prepared)?;
            }
            return Ok(None);
        };
        if let Some(prepared) = prepared {
            let record = self
                .ledger
                .schedule_publication(prepared.event)
                .map_err(|error| Self::error("schedule_event_ledger", error))?;
            if let Some(record) = record {
                let root = self.root_record(schedule.schedule)?.ok_or_else(|| {
                    Self::error(
                        "schedule_event_root",
                        "the schedule has no durable created root",
                    )
                })?;
                if Self::stored_matches_occurrence(&record, root.event, actual)? {
                    return Ok(Some(prepared));
                }
            }
            self.abandon(prepared)?;
        }
        self.prepare_claim(actual)
    }

    fn ready(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()> {
        self.ledger
            .mark_schedule_publication_ready(prepared.event)
            .map(|_| ())
            .map_err(|error| Self::error("schedule_event_ready", error))
    }

    fn abandon(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()> {
        self.ledger
            .abandon_schedule_publication(prepared.event)
            .map(|_| ())
            .map_err(|error| Self::error("schedule_event_abandon", error))
    }
}

#[cfg(test)]
mod tests;
