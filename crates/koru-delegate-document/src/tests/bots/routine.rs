// SPDX-License-Identifier: MPL-2.0
use super::*;

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

#[test]
fn routine_run4_and_ordinary_run3_have_distinct_write_cohorts() {
    for (header, version) in [(header(), 3), (bot_header(), 3), (routine_header(), 4)] {
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
