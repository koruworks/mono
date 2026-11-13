// SPDX-License-Identifier: MPL-2.0
//! The Archive driver's writes (P1.7 S4): stage moves, parking, completion
//! and Unarchive, each against the journal's own fences.
use super::*;

#[test]
fn boot_snapshot_reads_real_completed_archive_and_restored_current_revisions() {
    let (mut repo, bot, owner, op) = archived_setup();
    repo.advance_bot_lifecycle(
        owner.user_scope,
        op.operation,
        BotLifecycleStage::Captured,
        BotLifecycleStage::Settling,
        true,
        11,
    )
    .unwrap();
    repo.complete_bot_archive(owner.user_scope, op.operation, 20)
        .unwrap();
    let archived = repo
        .bot_boot_snapshot(
            owner,
            bot.id,
            &koru_delegate_core::BotBootAdmission::Missing {},
        )
        .unwrap();
    assert_eq!(archived.bot.lifecycle, BotLifecycle::Archived);
    assert_eq!(archived.admission.epoch, 3);
    assert!(archived.admission.paused);
    assert!(
        repo.bot_lifecycle_holders(owner, &[(bot.id, BotLifecycle::Archived)])
            .unwrap()
            .contains_key(&bot.id)
    );
    repo.unarchive_bot(&restore(&bot, owner, op.operation, 3))
        .unwrap();
    let restored = repo
        .bot_boot_snapshot(
            owner,
            bot.id,
            &koru_delegate_core::BotBootAdmission::Present {
                barrier: archived.admission,
            },
        )
        .unwrap();
    assert_eq!(restored.bot.lifecycle, BotLifecycle::Active);
    assert_eq!(restored.admission.epoch, 4);
    assert!(restored.admission.paused);
}

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
    // Parked keeps its barrier: the Bot stays held, as parked rather than
    // moving, and its barrier is found.
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::Parked));
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Parked)
    );
    assert_eq!(
        repo.open_bot_lifecycle(bot.id).unwrap(),
        Some((scope, op.operation))
    );
    // The Bot's read can name it, with why it stopped.
    let open = repo.bot_open_lifecycle(bot.id).unwrap().unwrap();
    assert_eq!(open.request_id, op.operation);
    assert_eq!(open.kind, BotLifecycleKind::Archive);
    assert_eq!(open.stage, BotLifecycleStage::Incomplete);
    assert_eq!(open.refusal, Some(BotLifecycleRefusal::PersonHolding));
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
    // The reversed Archive reads `reversed`, keeping why it had parked,
    // holds nothing, and never resumes; another operation may start.
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage, BotLifecycleStage::Reversed);
    assert_eq!(original.refusal, Some(BotLifecycleRefusal::WorkRunning));
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(repo.open_bot_lifecycle(bot.id).unwrap(), None);
    assert_eq!(repo.bot_open_lifecycle(bot.id).unwrap(), None);
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 41).unwrap());
}

/// How many rows of the journal a partial index holds, read through that
/// index with exactly its own predicate.
fn in_index(repo: &Repository, index: &str, predicate: &str) -> i64 {
    repo.connection
        .query_row(
            &format!(
                "SELECT count(*) FROM bot_lifecycle_intents INDEXED BY {index} WHERE {predicate}"
            ),
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// The predicate of every index of unfinished operations, as ai18 states it.
const OPEN_INDEXES: [(&str, &str); 4] = [
    (
        "bot_lifecycle_running",
        "bot_id IS NOT NULL AND stage NOT IN ('completed','incomplete','reversed')",
    ),
    (
        "bot_lifecycle_open",
        "bot_id IS NOT NULL AND stage NOT IN ('completed','reversed')",
    ),
    (
        "bot_lifecycle_resume",
        "updated_at_ms IS NOT NULL AND stage NOT IN ('completed','incomplete','reversed')",
    ),
    (
        "bot_lifecycle_chat_open",
        "chat_id IS NOT NULL AND stage NOT IN ('completed','reversed')",
    ),
];

/// A parked Archive that a Restore reverses is marked `reversed` on its own
/// row in the Restore's transaction: its progress read says so, keeping why
/// it had parked, and it leaves every index of unfinished operations by
/// that index's own predicate, so no hold, open-operation or resume read
/// needs to ask whether a Restore names it.
#[test]
fn a_reversed_parked_archive_reads_reversed_and_leaves_every_open_index() {
    let (mut repo, bot, owner, op) = archived_setup();
    let scope = owner.user_scope;
    repo.park_bot_lifecycle(
        scope,
        op.operation,
        BotLifecycleRefusal::OwnerUnavailable,
        15,
    )
    .unwrap();
    assert_eq!(
        in_index(&repo, "bot_lifecycle_open", OPEN_INDEXES[1].1),
        1,
        "parked is still open"
    );
    let current = revision(&repo, bot.id).0;
    let restored = repo
        .unarchive_bot(&restore(&bot, owner, op.operation, current))
        .unwrap();
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage.token(), "reversed");
    assert_eq!(original.progress().stage.token(), "reversed");
    assert_eq!(
        original.refusal,
        Some(BotLifecycleRefusal::OwnerUnavailable)
    );
    assert_eq!(original.completed_at_ms, None);
    assert_eq!(original.updated_at_ms, restored.completed_at_ms.unwrap());
    for (index, predicate) in OPEN_INDEXES {
        assert_eq!(in_index(&repo, index, predicate), 0, "{index}");
    }
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.bot_open_lifecycle(bot.id).unwrap(), None);
    assert!(
        repo.parked_for_owner_bot_lifecycle(None, 16)
            .unwrap()
            .is_empty()
    );
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 41).unwrap());
}

/// The normal Unarchive: a completed Archive that a Restore reverses is
/// marked `reversed` too, keeping its completion time and no refusal, so a
/// completed whole-Bot Archive or Trash that still reads `completed` is
/// exactly one that holds its Bot.
#[test]
fn a_reversed_completed_archive_reads_reversed_and_keeps_when_it_completed() {
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
    repo.unarchive_bot(&restore(&bot, owner, op.operation, archived))
        .unwrap();
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage.token(), "reversed");
    assert_eq!(original.refusal, None);
    assert_eq!(original.completed_at_ms, Some(20));
    assert!(
        repo.bot_lifecycle_holders(owner, &[(bot.id, BotLifecycle::Archived)])
            .unwrap()
            .is_empty()
    );
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
             WHERE bot_id=?1 AND command_kind IN ('start','continue','routine_occurrence') AND stage IN ('prepared','accepted') LIMIT ?2",
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
