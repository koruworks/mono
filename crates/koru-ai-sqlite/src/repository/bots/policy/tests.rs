// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::repository::bots::tests::fixture_bot;

fn fixture() -> (Repository, Bot, ProfileId, BotRequestId, BotPolicyBarrier) {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    bot.revision += 1;
    bot.policy_revision += 1;
    bot.access = BotAccess::Workspace;
    let request = BotRequestId::mint();
    let barrier = BotPolicyBarrier {
        key: koru_delegate_core::BotPolicyBarrierKey {
            owner: koru_delegate_core::BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            request: "12345678-1234-8234-8234-123456789abc".parse().unwrap(),
        },
        config_revision: bot.revision,
        policy_revision: bot.policy_revision,
        policy: bot_native_policy(&bot),
        effects_allowed: true,
    };
    (repo, bot, profile, request, barrier)
}
fn owner(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

#[test]
fn pending_access_is_atomic_replayable_and_effective_only_after_exact_barrier() {
    let (mut repo, bot, profile, request, barrier) = fixture();
    let fingerprint = "a".repeat(64);
    let first = repo
        .prepare_bot_access(&bot, profile, request, &fingerprint, 1, &barrier)
        .unwrap();
    assert_eq!(first.operation().status, BotOperationStatus::Pending);
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    assert_eq!(
        repo.prepare_bot_access(&bot, profile, request, &fingerprint, 1, &barrier)
            .unwrap(),
        first
    );
    assert!(
        repo.prepare_bot_access(&bot, profile, request, &"b".repeat(64), 1, &barrier)
            .is_err()
    );
    let first_page = repo.pending_bot_access(0, 1).unwrap();
    assert!(first_page.pending.is_empty());
    let rows = repo
        .pending_bot_access(first_page.scanned_through.unwrap(), 1)
        .unwrap();
    assert_eq!(rows.pending.len(), 1);
    assert!(
        repo.pending_bot_access(rows.scanned_through.unwrap(), 1)
            .unwrap()
            .scanned_through
            .is_none()
    );
    let mut changed = bot.clone();
    changed.revision += 1;
    changed.config.name = "later".into();
    assert!(
        repo.save_bot(
            &changed,
            profile,
            BotRequestId::mint(),
            "later",
            Some(bot.revision)
        )
        .is_err()
    );
    assert!(
        repo.prepare_bot_pause(
            owner(&bot),
            bot.id,
            BotRequestId::mint(),
            bot.revision,
            true,
            &"c".repeat(64),
            3
        )
        .is_err()
    );
    let mut observed = BotPolicyBarrierReceipt {
        barrier,
        state: BotPolicyBarrierState::Pending,
    };
    assert!(
        repo.acknowledge_bot_access(owner(&bot), bot.id, request, &observed, 3)
            .is_err()
    );
    observed.state = BotPolicyBarrierState::Effective;
    repo.connection.execute_batch("CREATE TRIGGER refuse_ack BEFORE UPDATE OF stage ON bot_command_receipts WHEN NEW.command_kind='update_access' AND NEW.stage='completed' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.acknowledge_bot_access(owner(&bot), bot.id, request, &observed, 3)
            .is_err()
    );
    assert!(
        !repo
            .bot_access_receipt(owner(&bot), bot.id, request)
            .unwrap()
            .unwrap()
            .completed
    );
    repo.connection
        .execute_batch("DROP TRIGGER refuse_ack")
        .unwrap();
    let complete = repo
        .acknowledge_bot_access(owner(&bot), bot.id, request, &observed, 3)
        .unwrap();
    assert_eq!(complete.operation().status, BotOperationStatus::Accepted);
    assert_eq!(
        repo.acknowledge_bot_access(owner(&bot), bot.id, request, &observed, 2)
            .unwrap(),
        complete
    );
    repo.save_bot(
        &changed,
        profile,
        BotRequestId::mint(),
        "later",
        Some(bot.revision),
    )
    .unwrap();
}

#[test]
fn failed_receipt_insert_rolls_back_desired_access_and_stale_completion_cannot_acknowledge() {
    let (mut repo, bot, profile, request, barrier) = fixture();
    let before = repo.bot(bot.id).unwrap().unwrap();
    repo.connection.execute_batch("CREATE TRIGGER refuse_policy BEFORE INSERT ON bot_command_receipts WHEN NEW.command_kind='update_access' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.prepare_bot_access(&bot, profile, request, &"a".repeat(64), 1, &barrier)
            .is_err()
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(before));
    assert!(
        repo.bot_access_receipt(owner(&bot), bot.id, request)
            .unwrap()
            .is_none()
    );
    repo.connection
        .execute_batch("DROP TRIGGER refuse_policy")
        .unwrap();
    repo.prepare_bot_access(&bot, profile, request, &"a".repeat(64), 1, &barrier)
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bots SET revision=revision+1 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(
        repo.acknowledge_bot_access(
            owner(&bot),
            bot.id,
            request,
            &BotPolicyBarrierReceipt {
                barrier,
                state: BotPolicyBarrierState::Effective
            },
            5
        )
        .is_err()
    );
    assert!(
        !repo
            .bot_access_receipt(owner(&bot), bot.id, request)
            .unwrap()
            .unwrap()
            .completed
    );
}

#[test]
fn completed_legacy_configuration_is_readable_without_inventing_pending_coordination() {
    let (mut repo, bot, profile, request, _) = fixture();
    repo.save_bot(&bot, profile, request, &"a".repeat(64), Some(1))
        .unwrap();
    let old = repo
        .bot_access_receipt(owner(&bot), bot.id, request)
        .unwrap()
        .unwrap();
    assert!(old.completed && old.barrier.is_none());
    assert_eq!(old.operation().status, BotOperationStatus::Accepted);
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(
        repo.bot_access_receipt(owner(&bot), bot.id, request)
            .is_err()
    );
}

#[test]
fn sparse_recovery_windows_and_per_bot_guards_bound_work_without_rewriting_history() {
    let (mut repo, bot, profile, request, barrier) = fixture();
    for _ in 0..80 {
        repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'update','completed',?5,1,1)",params![bot.user_scope.to_storage(),BotRequestId::mint().to_storage(),bot.id.to_storage(),"b".repeat(64),serde_json::to_string(&BotReceiptOutcome::Configuration {version:1,bot:bot.clone()}).unwrap()]).unwrap();
    }
    let before: i64 = repo
        .connection
        .query_row("SELECT COUNT(*) FROM bot_command_receipts", [], |r| {
            r.get(0)
        })
        .unwrap();
    repo.prepare_bot_access(&bot, profile, request, &"a".repeat(64), 1, &barrier)
        .unwrap();
    let mut after = 0;
    let mut visited = 0;
    let mut found = Vec::new();
    loop {
        let page = repo.pending_bot_access(after, 16).unwrap();
        let Some(scanned) = page.scanned_through else {
            break;
        };
        assert!(scanned - after <= 16, "raw-row work bound");
        visited += 1;
        after = scanned;
        found.extend(page.pending);
    }
    assert!(visited > 1);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].3, request);
    for sql in [
        "EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE bot_id=?1 AND command_kind='update_access' AND stage='prepared')",
        "EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE bot_id=?1 AND command_kind IN ('pause','resume') AND stage='prepared')",
    ] {
        let mut statement = repo.connection.prepare(sql).unwrap();
        let plan = statement
            .query_map([bot.id.to_storage()], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            plan.contains("SEARCH") && plan.contains("bot_command_guard"),
            "{plan}"
        );
    }
    let historical: i64 = repo
        .connection
        .query_row(
            "SELECT COUNT(*) FROM bot_command_receipts WHERE command_kind<>'update_access'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(historical, before);
}

#[test]
fn typed_policy_outcome_refuses_unknown_nested_configuration_and_wrong_binding() {
    let (mut repo, bot, profile, request, barrier) = fixture();
    repo.prepare_bot_access(&bot, profile, request, &"a".repeat(64), 1, &barrier)
        .unwrap();
    let outcome = BotReceiptOutcome::AccessPolicy {
        version: 1,
        bot: bot.clone(),
        barrier,
    };
    let mut json = serde_json::to_value(&outcome).unwrap();
    json["bot"]["config"]["model"]["hidden_authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotReceiptOutcome>(json).is_err());
    let mut json = serde_json::to_value(outcome).unwrap();
    json["barrier"]["config_revision"] = serde_json::json!(99);
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![request.to_storage(), json.to_string()],
        )
        .unwrap();
    assert!(
        repo.bot_access_receipt(owner(&bot), bot.id, request)
            .is_err()
    );
}
