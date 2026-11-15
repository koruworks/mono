// SPDX-License-Identifier: MPL-2.0
use super::super::seed::tests::fixture;
use super::*;

fn document() -> ScheduleDocument {
    let (header, name) = fixture();
    ScheduleDocument::create_named(&header, &name).unwrap()
}
fn activate(doc: &ScheduleDocument) {
    let mut header = doc.header().unwrap();
    header.definition.revision += 1;
    header.definition.effective_at_ms += 1;
    header.definition.lifecycle = ScheduleLifecycle::Active;
    doc.update_definition(header.initial_owner, &header.definition)
        .unwrap();
}

#[test]
fn routine_detail_shares_overview_identity_state_projection() {
    let doc = document();
    let first = doc.routine_detail().unwrap();
    assert_eq!(first.overview, doc.routine_overview().unwrap());
    assert_eq!(
        first.recurrence,
        doc.header().unwrap().definition.recurrence
    );
    let mut header = doc.header().unwrap();
    header.definition.revision += 1;
    header.definition.effective_at_ms += 100;
    header.definition.recurrence = koru_delegate_schedule::RecurrenceRule::zoned(
        "0 9 * * *".parse().unwrap(),
        "Europe/Berlin",
    )
    .unwrap();
    doc.update_definition(header.initial_owner, &header.definition)
        .unwrap();
    let current = doc.routine_detail().unwrap();
    assert_eq!(current.effective_at_ms, header.definition.effective_at_ms);
    assert_eq!(current.recurrence, header.definition.recurrence);
    assert_eq!(current.overview, doc.routine_overview().unwrap());
    assert_ne!(current.recurrence, first.recurrence);
}
#[test]
fn routine_detail_bounds_handoffs_without_firings() {
    let doc = document();
    // A malformed append-only firing is not part of this current projection.
    doc.doc.get_list(FIRINGS).push(true).unwrap();
    doc.doc.commit();
    assert!(doc.routine_detail().is_ok());
    for _ in 0..=HANDOFF_COUNT {
        doc.handoff_list().push(true).unwrap();
    }
    doc.doc.commit();
    assert!(doc.routine_detail().is_err());
    doc.root().insert(K_RETIRED, true).unwrap();
    doc.doc.commit();
    assert!(matches!(
        doc.routine_detail().unwrap().overview.state,
        ScheduleState::Retired
    ));
}

#[test]
fn routine_overview_preserves_state_precedence_and_current_name_revision() {
    let doc = document();
    let owner = doc.header().unwrap().initial_owner;
    let next = DeviceId::new();
    let handoff = HandoffId::mint();
    doc.request_handoff(handoff, next, next, 2).unwrap();
    assert!(matches!(
        doc.routine_overview().unwrap().state,
        ScheduleState::Paused { .. }
    ));
    activate(&doc);
    let name = ScheduleName::new("Current label").unwrap();
    doc.rename(owner, &doc.name().unwrap(), &name).unwrap();
    let observed = doc.routine_overview().unwrap();
    assert_eq!(observed.name, name);
    assert_eq!(observed.definition_revision, 2);
    assert_eq!(observed.state, doc.state().unwrap());
    assert!(
        matches!(observed.state, ScheduleState::HandoffPending { requests, .. } if requests == vec![handoff])
    );
    doc.confirm_handoff(handoff, owner, 3).unwrap();
    assert_eq!(
        doc.routine_overview().unwrap().state,
        ScheduleState::Active { owner: next }
    );
    doc.retire(next, 4).unwrap();
    assert_eq!(
        doc.routine_overview().unwrap().state,
        ScheduleState::Retired
    );
    // Existing state() gives marker presence priority, irrespective of body.
    doc.root().insert(K_RETIRED, false).unwrap();
    doc.handoff_list().push(true).unwrap();
    doc.doc.commit();
    assert_eq!(doc.routine_overview().unwrap().state, doc.state().unwrap());
}

#[test]
fn routine_overview_bounds_complete_handoff_set_without_tail_inference() {
    let doc = document();
    activate(&doc);
    let owner = doc.header().unwrap().initial_owner;
    let first = HandoffId::mint();
    let requested = HandoffEvent::Requested {
        handoff: first,
        previous: None,
        requested_by: owner,
        from: owner,
        to: DeviceId::new(),
        at_ms: 2,
    };
    doc.handoff_list()
        .push(encode(&requested, HANDOFFS).unwrap())
        .unwrap();
    // Unrelated later confirmations cannot hide the earliest unresolved request.
    for _ in 1..HANDOFF_COUNT {
        doc.handoff_list()
            .push(
                encode(
                    &HandoffEvent::Confirmed {
                        handoff: HandoffId::mint(),
                        confirmed_by: owner,
                        at_ms: 3,
                    },
                    HANDOFFS,
                )
                .unwrap(),
            )
            .unwrap();
    }
    doc.doc.commit();
    assert!(matches!(doc.routine_overview().unwrap().state,
        ScheduleState::HandoffPending { requests, .. } if requests == vec![first]));
    doc.handoff_list()
        .push(encode(&requested, HANDOFFS).unwrap())
        .unwrap();
    doc.doc.commit();
    assert!(doc.routine_overview().is_err());
    assert!(
        doc.state().is_ok(),
        "overview ceiling does not invalidate storage"
    );
}

