// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core as d;
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
mod routines;

#[test]
fn readable_submission_for_run_retains_archived_receipt_and_refuses_trash_and_foreign_owner() {
    let (mut repo, bot, owner, intent) = fixture();
    let saved = repo.prepare_bot_submission(&intent, HASH, 1, 2).unwrap();
    let run = intent.submission.request.run;
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.bot_readable_submission_for_run(owner, bot.id, run)
            .unwrap(),
        Some(saved)
    );
    assert!(
        repo.bot_submission_for_run(owner, bot.id, run).is_err(),
        "ordinary active-only route is unchanged"
    );
    assert!(
        repo.bot_readable_submission_for_run(
            BotOwner {
                device: DeviceId::new(),
                ..owner
            },
            bot.id,
            run
        )
        .is_err()
    );
    assert!(
        repo.bot_readable_submission_for_run(owner, bot.id, d::RunId::mint())
            .unwrap()
            .is_none()
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(
        repo.bot_readable_submission_for_run(owner, bot.id, run)
            .is_err()
    );
}

#[test]
fn boot_refusal_projection_bounds_prepared_rows_and_requires_one_exact_identity() {
    let (mut repo, bot, owner, intent) = fixture();
    let request = intent.submission.provenance.submission;
    repo.prepare_bot_submission(&intent, HASH, 1, 2).unwrap();
    let refused = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    let (_, barrier) = fence(&refused);
    let mut duplicate = None;
    for n in 1..=50 {
        let other = BotRequestId::mint();
        duplicate = Some(other);
        repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,outcome_json,created_at_ms,updated_at_ms) SELECT user_scope_id,?2,bot_id,fingerprint,command_kind,stage,?3,captured_config_revision,captured_policy_revision,submission_version,submission_json,outcome_json,created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id=?1",params![request.to_storage(),other.to_storage(),d::RunId::mint().to_storage()]).unwrap();
        let read = repo.bot_boot_snapshot(owner, bot.id, &d::BotBootAdmission::Missing {});
        let metadata = repo.bot_policy_metadata(owner, bot.id).unwrap();
        let projected = repo.bot_admission_barrier(&metadata, &d::BotBootAdmission::Missing {});
        assert_eq!(read.as_ref().map(|r| &r.admission), projected.as_ref());
        if n < 50 {
            assert_eq!(read.unwrap().admission, barrier)
        } else {
            assert!(matches!(read, Err(BotError::Storage)))
        }
    }
    repo.connection
        .execute(
            "DELETE FROM bot_command_receipts WHERE request_id=?1",
            [duplicate.unwrap().to_storage()],
        )
        .unwrap();
    let mut conflict = refused.outcome.unwrap();
    let BotExecutionOutcome::RefusalFence {
        barrier: ref mut conflicting,
        ..
    } = conflict
    else {
        panic!("fence")
    };
    conflicting.request = BotRequestId::mint();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![
                request.to_storage(),
                serde_json::to_string(&conflict).unwrap()
            ],
        )
        .unwrap();
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &d::BotBootAdmission::Missing {}),
        Err(BotError::Conflict)
    ));
}

#[test]
fn boot_replay_preserves_pending_refusal_and_recovers_erased_completed_identity() {
    use d::BotBootAdmission;
    let (mut repo, bot, owner, intent) = fixture();
    let request = intent.submission.provenance.submission;
    repo.prepare_bot_submission(&intent, HASH, 1, 2).unwrap();
    let refused = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    let (round, barrier) = fence(&refused);
    assert_eq!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .unwrap()
            .admission,
        barrier
    );
    let mut other = barrier.clone();
    other.request = BotRequestId::mint();
    assert!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Present { barrier: other })
            .is_err()
    );
    repo.acknowledge_bot_refusal_fence(owner, bot.id, request, round, &barrier, 4)
        .unwrap();
    repo.reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 5)
        .unwrap();
    assert_eq!(
        repo.bot_boot_snapshot(
            owner,
            bot.id,
            &BotBootAdmission::Present {
                barrier: barrier.clone()
            }
        )
        .unwrap()
        .admission,
        barrier
    );
}

