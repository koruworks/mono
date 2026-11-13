// SPDX-License-Identifier: MPL-2.0
use super::*;

fn fixture() -> (Repository, Bot, BotOwner, BotRequestId) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = super::super::tests::fixture_bot();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let request = BotRequestId::mint();
    repo.save_bot(&bot, ProfileId::new(), request, "create", None)
        .unwrap();
    (repo, bot, owner, request)
}

#[test]
fn current_pause_identity_wins_and_delivery_remains_pending() {
    let (mut repo, bot, owner, _) = fixture();
    let request = BotRequestId::mint();
    let pause = repo
        .prepare_bot_pause(owner, bot.id, request, 1, true, &"a".repeat(64), 2)
        .unwrap();
    let snapshot = repo
        .bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
        .unwrap();
    assert_eq!(snapshot.admission, pause.intent.barrier);
    let mut other = snapshot.admission.clone();
    other.request = BotRequestId::mint();
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Present { barrier: other }),
        Err(BotError::Conflict)
    ));
    let read = admission::pause_on(&repo.connection, owner.user_scope, request)
        .unwrap()
        .unwrap();
    assert!(!read.acknowledged);
    assert_eq!(read.stage, BotReceiptStage::Prepared);
}

#[test]
fn creation_identity_is_reused_only_after_current_canonical_capture() {
    let (repo, bot, owner, request) = fixture();
    let initial = repo
        .bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
        .unwrap();
    assert_eq!(initial.admission.request, request);
    repo.connection
        .execute(
            "UPDATE bots SET revision=2 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let changed = repo
        .bot_boot_snapshot(
            owner,
            bot.id,
            &BotBootAdmission::Present {
                barrier: initial.admission,
            },
        )
        .unwrap();
    assert_eq!(changed.admission.epoch, 2);
    assert_eq!(changed.admission.request, request);
    let mut lost = changed.admission.clone();
    lost.request = BotRequestId::mint();
    assert_eq!(
        repo.bot_boot_snapshot(
            owner,
            bot.id,
            &BotBootAdmission::Present {
                barrier: lost.clone()
            }
        )
        .unwrap()
        .admission,
        lost
    );
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Unavailable {}),
        Err(BotError::Unavailable)
    ));
    lost.epoch = 3;
    assert!(
        repo.bot_boot_snapshot(
            owner,
            bot.id,
            &BotBootAdmission::Present {
                barrier: lost.clone()
            }
        )
        .is_err()
    );
    lost.epoch = 2;
    lost.paused = true;
    assert!(
        repo.bot_boot_snapshot(
            owner,
            bot.id,
            &BotBootAdmission::Present {
                barrier: lost.clone()
            }
        )
        .is_err()
    );
    lost.paused = false;
    lost.owner.device = DeviceId::new();
    assert!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Present { barrier: lost })
            .is_err()
    );
}

#[test]
fn missing_ambiguous_and_foreign_creation_evidence_never_falls_back() {
    let (repo, bot, owner, request) = fixture();
    let duplicate = BotRequestId::mint();
    repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) SELECT user_scope_id,?2,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id=?1",params![request.to_storage(),duplicate.to_storage()]).unwrap();
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {}),
        Err(BotError::Conflict)
    ));
    repo.connection
        .execute(
            "DELETE FROM bot_command_receipts WHERE request_id=?1",
            [duplicate.to_storage()],
        )
        .unwrap();
    let foreign = BotReceiptOutcome::Configuration {
        version: 1,
        bot: super::super::tests::fixture_bot(),
    };
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![
                request.to_storage(),
                serde_json::to_string(&foreign).unwrap()
            ],
        )
        .unwrap();
    assert!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .is_err()
    );
    repo.connection
        .execute(
            "DELETE FROM bot_command_receipts WHERE request_id=?1",
            [request.to_storage()],
        )
        .unwrap();
    assert!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .is_err()
    );
}
