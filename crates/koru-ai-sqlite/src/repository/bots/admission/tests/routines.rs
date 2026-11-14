// SPDX-License-Identifier: MPL-2.0
//! Synthetic finalized receipts prove decoding, never delegate claim authority.
use super::*;

fn routine(mut intent: BotSubmissionIntent) -> BotSubmissionIntent {
    intent.version = 2;
    let routine = d::RoutineProvenance {
        schedule: d::ScheduleId::mint(),
        occurrence_ms: 60_000,
        definition: d::RoutineDefinitionIdentity {
            revision: 1,
            digest: d::RoutineDigest32::from_bytes([3; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    };
    intent.submission.request.origin = d::RunOrigin::Scheduled {
        schedule: routine.schedule,
        occurrence_ms: routine.occurrence_ms,
        revision: routine.definition.revision,
    };
    intent.submission.provenance.routine = Some(routine);
    intent.submission.provenance.ai_attempt_id = Some(uuid::Uuid::new_v4());
    intent.submission.provenance.assistant_message_id = Some(uuid::Uuid::new_v4());
    intent
}

fn insert_routine(repo: &Repository, intent: &BotSubmissionIntent) {
    let p = &intent.submission.provenance;
    repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'routine_occurrence','prepared',?5,?6,?7,2,?8,2,2)",params![p.owner.user_scope.to_storage(),p.submission.to_storage(),p.bot.to_storage(),HASH,intent.submission.request.run.to_storage(),p.config_revision,p.policy_revision,serde_json::to_string(intent).unwrap()]).unwrap();
}

fn receipt(repo: &Repository, i: &BotSubmissionIntent) -> Result<BotSubmissionReceipt, BotError> {
    let p = &i.submission.provenance;
    decode_submission(
        raw(&repo.connection, p.owner.user_scope, p.submission)?.unwrap(),
        p.owner.user_scope,
        p.submission,
    )
}

#[test]
fn routine_occurrence_decoder_requires_explicit_v2_origin_and_exact_retained_identity() {
    let (repo, _, _, base) = fixture();
    let intent = routine(base);
    insert_routine(&repo, &intent);
    let decoded = receipt(&repo, &intent).unwrap();
    assert_eq!(decoded.kind, BotOperationKind::RoutineOccurrence);
    assert_eq!(decoded.intent, intent);
    let mut cases = vec![];
    let mut changed = intent.clone();
    changed.version = 1;
    cases.push(serde_json::to_value(changed).unwrap());
    let mut changed = intent.clone();
    changed.submission.request.origin = d::RunOrigin::Manual;
    cases.push(serde_json::to_value(changed).unwrap());
    let mut changed = intent.clone();
    changed
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .occurrence_ms += 1;
    cases.push(serde_json::to_value(changed).unwrap());
    let mut changed = intent.clone();
    changed.submission.provenance.routine = None;
    cases.push(serde_json::to_value(changed).unwrap());
    let mut changed = intent.clone();
    changed.submission.provenance.submission = BotRequestId::mint();
    cases.push(serde_json::to_value(changed).unwrap());
    let mut changed = intent.clone();
    changed.submission.request.run = d::RunId::mint();
    cases.push(serde_json::to_value(changed).unwrap());
    let mut unknown = serde_json::to_value(&intent).unwrap();
    unknown["submission"]["provenance"]["routine"]["unknown"] = true.into();
    cases.push(unknown);
    for value in cases {
        repo.connection.execute("UPDATE bot_command_receipts SET submission_json=?1 WHERE command_kind='routine_occurrence'",[value.to_string()]).unwrap();
        assert!(receipt(&repo, &intent).is_err());
    }
    repo.connection.execute("UPDATE bot_command_receipts SET submission_json=?1 WHERE command_kind='routine_occurrence'",[serde_json::to_string(&intent).unwrap()]).unwrap();
    assert_eq!(receipt(&repo, &intent).unwrap(), decoded);
    for field in ["version", "kind", "expected", "config", "policy"] {
        let p = &intent.submission.provenance;
        let mut row = raw(&repo.connection, p.owner.user_scope, p.submission)
            .unwrap()
            .unwrap();
        match field {
            "version" => row.version = Some(1),
            "kind" => row.kind = "start".into(),
            "expected" => row.expected = Some(1),
            "config" => row.config = Some(2),
            "policy" => row.policy = Some(2),
            _ => unreachable!(),
        }
        assert!(
            decode_submission(row, p.owner.user_scope, p.submission).is_err(),
            "{field}"
        );
    }
}

#[test]
fn routine_receipts_never_enter_ordinary_fresh_admission_or_pending_recovery() {
    let (mut repo, bot, owner, ordinary) = fixture();
    let routine = routine(intent(&bot));
    assert!(matches!(
        repo.prepare_bot_submission(&routine, HASH, 1, 2),
        Err(BotError::Invalid)
    ));
    assert!(
        raw(
            &repo.connection,
            owner.user_scope,
            routine.submission.provenance.submission
        )
        .unwrap()
        .is_none()
    );
    repo.prepare_bot_submission(&ordinary, HASH, 1, 2).unwrap();
    insert_routine(&repo, &routine);
    for kind in ["routine_draft", "create_routine"] {
        let request = BotRequestId::mint();
        repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'prepared',?6,'{}',2,2)",params![owner.user_scope.to_storage(),request.to_storage(),bot.id.to_storage(),HASH,kind,(kind=="create_routine").then_some(1)]).unwrap();
    }
    let pending = repo.pending_bot_operations(0, 64).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].3, ordinary.submission.provenance.submission);
    let plan = repo
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {BOT_PENDING_OPERATIONS_SQL}"))
        .unwrap()
        .query_map(params![0i64, 64i64], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join(" ");
    assert!(plan.contains("bot_unresolved_coordination"), "{plan}");
    assert!(
        repo.pending_bot_operations(pending[0].0, 64)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repo.bot_admission_receipt(owner, bot.id, routine.submission.provenance.submission),
        Err(BotError::Conflict)
    ));
}

