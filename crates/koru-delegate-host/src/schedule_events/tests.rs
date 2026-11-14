// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use koru_delegate_core::{ActorRef, DeviceId, DocumentId, ProfileId, ScheduleId, WorkspaceId};
use koru_delegate_events::{
    ScheduleChain, ScheduleCreated, ScheduleFired, SchedulePaused, ScheduleRef, ScheduleSkipReason,
    ScheduleSkipped, ScheduleUpdated, Stamp,
};
use koru_delegate_schedule::{
    DelegationTemplate, RecurrenceRule, ScheduleContextFailureStage, ScheduleDefinition,
    ScheduleFiringOutcome, ScheduleHeader, ScheduleLifecycle, SchedulePauseReason,
};
use koru_delegate_sqlite::{
    Clock, RunsDb, SchedulePublicationFact, SchedulePublicationRef, SlotClaim, SlotOutcome,
    SlotState,
};
use koru_events::{Actor, AttrValue, EmitContext, EventId};

use super::{
    BATCH, BusScheduleEvents, DeliveryDisposition, ReconcileDisposition, definition_disposition,
    page_retry_needed, rejected_delivery_disposition,
};
mod phase;
mod routine;
use crate::ledger::SharedLedger;
use koru_delegate_runtime::ScheduleEvents;

struct MutableClock(Arc<AtomicI64>);

impl Clock for MutableClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn header(schedule: ScheduleId, profile: ProfileId) -> ScheduleHeader {
    ScheduleHeader {
        schedule,
        created_at_ms: 17,
        initial_owner: DeviceId::new(),
        definition: ScheduleDefinition {
            routine: None,
            revision: 1,
            effective_at_ms: 17,
            template: DelegationTemplate {
                requested_by: ActorRef::Person { profile },
                objective: koru_delegate_core::Objective::new("never in the event").unwrap(),
                agent: koru_delegate_core::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: koru_delegate_core::WorkspaceScope::Temporary,
                grants: BTreeSet::new(),
                context: Vec::new(),
            },
            recurrence: RecurrenceRule::floating("0 9 * * *".parse().unwrap()),
            lifecycle: ScheduleLifecycle::Active,
        },
    }
}

fn schedule_ref(schedule: ScheduleId) -> ScheduleRef {
    ScheduleRef {
        schedule,
        workspace: WorkspaceId::new(),
        document: DocumentId::from_uuid(schedule.as_uuid()),
    }
}

fn publication_ref(schedule: ScheduleRef) -> SchedulePublicationRef {
    SchedulePublicationRef {
        schedule: schedule.schedule,
        workspace: schedule.workspace,
        document: schedule.document,
    }
}

fn store_child(
    child: koru_events::Envelope,
    root: koru_events::Envelope,
    schedule: ScheduleRef,
    fact: SchedulePublicationFact,
) -> (koru_delegate_sqlite::SchedulePublicationRecord, EventId) {
    let temp = tempfile::tempdir().unwrap();
    let mut db = RunsDb::open(&temp.path().join("runs.db")).unwrap();
    db.prepare_schedule_publication(
        root.event_id,
        publication_ref(schedule),
        SchedulePublicationFact::Created { revision: 1 },
        &serde_json::to_string(&root).unwrap(),
    )
    .unwrap();
    db.prepare_schedule_publication(
        child.event_id,
        publication_ref(schedule),
        fact,
        &serde_json::to_string_pretty(&child).unwrap(),
    )
    .unwrap();
    (
        db.schedule_publication(child.event_id).unwrap().unwrap(),
        root.event_id,
    )
}

