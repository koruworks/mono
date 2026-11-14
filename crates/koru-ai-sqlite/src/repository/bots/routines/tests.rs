// SPDX-License-Identifier: MPL-2.0
//! In-memory persistence fixtures; confirmations are synthetic, not Store proof.
use super::*;
mod recovery;
fn hash() -> RoutineDigest32 {
    RoutineDigest32::from_bytes([7; 32])
}
pub(super) fn fixture() -> (Repository, Bot, BotOwner, BotRoutineCreate) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = super::super::tests::fixture_bot();
    repo.save_bot(
        &bot,
        ProfileId::new(),
        BotRequestId::mint(),
        &String::from(hash()),
        None,
    )
    .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let command: BotRoutineCreate = serde_json::from_value(serde_json::json!({
        "request":BotRequestId::mint(),"bot":bot.id,"expected_bot_revision":bot.revision,
        "name":"Private routine","objective":"Private task","documents":[DocumentId::new()],
        "recurrence":{"type":"floating","cron":"0 9 * * *"}
    }))
    .unwrap();
    (repo, bot, owner, command)
}
pub(super) fn prepared(
    repo: &mut Repository,
    owner: BotOwner,
    command: &BotRoutineCreate,
) -> BotRoutineCreation {
    repo.prepare_bot_routine_creation(owner, command, hash(), 10)
        .unwrap();
    repo.bot_routine_creation_by_request(owner, command.bot, command.request)
        .unwrap()
        .unwrap()
}
pub(super) fn confirm(c: &BotRoutineCreation) -> BotRoutineCreationTransition {
    BotRoutineCreationTransition::Confirmed(BotRoutineStoreConfirmation {
        identity: c.identity.clone(),
        origin_edit_revision: vec![1, 2, 3],
    })
}
fn counts(repo: &Repository) -> (u32, u32) {
    (
        repo.connection
            .query_row("SELECT count(*) FROM bot_routines", [], |r| r.get(0))
            .unwrap(),
        repo.connection
            .query_row("SELECT count(*) FROM bot_command_receipts", [], |r| {
                r.get(0)
            })
            .unwrap(),
    )
}

#[test]
fn paused_bot_prepares_one_paused_seed_and_replays_semantics_before_fresh_checks() {
    let (mut repo, bot, owner, command) = fixture();
    repo.connection
        .execute(
            "UPDATE bots SET paused=1 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let c = prepared(&mut repo, owner, &command);
    let before = counts(&repo);
    let encoded = serde_json::to_value(&c.identity.seed).unwrap();
    assert_eq!(
        encoded["header"]["definition"]["lifecycle"],
        serde_json::json!({"paused":{"reason":{"reason":"manual"}}})
    );
    assert_eq!(c.identity.grant.grant_revision, 1);
    assert_ne!(c.identity.schedule.as_uuid(), c.identity.store_request);
    let first = repo
        .bot_routine_creation_receipt(owner, bot.id, command.request)
        .unwrap()
        .unwrap();
    repo.connection.execute("UPDATE bots SET revision=revision+1,policy_revision=policy_revision+1,model='changed' WHERE bot_id=?1",[bot.id.to_storage()]).unwrap();
    assert_eq!(
        repo.prepare_bot_routine_creation(owner, &command, hash(), 999)
            .unwrap(),
        first
    );
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule)
            .unwrap(),
        Some(c)
    );
    assert_eq!(counts(&repo), before);
    for change in 0..6 {
        let mut other = command.clone();
        match change {
            0 => other.expected_bot_revision += 1,
            1 => other.name = serde_json::from_str("\"other\"").unwrap(),
            2 => other.objective = serde_json::from_str("\"other\"").unwrap(),
            3 => other.documents.clear(),
            4 => {
                other.recurrence =
                    serde_json::from_str(r#"{"type":"floating","cron":"1 9 * * *"}"#).unwrap()
            }
            _ => {}
        };
        let fp = if change == 5 {
            RoutineDigest32::from_bytes([8; 32])
        } else {
            hash()
        };
        assert_eq!(
            repo.prepare_bot_routine_creation(owner, &other, fp, 999),
            Err(BotError::Conflict)
        );
    }
    assert_eq!(counts(&repo), before);
}

