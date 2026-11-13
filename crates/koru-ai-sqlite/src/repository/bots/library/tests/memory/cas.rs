// SPDX-License-Identifier: MPL-2.0
use super::*;

fn current(repo: &Repository, owner: BotOwner, bot: BotId) -> BotMemoryCreation {
    repo.bot_library(owner, bot).unwrap().creation.unwrap()
}

#[test]
fn memory_cas_refuses_aba_and_stale_completions() {
    let (mut repo, bot, owner) = fixture();
    let first = prepared(&mut repo, &bot, owner);
    let wrong_state = BotMemoryObserved {
        state: BotMemoryState::Unconfirmed,
        ..first.observed()
    };
    assert_eq!(
        repo.transition_bot_memory_creation(
            &first.identity,
            wrong_state,
            BotMemoryTransition::Abandon,
            2
        ),
        Err(BotError::Conflict)
    );
    let abandoned = repo
        .transition_bot_memory_creation(
            &first.identity,
            first.observed(),
            BotMemoryTransition::Abandon,
            2,
        )
        .unwrap();
    let retried = repo
        .transition_bot_memory_creation(
            &first.identity,
            abandoned.observed(),
            BotMemoryTransition::Retry,
            1,
        )
        .unwrap();
    assert_eq!(retried.state, first.state);
    assert!(retried.updated_at_ms > first.updated_at_ms);
    assert_eq!(
        repo.transition_bot_memory_creation(
            &first.identity,
            first.observed(),
            BotMemoryTransition::Abandon,
            3
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bind_bot_memory(&confirmation(&first), first.observed(), 3),
        Err(BotError::Conflict)
    );
    assert_eq!(current(&repo, owner, bot.id), retried);
    let binding = repo
        .bind_bot_memory(&confirmation(&first), retried.observed(), 1)
        .unwrap();
    assert_eq!(
        repo.bind_bot_memory(&confirmation(&first), retried.observed(), 4),
        Err(BotError::Conflict)
    );
    let bound = current(&repo, owner, bot.id);
    assert_eq!(
        repo.bind_bot_memory(&confirmation(&first), bound.observed(), 4)
            .unwrap(),
        binding
    );
    assert_eq!(current(&repo, owner, bot.id), bound);
}

#[test]
fn memory_tokens_advance_with_equal_or_rollback_clock() {
    let (mut repo, bot, owner) = fixture();
    let first = prepared(&mut repo, &bot, owner);
    let mut c = first.clone();
    for action in [
        BotMemoryTransition::Unknown,
        BotMemoryTransition::Disagreement(BotMemoryRefusal::DocumentChanged),
        BotMemoryTransition::Abandon,
        BotMemoryTransition::Retry,
    ] {
        let next = repo
            .transition_bot_memory_creation(&c.identity, c.observed(), action, 1)
            .unwrap();
        assert_eq!(next.updated_at_ms, c.updated_at_ms + 1);
        let receipt_time: i64 = repo
            .connection
            .query_row(
                "SELECT updated_at_ms FROM bot_command_receipts WHERE request_id=?1",
                [c.identity.creation_request.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(receipt_time, next.updated_at_ms);
        c = next;
    }
    assert_eq!(
        repo.transition_bot_memory_creation(
            &c.identity,
            c.observed(),
            BotMemoryTransition::ReadFailure,
            100
        )
        .unwrap(),
        c
    );
    let unknown = repo
        .transition_bot_memory_creation(&c.identity, c.observed(), BotMemoryTransition::Unknown, 1)
        .unwrap();
    assert_eq!(
        repo.transition_bot_memory_creation(
            &unknown.identity,
            unknown.observed(),
            BotMemoryTransition::Unknown,
            100
        )
        .unwrap(),
        unknown
    );
    let alias = repo
        .prepare_bot_memory_creation(owner, bot.id, BotRequestId::mint(), 1, &seed(), HASH, 100)
        .unwrap();
    assert_eq!(alias, unknown);
    let binding = repo
        .bind_bot_memory(&confirmation(&unknown), unknown.observed(), 0)
        .unwrap();
    assert_eq!(binding.bound_at_ms, unknown.updated_at_ms + 1);
    assert_eq!(
        current(&repo, owner, bot.id).updated_at_ms,
        binding.bound_at_ms
    );
}

#[test]
fn memory_overflow_and_late_failures_roll_back_all_rows() {
    for case in [
        "overflow_bind",
        "overflow_transition",
        "late_bind",
        "late_transition",
    ] {
        let (mut repo, bot, owner) = fixture();
        let mut c = prepared(&mut repo, &bot, owner);
        if case.starts_with("overflow") {
            repo.connection
                .execute(
                    "UPDATE bot_memory_creation_intents SET updated_at_ms=?1",
                    [i64::MAX],
                )
                .unwrap();
            repo.connection.execute("UPDATE bot_command_receipts SET updated_at_ms=?1 WHERE command_kind='ensure_memory'",[i64::MAX]).unwrap();
            c = current(&repo, owner, bot.id);
        } else {
            repo.connection.execute_batch("CREATE TRIGGER fail_memory_receipt BEFORE UPDATE ON bot_command_receipts WHEN OLD.command_kind='ensure_memory' BEGIN SELECT RAISE(ABORT,'late memory fixture'); END;").unwrap();
        }
        let before = repo.bot(bot.id).unwrap().unwrap();
        let receipt_before: (String, i64) = repo
            .connection
            .query_row(
                "SELECT stage,updated_at_ms FROM bot_command_receipts WHERE request_id=?1",
                [c.identity.creation_request.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if case.ends_with("bind") {
            assert!(
                repo.bind_bot_memory(&confirmation(&c), c.observed(), 0)
                    .is_err()
            );
        } else {
            assert!(
                repo.transition_bot_memory_creation(
                    &c.identity,
                    c.observed(),
                    BotMemoryTransition::Abandon,
                    0
                )
                .is_err()
            );
        }
        assert_eq!(current(&repo, owner, bot.id), c, "{case}");
        assert_eq!(repo.bot(bot.id).unwrap().unwrap(), before);
        assert!(repo.bot_library(owner, bot.id).unwrap().binding.is_none());
        let after: (String, i64) = repo
            .connection
            .query_row(
                "SELECT stage,updated_at_ms FROM bot_command_receipts WHERE request_id=?1",
                [c.identity.creation_request.to_storage()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, receipt_before);
    }
}

#[test]
fn memory_role_exclusion_is_symmetric_and_corruption_is_not_absence() {
    for role_first in [false, true] {
        let (mut repo, bot, owner) = fixture();
        let c = prepared(&mut repo, &bot, owner);
        if !role_first {
            repo.bind_bot_memory(&confirmation(&c), c.observed(), 3)
                .unwrap();
        }
        let mut next = repo.bot(bot.id).unwrap().unwrap();
        let expected = next.revision;
        next.revision += 1;
        next.config.role = BotPage {
            workspace: bot.home_workspace,
            document: c.identity.candidate,
        };
        let receipts = receipt_count(&repo);
        let outcome = repo.save_bot(
            &next,
            ProfileId::new(),
            BotRequestId::mint(),
            OTHER_HASH,
            Some(expected),
        );
        if role_first {
            outcome.unwrap();
            assert_eq!(
                repo.bind_bot_memory(&confirmation(&c), c.observed(), 4),
                Err(BotError::Forbidden)
            );
            assert!(repo.bot_library(owner, bot.id).unwrap().binding.is_none());
        } else {
            assert_eq!(outcome, Err(BotError::Forbidden));
            assert_eq!(receipt_count(&repo), receipts);
            assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, expected);
            repo.connection
                .execute(
                    "UPDATE bots SET role_document_id=?1 WHERE bot_id=?2",
                    params![c.identity.candidate.to_storage(), bot.id.to_storage()],
                )
                .unwrap();
            assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::Storage));
        }
    }
}