#[test]
fn reconciliation_preserves_the_stored_origin_and_exact_body() {
    let root = tempfile::tempdir().unwrap();
    let mut db = RunsDb::open(&root.path().join("runs.db")).unwrap();
    let schedule = ScheduleId::mint();
    let workspace = WorkspaceId::new();
    let document = DocumentId::from_uuid(schedule.as_uuid());
    let profile = ProfileId::new();
    let header = header(schedule, profile);
    let original = EmitContext {
        session_id: koru_events::SessionId::mint(),
        device_id: Some(koru_events::DeviceId::mint()),
        profile_id: Some(koru_events::ProfileId(profile.as_uuid())),
        actor: Actor::system("ignored-default"),
    };
    let event = EventId::mint();
    let (envelope, _) = ScheduleChain::created(
        &original,
        ScheduleRef {
            schedule,
            workspace,
            document,
        },
        Stamp {
            event_id: event,
            occurred_at_ms: header.created_at_ms,
        },
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    let exact = serde_json::to_string_pretty(&envelope).unwrap();
    db.prepare_schedule_publication(
        event,
        SchedulePublicationRef {
            schedule,
            workspace,
            document,
        },
        SchedulePublicationFact::Created { revision: 1 },
        &exact,
    )
    .unwrap();
    let record = db.schedule_publication(event).unwrap().unwrap();

    assert!(BusScheduleEvents::stored_matches_created(&record, &header).unwrap());
    assert_eq!(record.body.as_str(), exact, "reconcile never rewrites JSON");
    let reparsed: koru_events::Envelope = serde_json::from_str(record.body.as_str()).unwrap();
    assert_eq!(reparsed.origin, envelope.origin);
    assert_eq!(reparsed.scope.profile_id, envelope.scope.profile_id);
}

#[test]
fn schedule_created_body_is_structurally_content_free() {
    let schedule = ScheduleId::mint();
    let workspace = WorkspaceId::new();
    let document = DocumentId::from_uuid(schedule.as_uuid());
    let header = header(schedule, ProfileId::new());
    let (envelope, _) = ScheduleChain::created(
        &EmitContext::system("delegate"),
        ScheduleRef {
            schedule,
            workspace,
            document,
        },
        Stamp::fresh(header.created_at_ms),
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    let body = serde_json::to_string(&envelope).unwrap();
    assert!(!body.contains("never in the event"));
    assert!(!body.contains("0 9 * * *"));
    assert!(!body.contains("temporary"));
}

#[test]
fn all_definition_lifecycles_are_classified_and_match_their_stored_envelope() {
    let schedule = ScheduleId::mint();
    let schedule_ref = schedule_ref(schedule);
    let mut header = header(schedule, ProfileId::new());
    let original = EmitContext {
        session_id: koru_events::SessionId::mint(),
        device_id: Some(koru_events::DeviceId::mint()),
        profile_id: Some(koru_events::ProfileId::mint()),
        actor: Actor::system("old-session"),
    };
    let (root, chain) = ScheduleChain::created(
        &original,
        schedule_ref,
        Stamp::fresh(header.created_at_ms),
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );

    header.definition.revision = 2;
    header.definition.effective_at_ms = 29;
    header.definition.lifecycle = ScheduleLifecycle::Active;
    assert_eq!(
        koru_delegate_runtime::schedule_definition_event(&header.definition),
        koru_delegate_runtime::ScheduleDefinitionEvent::Updated
    );
    let updated = chain.updated(
        Stamp::fresh(header.definition.effective_at_ms),
        ScheduleUpdated { revision: 2 },
    );
    let (record, root_id) = store_child(
        updated,
        root.clone(),
        schedule_ref,
        SchedulePublicationFact::Updated { revision: 2 },
    );
    assert!(
        BusScheduleEvents::stored_matches_definition(&record, root_id, &header.definition).unwrap()
    );

    header.definition.revision = 3;
    header.definition.effective_at_ms = 31;
    header.definition.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::ContextUnavailable {
            item: 4,
            stage: ScheduleContextFailureStage::Retention,
        },
    };
    assert!(matches!(
        koru_delegate_runtime::schedule_definition_event(&header.definition),
        koru_delegate_runtime::ScheduleDefinitionEvent::Paused(
            SchedulePauseReason::ContextUnavailable {
                item: 4,
                stage: ScheduleContextFailureStage::Retention
            }
        )
    ));
    let paused = chain.paused(
        Stamp::fresh(header.definition.effective_at_ms),
        SchedulePaused {
            revision: 3,
            reason: SchedulePauseReason::ContextUnavailable {
                item: 4,
                stage: ScheduleContextFailureStage::Retention,
            },
        },
    );
    let (record, root_id) = store_child(
        paused,
        root,
        schedule_ref,
        SchedulePublicationFact::Paused { revision: 3 },
    );
    assert!(
        BusScheduleEvents::stored_matches_definition(&record, root_id, &header.definition).unwrap()
    );
    assert_eq!(
        record.body.as_str(),
        serde_json::to_string_pretty(&BusScheduleEvents::parse(&record).unwrap()).unwrap()
    );
}

#[test]
fn fired_missed_busy_and_retired_claims_have_the_exact_public_shape() {
    let schedule = ScheduleId::mint();
    let schedule_ref = schedule_ref(schedule);
    let event_ref = koru_delegate_runtime::ScheduleEventRef {
        schedule,
        workspace: schedule_ref.workspace,
        document: schedule_ref.document,
    };
    let original = EmitContext {
        session_id: koru_events::SessionId::mint(),
        device_id: Some(koru_events::DeviceId::mint()),
        profile_id: Some(koru_events::ProfileId::mint()),
        actor: Actor::system("old-session"),
    };
    let profile = ProfileId::new();
    let header = header(schedule, profile);
    let (root, chain) = ScheduleChain::created(
        &original,
        schedule_ref,
        Stamp::fresh(header.created_at_ms),
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    let planned = koru_delegate_core::RunId::mint();
    let holder = koru_delegate_core::RunId::mint();
    let fired_claim = SlotClaim {
        schedule,
        occurrence_ms: 100,
        revision: 2,
        run: planned,
        state: SlotState::Settled(SlotOutcome::Fired),
        busy_holder: None,
        claimed_at_ms: 101,
    };
    let fired_fact = BusScheduleEvents::occurrence_from_claim(event_ref, &fired_claim).unwrap();
    assert_eq!(fired_fact.outcome, ScheduleFiringOutcome::Fired);
    let fired = chain.fired(
        Stamp::fresh(fired_claim.claimed_at_ms),
        ScheduleFired {
            revision: fired_claim.revision,
            occurrence_ms: fired_claim.occurrence_ms,
            run: planned,
        },
    );
    let (record, root_id) = store_child(
        fired,
        root.clone(),
        schedule_ref,
        SchedulePublicationFact::Fired {
            revision: 2,
            occurrence_ms: 100,
        },
    );
    assert!(BusScheduleEvents::stored_matches_occurrence(&record, root_id, fired_fact).unwrap());
    let fired_envelope = BusScheduleEvents::parse(&record).unwrap();
    assert_eq!(fired_envelope.occurred_at_ms, fired_claim.claimed_at_ms);
    assert_eq!(
        fired_envelope.attrs.get("run"),
        Some(&AttrValue::Id(planned.as_uuid()))
    );
    let pending_claim = SlotClaim {
        state: SlotState::Claimed,
        ..fired_claim
    };
    assert!(BusScheduleEvents::occurrence_from_claim(event_ref, &pending_claim).is_none());
    assert_eq!(
        BusScheduleEvents::pending_fired_from_claim(event_ref, &pending_claim)
            .unwrap()
            .run,
        planned
    );

    let missed_claim = SlotClaim {
        occurrence_ms: 200,
        state: SlotState::Settled(SlotOutcome::SkippedMissed),
        claimed_at_ms: 201,
        ..fired_claim
    };
    let missed_fact = BusScheduleEvents::occurrence_from_claim(event_ref, &missed_claim).unwrap();
    let missed = chain.skipped(
        Stamp::fresh(missed_claim.claimed_at_ms),
        ScheduleSkipped {
            revision: missed_claim.revision,
            occurrence_ms: missed_claim.occurrence_ms,
            reason: ScheduleSkipReason::missed(),
        },
    );
    let (record, root_id) = store_child(
        missed,
        root.clone(),
        schedule_ref,
        SchedulePublicationFact::Skipped {
            revision: 2,
            occurrence_ms: 200,
        },
    );
    assert!(BusScheduleEvents::stored_matches_occurrence(&record, root_id, missed_fact).unwrap());
    let missed_envelope = BusScheduleEvents::parse(&record).unwrap();
    assert!(!missed_envelope.attrs.contains_key("run"));
    assert!(!missed_envelope.attrs.contains_key("holder"));
    assert!(!record.body.as_str().contains(&planned.to_storage()));

    let busy_claim = SlotClaim {
        occurrence_ms: 300,
        run: koru_delegate_core::RunId::mint(),
        state: SlotState::Settled(SlotOutcome::SkippedBusy),
        busy_holder: Some(holder),
        claimed_at_ms: 301,
        ..fired_claim
    };
    let busy_fact = BusScheduleEvents::occurrence_from_claim(event_ref, &busy_claim).unwrap();
    let busy = chain.skipped(
        Stamp::fresh(busy_claim.claimed_at_ms),
        ScheduleSkipped {
            revision: busy_claim.revision,
            occurrence_ms: busy_claim.occurrence_ms,
            reason: ScheduleSkipReason::busy(holder),
        },
    );
    let (record, root_id) = store_child(
        busy,
        root,
        schedule_ref,
        SchedulePublicationFact::Skipped {
            revision: 2,
            occurrence_ms: 300,
        },
    );
    assert!(BusScheduleEvents::stored_matches_occurrence(&record, root_id, busy_fact).unwrap());
    let busy_envelope = BusScheduleEvents::parse(&record).unwrap();
    assert_eq!(
        busy_envelope.attrs.get("holder"),
        Some(&AttrValue::Id(holder.as_uuid()))
    );
    assert!(!busy_envelope.attrs.contains_key("run"));
    assert!(!record.body.as_str().contains(&busy_claim.run.to_storage()));

    let retired = SlotClaim {
        state: SlotState::Settled(SlotOutcome::SkippedRetired),
        ..busy_claim
    };
    assert!(BusScheduleEvents::occurrence_from_claim(event_ref, &retired).is_none());
    assert!(!record.body.as_str().contains("delegate.scope.skipped"));
}

#[test]
fn advanced_definition_authority_neither_emits_abandons_nor_polls() {
    assert_eq!(
        definition_disposition(2, 3),
        ReconcileDisposition::AwaitAuthorityWake
    );
}

#[test]
fn store_ack_after_restart_moves_the_original_created_event_to_ready() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger.clone());
    let schedule = ScheduleId::mint();
    let header = header(schedule, ProfileId::new());
    let reference = schedule_ref(schedule);
    let original = EmitContext {
        session_id: koru_events::SessionId::mint(),
        device_id: Some(koru_events::DeviceId::mint()),
        profile_id: Some(koru_events::ProfileId::mint()),
        actor: Actor::system("before-restart"),
    };
    let event = EventId::mint();
    let (envelope, _) = ScheduleChain::created(
        &original,
        reference,
        Stamp {
            event_id: event,
            occurred_at_ms: header.created_at_ms,
        },
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    let exact = serde_json::to_string_pretty(&envelope).unwrap();
    ledger
        .prepare_schedule_publication(
            event,
            publication_ref(reference),
            SchedulePublicationFact::Created { revision: 1 },
            &exact,
        )
        .unwrap();
    let prepared = ledger.schedule_publications(schedule).unwrap().remove(0);

    assert_eq!(
        events
            .reconcile_created_authority(&prepared, &header)
            .unwrap(),
        ReconcileDisposition::Settled
    );
    let ready = ledger.schedule_publications(schedule).unwrap().remove(0);
    assert_eq!(ready.event, event);
    assert!(ready.ready_at_ms.is_some());
    assert_eq!(ready.body.as_str(), exact);
}

#[test]
fn advanced_definition_keeps_the_prepared_row_out_of_the_ready_queue() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger.clone());
    let schedule_id = ScheduleId::mint();
    let mut header = header(schedule_id, ProfileId::new());
    let reference = koru_delegate_runtime::ScheduleEventRef {
        schedule: schedule_id,
        workspace: WorkspaceId::new(),
        document: DocumentId::from_uuid(schedule_id.as_uuid()),
    };
    events.existing_created(reference, &header).unwrap();
    header.definition.revision = 2;
    header.definition.effective_at_ms = 25;
    header.definition.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    let prepared = events
        .prepare_definition(reference, &header.definition)
        .unwrap();
    let record = ledger
        .schedule_publications(schedule_id)
        .unwrap()
        .into_iter()
        .find(|row| row.event == prepared.event)
        .unwrap();
    let mut current = header.definition.clone();
    current.revision = 3;
    current.effective_at_ms = 30;
    current.lifecycle = ScheduleLifecycle::Active;

    assert_eq!(
        events
            .reconcile_definition_authority(&record, 2, &current)
            .unwrap(),
        ReconcileDisposition::AwaitAuthorityWake
    );
    let retained = ledger
        .schedule_publications(schedule_id)
        .unwrap()
        .into_iter()
        .find(|row| row.event == prepared.event)
        .unwrap();
    assert!(retained.ready_at_ms.is_none());
    assert!(retained.acknowledged_at_ms.is_none());
}