#[test]
fn shared_receipt_namespace_refuses_both_directions_and_rollback_is_atomic() {
    let (mut repo, bot, owner, mut command) = fixture();
    let request = BotRequestId::mint();
    repo.create_bot_chat(
        &bot,
        ProfileId::new(),
        request,
        &String::from(hash()),
        Timestamp::from_millis(2),
    )
    .unwrap();
    command.request = request;
    let before = counts(&repo);
    assert_eq!(
        repo.prepare_bot_routine_creation(owner, &command, hash(), 3),
        Err(BotError::Conflict)
    );
    assert_eq!(counts(&repo), before);
    command.request = BotRequestId::mint();
    let c = prepared(&mut repo, owner, &command);
    assert_eq!(
        repo.bot_receipt(owner.user_scope, command.request, &String::from(hash())),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.prepare_bot_memory_creation(
            owner,
            bot.id,
            command.request,
            bot.revision,
            &BotMemorySeed {
                title: "Memory".into(),
                hash: [1; 32]
            },
            &String::from(hash()),
            4
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_routine_creation_by_request(owner, bot.id, request),
        Err(BotError::Conflict)
    );
    repo.connection.execute_batch("CREATE TEMP TRIGGER fail_routine BEFORE INSERT ON bot_routines BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    command.request = BotRequestId::mint();
    let before = counts(&repo);
    assert!(
        repo.prepare_bot_routine_creation(owner, &command, hash(), 5)
            .is_err()
    );
    assert_eq!(counts(&repo), before);
    assert!(
        repo.bot_routine_creation_by_request(owner, bot.id, command.request)
            .unwrap()
            .is_none()
    );
    assert!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule)
            .unwrap()
            .is_some()
    );
}

#[test]
fn unknown_cas_terminal_bind_and_old_completed_receipt_are_monotonic() {
    let (mut repo, bot, owner, command) = fixture();
    let c = prepared(&mut repo, owner, &command);
    let receipt = repo
        .bot_routine_creation_receipt(owner, bot.id, command.request)
        .unwrap()
        .unwrap();
    let unknown = repo
        .transition_bot_routine_creation(
            &c.identity,
            c.observed(),
            BotRoutineCreationTransition::Unconfirmed,
            0,
        )
        .unwrap();
    assert_eq!(unknown.updated_at_ms, c.updated_at_ms + 1);
    assert_eq!(
        repo.bot_routine_creation_receipt(owner, bot.id, command.request)
            .unwrap()
            .unwrap(),
        receipt
    );
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 20),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.transition_bot_routine_creation(
            &c.identity,
            unknown.observed(),
            BotRoutineCreationTransition::Unconfirmed,
            100
        )
        .unwrap(),
        unknown
    );
    let bound = repo
        .transition_bot_routine_creation(&c.identity, unknown.observed(), confirm(&c), 20)
        .unwrap();
    let final_receipt = repo
        .prepare_bot_routine_creation(owner, &command, hash(), 21)
        .unwrap();
    assert_eq!(final_receipt.state, BotRoutineCreationState::Bound);
    repo.connection.execute("UPDATE bots SET revision=revision+1,policy_revision=policy_revision+1,model='later' WHERE bot_id=?1",[bot.id.to_storage()]).unwrap();
    assert_eq!(
        repo.prepare_bot_routine_creation(owner, &command, hash(), 99)
            .unwrap(),
        final_receipt
    );
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 99)
            .unwrap(),
        bound
    );
    let mut bad = confirm(&c);
    if let BotRoutineCreationTransition::Confirmed(p) = &mut bad {
        p.origin_edit_revision = vec![9];
    }
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, bound.observed(), bad, 100),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.transition_bot_routine_creation(
            &c.identity,
            bound.observed(),
            BotRoutineCreationTransition::Unconfirmed,
            100
        ),
        Err(BotError::Conflict)
    );
}

