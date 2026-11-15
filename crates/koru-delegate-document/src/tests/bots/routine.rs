// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::version::Compatibility;

fn routine_header() -> RunHeader {
    let mut header = bot_header();
    let routine = RoutineProvenance {
        schedule: ScheduleId::from_uuid(v4(60)),
        occurrence_ms: 20,
        definition: RoutineDefinitionIdentity {
            revision: 2,
            digest: RoutineDigest32::from_bytes([1; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    };
    header.origin = RunOrigin::Scheduled {
        schedule: routine.schedule,
        occurrence_ms: routine.occurrence_ms,
        revision: routine.definition.revision,
    };
    header.bot.as_mut().unwrap().routine = Some(routine);
    header
}

fn import_change(doc: &RunDocument, change: impl FnOnce(&loro::LoroMap)) {
    let other = loro::LoroDoc::new();
    other.import(&doc.export_snapshot().unwrap()).unwrap();
    let before = other.oplog_vv();
    change(&other.get_map("run"));
    other.commit();
    doc.import(&other.export(loro::ExportMode::updates(&before)).unwrap())
        .unwrap();
}

/// BotRun5 creation (contract section 8): every new Bot Run, Manual or
/// Routine, is created at 5; ordinary creation is unchanged at 3.
#[test]
fn bot_runs_are_created_at_bot5_and_ordinary_creation_stays_run3() {
    for (header, version) in [(header(), 3), (bot_header(), 5), (routine_header(), 5)] {
        let run = RunDocument::create(&header).unwrap();
        assert_eq!(run.schema_version(), Some(version));
        assert_eq!(run.header().unwrap(), header);
        run.append_step(&step(StepKind::Started, 30)).unwrap();
        run.set_status(&RunStatus::running()).unwrap();
        assert_eq!(run.schema_version(), Some(version));
        assert_eq!(run.timeline_len(), 1);
    }
}

#[test]
fn routine_run4_invalid_creation_never_stamps_or_mutates_the_document() {
    let mut header = routine_header();
    header.origin = RunOrigin::Manual;
    let raw = loro::LoroDoc::new();
    let before = raw.oplog_vv();
    assert!(RunDocument::initialize(raw.clone(), &header).is_err());
    assert_eq!(raw.oplog_vv(), before);
    assert!(raw.get_map("run").get("schema").is_none());
}

#[test]
fn routine_run_import_rechecks_schema_and_required_subject_before_writing() {
    for (header, schema) in [(routine_header(), 3), (bot_header(), 4), (header(), 4)] {
        let run = RunDocument::create(&header).unwrap();
        import_change(&run, |map| {
            map.insert("schema", schema).unwrap();
        });
        let before = run.version();
        assert!(run.header().is_err());
        assert!(run.append_step(&step(StepKind::Completed, 30)).is_err());
        assert!(run.set_status(&RunStatus::Completed).is_err());
        assert_eq!(run.version(), before);
    }
}

#[test]
fn routine_run_import_rechecks_bot_folder_scope_without_mutation() {
    for (header, scope) in [
        (routine_header(), WorkspaceScope::Temporary),
        (bot_header(), WorkspaceScope::Temporary),
        (
            header(),
            WorkspaceScope::BotFolder {
                folder: BotWorkingFolder::new("/bot".into()).unwrap(),
            },
        ),
    ] {
        let run = RunDocument::create(&header).unwrap();
        import_change(&run, |map| {
            map.insert(
                "scope",
                loro::LoroValue::from(serde_json::to_value(scope).unwrap()),
            )
            .unwrap();
        });
        let before = run.version();
        assert!(run.header().is_err());
        assert!(run.append_step(&step(StepKind::Completed, 30)).is_err());
        assert!(run.set_status(&RunStatus::Completed).is_err());
        assert_eq!(run.version(), before);
    }
}

#[test]
fn routine_run_import_rechecks_origin_and_newer_schema_on_retained_handles() {
    let run = RunDocument::create(&bot_header()).unwrap();
    let origin = routine_header().origin;
    import_change(&run, |map| {
        map.insert(
            "origin",
            loro::LoroValue::from(serde_json::to_value(origin).unwrap()),
        )
        .unwrap();
    });
    let before = run.version();
    assert!(run.header().is_err());
    assert!(run.append_step(&step(StepKind::Completed, 30)).is_err());
    assert_eq!(run.version(), before);

    let run = RunDocument::create(&header()).unwrap();
    import_change(&run, |map| {
        map.insert("schema", RUN_DOCUMENT_SCHEMA_VERSION + 1)
            .unwrap();
    });
    let before = run.version();
    assert!(!run.compatibility().writable());
    assert!(run.header().is_ok());
    assert!(matches!(
        run.set_status(&RunStatus::Completed),
        Err(RunDocumentError::ReadOnly { .. })
    ));
    assert_eq!(run.version(), before);
}

/// A Bot Run created at 5, Manual or Routine, is readable and writable by
/// this build, across a snapshot reopen.
#[test]
fn a_bot_run_created_at_5_is_readable_and_writable_by_this_build() {
    for header in [bot_header(), routine_header()] {
        let run = RunDocument::create(&header).unwrap();
        assert_eq!(run.schema_version(), Some(RUN_BOT_DOCUMENT_SCHEMA_VERSION));
        let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
        assert_eq!(reopened.compatibility(), Compatibility::Writable);
        assert_eq!(reopened.header().unwrap(), header);
        reopened.append_step(&step(StepKind::Started, 30)).unwrap();
        reopened.set_status(&RunStatus::running()).unwrap();
        assert_eq!(reopened.timeline_len(), 1);
        assert_eq!(reopened.schema_version(), Some(5));
    }
}

/// A Bot Run the previous default created (Manual at 3, Routine at 4) still
/// opens, reads and writes here, and keeps its format until a projection.
#[test]
fn a_bot_run_created_at_3_or_4_by_the_previous_default_still_opens_and_writes() {
    for (header, legacy) in [(bot_header(), 3i64), (routine_header(), 4i64)] {
        let created = RunDocument::create(&header).unwrap();
        import_change(&created, |map| {
            map.insert("schema", legacy).unwrap();
        });
        let reopened = RunDocument::from_snapshot(&created.export_snapshot().unwrap()).unwrap();
        assert_eq!(reopened.schema_version(), Some(legacy));
        assert_eq!(reopened.compatibility(), Compatibility::Writable);
        assert_eq!(reopened.header().unwrap(), header);
        reopened.append_step(&step(StepKind::Started, 30)).unwrap();
        reopened.set_status(&RunStatus::running()).unwrap();
        assert_eq!(reopened.schema_version(), Some(legacy));
    }
}

/// A Bot5 Run written by a newer build (format 6) still opens read-only: it
/// reads, and every write refuses, as "needs newer Koru".
#[test]
fn a_version_6_bot_run_still_reads_as_needs_newer_koru() {
    for header in [bot_header(), routine_header()] {
        let run = RunDocument::create(&header).unwrap();
        import_change(&run, |map| {
            map.insert("schema", RUN_DOCUMENT_SCHEMA_VERSION + 1)
                .unwrap();
        });
        let before = run.version();
        assert!(!run.compatibility().writable());
        assert!(run.header().is_ok());
        assert!(matches!(
            run.set_status(&RunStatus::Completed),
            Err(RunDocumentError::ReadOnly {
                found: 6,
                understood: 5
            })
        ));
        assert_eq!(run.version(), before);
    }
}
