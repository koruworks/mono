// SPDX-License-Identifier: MPL-2.0
use super::super::recent;
use super::*;

fn linked(repo: &mut Repository, input: &BotRoutineCapturedSources) -> BotRoutineStoredPreparation {
    let draft = prepare(repo, input);
    let mut record = claim(&draft);
    let row = finalize(repo, input, &record);
    record.link_state = RoutineLinkState::Terminal {
        reason: RoutineTerminalReason::Stopped,
    };
    finalize(repo, input, &record);
    row
}
fn read(
    repo: &Repository,
    input: &BotRoutineCapturedSources,
) -> Result<BotRoutineLocalConversations, BotError> {
    recent::read(
        repo,
        owner(&input.slot),
        input.slot.bot,
        input.slot.schedule,
        input.canonical.home,
    )
    .map(|r| r.observation())
}

#[test]
fn listing_recent_selects_five_before_joins_and_uses_existing_partial_index() {
    let (mut repo, mut input) = fixture();
    let mut ids = Vec::new();
    for _ in 0..6 {
        ids.push(linked(&mut repo, &input).prepared.ids.run);
        input.slot.occurrence_ms += 60_000;
    }
    let BotRoutineLocalConversations::Available { items, more } = read(&repo, &input).unwrap()
    else {
        panic!("local references")
    };
    assert!(more);
    assert_eq!(
        items.iter().map(|r| r.run_id).collect::<Vec<_>>(),
        ids.into_iter().rev().take(4).collect::<Vec<_>>()
    );
    let sql = format!("EXPLAIN QUERY PLAN {}", recent::candidates_sql());
    let mut q = repo.connection.prepare(&sql).unwrap();
    let plans = q
        .query_map(
            params![
                input.slot.owner.user_scope.to_storage(),
                input.slot.owner.device.to_storage(),
                input.slot.bot.to_storage(),
                input.slot.schedule.to_storage()
            ],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plans
            .iter()
            .any(|p| p.contains("bot_routine_conversations")),
        "{plans:?}"
    );
    assert!(
        !plans.iter().any(|p| p.contains("TEMP B-TREE")),
        "{plans:?}"
    );
}

#[test]
fn listing_recent_corrupt_fifth_refuses_instead_of_backfilling_or_empty_history() {
    let (mut repo, mut input) = fixture();
    let mut rows = Vec::new();
    for _ in 0..6 {
        rows.push(linked(&mut repo, &input));
        input.slot.occurrence_ms += 60_000;
    }
    let fifth = &rows[1];
    repo.connection.execute("UPDATE bot_command_receipts SET captured_config_revision=captured_config_revision+1 WHERE request_id=?1",[fifth.prepared.ids.submission.to_storage()]).unwrap();
    assert_eq!(read(&repo, &input), Err(BotError::Storage));
    let snapshot = repo
        .routine_list_snapshot(owner(&input.slot), input.slot.bot, None, 16)
        .unwrap();
    let (_, _, item) = repo.routine_list_entry(&snapshot, 0).unwrap();
    assert_eq!(
        item.local_conversations,
        BotRoutineLocalConversations::Unavailable {}
    );
}

#[test]
fn listing_recent_uses_owning_receipt_stage_matrix_and_exact_outcome_subject() {
    let (mut repo, input) = fixture();
    let row = linked(&mut repo, &input);
    let p = &row.prepared;
    let accepted = BotExecutionOutcome::Accepted {
        version: 1,
        admission: koru_delegate_core::BotAdmissionView {
            key: koru_delegate_core::BotSubmissionKey {
                owner: input.slot.owner,
                bot: input.slot.bot,
                request: p.ids.submission,
            },
            run: p.ids.run,
            state: koru_delegate_core::BotAdmissionState::Settled,
            queue_sequence: 1,
            fence: None,
        },
    };
    let rejected = BotExecutionOutcome::Rejected {
        version: 1,
        reason: BotAdmissionRefusal::Rejected,
    };
    for (stage, outcome, valid) in [
        ("prepared", None, true),
        ("accepted", Some(&accepted), true),
        ("completed", Some(&accepted), true),
        ("rejected", Some(&rejected), true),
        ("accepted", None, false),
        ("prepared", Some(&accepted), false),
        ("completed", Some(&rejected), false),
    ] {
        repo.connection
            .execute(
                "UPDATE bot_command_receipts SET stage=?1,outcome_json=?2 WHERE request_id=?3",
                params![
                    stage,
                    outcome.map(|o| serde_json::to_string(o).unwrap()),
                    p.ids.submission.to_storage()
                ],
            )
            .unwrap();
        assert_eq!(read(&repo, &input).is_ok(), valid, "{stage}");
    }
    let mut wrong = accepted;
    let BotExecutionOutcome::Accepted { admission, .. } = &mut wrong else {
        unreachable!()
    };
    admission.key.request = BotRequestId::mint();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET stage='accepted',outcome_json=?1 WHERE request_id=?2",
            params![
                serde_json::to_string(&wrong).unwrap(),
                p.ids.submission.to_storage()
            ],
        )
        .unwrap();
    assert_eq!(read(&repo, &input), Err(BotError::Storage));
}