#[test]
fn boot_refusal_projection_does_not_read_task_and_oversize_is_not_missing() {
    use d::BotBootAdmission;
    let (mut repo, bot, owner, intent) = fixture();
    let request = intent.submission.provenance.submission;
    repo.prepare_bot_submission(&intent, HASH, 1, 2).unwrap();
    let refused = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    let (_, barrier) = fence(&refused);
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET submission_json='{}' WHERE request_id=?1",
            [request.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .unwrap()
            .admission,
        barrier
    );
    let json =
        serde_json::to_string(refused.outcome.as_ref().unwrap()).unwrap() + &" ".repeat(8192);
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![request.to_storage(), json],
        )
        .unwrap();
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {}),
        Err(BotError::Storage)
    ));
}
fn fixture() -> (Repository, Bot, BotOwner, BotSubmissionIntent) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = super::super::tests::fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let intent = intent(&bot);
    (repo, bot, owner, intent)
}
fn intent(bot: &Bot) -> BotSubmissionIntent {
    BotSubmissionIntent {
        version: 1,
        submission: d::BotSubmission {
            provenance: d::BotProvenance {
                parent: None,
                routine: None,
                owner: d::BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot: bot.id,
                submission: BotRequestId::mint(),
                chat_id: bot.main_chat.as_uuid(),
                ai_attempt_id: None,
                assistant_message_id: None,
                config_revision: bot.revision,
                policy_revision: bot.policy_revision,
                continued_from: None,
            },
            policy: d::CapturedBotPolicy {
                workspace: bot.home_workspace,
                destination: bot.home_workspace,
                access: d::BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            request: d::DelegationRequest {
                run: d::RunId::mint(),
                requested_by: d::ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: d::Objective::new("PRIVATE-CURRENT-TASK").unwrap(),
                agent: d::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: d::WorkspaceScope::BotFolder {
                    folder: d::BotWorkingFolder::new(bot.config.working_folder.clone()).unwrap(),
                },
                context: vec![],
                grants: [bot.home_workspace].into_iter().collect(),
                executing_device: bot.owner_device,
                origin: d::RunOrigin::Manual,
            },
            workspace: bot.home_workspace,
            input: d::PendingInput {
                idempotency_key: d::IdempotencyKey::mint(),
                text: "PRIVATE-CURRENT-TASK".into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
        },
    }
}
fn view(i: &BotSubmissionIntent, state: d::BotAdmissionState) -> d::BotAdmissionView {
    let p = &i.submission.provenance;
    d::BotAdmissionView {
        key: d::BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        },
        run: i.submission.request.run,
        state,
        queue_sequence: 1,
        fence: None,
    }
}
fn fence(r: &BotSubmissionReceipt) -> (u32, d::BotAdmissionBarrier) {
    match &r.outcome {
        Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }) => (*round, barrier.clone()),
        _ => panic!("expected fence"),
    }
}
#[test]
fn another_conversation_shares_the_bots_first_task_fence() {
    let (mut repo, bot, _, first) = fixture();
    let other = repo
        .create_bot_chat(
            &bot,
            ProfileId::new(),
            BotRequestId::mint(),
            "other",
            Timestamp::from_millis(2),
        )
        .unwrap();
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    let mut second = intent(&bot);
    second.submission.provenance.chat_id = other.chat_id.as_uuid();
    // The second conversation cannot open a parallel first task for the Bot.
    assert_eq!(
        repo.prepare_bot_submission(&second, HASH, 1, 3),
        Err(BotError::FolderLocked)
    );
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(first.submission.provenance.submission)
    );
}

