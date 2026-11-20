// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_schedule::*;
use koru_recurrence::{CronExpression, RecurrenceRule};
use loro::{LoroValue, ValueOrContainer, VersionVector};

fn definition(revision: u32) -> ScheduleDefinition {
    ScheduleDefinition {
        routine: None,
        revision,
        effective_at_ms: revision as i64 * 1_000,
        template: DelegationTemplate {
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(v4(1)),
            },
            objective: Objective::new("毎朝 راجع التقرير").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: Some(ModelName::new("gpt-5.4").unwrap()),
            scope: WorkspaceScope::Temporary,
            grants: BTreeSet::from([WorkspaceId::from_uuid(v4(2))]),
            context: vec![ScheduleContextPointer::Document {
                workspace: WorkspaceId::from_uuid(v4(2)),
                document: DocumentId::from_uuid(v4(3)),
            }],
        },
        recurrence: RecurrenceRule::floating(CronExpression::parse("0 9 * * MON-FRI").unwrap()),
        lifecycle: ScheduleLifecycle::Active,
    }
}

fn schedule() -> ScheduleDocument {
    ScheduleDocument::create(&ScheduleHeader {
        schedule: ScheduleId::from_uuid(v4(4)),
        created_at_ms: 1,
        initial_owner: device(5),
        definition: definition(1),
    })
    .unwrap()
}

fn run_plan(occurrence_ms: i64, run: RunId) -> ScheduleClaimPlan {
    ScheduleClaimPlan::Run {
        occurrence_ms,
        run,
        pinned_context: vec![RunContextItem::Document {
            workspace: WorkspaceId::from_uuid(v4(2)),
            document: DocumentId::from_uuid(v4(3)),
            version: DocumentVersionId::from_uuid(v4(30)),
        }],
    }
}

fn skip_plan(occurrence_ms: i64, run: RunId) -> ScheduleClaimPlan {
    ScheduleClaimPlan::Skip {
        occurrence_ms,
        run,
        outcome: ScheduleFiringOutcome::SkippedMissed,
        busy_holder: None,
    }
}

#[test]
fn retained_routine_header_decodes_but_cannot_claim_or_mutate() {
    let schedule = schedule();
    let mut header = schedule.header().unwrap();
    header.definition.routine = Some(RoutineAssociation {
        owner: BotOwner {
            user_scope: koru_store_core::UserScopeId::new(),
            device: header.initial_owner,
        },
        bot: BotId::from_uuid(v4(40)),
        home: WorkspaceId::from_uuid(v4(41)),
        creation_request: BotRequestId::from_uuid(v4(42)),
    });
    assert!(ScheduleDocument::create(&header).is_err());
    let root = schedule.doc().get_map("schedule");
    root.insert("schema", BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION)
        .unwrap();
    root.insert(
        "definition",
        LoroValue::from(serde_json::to_value(&header.definition).unwrap()),
    )
    .unwrap();
    schedule.doc().commit();
    let before = schedule.version();
    assert_eq!(schedule.header().unwrap(), header);
    assert!(!schedule.is_writable());
    assert!(!schedule.may_claim(header.initial_owner).unwrap());
    assert!(
        schedule
            .claim_authority(
                WorkspaceId::from_uuid(v4(41)),
                header.initial_owner,
                run_plan(2_000, RunId::from_uuid(v4(43)))
            )
            .is_err()
    );
    assert!(
        schedule
            .update_definition(header.initial_owner, &header.definition)
            .is_err()
    );
    assert_eq!(schedule.version(), before);
}