#[test]
fn ambiguous_oldest_batch_rotates_to_a_later_actionable_row_on_the_next_real_wake() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    for _ in 0..=BATCH {
        let schedule = ScheduleId::mint();
        let mut header = header(schedule, ProfileId::new());
        let reference = schedule_ref(schedule);
        let (root, chain) = ScheduleChain::created(
            &EmitContext::system("delegate"),
            reference,
            Stamp::fresh(header.created_at_ms),
            ScheduleCreated {
                requested_by: header.definition.template.requested_by,
                revision: 1,
            },
        );
        ledger
            .prepare_schedule_publication(
                root.event_id,
                publication_ref(reference),
                SchedulePublicationFact::Created { revision: 1 },
                &serde_json::to_string(&root).unwrap(),
            )
            .unwrap();
        ledger
            .mark_schedule_publication_ready(root.event_id)
            .unwrap();
        ledger
            .acknowledge_schedule_publication(root.event_id)
            .unwrap();
        header.definition.revision = 2;
        header.definition.effective_at_ms = 20;
        let child = chain.updated(
            Stamp::fresh(header.definition.effective_at_ms),
            ScheduleUpdated { revision: 2 },
        );
        ledger
            .prepare_schedule_publication(
                child.event_id,
                publication_ref(reference),
                SchedulePublicationFact::Updated { revision: 2 },
                &serde_json::to_string(&child).unwrap(),
            )
            .unwrap();
    }
    let ordered = ledger.prepared_schedule_publications(BATCH + 1).unwrap();
    let later = ordered[BATCH].event;
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger);

    let (oldest, more) = events.prepared_page(BATCH).unwrap();
    assert_eq!(oldest.len(), BATCH);
    assert!(more);
    assert!(!page_retry_needed(
        more,
        &[ReconcileDisposition::AwaitAuthorityWake; BATCH]
    ));
    events.prepared_cursor = oldest.last().map(Into::into);

    let (next_wake, _) = events.prepared_page(BATCH).unwrap();
    assert_eq!(next_wake.len(), 1);
    assert_eq!(next_wake[0].event, later);
}