#[test]
fn first_intent_and_folder_guard_are_atomic_and_exact_retry_is_immutable() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    let prepared = repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
    assert_eq!(
        repo.prepare_bot_submission(&first, HASH, 1, 3).unwrap(),
        prepared
    );
    let mut changed = first.clone();
    changed.submission.input.text.push('x');
    assert_eq!(
        repo.prepare_bot_submission(&changed, HASH, 1, 3),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.prepare_bot_submission(&intent(&bot), HASH, 1, 3),
        Err(BotError::FolderLocked)
    );
    let read = repo
        .bot_admission_receipt(owner, bot.id, request)
        .unwrap()
        .unwrap()
        .operation();
    let encoded = serde_json::to_string(&read).unwrap();
    assert!(!encoded.contains("PRIVATE"));
    // Rollback after the helper has performed both mutations, as P1.5 composition must.
    let (mut other, other_bot, _, other_intent) = fixture();
    {
        let tx = other.connection.transaction().unwrap();
        prepare_bot_submission_tx(&tx, &other_intent, HASH, 1, 2).unwrap();
    }
    assert_eq!(
        other
            .bot(other_bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request,
        None
    );
    assert!(
        read_bot_submission_on(
            &other.connection,
            other_bot.user_scope,
            other_intent.submission.provenance.submission
        )
        .unwrap()
        .is_none()
    );
    // A late insert failure must also roll the provisional lock back.
    other.connection.execute_batch("CREATE TRIGGER refuse_intent BEFORE INSERT ON bot_command_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    assert!(
        other
            .prepare_bot_submission(&other_intent, HASH, 1, 2)
            .is_err()
    );
    assert_eq!(
        other
            .bot(other_bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request,
        None
    );
}
#[test]
fn exact_ordered_refusal_fence_is_the_only_unlock_and_stale_rounds_refuse() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    let r = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Capacity,
            false,
            3,
        )
        .unwrap();
    let (round, barrier) = fence(&r);
    assert!(
        repo.reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 4)
            .is_err()
    );
    let mut wrong = barrier.clone();
    wrong.request = BotRequestId::mint();
    assert!(
        repo.acknowledge_bot_refusal_fence(owner, bot.id, request, round, &wrong, 4)
            .is_err()
    );
    repo.acknowledge_bot_refusal_fence(owner, bot.id, request, round, &barrier, 4)
        .unwrap();
    assert!(
        repo.prepare_bot_refusal_fence(owner, bot.id, request, BotAdmissionRefusal::Stale, true, 5)
            .is_err()
    );
    let pause = repo
        .prepare_bot_pause(
            owner,
            bot.id,
            BotRequestId::mint(),
            barrier.epoch,
            true,
            HASH,
            5,
        )
        .unwrap();
    assert!(
        repo.prepare_bot_refusal_fence(owner, bot.id, request, BotAdmissionRefusal::Stale, true, 5)
            .is_err()
    );
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        5,
    )
    .unwrap();
    let r = repo
        .prepare_bot_refusal_fence(owner, bot.id, request, BotAdmissionRefusal::Stale, true, 5)
        .unwrap();
    let (next, new_barrier) = fence(&r);
    assert!(
        repo.reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 6)
            .is_err()
    );
    repo.acknowledge_bot_refusal_fence(owner, bot.id, request, next, &new_barrier, 6)
        .unwrap();
    let rejected = repo
        .reject_bot_submission_after_fence(owner, bot.id, request, next, &new_barrier, 7)
        .unwrap();
    // The renewal was for a stale fence; the refusal keeps its own reason.
    assert!(matches!(
        rejected.outcome,
        Some(BotExecutionOutcome::Rejected {
            reason: BotAdmissionRefusal::Capacity,
            ..
        })
    ));
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(current.folder_lock_request, None);
    assert_eq!(current.folder_frozen_at_ms, None);
    assert_eq!(
        repo.prepare_bot_submission(&first, HASH, 1, 8)
            .unwrap()
            .stage,
        BotReceiptStage::Rejected
    );
    let resume = repo
        .prepare_bot_pause(
            owner,
            bot.id,
            BotRequestId::mint(),
            current.revision,
            false,
            HASH,
            8,
        )
        .unwrap();
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        resume.intent.barrier.request,
        &resume.intent.barrier,
        8,
    )
    .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    let second = intent(&current);
    repo.prepare_bot_submission(&second, HASH, current.revision, 9)
        .unwrap();
}
#[test]
fn accepted_even_refused_identity_freezes_folder_and_prevents_late_unlock() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    let r = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Rejected,
            false,
            3,
        )
        .unwrap();
    let (round, barrier) = fence(&r);
    repo.acknowledge_bot_refusal_fence(owner, bot.id, request, round, &barrier, 4)
        .unwrap();
    let accepted = view(&first, d::BotAdmissionState::Refused);
    repo.acknowledge_bot_submission(owner, bot.id, request, &accepted, 5)
        .unwrap();
    assert!(
        repo.reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 6)
            .is_err()
    );
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(current.folder_frozen_at_ms, Some(5));
    assert_eq!(current.folder_lock_request, None);
    repo.prepare_bot_submission(&intent(&current), HASH, current.revision, 7)
        .unwrap();
    repo.prepare_bot_submission(&intent(&current), HASH, current.revision, 8)
        .unwrap();
    let mut wrong = accepted.clone();
    wrong.run = d::RunId::mint();
    assert!(
        repo.acknowledge_bot_submission(owner, bot.id, request, &wrong, 9)
            .is_err()
    );
    repo.acknowledge_bot_submission(owner, bot.id, request, &accepted, 10)
        .unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_frozen_at_ms,
        Some(5)
    );
}
#[test]
fn pause_is_atomic_exact_and_blocks_new_admission_until_acknowledged() {
    let (mut repo, bot, owner, _) = fixture();
    let request = BotRequestId::mint();
    let paused = repo
        .prepare_bot_pause(owner, bot.id, request, 1, true, HASH, 2)
        .unwrap();
    assert_eq!(
        repo.prepare_bot_pause(owner, bot.id, request, 1, true, HASH, 3)
            .unwrap(),
        paused
    );
    assert!(
        repo.prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 2, false, HASH, 3)
            .is_err()
    );
    let mut wrong = paused.intent.barrier.clone();
    wrong.epoch += 1;
    assert!(
        repo.acknowledge_bot_pause(owner, bot.id, request, &wrong, 4)
            .is_err()
    );
    repo.acknowledge_bot_pause(owner, bot.id, request, &paused.intent.barrier, 4)
        .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert!(
        repo.prepare_bot_submission(&intent(&current), HASH, 2, 4)
            .is_err()
    );
    let resumed = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 2, false, HASH, 5)
        .unwrap();
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert!(
        repo.prepare_bot_submission(&intent(&current), HASH, 3, 6)
            .is_err()
    );
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        resumed.intent.barrier.request,
        &resumed.intent.barrier,
        6,
    )
    .unwrap();
    repo.prepare_bot_submission(&intent(&current), HASH, 3, 7)
        .unwrap();
}
#[test]
fn malformed_foreign_and_recovery_rows_are_bounded_and_fail_closed() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    let mut foreign = first.clone();
    foreign.submission.provenance.owner.user_scope = UserScopeId::new();
    assert!(repo.prepare_bot_submission(&foreign, HASH, 1, 2).is_err());
    let mut foreign = first.clone();
    foreign.submission.provenance.chat_id = Uuid::new_v4();
    assert!(repo.prepare_bot_submission(&foreign, HASH, 1, 2).is_err());
    let mut foreign = first.clone();
    foreign.submission.provenance.config_revision = 2;
    assert!(repo.prepare_bot_submission(&foreign, HASH, 1, 2).is_err());
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    assert!(
        repo.bot_admission_receipt(
            BotOwner {
                device: DeviceId::from_uuid(Uuid::new_v4()),
                ..owner
            },
            bot.id,
            request
        )
        .is_err()
    );
    let mut foreign = first.clone();
    foreign.submission.policy.access = d::BotAccess::Workspace;
    assert!(repo.prepare_bot_submission(&foreign, HASH, 1, 2).is_err());
    let mut foreign = first.clone();
    foreign
        .submission
        .policy
        .selected_pages
        .insert(DocumentId::new());
    assert!(repo.prepare_bot_submission(&foreign, HASH, 1, 2).is_err());
    let rows = repo.pending_bot_operations(0, 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, request);
    assert!(
        repo.pending_bot_operations(rows[0].0, 64)
            .unwrap()
            .is_empty()
    );
    let mut payload = serde_json::to_value(&first).unwrap();
    payload["submission"]["input"]["ignored_intent"] = true.into();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET submission_json=?1 WHERE request_id=?2",
            params![payload.to_string(), request.to_storage()],
        )
        .unwrap();
    assert!(repo.bot_admission_receipt(owner, bot.id, request).is_err());
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
}
#[test]
fn accepted_recovery_projection_precedes_later_pending_without_becoming_pending_again() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    repo.acknowledge_bot_submission(
        owner,
        bot.id,
        request,
        &view(&first, d::BotAdmissionState::Queued),
        3,
    )
    .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET stage='accepted' WHERE request_id=?1",
            [request.to_storage()],
        )
        .unwrap();
    let second = intent(&bot);
    repo.prepare_bot_submission(&second, HASH, 1, 4).unwrap();
    let rows = repo.pending_bot_operations(0, 16).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        repo.bot_admission_receipt(owner, bot.id, rows[0].3)
            .unwrap()
            .unwrap()
            .operation()
            .status,
        BotOperationStatus::Accepted
    );
    assert_eq!(
        repo.bot_admission_receipt(owner, bot.id, rows[1].3)
            .unwrap()
            .unwrap()
            .operation()
            .status,
        BotOperationStatus::Pending
    );
}
#[test]
fn accepted_receipt_refuses_foreign_nested_execution_fence_and_invalid_queue_sequence() {
    let (mut repo, bot, owner, intent) = fixture();
    let request = intent.submission.provenance.submission;
    repo.prepare_bot_submission(&intent, HASH, 1, 2).unwrap();
    let mut accepted = view(&intent, d::BotAdmissionState::Executing);
    accepted.fence = Some(d::BotExecutionFence {
        owner: intent.submission.provenance.owner,
        bot: bot.id,
        run: intent.submission.request.run,
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::FIRST,
        session: d::SessionId::mint(),
        turn: None,
    });
    let mut wrong = accepted.clone();
    wrong.fence.as_mut().unwrap().owner.user_scope = UserScopeId::new();
    assert!(
        repo.acknowledge_bot_submission(owner, bot.id, request, &wrong, 3)
            .is_err()
    );
    let mut wrong = accepted.clone();
    wrong.fence.as_mut().unwrap().bot = BotId::mint();
    assert!(
        repo.acknowledge_bot_submission(owner, bot.id, request, &wrong, 3)
            .is_err()
    );
    let mut wrong = accepted.clone();
    wrong.fence.as_mut().unwrap().run = d::RunId::mint();
    assert!(
        repo.acknowledge_bot_submission(owner, bot.id, request, &wrong, 3)
            .is_err()
    );
    let mut wrong = accepted.clone();
    wrong.queue_sequence = 0;
    assert!(
        repo.acknowledge_bot_submission(owner, bot.id, request, &wrong, 3)
            .is_err()
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().folder_frozen_at_ms, None);
    repo.acknowledge_bot_submission(owner, bot.id, request, &accepted, 4)
        .unwrap();
    let mut corrupted = accepted;
    corrupted.fence.as_mut().unwrap().run = d::RunId::mint();
    let outcome = BotExecutionOutcome::Accepted {
        version: 1,
        admission: corrupted,
    };
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?1 WHERE request_id=?2",
            params![
                serde_json::to_string(&outcome).unwrap(),
                request.to_storage()
            ],
        )
        .unwrap();
    assert!(repo.bot_admission_receipt(owner, bot.id, request).is_err());
}
#[test]
fn unresolved_intents_are_bounded_without_preventing_exact_retry_or_finalization() {
    let (mut repo, bot, owner, first) = fixture();
    let request = first.submission.provenance.submission;
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    repo.acknowledge_bot_submission(
        owner,
        bot.id,
        request,
        &view(&first, d::BotAdmissionState::Settled),
        3,
    )
    .unwrap();
    let pending = (0..50).map(|_| intent(&bot)).collect::<Vec<_>>();
    for item in &pending {
        repo.prepare_bot_submission(item, HASH, 1, 4).unwrap();
    }
    let overflow = intent(&bot);
    assert_eq!(
        repo.prepare_bot_submission(&overflow, HASH, 1, 5),
        Err(BotError::Conflict)
    );
    repo.prepare_bot_submission(&pending[0], HASH, 1, 5)
        .unwrap();
    repo.acknowledge_bot_submission(
        owner,
        bot.id,
        pending[0].submission.provenance.submission,
        &view(&pending[0], d::BotAdmissionState::Queued),
        6,
    )
    .unwrap();
    repo.prepare_bot_submission(&overflow, HASH, 1, 7).unwrap();
    assert_eq!(repo.pending_bot_operations(0, 64).unwrap().len(), 50);
}