#[test]
fn bind_refuses_owner_config_policy_profile_folder_model_and_proof_drift() {
    for drift in 0..7 {
        let (mut repo, bot, owner, command) = fixture();
        let c = prepared(&mut repo, owner, &command);
        let mut proof = confirm(&c);
        match drift {
            0 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET owner_device_id=?2 WHERE bot_id=?1",
                        params![bot.id.to_storage(), DeviceId::new().to_storage()],
                    )
                    .unwrap();
            }
            1 => {
                repo.connection
                    .execute(
                        // Reasoning is not an ordinary Schedule template field;
                        // its real settings edit is fenced by the Bot revision.
                        "UPDATE bots SET reasoning='off',revision=revision+1 WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            2 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET policy_revision=policy_revision+1 WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            3 => {
                repo.connection
                    .execute(
                        "UPDATE chats SET profile_id=?2 WHERE id=?1",
                        params![bot.main_chat.to_storage(), ProfileId::new().to_storage()],
                    )
                    .unwrap();
            }
            4 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET working_folder=working_folder||'/changed' WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            5 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET model='changed' WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            _ => {
                if let BotRoutineCreationTransition::Confirmed(p) = &mut proof {
                    p.identity.store_request = uuid::Uuid::new_v4();
                }
            }
        }
        assert!(
            repo.transition_bot_routine_creation(&c.identity, c.observed(), proof, 20)
                .is_err(),
            "drift {drift}"
        );
        let state: String = repo
            .connection
            .query_row("SELECT creation_state FROM bot_routines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "prepared");
    }
}

#[test]
fn conflict_is_terminal_and_lost_replay_never_resets_or_replaces_identity() {
    let (mut repo, bot, owner, command) = fixture();
    let c = prepared(&mut repo, owner, &command);
    let transition =
        BotRoutineCreationTransition::Conflict(BotRoutineCreationRefusal::DocumentChanged);
    let conflict = repo
        .transition_bot_routine_creation(&c.identity, c.observed(), transition.clone(), 20)
        .unwrap();
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, c.observed(), transition, 30)
            .unwrap(),
        conflict
    );
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, conflict.observed(), confirm(&c), 30),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.transition_bot_routine_creation(
            &c.identity,
            conflict.observed(),
            BotRoutineCreationTransition::Unconfirmed,
            30
        ),
        Err(BotError::Conflict)
    );
    let receipt = repo
        .prepare_bot_routine_creation(owner, &command, hash(), 40)
        .unwrap();
    assert_eq!(receipt.state, BotRoutineCreationState::Conflict);
    assert_eq!(receipt.route.schedule, c.identity.schedule);
    assert_eq!(counts(&repo).0, 1);
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule)
            .unwrap()
            .unwrap(),
        conflict
    );
}

#[test]
fn bounded_reads_reject_hash_identity_shape_and_receipt_corruption() {
    for sql in [
        "UPDATE bot_routines SET seed_hash=zeroblob(32)",
        "UPDATE bot_routines SET grant_digest=zeroblob(32)",
        "UPDATE bot_routines SET seed_title='different'",
        "UPDATE bot_routines SET store_request_id='00000000-0000-5000-8000-000000000001'",
        "UPDATE bot_routines SET seed_json=json_set(seed_json,'$.header.definition.template.unknown',1)",
        "UPDATE bot_routines SET grant_json=json_set(grant_json,'$.digest',printf('%064d',0))",
        "UPDATE bot_command_receipts SET outcome_json=json_set(outcome_json,'$.state','bound') WHERE command_kind='create_routine'",
        "UPDATE bot_command_receipts SET outcome_json=json_set(outcome_json,'$.unexpected',1) WHERE command_kind='create_routine'",
        "PRAGMA ignore_check_constraints=ON; UPDATE bot_routines SET seed_json=printf('%4194305s','')",
        "PRAGMA ignore_check_constraints=ON; UPDATE bot_routines SET origin_edit_revision=zeroblob(65537)",
    ] {
        let (mut repo, bot, owner, command) = fixture();
        let c = prepared(&mut repo, owner, &command);
        repo.connection.execute_batch(sql).unwrap();
        assert_eq!(
            repo.bot_routine_creation(owner, bot.id, c.identity.schedule),
            Err(BotError::Storage),
            "{sql}"
        );
        assert!(
            repo.prepare_bot_routine_creation(owner, &command, hash(), 50)
                .is_err()
        );
        assert_eq!(counts(&repo).0, 1);
    }
}

