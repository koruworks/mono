// SPDX-License-Identifier: MPL-2.0

//! The run's own compatibility gate: `run.schema` against this build's
//! version through the store's `Compatibility`. Read-only, never refused;
//! never the node model's key.

use koru_store_core::version::Compatibility;

use super::*;

/// A run stamped by a newer build than this one.
fn a_newer_run() -> RunDocument {
    let run = a_run_with(2);
    run.set_status(&RunStatus::running()).unwrap();
    run.doc()
        .get_map("run")
        .insert("schema", RUN_DOCUMENT_SCHEMA_VERSION + 1)
        .unwrap();
    run.doc().commit();
    RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap()
}

/// A run written by a newer build opens read-only: the header and the
/// timeline still read — a second device shows the run rather than a
/// refusal — and every write says why it will not.
#[test]
fn a_run_written_by_a_newer_build_opens_read_only() {
    let run = a_newer_run();
    assert_eq!(
        run.compatibility(),
        Compatibility::TooNew {
            found: 4,
            understood: 3
        }
    );
    assert_eq!(run.header().unwrap().status, RunStatus::running());
    assert_eq!(run.timeline_len(), 2);
    assert_eq!(run.timeline_window(0, 10).len(), 2);

    let refused = RunDocumentError::ReadOnly {
        found: 4,
        understood: 3,
    };
    assert_eq!(
        run.append_step(&step(StepKind::Completed, 5)).err(),
        Some(refused.clone())
    );
    assert_eq!(
        run.record_usage(&usage(1, 0, 1)).err(),
        Some(refused.clone())
    );
    assert_eq!(
        run.record_model(&identity(0x30)).err(),
        Some(refused.clone())
    );
    assert_eq!(
        run.set_status(&RunStatus::Completed).err(),
        Some(refused.clone())
    );
    assert_eq!(
        run.register_result(&result(10)).err(),
        Some(refused.clone())
    );
    assert_eq!(run.set_rating(&rating()).err(), Some(refused.clone()));
    assert_eq!(run.refresh_economics(9).err(), Some(refused));
    assert_eq!(run.timeline_len(), 2, "nothing was written");

    // Sync is not a write: the read-only replica still receives what the
    // (newer) executing device appends. That device is a raw document over
    // the same history, since this build's writer would refuse too.
    let executing = loro::LoroDoc::new();
    executing.import(&run.export_snapshot().unwrap()).unwrap();
    let before = executing.oplog_vv();
    let a_step = a_run_with(1)
        .doc()
        .get_list("timeline")
        .get(0)
        .and_then(|item| item.into_value().ok())
        .unwrap();
    executing.get_list("timeline").push(a_step).unwrap();
    executing.commit();
    run.import(
        &executing
            .export(loro::ExportMode::updates(&before))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(run.timeline_len(), 3);
    assert!(run.timeline_window(0, 1)[0].step().is_some());
}

/// This build's own run is writable, and the version it stamps is the one
/// the gate compares against.
#[test]
fn a_run_this_build_wrote_is_writable() {
    let run = a_run_with(1);
    assert_eq!(run.compatibility(), Compatibility::Writable);
    assert_eq!(run.schema_version(), Some(RUN_DOCUMENT_SCHEMA_VERSION));
    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert!(reopened.compatibility().writable());
}

/// Schema 1 predates scheduled provenance. It remains readable and writable
/// by this build, and the missing field has the only honest interpretation:
/// a person created it manually.
#[test]
fn a_schema_one_run_defaults_to_manual_origin() {
    let run = a_run_with(1);
    let map = run.doc().get_map("run");
    map.insert("schema", 1).unwrap();
    map.delete("origin").unwrap();
    run.doc().commit();

    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.compatibility(), Compatibility::Writable);
    assert_eq!(
        reopened.header().unwrap().origin,
        koru_delegate_core::RunOrigin::Manual
    );
}