/// What a lifecycle capture refused, and the Bot it was refused on.
struct Waiting {
    repo: Repository,
    bot: Bot,
    owner: BotOwner,
    /// A second conversation of the Bot.
    other: ChatId,
    /// Start receipts the owner never admitted: two in the main
    /// conversation, then one in `other`.
    requests: Vec<BotRequestId>,
}
/// A Bot whose first task was admitted (so its folder is frozen and more
/// may wait beside each other), with three Start receipts its owner never
/// admitted.
fn waiting() -> Waiting {
    let (mut repo, bot, owner, first) = fixture();
    let other = repo
        .create_bot_chat(
            &bot,
            ProfileId::new(),
            BotRequestId::mint(),
            "other",
            Timestamp::from_millis(2),
        )
        .unwrap()
        .chat_id;
    repo.prepare_bot_submission(&first, HASH, 1, 2).unwrap();
    repo.acknowledge_bot_submission(
        owner,
        bot.id,
        first.submission.provenance.submission,
        &view(&first, d::BotAdmissionState::Settled),
        3,
    )
    .unwrap();
    let requests = [bot.main_chat, bot.main_chat, other]
        .iter()
        .map(|chat| {
            let mut waiting = intent(&bot);
            waiting.submission.provenance.chat_id = chat.as_uuid();
            repo.prepare_bot_submission(&waiting, HASH, 1, 4).unwrap();
            waiting.submission.provenance.submission
        })
        .collect();
    Waiting {
        repo,
        bot,
        owner,
        other,
        requests,
    }
}
fn lifecycle(
    bot: &Bot,
    owner: BotOwner,
    kind: BotLifecycleKind,
    chat: Option<ChatId>,
    revision: u64,
) -> crate::LifecycleCapture {
    crate::LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind,
        chat,
        fingerprint: HASH.into(),
        expected_revision: revision,
        source: None,
        members: vec![],
        now: 20,
    }
}
fn outcome(
    repo: &Repository,
    owner: BotOwner,
    request: BotRequestId,
) -> Option<BotExecutionOutcome> {
    read_bot_submission_on(&repo.connection, owner.user_scope, request)
        .unwrap()
        .unwrap()
        .outcome
}
#[test]
fn a_whole_bot_capture_refuses_unadmitted_work_for_good_behind_its_own_pause() {
    let Waiting {
        mut repo,
        bot,
        owner,
        requests,
        ..
    } = waiting();
    let revision = repo.bot(bot.id).unwrap().unwrap().revision;
    let archive = repo
        .capture_bot_lifecycle(&lifecycle(
            &bot,
            owner,
            BotLifecycleKind::Archive,
            None,
            revision,
        ))
        .unwrap();
    let Some(BotAdmissionReceipt::Pause(pause)) = repo
        .bot_admission_receipt(owner, bot.id, archive.pause_request.unwrap())
        .unwrap()
    else {
        panic!("pause receipt")
    };
    // The operation's pause is the fence of each, in every conversation:
    // newer than its revision, and paused.
    for request in &requests {
        assert_eq!(
            outcome(&repo, owner, *request),
            Some(BotExecutionOutcome::RefusalFence {
                version: 1,
                round: 1,
                barrier: pause.intent.barrier.clone(),
                acknowledged: false,
                reason: BotAdmissionRefusal::Rejected,
            })
        );
    }
    // Only the pause moved the revision, and nothing is admitted meanwhile.
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, revision + 1);
    assert!(!repo.bot_admitting(bot.id).unwrap());
    // Reversing the parked operation leaves every one of them refused.
    repo.park_bot_lifecycle(
        owner.user_scope,
        archive.operation,
        BotLifecycleRefusal::OwnerUnavailable,
        21,
    )
    .unwrap();
    let mut restore = lifecycle(&bot, owner, BotLifecycleKind::Restore, None, revision + 1);
    restore.source = Some(archive.operation);
    repo.unarchive_bot(&restore).unwrap();
    for request in &requests {
        assert!(matches!(
            outcome(&repo, owner, *request),
            Some(BotExecutionOutcome::RefusalFence { .. })
        ));
    }
    // Asked to refuse one again, the stored fence stands, even while the
    // pause still waits for its owner.
    let kept = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            requests[0],
            BotAdmissionRefusal::Rejected,
            false,
            22,
        )
        .unwrap();
    assert_eq!(kept.outcome, outcome(&repo, owner, requests[0]));
    // Paused, and its pause still unacknowledged: nothing is admitted; once
    // acknowledged it is still paused until a Resume.
    assert!(!repo.bot_admitting(bot.id).unwrap());
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        23,
    )
    .unwrap();
    assert!(!repo.bot_admitting(bot.id).unwrap());
}
#[test]
fn a_conversation_capture_refuses_only_its_own_work_behind_one_fence() {
    let Waiting {
        mut repo,
        bot,
        owner,
        other,
        requests,
    } = waiting();
    let revision = repo.bot(bot.id).unwrap().unwrap().revision;
    assert!(repo.bot_admitting(bot.id).unwrap());
    // A pause still waiting for its owner: the fence would have to wait for
    // it, so the capture answers that another change is settling.
    let pause = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), revision, true, HASH, 5)
        .unwrap();
    assert_eq!(
        repo.capture_bot_lifecycle(&lifecycle(
            &bot,
            owner,
            BotLifecycleKind::TrashChat,
            Some(bot.main_chat),
            revision + 1,
        )),
        Err(BotError::Settling)
    );
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        6,
    )
    .unwrap();
    let revision = revision + 1;
    repo.capture_bot_lifecycle(&lifecycle(
        &bot,
        owner,
        BotLifecycleKind::TrashChat,
        Some(bot.main_chat),
        revision,
    ))
    .unwrap();
    // One fence for both of the conversation's, at one new revision.
    let fenced = outcome(&repo, owner, requests[0]);
    assert_eq!(fenced, outcome(&repo, owner, requests[1]));
    let Some(BotExecutionOutcome::RefusalFence { barrier, round, .. }) = &fenced else {
        panic!("fenced: {fenced:?}")
    };
    assert_eq!(
        (*round, barrier.epoch, barrier.paused),
        (1, revision + 1, true)
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, revision + 1);
    // The sibling conversation's waiting task is not touched.
    assert_eq!(outcome(&repo, owner, requests[2]), None);
    let _ = other;
}
#[test]
fn a_conversation_capture_with_nothing_to_refuse_moves_nothing() {
    let (mut repo, bot, owner, _) = fixture();
    let revision = repo.bot(bot.id).unwrap().unwrap().revision;
    repo.capture_bot_lifecycle(&lifecycle(
        &bot,
        owner,
        BotLifecycleKind::TrashChat,
        Some(bot.main_chat),
        revision,
    ))
    .unwrap();
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, revision);
}
#[test]
fn a_change_waiting_for_its_owner_answers_settling_never_a_reused_request() {
    let (mut repo, bot, owner, _) = fixture();
    repo.prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 1, true, HASH, 2)
        .unwrap();
    assert_eq!(
        repo.prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 2, false, HASH, 3),
        Err(BotError::Settling)
    );
    assert_eq!(
        repo.capture_bot_lifecycle(&lifecycle(&bot, owner, BotLifecycleKind::Archive, None, 2)),
        Err(BotError::Settling)
    );
    assert_eq!(BotError::Settling.code(), "bot_operation_pending");
}
#[test]
fn a_definitely_refused_pause_settles_and_is_offered_again_newer_or_not_at_all() {
    let (mut repo, bot, owner, _) = fixture();
    let first = BotRequestId::mint();
    let paused = repo
        .prepare_bot_pause(owner, bot.id, first, 1, true, HASH, 2)
        .unwrap();
    // Refused stale: settled rejected, and the same state offered again at
    // the next revision, a newer epoch.
    let again = repo
        .settle_refused_bot_pause(owner, bot.id, first, true, 3)
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.bot_admission_receipt(owner, bot.id, first)
            .unwrap()
            .unwrap()
            .operation()
            .status,
        BotOperationStatus::Rejected
    );
    assert!(again.intent.barrier.paused);
    assert_eq!(again.intent.barrier.epoch, paused.intent.barrier.epoch + 1);
    // Refused for good: settled rejected, nothing follows, nothing waits.
    assert_eq!(
        repo.settle_refused_bot_pause(owner, bot.id, again.intent.barrier.request, false, 4)
            .unwrap(),
        None
    );
    let current = repo.bot(bot.id).unwrap().unwrap();
    assert!(current.paused);
    repo.prepare_bot_pause(
        owner,
        bot.id,
        BotRequestId::mint(),
        current.revision,
        false,
        HASH,
        5,
    )
    .unwrap();
    // An already settled one is left as it is.
    assert_eq!(
        repo.settle_refused_bot_pause(owner, bot.id, first, true, 6)
            .unwrap(),
        None
    );
}
