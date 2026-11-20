// SPDX-License-Identifier: MPL-2.0

//! The shape of proposal §1.3, on the document rather than on paper: which
//! root containers exist, which keys the `run` map holds, and the one key
//! that must never appear.

use loro::LoroValue;

use super::*;

fn keys(value: &LoroValue) -> BTreeSet<String> {
    match value {
        LoroValue::Map(map) => map.keys().cloned().collect(),
        other => panic!("expected a map, got {other:?}"),
    }
}

/// **F3.** The projection reads `meta.schema_version` as the node model's
/// version on every commit and marks a document it does not understand
/// incompatible. A run's version is `run.schema`, and nothing is written
/// under `meta` — not the version, not anything.
#[test]
fn the_version_is_run_schema_and_meta_is_never_written() {
    let run = a_run_with(3);
    run.set_status(&RunStatus::running()).unwrap();
    run.register_result(&result(10)).unwrap();
    run.record_model(&identity(0x30)).unwrap();
    run.record_usage(&usage(1, 0, 100)).unwrap();
    run.set_rating(&rating()).unwrap();
    run.refresh_economics(2_000).unwrap();

    assert_eq!(
        run.schema_version(),
        Some(RUN_INTERACTIVE_DOCUMENT_SCHEMA_VERSION)
    );
    let roots = keys(&run.doc().get_deep_value());
    assert_eq!(
        roots,
        BTreeSet::from(["run".to_owned(), "timeline".to_owned()]),
        "two root containers and no `meta`"
    );
    assert!(
        run.doc().get_map("meta").get("schema_version").is_none(),
        "the node model's key is never written by a run"
    );
}

/// The `run` map holds exactly the header's fields under the proposal's
/// spelling, plus `schema` and `models`.
#[test]
fn the_run_map_holds_the_headers_fields_under_the_proposals_keys() {
    let run = RunDocument::create(&header()).unwrap();
    let stored = keys(&run.doc().get_map("run").get_deep_value());
    let expected: BTreeSet<String> = [
        "schema",
        "id",
        "created_at_ms",
        "requested_by",
        "objective",
        "agent",
        "origin",
        "scope",
        "context",
        "grants",
        "status",
        "environment",
        "results",
        "models",
        "economics",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(stored, expected);
    // Absent until rated (D44): an unrated run is a normal state, not a
    // null.
    assert!(!stored.contains("rating"));
    run.set_rating(&rating()).unwrap();
    assert!(keys(&run.doc().get_map("run").get_deep_value()).contains("rating"));
}

/// What goes in comes out: the header the executing device wrote is the
/// header every device reads, after every kind of write and a snapshot.
#[test]
fn a_header_round_trips_through_the_document_and_a_snapshot() {
    let run = RunDocument::create(&header()).unwrap();
    assert_eq!(run.header().unwrap(), header());

    run.set_status(&RunStatus::running()).unwrap();
    run.register_result(&result(10)).unwrap();
    run.register_result(&result(11)).unwrap();
    run.set_rating(&rating()).unwrap();
    run.set_status(&RunStatus::Completed).unwrap();

    let expected = RunHeader {
        status: RunStatus::Completed,
        artifacts: vec![result(10), result(11)],
        rating: Some(rating()),
        ..header()
    };
    assert_eq!(run.header().unwrap(), expected);
    assert_eq!(run.header().unwrap().executing_device(), Some(device(9)));

    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.header().unwrap(), expected);
    assert_eq!(reopened.timeline_len(), 0);
}

/// A key a newer build adds to the `run` map is read past by this build
/// and left in place by every write this build makes — preserve and ignore
/// (ROADMAP §8), the same promise `RunHeader`'s serde makes one layer down.
#[test]
fn a_key_from_a_newer_build_is_read_past_and_left_in_place() {
    let run = RunDocument::create(&header()).unwrap();
    run.doc()
        .get_map("run")
        .insert("a_field_from_m4", "worktree")
        .unwrap();
    run.doc().commit();

    assert_eq!(run.header().unwrap(), header());
    run.set_status(&RunStatus::running()).unwrap();
    run.register_result(&result(10)).unwrap();
    let after = run.doc().get_map("run").get_deep_value();
    assert!(keys(&after).contains("a_field_from_m4"));
    assert!(matches!(&after["a_field_from_m4"], LoroValue::String(s) if s.as_str() == "worktree"));
}

/// Initializing twice would overwrite a run's identity with another's.
#[test]
fn a_document_that_already_holds_a_run_is_not_initialized_again() {
    let run = RunDocument::create(&header()).unwrap();
    let doc = loro::LoroDoc::new();
    doc.import(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        RunDocument::initialize(doc, &header()).err(),
        Some(RunDocumentError::AlreadyInitialized)
    );
}