#[test]
fn publisher_reconciliation_waits_for_the_live_authority_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    let schedule = ScheduleId::mint();
    let header = header(schedule, ProfileId::new());
    let reference = schedule_ref(schedule);
    let (envelope, _) = ScheduleChain::created(
        &EmitContext::system("delegate"),
        reference,
        Stamp::fresh(header.created_at_ms),
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    ledger
        .prepare_schedule_publication(
            envelope.event_id,
            publication_ref(reference),
            SchedulePublicationFact::Created { revision: 1 },
            &serde_json::to_string(&envelope).unwrap(),
        )
        .unwrap();
    let gate = std::sync::Arc::new(super::ScheduleEventAuthorityGate::new());
    let authority = gate.enter().unwrap();
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::publisher(client, ledger, std::sync::Arc::clone(&gate));
    let (finished, wait) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let result = events.reconcile_prepared(1);
        let _ = finished.send(result);
    });

    assert!(matches!(
        wait.recv_timeout(std::time::Duration::from_millis(30)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    drop(authority);
    assert!(
        wait.recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    thread.join().unwrap();
}

#[test]
fn transient_authority_retry_is_revisited_after_the_cursor_wraps() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    let schedule = ScheduleId::mint();
    let header = header(schedule, ProfileId::new());
    let reference = schedule_ref(schedule);
    let (envelope, _) = ScheduleChain::created(
        &EmitContext::system("delegate"),
        reference,
        Stamp::fresh(header.created_at_ms),
        ScheduleCreated {
            requested_by: header.definition.template.requested_by,
            revision: 1,
        },
    );
    ledger
        .prepare_schedule_publication(
            envelope.event_id,
            publication_ref(reference),
            SchedulePublicationFact::Created { revision: 1 },
            &serde_json::to_string(&envelope).unwrap(),
        )
        .unwrap();
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger);

    assert!(events.reconcile_prepared(1).unwrap().retry_needed);
    let first_cursor = events.prepared_cursor;
    assert!(first_cursor.is_some());
    assert!(events.reconcile_prepared(1).unwrap().retry_needed);
    assert_eq!(events.prepared_cursor, first_cursor);
}

#[test]
fn failed_authority_row_does_not_starve_a_later_actionable_row() {
    let temp = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicI64::new(1_000));
    let mut db = RunsDb::open_with_clock(
        &temp.path().join("runs.db"),
        Box::new(MutableClock(Arc::clone(&now))),
    )
    .unwrap();

    let failed_schedule = ScheduleId::mint();
    let failed_header = header(failed_schedule, ProfileId::new());
    let failed_ref = schedule_ref(failed_schedule);
    let (failed, _) = ScheduleChain::created(
        &EmitContext::system("delegate"),
        failed_ref,
        Stamp::fresh(failed_header.created_at_ms),
        ScheduleCreated {
            requested_by: failed_header.definition.template.requested_by,
            revision: 1,
        },
    );
    db.prepare_schedule_publication(
        failed.event_id,
        publication_ref(failed_ref),
        SchedulePublicationFact::Created { revision: 1 },
        &serde_json::to_string(&failed).unwrap(),
    )
    .unwrap();

    // queued_at_ms is the cursor's primary key. Keep the actionable row
    // observably later without depending on random EventId ordering.
    now.store(1_001, Ordering::SeqCst);
    let actionable_schedule = ScheduleId::mint();
    let actionable = EventId::mint();
    db.prepare_schedule_publication(
        actionable,
        publication_ref(schedule_ref(actionable_schedule)),
        SchedulePublicationFact::Created { revision: 1 },
        "{}",
    )
    .unwrap();

    let ledger = SharedLedger::new(db);
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger.clone());

    let report = events.reconcile_prepared(2).unwrap();

    assert!(
        report.retry_needed,
        "the failed authority row keeps backoff armed"
    );
    assert!(
        ledger
            .schedule_publication(failed.event_id)
            .unwrap()
            .unwrap()
            .acknowledged_at_ms
            .is_none(),
        "the retryable row remains prepared"
    );
    assert!(
        ledger.schedule_publication(actionable).unwrap().is_none(),
        "the later conclusively invalid row is abandoned on the same bounded pass"
    );
}