#[test]
fn summary_keyset_is_bounded_and_private_and_wrong_owner_is_refused() {
    let (mut repo, bot, owner, mut command) = fixture();
    for _ in 0..18 {
        command.request = BotRequestId::mint();
        prepared(&mut repo, owner, &command);
    }
    let first = repo
        .bot_routine_creation_page(owner, bot.id, None, 16)
        .unwrap();
    assert_eq!(first.items.len(), 16);
    assert!(
        first
            .items
            .windows(2)
            .all(|p| p[0].route.schedule.to_storage() < p[1].route.schedule.to_storage())
    );
    let second = repo
        .bot_routine_creation_page(owner, bot.id, first.next, 16)
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.next.is_none());
    assert!(!format!("{first:?}").contains("Private"));
    for limit in [0, 17] {
        assert_eq!(
            repo.bot_routine_creation_page(owner, bot.id, None, limit),
            Err(BotError::Invalid)
        );
    }
    let foreign = BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::new(),
    };
    assert_eq!(
        repo.bot_routine_creation_page(foreign, bot.id, None, 16),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        repo.bot_routine_creation_by_request(foreign, bot.id, command.request),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(
        repo.bot_routine_creation_page(owner, bot.id, None, 16)
            .is_ok()
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.bot_routine_creation_page(owner, bot.id, None, 16),
        Err(BotError::NotFound)
    );
}

#[test]
fn fresh_source_refusal_and_transition_faults_leave_no_partial_receipt() {
    for sql in [
        "UPDATE bots SET revision=revision+1",
        "UPDATE bots SET provider='not-an-agent'",
        "UPDATE bots SET working_folder='relative'",
        "UPDATE chats SET profile_id=NULL",
    ] {
        let (mut repo, _, owner, command) = fixture();
        repo.connection.execute_batch(sql).unwrap();
        let before = counts(&repo);
        assert!(
            repo.prepare_bot_routine_creation(owner, &command, hash(), 10)
                .is_err()
        );
        assert_eq!(counts(&repo), before);
    }
    let (mut repo, bot, owner, command) = fixture();
    let c = prepared(&mut repo, owner, &command);
    repo.connection.execute_batch("CREATE TEMP TRIGGER fail_receipt BEFORE UPDATE ON bot_command_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 20)
            .is_err()
    );
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule)
            .unwrap(),
        Some(c.clone())
    );
    repo.connection
        .execute_batch(
            "DROP TRIGGER fail_receipt; UPDATE bot_routines SET updated_at_ms=9223372036854775807;",
        )
        .unwrap();
    let max = repo
        .bot_routine_creation(owner, bot.id, c.identity.schedule)
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.transition_bot_routine_creation(&c.identity, max.observed(), confirm(&c), 30),
        Err(BotError::Storage)
    );
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule)
            .unwrap(),
        Some(max)
    );
}

#[test]
fn trash_refuses_pending_transitions_and_terminal_replay_without_mutation() {
    for terminal in [false, true] {
        let (mut repo, bot, owner, command) = fixture();
        let c = prepared(&mut repo, owner, &command);
        if terminal {
            repo.transition_bot_routine_creation(&c.identity, c.observed(), confirm(&c), 20)
                .unwrap();
        }
        repo.connection
            .execute(
                "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
                [bot.id.to_storage()],
            )
            .unwrap();
        let before: (String, i64) = repo
            .connection
            .query_row(
                "SELECT creation_state,updated_at_ms FROM bot_routines",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        for transition in [
            BotRoutineCreationTransition::Unconfirmed,
            confirm(&c),
            BotRoutineCreationTransition::Conflict(BotRoutineCreationRefusal::DocumentChanged),
        ] {
            assert_eq!(
                repo.transition_bot_routine_creation(&c.identity, c.observed(), transition, 30),
                Err(BotError::NotFound)
            );
        }
        let after: (String, i64) = repo
            .connection
            .query_row(
                "SELECT creation_state,updated_at_ms FROM bot_routines",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, before);
    }
}

#[test]
fn summary_rejects_regressed_timestamp_without_loading_seed_payload() {
    let (mut repo, bot, owner, command) = fixture();
    let c = prepared(&mut repo, owner, &command);
    repo.connection
        .execute("UPDATE bot_routines SET updated_at_ms=created_at_ms-1", [])
        .unwrap();
    assert_eq!(
        repo.bot_routine_creation_page(owner, bot.id, None, 16),
        Err(BotError::Storage)
    );
    assert_eq!(
        repo.bot_routine_creation(owner, bot.id, c.identity.schedule),
        Err(BotError::Storage)
    );
}