/// A page, an empty document, a manifest: none of them is a run, and none
/// is mistaken for one.
#[test]
fn a_document_without_run_schema_is_not_a_run() {
    let empty = loro::LoroDoc::new();
    assert_eq!(
        RunDocument::adopt(empty).err(),
        Some(RunDocumentError::NotARun)
    );
    let page = loro::LoroDoc::new();
    page.get_map("meta").insert("schema_version", 4).unwrap();
    page.commit();
    assert_eq!(
        RunDocument::adopt(page).err(),
        Some(RunDocumentError::NotARun)
    );
}

/// **Privacy is structural.** The errors this crate reports carry a place
/// and a version, never a value: an objective or a rating note cannot reach
/// a log through one.
#[test]
fn no_error_carries_content() {
    let run = RunDocument::create(&header()).unwrap();
    run.set_rating(&rating()).unwrap();
    run.doc()
        .get_map("run")
        .insert("status", OBJECTIVE_SENTINEL)
        .unwrap();
    run.doc().commit();
    let error = run.header().unwrap_err();
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("SENTINEL"), "{rendered}");
    for error in [
        RunDocumentError::NotARun,
        RunDocumentError::ReadOnly {
            found: 2,
            understood: 1,
        },
        RunDocumentError::AlreadyInitialized,
        RunDocumentError::Malformed {
            part: Part::Timeline,
        },
    ] {
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("SENTINEL"), "{rendered}");
    }
}

/// The stored names, pinned as literals. `koru_core::explore` pins the
/// same two tokens on its side; `docs/architecture/DATAMODEL.md` says why
/// neither may change.
#[test]
fn the_stored_names_are_pinned() {
    assert_eq!(RUN_KIND, "koru-run");
    assert_eq!(SCHEDULE_KIND, "koru-schedule");
    assert_eq!(RUN_DOCUMENT_SCHEMA_VERSION, 6);
    assert_eq!(RUN_BOT_DOCUMENT_SCHEMA_VERSION, 6);
    assert_eq!(RUN_BOT_DOCUMENT_SCHEMA_VERSION, 6);
    assert_eq!(RUN_BOT_PREVIOUS_DOCUMENT_SCHEMA_VERSION, 5);
    assert_eq!(RUN_INTERACTIVE_DOCUMENT_SCHEMA_VERSION, 3);
    assert_eq!(RUN_ROUTINE_DOCUMENT_SCHEMA_VERSION, 4);
}

#[test]
fn requested_model_survives_run_writes_snapshot_and_unknown_fields() {
    let mut chosen = header();
    chosen.requested_model = Some(ModelName::new("deepseek/deepseek-chat").unwrap());
    let run = RunDocument::create(&chosen).unwrap();
    run.doc()
        .get_map("run")
        .insert("future_choice_metadata", "keep-me")
        .unwrap();
    run.set_status(&RunStatus::running()).unwrap();
    run.register_result(&result(10)).unwrap();
    run.set_rating(&rating()).unwrap();
    run.refresh_economics(2000).unwrap();
    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        reopened.header().unwrap().requested_model,
        chosen.requested_model
    );
    let stored = reopened.doc().get_map("run").get_deep_value();
    assert_eq!(
        stored["requested_model"],
        LoroValue::from("deepseek/deepseek-chat")
    );
    assert_eq!(stored["future_choice_metadata"], LoroValue::from("keep-me"));

    reopened
        .doc()
        .get_map("run")
        .delete("requested_model")
        .unwrap();
    assert_eq!(
        reopened.header().unwrap().requested_model,
        None,
        "old runs use the provider default"
    );
}

#[test]
fn failure_reason_survives_snapshot_and_clears_on_a_new_attempt() {
    let run = RunDocument::create(&header()).unwrap();
    run.doc()
        .get_map("run")
        .insert("future_key", "preserve")
        .unwrap();
    let status = RunStatus::Failed {
        stage: FailureStage::SessionCreation,
    };
    let reason = RunFailureReason::CapabilityNotNegotiated;
    run.set_status_with_failure_reason(&status, Some(reason))
        .unwrap();
    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.header().unwrap().status, status);
    assert_eq!(reopened.header().unwrap().failure_reason, Some(reason));
    reopened.set_status(&RunStatus::Starting).unwrap();
    assert_eq!(reopened.header().unwrap().failure_reason, None);
    assert!(reopened.doc().get_map("run").get("future_key").is_some());
}