#[test]
fn disabled_and_permanent_delivery_finish_while_transient_delivery_retries() {
    assert_eq!(
        rejected_delivery_disposition(false, None),
        DeliveryDisposition::Acknowledge
    );
    assert_eq!(
        rejected_delivery_disposition(true, Some(koru_store_core::Code::InvalidRequest)),
        DeliveryDisposition::Acknowledge
    );
    assert_eq!(
        rejected_delivery_disposition(true, Some(koru_store_core::Code::StoreUnreachable)),
        DeliveryDisposition::Retry
    );
}

#[test]
fn live_and_repair_paths_converge_on_the_first_semantic_row() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = SharedLedger::new(RunsDb::open(&temp.path().join("runs.db")).unwrap());
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger.clone());
    let schedule = ScheduleId::mint();
    let header = header(schedule, ProfileId::new());
    let schedule = koru_delegate_runtime::ScheduleEventRef {
        schedule,
        workspace: WorkspaceId::new(),
        document: DocumentId::from_uuid(schedule.as_uuid()),
    };
    events.existing_created(schedule, &header).unwrap();
    let claim = SlotClaim {
        schedule: schedule.schedule,
        occurrence_ms: 41,
        revision: 1,
        run: koru_delegate_core::RunId::mint(),
        state: SlotState::Settled(SlotOutcome::Fired),
        busy_holder: None,
        claimed_at_ms: 43,
    };

    let live = events
        .reconcile_occurrence(schedule, None, &claim)
        .unwrap()
        .unwrap();
    let body = ledger
        .schedule_publications(schedule.schedule)
        .unwrap()
        .into_iter()
        .find(|row| row.event == live.event)
        .unwrap()
        .body
        .as_str()
        .to_owned();
    let repair = events
        .reconcile_occurrence(schedule, None, &claim)
        .unwrap()
        .unwrap();

    assert_eq!(repair.event, live.event);
    let repaired = ledger
        .schedule_publications(schedule.schedule)
        .unwrap()
        .into_iter()
        .find(|row| row.event == repair.event)
        .unwrap();
    assert_eq!(repaired.body.as_str(), body);
    assert_eq!(
        ledger
            .schedule_publications(schedule.schedule)
            .unwrap()
            .len(),
        2,
        "one created root and one occurrence row"
    );
}