#[test]
fn private_routine_receipts_conflict_with_ordinary_readers_even_for_equal_fingerprints() {
    let (mut repo, bot, owner, base) = fixture();
    let occurrence = routine(base);
    insert_routine(&repo, &occurrence);
    let mut requests = vec![occurrence.submission.provenance.submission];
    for kind in ["routine_draft", "create_routine"] {
        let request = BotRequestId::mint();
        repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,expected_revision,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'prepared',?6,'{}',2,2)",params![owner.user_scope.to_storage(),request.to_storage(),bot.id.to_storage(),HASH,kind,(kind=="create_routine").then_some(1)]).unwrap();
        requests.push(request);
    }
    for request in requests {
        assert!(matches!(
            repo.bot_receipt(owner.user_scope, request, HASH),
            Err(BotError::Conflict)
        ));
        assert!(matches!(
            repo.create_bot_chat(
                &bot,
                ProfileId::new(),
                request,
                HASH,
                Timestamp::from_millis(3)
            ),
            Err(BotError::Conflict)
        ));
        assert!(matches!(
            pause_on(&repo.connection, owner.user_scope, request),
            Err(BotError::Conflict)
        ));
        let knowledge = PreparedBotKnowledge {
            owner,
            bot: bot.id,
            request,
            expected_revision: 1,
            pages: vec![],
        };
        assert!(matches!(
            repo.bot_knowledge_receipt(&knowledge, HASH),
            Err(BotError::Conflict)
        ));
        let mut ordinary = intent(&bot);
        ordinary.submission.provenance.submission = request;
        assert!(matches!(
            repo.prepare_bot_submission(&ordinary, HASH, 1, 3),
            Err(BotError::Conflict)
        ));
    }
}

#[test]
fn finalized_routine_preparations_count_toward_existing_fifty_unresolved_bound() {
    let (mut repo, bot, _, ordinary) = fixture();
    for _ in 0..50 {
        insert_routine(&repo, &routine(intent(&bot)));
    }
    assert!(matches!(
        repo.prepare_bot_submission(&ordinary, HASH, 1, 3),
        Err(BotError::Conflict)
    ));
    let p = &ordinary.submission.provenance;
    assert!(
        raw(&repo.connection, p.owner.user_scope, p.submission)
            .unwrap()
            .is_none()
    );
    assert!(repo.pending_bot_operations(0, 64).unwrap().is_empty());
}