#[test]
fn shape_round_trip_and_newer_gate_are_local_to_the_schedule() {
    let schedule = schedule();
    assert!(
        schedule
            .doc()
            .get_map("meta")
            .get("schema_version")
            .is_none()
    );
    assert_eq!(schedule.header().unwrap().definition, definition(1));
    let reopened = ScheduleDocument::from_snapshot(&schedule.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        reopened
            .header()
            .unwrap()
            .definition
            .template
            .requested_model,
        Some(ModelName::new("gpt-5.4").unwrap())
    );

    reopened
        .doc()
        .get_map("schedule")
        .insert("schema", SCHEDULE_DOCUMENT_SCHEMA_VERSION + 1)
        .unwrap();
    reopened.doc().commit();
    let newer = ScheduleDocument::from_snapshot(&reopened.export_snapshot().unwrap()).unwrap();
    assert!(matches!(
        newer.compatibility(),
        koru_store_core::version::Compatibility::TooNew { .. }
    ));
    assert!(matches!(
        newer.update_definition(device(5), &definition(2)),
        Err(ScheduleDocumentError::ReadOnly { .. })
    ));
    assert!(matches!(
        newer.claim_authority(
            WorkspaceId::from_uuid(v4(2)),
            device(5),
            skip_plan(60_000, RunId::from_uuid(v4(31))),
        ),
        Err(ScheduleDocumentError::ReadOnly { .. })
    ));

    // A long-lived handle can learn the newer schema from sync after open;
    // the next local write must be refused without requiring a reopen.
    let replica = ScheduleDocument::from_snapshot(&schedule.export_snapshot().unwrap()).unwrap();
    let before = replica.version();
    replica
        .doc()
        .get_map("schedule")
        .insert("schema", SCHEDULE_DOCUMENT_SCHEMA_VERSION + 1)
        .unwrap();
    replica.doc().commit();
    schedule
        .import(&replica.export_updates_since(&before).unwrap())
        .unwrap();
    assert!(matches!(
        schedule.update_definition(device(5), &definition(2)),
        Err(ScheduleDocumentError::ReadOnly { .. })
    ));
    assert!(matches!(
        schedule.claim_authority(
            WorkspaceId::from_uuid(v4(2)),
            device(5),
            skip_plan(60_000, RunId::from_uuid(v4(32))),
        ),
        Err(ScheduleDocumentError::ReadOnly { .. })
    ));
}

#[test]
fn name_round_trips_and_missing_schema_one_key_falls_back_without_a_bump() {
    let header = schedule().header().unwrap();
    let authored = ScheduleName::new("每日研究").unwrap();
    let named = ScheduleDocument::create_named(&header, &authored).unwrap();
    assert_eq!(named.name().unwrap(), authored);
    assert_eq!(
        ScheduleDocument::from_snapshot(&named.export_snapshot().unwrap())
            .unwrap()
            .name()
            .unwrap(),
        authored
    );

    named.doc().get_map("schedule").delete("name").unwrap();
    named.doc().commit();
    let legacy = ScheduleDocument::from_snapshot(&named.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        legacy.name().unwrap(),
        ScheduleName::from_objective(&header.definition.template.objective)
    );
    assert!(matches!(
        legacy.doc().get_map("schedule").get("schema"),
        Some(ValueOrContainer::Value(LoroValue::I64(1)))
    ));
}

#[test]
fn definition_edits_preserve_the_independent_name_key() {
    let schedule = schedule();
    let before = schedule.name().unwrap();
    schedule
        .doc()
        .get_map("schedule")
        .insert("future_unknown", "preserve me")
        .unwrap();
    schedule.doc().commit();
    schedule
        .update_definition(device(5), &definition(2))
        .unwrap();
    assert_eq!(schedule.name().unwrap(), before);
    assert!(
        schedule
            .doc()
            .get_map("schedule")
            .get("future_unknown")
            .is_some()
    );
}