#[test]
fn listing_recent_missing_foreign_and_duplicate_associations_are_not_filtered() {
    for sql in [
        "UPDATE inference_attempts SET operation_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_chat_runs SET owner_device_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE messages SET role='assistant' WHERE role='user'",
        "UPDATE chats SET profile_id=NULL",
        "DELETE FROM bot_chats WHERE purpose<>'main'",
    ] {
        let (mut repo, input) = fixture();
        linked(&mut repo, &input);
        repo.connection
            .execute("PRAGMA foreign_keys=OFF", [])
            .unwrap();
        repo.connection.execute(sql, []).unwrap();
        assert!(read(&repo, &input).is_err(), "{sql}");
    }
    // Unique indexes already prevent ordinary duplicate correlation; retain the
    // exact count guard and prove a second row cannot disguise a missing join.
    let (mut repo, mut input) = fixture();
    let first = linked(&mut repo, &input);
    input.slot.occurrence_ms += 60_000;
    let second = linked(&mut repo, &input);
    assert!(
        repo.connection
            .execute(
                "UPDATE bot_chat_runs SET run_id=?1 WHERE ai_attempt_id=?2",
                params![
                    first.prepared.ids.run.to_storage(),
                    second.prepared.ids.attempt.to_string()
                ]
            )
            .is_err()
    );
    assert!(read(&repo, &input).is_ok());
}

#[test]
fn listing_recent_scalar_bounds_and_payload_omission_do_not_weaken_full_reader() {
    let (mut repo, input) = fixture();
    let row = linked(&mut repo, &input);
    // These normalized signed fields intentionally do not cause the projection
    // to fetch/reinterpret the capture. The authority reader below still does.
    repo.connection.execute("UPDATE bot_routine_preparations SET occurrence_ms=-60000,created_at_ms=-60001,updated_at_ms=-60000",[]).unwrap();
    repo.connection.execute("UPDATE bot_command_receipts SET created_at_ms=-60001,updated_at_ms=-60000 WHERE command_kind='routine_occurrence'",[]).unwrap();
    let BotRoutineLocalConversations::Available { items, .. } = read(&repo, &input).unwrap() else {
        panic!("signed local observation")
    };
    assert_eq!(items[0].occurrence_ms, -60000);
    repo.connection
        .execute(
            "UPDATE bot_routine_preparations SET capture_json='{}',submission_json='{}'",
            [],
        )
        .unwrap();
    repo.connection.execute("UPDATE bot_command_receipts SET submission_json='{}' WHERE command_kind='routine_occurrence'",[]).unwrap();
    assert!(read(&repo, &input).is_ok());
    assert!(
        repo.routine_occurrence_for_run(owner(&input.slot), input.slot.bot, row.prepared.ids.run)
            .is_err()
    );
    repo.connection
        .execute("PRAGMA ignore_check_constraints=ON", [])
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_routine_preparations SET definition_digest=zeroblob(1000000)",
            [],
        )
        .unwrap();
    assert_eq!(read(&repo, &input), Err(BotError::Storage));
}

#[test]
fn listing_recent_chat_trash_retains_reference_and_final_visibility_drift_refuses() {
    let (mut repo, input) = fixture();
    let row = linked(&mut repo, &input);
    let snapshot = repo
        .routine_list_snapshot(owner(&input.slot), input.slot.bot, None, 16)
        .unwrap();
    let (_, checkpoint, _) = repo.routine_list_entry(&snapshot, 0).unwrap();
    // Capture the real operation before applying its existing fixture-only
    // Trash effect, preserving the journal's relational identity.
    let chat = ChatId::from_uuid(row.prepared.ids.chat);
    let operation = repo
        .capture_bot_lifecycle(&crate::LifecycleCapture {
            operation: BotRequestId::mint(),
            owner: owner(&input.slot),
            bot: input.slot.bot,
            kind: BotLifecycleKind::TrashChat,
            chat: Some(chat),
            fingerprint: "a".repeat(64),
            expected_revision: input.canonical.config_revision,
            source: None,
            members: vec![],
            now: 70000,
        })
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, chat, 70001)
        .unwrap();
    let BotRoutineLocalConversations::Available { items, more } = read(&repo, &input).unwrap()
    else {
        panic!("retained reference")
    };
    assert!(!more);
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].conversation,
        BotRoutineConversationVisibility::Unavailable
    );
    assert!(
        repo.validate_routine_list(&snapshot, &[checkpoint])
            .is_err()
    );
}

#[test]
fn listing_recent_real_finalizer_keeps_materialized_terminal_history_linked() {
    for reason in [
        RoutineTerminalReason::Stopped,
        RoutineTerminalReason::AlreadyTerminal,
    ] {
        let (mut repo, input) = fixture();
        let draft = prepare(&mut repo, &input);
        let mut record = claim(&draft);
        let row = finalize(&mut repo, &input, &record);
        record.link_state = RoutineLinkState::Terminal { reason };
        assert_eq!(
            finalize(&mut repo, &input, &record).stage,
            BotRoutineStoredStage::Linked
        );
        let BotRoutineLocalConversations::Available { items, .. } = read(&repo, &input).unwrap()
        else {
            panic!("retained history")
        };
        assert_eq!(items[0].run_id, row.prepared.ids.run);
        let (mut unmaterialized, input) = fixture();
        let draft = prepare(&mut unmaterialized, &input);
        let mut record = claim(&draft);
        record.link_state = RoutineLinkState::Terminal { reason };
        finalize(&mut unmaterialized, &input, &record);
        assert_eq!(
            read(&unmaterialized, &input).unwrap(),
            BotRoutineLocalConversations::Available {
                items: vec![],
                more: false
            }
        );
    }
}