#[test]
fn finalized_routine_receipts_hold_folder_binding_and_lifecycle_unresolved_guards() {
    let (mut repo, bot, owner, ordinary) = fixture();
    let occurrence = routine(intent(&bot));
    insert_routine(&repo, &occurrence);
    let request = occurrence.submission.provenance.submission;
    assert_eq!(
        repo.bot_unresolved_submissions(bot.id, 64).unwrap(),
        vec![(owner.user_scope, request)]
    );
    assert!(matches!(
        repo.prepare_bot_submission(&ordinary, HASH, 1, 3),
        Err(BotError::FolderLocked)
    ));
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().folder_lock_request, None);

    let (mut repo, bot, owner, ordinary) = fixture();
    let request = ordinary.submission.provenance.submission;
    repo.prepare_bot_submission(&ordinary, HASH, 1, 2).unwrap();
    let refused = repo
        .prepare_bot_refusal_fence(
            owner,
            bot.id,
            request,
            BotAdmissionRefusal::Capacity,
            false,
            3,
        )
        .unwrap();
    let (round, barrier) = fence(&refused);
    repo.acknowledge_bot_refusal_fence(owner, bot.id, request, round, &barrier, 4)
        .unwrap();
    let occurrence = routine(intent(&bot));
    insert_routine(&repo, &occurrence);
    assert!(matches!(
        repo.reject_bot_submission_after_fence(owner, bot.id, request, round, &barrier, 5),
        Err(BotError::FolderLocked)
    ));
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
    assert_eq!(
        repo.bot_unresolved_submissions(bot.id, 64).unwrap().len(),
        2
    );
}

#[test]
fn explicit_receipt_decoder_preserves_v1_continue_and_refuses_kind_drift() {
    let (mut repo, bot, owner, mut continuation) = fixture();
    continuation.submission.provenance.continued_from = Some(d::RunId::mint());
    let saved = repo
        .prepare_bot_submission(&continuation, HASH, 1, 2)
        .unwrap();
    assert_eq!(saved.kind, BotOperationKind::Continue);
    assert_eq!(receipt(&repo, &continuation).unwrap(), saved);
    assert_eq!(
        repo.prepare_bot_submission(&continuation, HASH, 999, 3)
            .unwrap(),
        saved
    );
    let p = &continuation.submission.provenance;
    let mut row = raw(&repo.connection, owner.user_scope, p.submission)
        .unwrap()
        .unwrap();
    row.kind = "start".into();
    assert!(decode_submission(row, owner.user_scope, p.submission).is_err());
    let mut row = raw(&repo.connection, owner.user_scope, p.submission)
        .unwrap()
        .unwrap();
    row.kind = "routine_occurrence".into();
    assert!(decode_submission(row, owner.user_scope, p.submission).is_err());
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(p.submission)
    );
}

#[test]
fn routine_continuation_refuses_before_receipt_or_folder_mutation_but_exact_replay_wins() {
    let (mut repo, bot, owner, mut continuation) = fixture();
    let occurrence = routine(intent(&bot));
    let run = occurrence.submission.request.run;
    continuation.submission.provenance.continued_from = Some(run);
    // A historical immutable Continue is returned even if its predecessor is
    // now recognized as Routine. No new attempt is authorized by this replay.
    let saved = repo
        .prepare_bot_submission(&continuation, HASH, 1, 2)
        .unwrap();
    insert_routine(&repo, &occurrence);
    let before = repo.connection.total_changes();
    assert_eq!(
        repo.prepare_bot_submission(&continuation, HASH, 999, 3)
            .unwrap(),
        saved
    );
    assert_eq!(repo.connection.total_changes(), before);
    let mut fresh = intent(&bot);
    fresh.submission.provenance.continued_from = Some(run);
    assert!(matches!(
        repo.prepare_bot_submission(&fresh, HASH, 1, 4),
        Err(BotError::RoutineRetryUnsupported)
    ));
    assert_eq!(repo.connection.total_changes(), before);
    assert!(
        raw(
            &repo.connection,
            owner.user_scope,
            fresh.submission.provenance.submission
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(continuation.submission.provenance.submission)
    );
}