#[test]
fn routine_overview_refuses_oversized_or_malformed_handoff_value() {
    for raw in [
        LoroValue::from("x".repeat(HANDOFF_BYTES + 1)),
        LoroValue::Bool(true),
    ] {
        let doc = document();
        doc.handoff_list().push(raw).unwrap();
        doc.doc.commit();
        assert!(doc.routine_overview().is_err());
    }
}

#[test]
fn routine_overview_counts_raw_bytes_and_depth_before_expansion() {
    let escaped = LoroValue::from("a\n\"\\");
    let exact = serde_json::to_vec(&serde_json::Value::from(escaped.clone()))
        .unwrap()
        .len();
    assert!(Budget(exact).value(&escaped, 0).is_ok());
    assert!(Budget(exact - 1).value(&escaped, 0).is_err());
    let full = LoroValue::from("x".repeat(PROJECTION_BYTES - 2));
    assert!(Budget(PROJECTION_BYTES).value(&full, 0).is_ok());
    assert!(Budget(PROJECTION_BYTES - 1).value(&full, 0).is_err());
    let mut nested = LoroValue::Null;
    for _ in 0..VALUE_DEPTH {
        nested = LoroValue::from(vec![nested]);
    }
    assert!(Budget(PROJECTION_BYTES).value(&nested, 0).is_ok());
    nested = LoroValue::from(vec![nested]);
    assert!(Budget(PROJECTION_BYTES).value(&nested, 0).is_err());
    let doc = document();
    doc.root().insert(K_DEFINITION, nested).unwrap();
    doc.doc.commit();
    assert!(doc.routine_overview().is_err());
}

#[test]
fn routine_overview_bounds_raw_name_and_definition_but_retirement_is_presence_only() {
    let doc = document();
    doc.root().insert(K_NAME, "🙂".repeat(128)).unwrap();
    doc.doc.commit();
    assert_eq!(doc.routine_overview().unwrap().name.char_count(), 128);
    for key in [K_NAME, K_DEFINITION] {
        let doc = document();
        doc.root()
            .insert(key, "x".repeat(PROJECTION_BYTES + 1))
            .unwrap();
        doc.doc.commit();
        assert!(doc.routine_overview().is_err(), "{key}");
    }
    for container in [false, true] {
        let doc = document();
        if container {
            doc.root()
                .insert_container(K_RETIRED, LoroMap::new())
                .unwrap();
        } else {
            doc.root()
                .insert(K_RETIRED, "x".repeat(PROJECTION_BYTES + 1))
                .unwrap();
        }
        doc.handoff_list().push("malformed").unwrap();
        doc.doc.commit();
        assert_eq!(
            doc.routine_overview().unwrap().state,
            ScheduleState::Retired
        );
    }
}

#[test]
fn routine_overview_never_decodes_firing_history() {
    let doc = document();
    let expected = doc.routine_overview().unwrap();
    doc.firing_list()
        .push("x".repeat(PROJECTION_BYTES + 1))
        .unwrap();
    for _ in 0..1024 {
        doc.firing_list().push(false).unwrap();
    }
    doc.doc.commit();
    assert!(doc.firings().is_err());
    assert_eq!(doc.routine_overview().unwrap(), expected);
}

#[test]
fn routine_overview_requires_schema_two_and_coherent_required_identity() {
    for mutation in 0..5 {
        let doc = document();
        match mutation {
            0 => {
                doc.root().insert(K_SCHEMA, 1).unwrap();
            }
            1 => {
                doc.root().insert(K_SCHEMA, 3).unwrap();
            }
            2 => {
                doc.root().delete(K_NAME).unwrap();
            }
            3 => {
                doc.root()
                    .insert(K_OWNER, encode(&DeviceId::new(), K_OWNER).unwrap())
                    .unwrap();
            }
            _ => {
                let mut definition = doc.header().unwrap().definition;
                definition.revision = 0;
                doc.root()
                    .insert(K_DEFINITION, encode(&definition, K_DEFINITION).unwrap())
                    .unwrap();
            }
        }
        doc.doc.commit();
        assert!(doc.routine_overview().is_err(), "mutation {mutation}");
    }
}

#[test]
fn routine_overview_reports_applied_state_not_absence_of_parked_imports() {
    let doc = document();
    let expected = doc.routine_overview().unwrap();
    let older = doc.doc.state_frontiers();
    let remote = doc.doc.fork();
    remote
        .get_map(ROOT)
        .insert(K_NAME, "Missing ancestor")
        .unwrap();
    remote.commit();
    let before = remote.oplog_vv();
    remote
        .get_map(ROOT)
        .insert(K_NAME, "Dependent future")
        .unwrap();
    remote.commit();
    let later = remote.export(ExportMode::updates(&before)).unwrap();
    assert!(doc.doc.import(&later).unwrap().pending.is_some());
    assert_eq!(doc.routine_overview().unwrap(), expected);
    doc.doc
        .get_map(ROOT)
        .insert(K_NAME, "Applied locally")
        .unwrap();
    doc.doc.commit();
    doc.doc.checkout(&older).unwrap();
    assert!(doc.doc.is_detached());
    assert!(doc.routine_overview().is_err());
}