#[test]
fn rename_is_owner_only_conflict_aware_and_retry_safe() {
    let schedule = schedule();
    let original = schedule.name().unwrap();
    let renamed = ScheduleName::new("Research weekly launches").unwrap();

    assert_eq!(
        schedule.rename(device(5), &original, &renamed).unwrap(),
        ScheduleWrite::Applied
    );
    assert_eq!(schedule.name().unwrap(), renamed);
    assert_eq!(
        schedule.rename(device(5), &original, &renamed).unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert!(matches!(
        schedule.rename(
            device(5),
            &original,
            &ScheduleName::new("Overwrite newer work").unwrap()
        ),
        Err(ScheduleDocumentError::NameConflict)
    ));
    assert!(matches!(
        schedule.rename(
            device(6),
            &renamed,
            &ScheduleName::new("Remote rename").unwrap()
        ),
        Err(ScheduleDocumentError::NotOwner)
    ));
}

#[test]
fn retired_schedule_refuses_rename() {
    let schedule = schedule();
    let original = schedule.name().unwrap();
    schedule.retire(device(5), 20).unwrap();
    assert!(matches!(
        schedule.rename(
            device(5),
            &original,
            &ScheduleName::new("Too late").unwrap()
        ),
        Err(ScheduleDocumentError::Retired)
    ));
}

#[test]
fn retirement_is_owner_only_and_an_exact_retry_is_free() {
    let schedule = schedule();
    assert!(matches!(
        schedule.retire(device(6), 19),
        Err(ScheduleDocumentError::NotOwner)
    ));
    assert_eq!(
        schedule.retire(device(5), 20).unwrap(),
        ScheduleWrite::Applied
    );
    let retired = schedule.export_snapshot().unwrap();
    assert_eq!(
        schedule.retire(device(5), 99).unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert_eq!(schedule.export_snapshot().unwrap(), retired);
    assert!(matches!(
        schedule.retire(device(6), 100),
        Err(ScheduleDocumentError::NotOwner)
    ));
}

#[test]
fn edit_is_one_owner_revision_and_an_exact_retry_is_free() {
    let schedule = schedule();
    let original_name = schedule.name().unwrap();
    let name = ScheduleName::new("Weekly launch research").unwrap();
    let mut edited = definition(2);
    edited.effective_at_ms = 2_001;
    edited.template.objective = Objective::new("Research launches and summarize them").unwrap();
    edited.template.requested_model = Some(ModelName::new("gpt-test").unwrap());
    edited.recurrence = RecurrenceRule::floating(CronExpression::parse("*/5 * * * *").unwrap());

    assert_eq!(
        schedule
            .edit(device(5), 1, &original_name, &name, &edited)
            .unwrap(),
        ScheduleWrite::Applied
    );
    assert_eq!(schedule.name().unwrap(), name);
    assert_eq!(schedule.header().unwrap().definition, edited);
    assert_eq!(
        schedule
            .edit(device(5), 1, &original_name, &name, &edited)
            .unwrap(),
        ScheduleWrite::AlreadyApplied
    );

    let before = schedule.export_snapshot().unwrap();
    let mut overwrite = edited.clone();
    overwrite.template.objective = Objective::new("Overwrite newer work").unwrap();
    assert!(matches!(
        schedule.edit(device(5), 1, &original_name, &name, &overwrite),
        Err(ScheduleDocumentError::RevisionConflict { .. })
    ));
    assert_eq!(schedule.export_snapshot().unwrap(), before);
    assert!(matches!(
        schedule.edit(device(6), 2, &name, &name, &definition(3)),
        Err(ScheduleDocumentError::NotOwner)
    ));
}

#[test]
fn edit_refuses_while_ownership_handoff_is_pending() {
    let schedule = schedule();
    let original_name = schedule.name().unwrap();
    schedule
        .request_handoff(HandoffId::from_uuid(v4(70)), device(5), device(6), 2_000)
        .unwrap();
    assert!(matches!(
        schedule.edit(
            device(5),
            1,
            &original_name,
            &ScheduleName::new("Not during handoff").unwrap(),
            &definition(2),
        ),
        Err(ScheduleDocumentError::HandoffPending)
    ));
}

#[test]
fn lifecycle_changes_are_owner_only_state_aware_and_retry_safe() {
    let schedule = schedule();
    let paused = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    assert_eq!(
        schedule
            .set_lifecycle(device(5), 1, paused.clone(), 2_001)
            .unwrap(),
        ScheduleWrite::Applied
    );
    let after_pause = schedule.header().unwrap().definition;
    assert_eq!(after_pause.revision, 2);
    assert_eq!(after_pause.lifecycle, paused);
    assert_eq!(
        schedule
            .set_lifecycle(device(5), 1, paused, 99_000)
            .unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert_eq!(schedule.header().unwrap().definition, after_pause);

    assert_eq!(
        schedule
            .set_lifecycle(device(5), 2, ScheduleLifecycle::Active, 2_002)
            .unwrap(),
        ScheduleWrite::Applied
    );
    assert_eq!(schedule.header().unwrap().definition.revision, 3);
    assert!(matches!(
        schedule.set_lifecycle(device(5), 3, ScheduleLifecycle::Active, 2_003),
        Err(ScheduleDocumentError::LifecycleConflict)
    ));
    assert!(matches!(
        schedule.set_lifecycle(
            device(6),
            3,
            ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual
            },
            2_003
        ),
        Err(ScheduleDocumentError::NotOwner)
    ));
}

#[test]
fn lifecycle_changes_refuse_a_pending_handoff() {
    let schedule = schedule();
    schedule
        .request_handoff(HandoffId::from_uuid(v4(70)), device(5), device(6), 2_000)
        .unwrap();
    assert!(matches!(
        schedule.set_lifecycle(
            device(5),
            1,
            ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual
            },
            2_001
        ),
        Err(ScheduleDocumentError::HandoffPending)
    ));
}

#[test]
fn creation_rejects_zero_revision_and_precreation_activation() {
    let mut header = ScheduleHeader {
        schedule: ScheduleId::from_uuid(v4(4)),
        created_at_ms: 2_000,
        initial_owner: device(5),
        definition: definition(1),
    };
    assert!(matches!(
        ScheduleDocument::create(&header),
        Err(ScheduleDocumentError::EffectiveTimeBeforeCreation)
    ));
    header.definition.effective_at_ms = 2_000;
    header.definition.revision = 0;
    assert!(matches!(
        ScheduleDocument::create(&header),
        Err(ScheduleDocumentError::RevisionZero)
    ));
}

#[test]
fn definition_is_coherent_and_firings_commute_with_edits() {
    let left = schedule();
    let base = left.export_snapshot().unwrap();
    let right = ScheduleDocument::from_snapshot(&base).unwrap();
    let left_before = left.version();
    let right_before = right.version();
    let mut edited = definition(2);
    edited.template.requested_model = Some(ModelName::new("deepseek/deepseek-chat").unwrap());
    left.update_definition(device(5), &edited).unwrap();
    let firing = ScheduleFiring {
        occurrence_ms: 60_000,
        revision: 1,
        run: RunId::from_uuid(v4(7)),
        outcome: ScheduleFiringOutcome::Fired,
        busy_holder: None,
        at: 60_010,
    };
    right.append_firing(&firing).unwrap();
    let l = left.export_updates_since(&left_before).unwrap();
    let r = right.export_updates_since(&right_before).unwrap();
    left.import(&r).unwrap();
    right.import(&l).unwrap();
    assert_eq!(left.header().unwrap().definition, edited);
    assert_eq!(right.header().unwrap().definition, edited);
    assert_eq!(left.firings().unwrap(), vec![firing.clone()]);
    assert_eq!(right.firings().unwrap(), vec![firing]);
}

#[test]
fn concurrent_root_creation_converges_to_one_restorable_claim_authority() {
    let left_header = schedule().header().unwrap();
    let mut right_header = left_header.clone();
    right_header.initial_owner = device(6);
    right_header.created_at_ms += 1;
    right_header.definition.effective_at_ms += 1;
    let left = ScheduleDocument::create(&left_header).unwrap();
    let right = ScheduleDocument::create(&right_header).unwrap();
    let left_update = left
        .export_updates_since(&VersionVector::default())
        .unwrap();
    let right_update = right
        .export_updates_since(&VersionVector::default())
        .unwrap();

    left.import(&right_update).unwrap();
    right.import(&left_update).unwrap();
    assert_eq!(left.header().unwrap(), right.header().unwrap());

    let restored = ScheduleDocument::from_snapshot(&left.export_snapshot().unwrap()).unwrap();
    let owner = restored.header().unwrap().initial_owner;
    assert!(restored.may_claim(owner).unwrap());
    assert!(
        !restored
            .may_claim(if owner == device(5) {
                device(6)
            } else {
                device(5)
            })
            .unwrap()
    );
}

#[test]
fn handoff_requires_owner_confirmation_and_retirement_dominates_stale_edit() {
    let owner = device(5);
    let target = device(6);
    let schedule = schedule();
    let id = HandoffId::from_uuid(v4(8));
    schedule.request_handoff(id, target, target, 10).unwrap();
    assert!(schedule.may_claim(owner).unwrap());
    assert!(!schedule.may_claim(target).unwrap());
    assert_eq!(
        schedule.confirm_handoff(id, target, 11),
        Err(ScheduleDocumentError::NotOwner)
    );
    schedule.confirm_handoff(id, owner, 12).unwrap();
    assert_eq!(
        schedule.confirm_handoff(id, owner, 12).unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert_eq!(
        schedule.request_handoff(id, target, target, 10).unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert!(!schedule.may_claim(owner).unwrap());
    assert!(schedule.may_claim(target).unwrap());

    let stale = ScheduleDocument::from_snapshot(&schedule.export_snapshot().unwrap()).unwrap();
    let stale_version = stale.version();
    schedule.retire(target, 20).unwrap();
    stale.update_definition(target, &definition(2)).unwrap();
    let update = stale.export_updates_since(&stale_version).unwrap();
    schedule.import(&update).unwrap();
    assert_eq!(schedule.state().unwrap(), ScheduleState::Retired);
    assert!(!schedule.may_claim(target).unwrap());
}

#[test]
fn firing_history_is_idempotent_immutable_and_supplies_the_missed_cursor() {
    let schedule = schedule();
    let missed = ScheduleFiring {
        occurrence_ms: 60_000,
        revision: 1,
        run: RunId::from_uuid(v4(10)),
        outcome: ScheduleFiringOutcome::SkippedMissed,
        busy_holder: None,
        at: 3_600_000,
    };
    assert_eq!(
        schedule.append_firing(&missed).unwrap(),
        ScheduleWrite::Applied
    );
    assert_eq!(
        schedule.append_firing(&missed).unwrap(),
        ScheduleWrite::AlreadyApplied
    );
    assert_eq!(schedule.observed_through_ms().unwrap(), Some(3_600_000));
    let changed = ScheduleFiring {
        revision: 2,
        ..missed
    };
    assert_eq!(
        schedule.append_firing(&changed),
        Err(ScheduleDocumentError::FiringConflict)
    );
}

#[test]
fn imported_roots_converge_on_the_same_handoff_chain() {
    let a = schedule();
    let snapshot = a.export_snapshot().unwrap();
    let b = ScheduleDocument::from_snapshot(&snapshot).unwrap();
    let base_a = a.version();
    let base_b = b.version();
    let id = HandoffId::from_uuid(v4(9));
    a.request_handoff(id, device(6), device(6), 10).unwrap();
    b.request_handoff(id, device(6), device(6), 10).unwrap();
    b.confirm_handoff(id, device(5), 11).unwrap();
    let ua = a.export_updates_since(&base_a).unwrap();
    let ub = b.export_updates_since(&base_b).unwrap();
    a.import(&ub).unwrap();
    b.import(&ua).unwrap();
    assert_eq!(a.handoff_state().unwrap().owner, device(6));
    assert_eq!(a.handoff_state().unwrap(), b.handoff_state().unwrap());
}

#[test]
fn pause_reason_round_trips_and_blocks_claims() {
    let schedule = schedule();
    let mut paused = definition(2);
    paused.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::ContextUnavailable {
            item: 4,
            stage: ScheduleContextFailureStage::Materialize,
        },
    };
    schedule.update_definition(device(5), &paused).unwrap();
    assert_eq!(
        schedule.state().unwrap(),
        ScheduleState::Paused {
            owner: device(5),
            reason: SchedulePauseReason::ContextUnavailable {
                item: 4,
                stage: ScheduleContextFailureStage::Materialize,
            },
        }
    );
    assert!(!schedule.may_claim(device(5)).unwrap());
    let debug = format!("{:?}", schedule.state().unwrap());
    assert!(!debug.contains("Objective"));
    assert!(!debug.contains('/') && !debug.contains('\\'));
}

#[test]
fn every_edit_and_resume_advance_the_revision_activation_boundary() {
    let schedule = schedule();
    let mut edit = definition(2);
    edit.effective_at_ms = definition(1).effective_at_ms;
    assert_eq!(
        schedule.update_definition(device(5), &edit),
        Err(ScheduleDocumentError::EffectiveTimeDidNotAdvance)
    );

    edit.effective_at_ms += 1;
    edit.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    schedule.update_definition(device(5), &edit).unwrap();

    let mut resumed = definition(3);
    resumed.effective_at_ms = edit.effective_at_ms;
    assert_eq!(
        schedule.update_definition(device(5), &resumed),
        Err(ScheduleDocumentError::EffectiveTimeDidNotAdvance)
    );
    resumed.effective_at_ms += 1;
    schedule.update_definition(device(5), &resumed).unwrap();
    assert_eq!(
        schedule.header().unwrap().definition.effective_at_ms,
        resumed.effective_at_ms
    );
}

#[test]
fn busy_firing_requires_and_retains_the_holding_run() {
    let schedule = schedule();
    let planned = RunId::from_uuid(v4(10));
    let holder = RunId::from_uuid(v4(11));
    let busy = ScheduleFiring {
        occurrence_ms: 120_000,
        revision: 1,
        run: planned,
        outcome: ScheduleFiringOutcome::SkippedBusy,
        busy_holder: Some(holder),
        at: 120_005,
    };
    schedule.append_firing(&busy).unwrap();
    assert_eq!(schedule.firings().unwrap(), vec![busy.clone()]);
    assert_eq!(
        schedule.append_firing(&ScheduleFiring {
            occurrence_ms: 180_000,
            busy_holder: None,
            ..busy.clone()
        }),
        Err(ScheduleDocumentError::Malformed("firings"))
    );
    assert_eq!(
        schedule.append_firing(&ScheduleFiring {
            occurrence_ms: 240_000,
            outcome: ScheduleFiringOutcome::Fired,
            busy_holder: Some(holder),
            ..busy
        }),
        Err(ScheduleDocumentError::Malformed("firings"))
    );
}

#[test]
fn concurrent_firing_retries_canonicalize_and_conflicts_are_visible() {
    let exact_a = schedule();
    let snapshot = exact_a.export_snapshot().unwrap();
    let exact_b = ScheduleDocument::from_snapshot(&snapshot).unwrap();
    let base_a = exact_a.version();
    let base_b = exact_b.version();
    let firing = ScheduleFiring {
        occurrence_ms: 300_000,
        revision: 1,
        run: RunId::from_uuid(v4(40)),
        outcome: ScheduleFiringOutcome::Fired,
        busy_holder: None,
        at: 300_010,
    };
    exact_a.append_firing(&firing).unwrap();
    exact_b.append_firing(&firing).unwrap();
    let update_a = exact_a.export_updates_since(&base_a).unwrap();
    let update_b = exact_b.export_updates_since(&base_b).unwrap();
    exact_a.import(&update_b).unwrap();
    exact_b.import(&update_a).unwrap();
    assert_eq!(exact_a.firings().unwrap(), vec![firing.clone()]);
    assert_eq!(exact_b.firings().unwrap(), vec![firing.clone()]);

    let conflict_a = schedule();
    let snapshot = conflict_a.export_snapshot().unwrap();
    let conflict_b = ScheduleDocument::from_snapshot(&snapshot).unwrap();
    let base_a = conflict_a.version();
    let base_b = conflict_b.version();
    conflict_a.append_firing(&firing).unwrap();
    let run_token = conflict_a
        .claim_authority(
            WorkspaceId::from_uuid(v4(2)),
            device(5),
            run_plan(301_000, RunId::from_uuid(v4(42))),
        )
        .unwrap()
        .unwrap();
    let skip_token = conflict_a
        .claim_authority(
            WorkspaceId::from_uuid(v4(2)),
            device(5),
            skip_plan(302_000, RunId::from_uuid(v4(43))),
        )
        .unwrap()
        .unwrap();
    conflict_b
        .append_firing(&ScheduleFiring {
            run: RunId::from_uuid(v4(41)),
            ..firing
        })
        .unwrap();
    let update_a = conflict_a.export_updates_since(&base_a).unwrap();
    let update_b = conflict_b.export_updates_since(&base_b).unwrap();
    conflict_a.import(&update_b).unwrap();
    conflict_b.import(&update_a).unwrap();
    assert_eq!(
        conflict_a.firings(),
        Err(ScheduleDocumentError::FiringConflict)
    );
    assert_eq!(
        conflict_b.firings(),
        Err(ScheduleDocumentError::FiringConflict)
    );
    assert!(matches!(
        run_token.into_run(),
        Err(ScheduleDocumentError::FiringConflict)
    ));
    assert!(matches!(
        skip_token.into_skip(),
        Err(ScheduleDocumentError::FiringConflict)
    ));
    assert!(matches!(
        conflict_a.claim_authority(
            WorkspaceId::from_uuid(v4(2)),
            device(5),
            skip_plan(301_000, RunId::from_uuid(v4(42))),
        ),
        Err(ScheduleDocumentError::FiringConflict)
    ));
}

#[test]
fn claim_authority_is_plan_bound_single_use_and_stale_after_every_authority_change() {
    let workspace = WorkspaceId::from_uuid(v4(2));
    let owner = device(5);

    let live = schedule();
    let token = live
        .claim_authority(workspace, owner, run_plan(60_000, RunId::from_uuid(v4(42))))
        .unwrap()
        .unwrap();
    let claim = token.into_run().unwrap();
    let debug = format!("{claim:?}");
    assert!(!debug.contains("毎朝"));
    assert!(!debug.contains("التقرير"));
    assert_eq!(claim.schedule, live.header().unwrap().schedule);
    assert_eq!(claim.revision, 1);
    assert_eq!(claim.workspace, workspace);
    assert_eq!(claim.occurrence_ms, 60_000);
    assert_eq!(
        claim.request.requested_model,
        Some(ModelName::new("gpt-5.4").unwrap())
    );
    assert_eq!(claim.request.executing_device, owner);
    assert_eq!(claim.request.context.len(), 1);

    let mismatched = ScheduleClaimPlan::Run {
        occurrence_ms: 60_000,
        run: RunId::from_uuid(v4(43)),
        pinned_context: vec![RunContextItem::Document {
            workspace,
            document: DocumentId::from_uuid(v4(44)),
            version: DocumentVersionId::from_uuid(v4(45)),
        }],
    };
    assert!(matches!(
        live.claim_authority(workspace, owner, mismatched),
        Err(ScheduleDocumentError::ClaimBindingMismatch)
    ));

    let edited = schedule();
    let token = edited
        .claim_authority(
            workspace,
            owner,
            skip_plan(61_000, RunId::from_uuid(v4(46))),
        )
        .unwrap()
        .unwrap();
    edited.update_definition(owner, &definition(2)).unwrap();
    assert!(matches!(
        token.into_skip(),
        Err(ScheduleDocumentError::StaleClaimAuthority)
    ));

    let paused = schedule();
    let token = paused
        .claim_authority(
            workspace,
            owner,
            skip_plan(62_000, RunId::from_uuid(v4(47))),
        )
        .unwrap()
        .unwrap();
    let mut pause = definition(2);
    pause.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    paused.update_definition(owner, &pause).unwrap();
    assert!(matches!(
        token.into_skip(),
        Err(ScheduleDocumentError::StaleClaimAuthority)
    ));

    let retired = schedule();
    let token = retired
        .claim_authority(
            workspace,
            owner,
            skip_plan(63_000, RunId::from_uuid(v4(48))),
        )
        .unwrap()
        .unwrap();
    retired.retire(owner, 20).unwrap();
    assert!(matches!(
        token.into_skip(),
        Err(ScheduleDocumentError::StaleClaimAuthority)
    ));

    let handed_off = schedule();
    let token = handed_off
        .claim_authority(
            workspace,
            owner,
            skip_plan(64_000, RunId::from_uuid(v4(49))),
        )
        .unwrap()
        .unwrap();
    let handoff = HandoffId::from_uuid(v4(50));
    handed_off
        .request_handoff(handoff, device(6), device(6), 30)
        .unwrap();
    handed_off.confirm_handoff(handoff, owner, 31).unwrap();
    assert!(matches!(
        token.into_skip(),
        Err(ScheduleDocumentError::StaleClaimAuthority)
    ));
}

#[test]
fn revision_exhaustion_is_a_named_refusal() {
    let mut exhausted = definition(u32::MAX);
    exhausted.effective_at_ms = 1_000;
    let schedule = ScheduleDocument::create(&ScheduleHeader {
        schedule: ScheduleId::from_uuid(v4(51)),
        created_at_ms: 1,
        initial_owner: device(5),
        definition: exhausted.clone(),
    })
    .unwrap();
    exhausted.effective_at_ms += 1;
    assert_eq!(
        schedule.update_definition(device(5), &exhausted),
        Err(ScheduleDocumentError::RevisionExhausted)
    );
}
