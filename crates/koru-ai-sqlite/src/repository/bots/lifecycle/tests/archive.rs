// SPDX-License-Identifier: MPL-2.0
//! The Archive driver's writes (P1.7 S4): stage moves, parking, completion
//! and Unarchive, each against the journal's own fences.
use super::*;

fn archived_setup() -> (Repository, Bot, BotOwner, LifecycleOperation) {
    let (mut repo, bot, owner) = setup();
    let op = repo
        .capture_bot_lifecycle(&capture(&bot, owner, BotLifecycleKind::Archive))
        .unwrap();
    (repo, bot, owner, op)
}

fn restore(bot: &Bot, owner: BotOwner, source: BotRequestId, revision: u64) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind: BotLifecycleKind::Restore,
        chat: None,
        fingerprint: HASH.into(),
        expected_revision: revision,
        source: Some(source),
        members: vec![],
        now: 40,
    }
}

fn revision(repo: &Repository, bot: BotId) -> (u64, BotLifecycle) {
    let (bot, lifecycle) = repo.bot_any_lifecycle(bot).unwrap().unwrap();
    (bot.revision, lifecycle)
}

#[test]
fn archive_completes_only_when_settling_with_an_effective_barrier() {
    let (mut repo, bot, owner, op) = archived_setup();
    let scope = owner.user_scope;
    // Captured: not yet settled, and the barrier is still pending.
    assert_eq!(
        repo.complete_bot_archive(scope, op.operation, 20),
        Err(BotError::Conflict)
    );
    assert!(
        repo.advance_bot_lifecycle(
            scope,
            op.operation,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            false,
            11
        )
        .unwrap()
    );
    // A stale move from the old stage is a no-op, never a second transition.
    assert!(
        !repo
            .advance_bot_lifecycle(
                scope,
                op.operation,
                BotLifecycleStage::Captured,
                BotLifecycleStage::Settling,
                true,
                12
            )
            .unwrap()
    );
    assert_eq!(
        repo.complete_bot_archive(scope, op.operation, 20),
        Err(BotError::Conflict)
    );
    assert!(
        repo.advance_bot_lifecycle(
            scope,
            op.operation,
            BotLifecycleStage::Settling,
            BotLifecycleStage::Settling,
            true,
            13
        )
        .unwrap()
    );
    let before = revision(&repo, bot.id);
    assert_eq!(before, (bot.revision + 1, BotLifecycle::Active));
    let done = repo.complete_bot_archive(scope, op.operation, 20).unwrap();
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert_eq!(done.completed_at_ms, Some(20));
    assert!(
        done.members
            .iter()
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    assert_eq!(
        revision(&repo, bot.id),
        (bot.revision + 2, BotLifecycle::Archived)
    );
    // Nothing is hidden: the conversation stays associated and out of Trash.
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    // Ordinary Bot reads refuse an archived Bot; the lifecycle read finds it.
    assert_eq!(repo.bot(bot.id), Err(BotError::Forbidden));
    assert_eq!(
        repo.bots_in_lifecycle(owner, bot.home_workspace, BotLifecycle::Archived)
            .unwrap()
            .len(),
        1
    );
    // Completing twice is refused; the record is final.
    assert!(repo.complete_bot_archive(scope, op.operation, 21).is_err());
}

#[test]
fn unarchive_returns_the_bot_active_and_paused_and_replays_exactly() {
    let (mut repo, bot, owner, op) = archived_setup();
    let scope = owner.user_scope;
    repo.advance_bot_lifecycle(
        scope,
        op.operation,
        BotLifecycleStage::Captured,
        BotLifecycleStage::Settling,
        true,
        11,
    )
    .unwrap();
    repo.complete_bot_archive(scope, op.operation, 20).unwrap();
    let archived = revision(&repo, bot.id).0;
    // A stale expected revision is refused before anything is written.
    assert_eq!(
        repo.unarchive_bot(&restore(&bot, owner, op.operation, archived - 1)),
        Err(BotError::RevisionConflict)
    );
    let request = restore(&bot, owner, op.operation, archived);
    let done = repo.unarchive_bot(&request).unwrap();
    assert_eq!(done.kind, BotLifecycleKind::Restore);
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert_eq!(done.source, Some(op.operation));
    assert_eq!(
        revision(&repo, bot.id),
        (archived + 1, BotLifecycle::Active)
    );
    let active = repo.bot(bot.id).unwrap().unwrap();
    assert!(active.paused, "a restored Bot comes back paused");
    // An exact repeat returns the stored Restore and changes nothing.
    assert_eq!(repo.unarchive_bot(&request).unwrap(), done);
    assert_eq!(
        revision(&repo, bot.id),
        (archived + 1, BotLifecycle::Active)
    );
    // An Archive is reversed at most once.
    assert!(
        repo.unarchive_bot(&restore(&bot, owner, op.operation, archived + 1))
            .is_err()
    );
    // A Restore names only an Archive in this slice.
    let mut trash = restore(&bot, owner, op.operation, archived + 1);
    trash.kind = BotLifecycleKind::RestoreChat;
    assert_eq!(repo.unarchive_bot(&trash), Err(BotError::Invalid));
}

#[test]
fn a_parked_archive_resumes_once_and_a_reversed_one_holds_nothing() {
    let (mut repo, bot, owner, op) = archived_setup();
    let scope = owner.user_scope;
    assert!(
        repo.park_bot_lifecycle(scope, op.operation, BotLifecycleRefusal::PersonHolding, 15)
            .unwrap()
    );
    let parked = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(parked.stage, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::PersonHolding));
    // Parked keeps its barrier: the Bot stays held, and its barrier is found.
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::Pending));
    assert_eq!(
        repo.open_bot_lifecycle(bot.id).unwrap(),
        Some((scope, op.operation))
    );
    // Parking twice is a no-op; a resume moves it back to captured once.
    assert!(
        !repo
            .park_bot_lifecycle(scope, op.operation, BotLifecycleRefusal::WorkRunning, 16)
            .unwrap()
    );
    assert!(repo.resume_bot_lifecycle(scope, op.operation, 17).unwrap());
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 18).unwrap());
    let resumed = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.stage, BotLifecycleStage::Captured);
    assert_eq!(resumed.refusal, None);
    // A moving Archive cannot be reversed; park it, then Unarchive.
    let current = revision(&repo, bot.id).0;
    assert!(
        repo.unarchive_bot(&restore(&bot, owner, op.operation, current))
            .is_err()
    );
    repo.park_bot_lifecycle(scope, op.operation, BotLifecycleRefusal::WorkRunning, 19)
        .unwrap();
    repo.unarchive_bot(&restore(&bot, owner, op.operation, current))
        .unwrap();
    // The reversed Archive keeps its own record as it stopped, holds nothing,
    // and never resumes; another operation may start.
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage, BotLifecycleStage::Incomplete);
    assert!(repo.bot_lifecycle_reversed(scope, op.operation).unwrap());
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(repo.open_bot_lifecycle(bot.id).unwrap(), None);
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 41).unwrap());
}

#[test]
fn unresolved_submissions_read_through_the_per_bot_guard_index() {
    let (repo, bot, _) = setup();
    assert!(
        repo.bot_unresolved_submissions(bot.id, 16)
            .unwrap()
            .is_empty()
    );
    let plan: Vec<String> = repo
        .connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_command_guard
             WHERE bot_id=?1 AND command_kind IN ('start','continue') AND stage IN ('prepared','accepted') LIMIT ?2",
        )
        .unwrap()
        .query_map(params![bot.id.to_storage(), 16], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter().any(|row| row.contains("bot_command_guard")),
        "{plan:?}"
    );
}
