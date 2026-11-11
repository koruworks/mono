// SPDX-License-Identifier: MPL-2.0
//! Trash and Restore of a Bot and of one conversation (P1.7 S6, S7): each is
//! an AI effect only (O41), applied in one transaction against the
//! journal's own fences, and a Restore gives back exactly what its own Trash
//! took.
use super::*;

fn revision(repo: &Repository, bot: BotId) -> (u64, BotLifecycle, bool) {
    let (bot, lifecycle) = repo.bot_any_lifecycle(bot).unwrap().unwrap();
    (bot.revision, lifecycle, bot.paused)
}

fn chat_member(chat_id: ChatId, trashed: bool) -> LifecycleMemberDraft {
    LifecycleMemberDraft {
        subject: BotLifecycleSubject::Chat { chat_id },
        reason: BotLifecycleReason::OwnedConversation,
        prior_state: if trashed {
            BotLifecyclePriorState::Trashed
        } else {
            BotLifecyclePriorState::Live
        },
        intended: if trashed {
            BotLifecycleIntended::Retain
        } else {
            BotLifecycleIntended::Hide
        },
        retained: trashed.then_some(BotLifecycleRetained::AlreadyTrashed),
    }
}

/// A whole-Bot Trash capturing every conversation the way the driver does.
fn trash_capture(repo: &Repository, bot: &Bot, owner: BotOwner, revision: u64) -> LifecycleCapture {
    let (lifecycle, prior) = match repo.bot_any_lifecycle(bot.id).unwrap().unwrap().1 {
        BotLifecycle::Archived => (BotLifecycle::Archived, BotLifecyclePriorState::Archived),
        other => (other, BotLifecyclePriorState::Live),
    };
    assert_ne!(lifecycle, BotLifecycle::Trashed);
    let members = std::iter::once(LifecycleMemberDraft {
        subject: BotLifecycleSubject::Bot,
        reason: BotLifecycleReason::Bot,
        prior_state: prior,
        intended: BotLifecycleIntended::Hide,
        retained: None,
    })
    .chain(
        repo.lifecycle_bot_chats(bot.id)
            .unwrap()
            .into_iter()
            .map(|(chat, trashed)| chat_member(chat, trashed)),
    )
    .collect();
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind: BotLifecycleKind::Trash,
        chat: None,
        fingerprint: HASH.into(),
        expected_revision: revision,
        source: None,
        members,
        now: 30,
    }
}

fn chat_trash_capture(bot: &Bot, owner: BotOwner, chat: ChatId, revision: u64) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind: BotLifecycleKind::TrashChat,
        chat: Some(chat),
        fingerprint: HASH.into(),
        expected_revision: revision,
        source: None,
        members: vec![chat_member(chat, false)],
        now: 20,
    }
}

fn restore(
    bot: &Bot,
    owner: BotOwner,
    kind: BotLifecycleKind,
    chat: Option<ChatId>,
    source: BotRequestId,
    revision: u64,
) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind,
        chat,
        fingerprint: HASH.into(),
        expected_revision: revision,
        source: Some(source),
        members: vec![],
        now: 50,
    }
}

fn settle(repo: &mut Repository, scope: UserScopeId, op: BotRequestId, effective: bool) {
    assert!(
        repo.advance_bot_lifecycle(
            scope,
            op,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            effective,
            25
        )
        .unwrap()
    );
}

/// The operation each conversation is in Trash under, if any.
fn trashed_under(repo: &Repository, chat: ChatId) -> Option<String> {
    repo.connection
        .query_row(
            "SELECT operation_id FROM bot_chat_trash WHERE chat_id=?1",
            [chat.to_storage()],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}

fn another_chat(repo: &mut Repository, bot: &Bot) -> ChatId {
    let current = repo.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    repo.create_bot_chat(
        &current,
        ProfileId::new(),
        BotRequestId::mint(),
        "another",
        Timestamp::now(),
    )
    .unwrap()
    .chat_id
}

/// A conversation put into Trash on its own, completed.
fn trash_chat_alone(
    repo: &mut Repository,
    bot: &Bot,
    owner: BotOwner,
    chat: ChatId,
) -> BotRequestId {
    let current = revision(repo, bot.id).0;
    let op = repo
        .capture_bot_lifecycle(&chat_trash_capture(bot, owner, chat, current))
        .unwrap();
    settle(repo, owner.user_scope, op.operation, false);
    repo.complete_bot_chat_trash(owner.user_scope, op.operation, 26)
        .unwrap();
    op.operation
}

#[test]
fn bot_trash_hides_the_bot_and_restore_returns_only_what_it_took() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let other = another_chat(&mut repo, &bot);
    let alone = another_chat(&mut repo, &bot);
    let alone_op = trash_chat_alone(&mut repo, &bot, owner, alone);
    let start = revision(&repo, bot.id).0;
    let op = repo
        .capture_bot_lifecycle(&trash_capture(&repo, &bot, owner, start))
        .unwrap();
    // The Trash pauses the active Bot first, like Archive.
    assert!(op.pause_request.is_some() && op.policy_request.is_some());
    assert_eq!(op.prior_lifecycle, BotLifecycle::Active);
    assert_eq!(
        revision(&repo, bot.id),
        (start + 1, BotLifecycle::Active, true)
    );
    // The conversation already in Trash is retained as it was.
    let retained = op
        .members
        .iter()
        .find(|m| m.subject == BotLifecycleSubject::Chat { chat_id: alone })
        .unwrap();
    assert_eq!(retained.stage, BotLifecycleMemberStage::Retained);
    assert_eq!(
        retained.retained,
        Some(BotLifecycleRetained::AlreadyTrashed)
    );
    // The result says the pages and Run records stay where they are.
    assert_eq!(
        op.progress().left_in_place,
        vec![BotLeftInPlace::Pages, BotLeftInPlace::RunRecords]
    );
    // Not settled, then settled without an effective barrier: refused.
    assert_eq!(
        repo.complete_bot_trash(scope, op.operation, 40),
        Err(BotError::Conflict)
    );
    settle(&mut repo, scope, op.operation, false);
    assert_eq!(
        repo.complete_bot_trash(scope, op.operation, 40),
        Err(BotError::Conflict)
    );
    // Archive's completion never completes a Trash.
    assert_eq!(
        repo.complete_bot_archive(scope, op.operation, 40),
        Err(BotError::Conflict)
    );
    repo.advance_bot_lifecycle(
        scope,
        op.operation,
        BotLifecycleStage::Settling,
        BotLifecycleStage::Settling,
        true,
        41,
    )
    .unwrap();
    let done = repo.complete_bot_trash(scope, op.operation, 42).unwrap();
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert!(done.members.iter().all(|m| matches!(
        m.stage,
        BotLifecycleMemberStage::Acknowledged | BotLifecycleMemberStage::Retained
    )));
    assert_eq!(
        revision(&repo, bot.id),
        (start + 2, BotLifecycle::Trashed, true)
    );
    let trash = Some(op.operation.to_storage());
    assert_eq!(trashed_under(&repo, bot.main_chat), trash);
    assert_eq!(trashed_under(&repo, other), trash);
    assert_eq!(trashed_under(&repo, alone), Some(alone_op.to_storage()));
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::BotTrashed));
    // A conversation of a trashed Bot reads the Bot's hold, the one trashed
    // on its own too: restoring the Bot comes first.
    assert_eq!(
        repo.bot_chat_hold(alone).unwrap(),
        Some(BotHold::BotTrashed)
    );
    assert!(
        repo.visible_bot_chats()
            .unwrap()
            .iter()
            .all(|(id, _)| *id != bot.id)
    );
    assert_eq!(
        repo.bots_in_lifecycle(owner, bot.home_workspace, BotLifecycle::Trashed)
            .unwrap()
            .len(),
        1
    );
    // Completing twice is refused; the record is final.
    assert!(repo.complete_bot_trash(scope, op.operation, 43).is_err());
    // Restore: a stale revision is refused before anything is written.
    let trashed = start + 2;
    assert_eq!(
        repo.restore_trashed_bot(&restore(
            &bot,
            owner,
            BotLifecycleKind::Restore,
            None,
            op.operation,
            trashed - 1
        )),
        Err(BotError::RevisionConflict)
    );
    let request = restore(
        &bot,
        owner,
        BotLifecycleKind::Restore,
        None,
        op.operation,
        trashed,
    );
    let restored = repo.restore_trashed_bot(&request).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert!(restored.progress().left_in_place.is_empty());
    assert_eq!(
        revision(&repo, bot.id),
        (trashed + 1, BotLifecycle::Active, true),
        "a restored Bot comes back paused"
    );
    assert_eq!(trashed_under(&repo, bot.main_chat), None);
    assert_eq!(trashed_under(&repo, other), None);
    // The conversation trashed on its own stays in Trash.
    assert_eq!(trashed_under(&repo, alone), Some(alone_op.to_storage()));
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.bot_chat_hold(alone).unwrap(), Some(BotHold::Trashed));
    // An exact repeat returns the stored Restore and changes nothing.
    assert_eq!(repo.restore_trashed_bot(&request).unwrap(), restored);
    assert_eq!(revision(&repo, bot.id).0, trashed + 1);
    // A Trash is reversed at most once, and Unarchive never reverses one.
    let again = restore(
        &bot,
        owner,
        BotLifecycleKind::Restore,
        None,
        op.operation,
        trashed + 1,
    );
    assert!(repo.restore_trashed_bot(&again).is_err());
    assert_eq!(repo.unarchive_bot(&again), Err(BotError::Invalid));
    // Only its own operation's Restore brings that conversation back.
    let back = repo
        .restore_bot_chat(&restore(
            &bot,
            owner,
            BotLifecycleKind::RestoreChat,
            Some(alone),
            alone_op,
            trashed + 1,
        ))
        .unwrap();
    assert_eq!(back.stage, BotLifecycleStage::Completed);
    assert_eq!(trashed_under(&repo, alone), None);
}

#[test]
fn an_archived_bot_is_trashed_without_a_new_pause_and_restored_archived() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let archive = repo
        .capture_bot_lifecycle(&capture(&bot, owner, BotLifecycleKind::Archive))
        .unwrap();
    settle(&mut repo, scope, archive.operation, true);
    repo.complete_bot_archive(scope, archive.operation, 28)
        .unwrap();
    let archived = revision(&repo, bot.id).0;
    let op = repo
        .capture_bot_lifecycle(&trash_capture(&repo, &bot, owner, archived))
        .unwrap();
    assert_eq!(op.prior_lifecycle, BotLifecycle::Archived);
    assert_eq!(
        (op.pause_request, op.policy_request, op.barrier_revision),
        (None, None, None)
    );
    assert_eq!(revision(&repo, bot.id).0, archived, "no second pause");
    assert_eq!(
        repo.complete_bot_trash(scope, op.operation, 40),
        Err(BotError::Conflict),
        "it still settles first"
    );
    settle(&mut repo, scope, op.operation, false);
    repo.complete_bot_trash(scope, op.operation, 41).unwrap();
    assert_eq!(
        revision(&repo, bot.id),
        (archived + 1, BotLifecycle::Trashed, true)
    );
    repo.restore_trashed_bot(&restore(
        &bot,
        owner,
        BotLifecycleKind::Restore,
        None,
        op.operation,
        archived + 1,
    ))
    .unwrap();
    // Back where it was trashed from: archived, and paused.
    assert_eq!(
        revision(&repo, bot.id),
        (archived + 2, BotLifecycle::Archived, true)
    );
    assert_eq!(trashed_under(&repo, bot.main_chat), None);
}

#[test]
fn a_parked_trash_is_reversed_in_place_and_holds_nothing_after() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let start = revision(&repo, bot.id).0;
    let op = repo
        .capture_bot_lifecycle(&trash_capture(&repo, &bot, owner, start))
        .unwrap();
    // A moving Trash cannot be reversed.
    assert!(
        repo.restore_trashed_bot(&restore(
            &bot,
            owner,
            BotLifecycleKind::Restore,
            None,
            op.operation,
            start + 1
        ))
        .is_err()
    );
    repo.park_bot_lifecycle(scope, op.operation, BotLifecycleRefusal::WorkRunning, 31)
        .unwrap();
    assert_eq!(repo.bot_hold(bot.id).unwrap(), Some(BotHold::Parked));
    repo.restore_trashed_bot(&restore(
        &bot,
        owner,
        BotLifecycleKind::Restore,
        None,
        op.operation,
        start + 1,
    ))
    .unwrap();
    // It never trashed anything; the Bot stays active and paused.
    assert_eq!(
        revision(&repo, bot.id),
        (start + 2, BotLifecycle::Active, true)
    );
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.open_bot_lifecycle(bot.id).unwrap(), None);
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 60).unwrap());
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage.token(), "reversed");
    assert_eq!(original.refusal, Some(BotLifecycleRefusal::WorkRunning));
}

#[test]
fn a_conversation_trash_leaves_the_bot_as_it_is_and_restores_only_its_row() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let sibling = another_chat(&mut repo, &bot);
    let start = revision(&repo, bot.id);
    let op = repo
        .capture_bot_lifecycle(&chat_trash_capture(&bot, owner, bot.main_chat, start.0))
        .unwrap();
    // No pause, no barrier, no revision: the sibling is not disturbed.
    assert_eq!((op.pause_request, op.policy_request), (None, None));
    assert_eq!(revision(&repo, bot.id), start);
    assert_eq!(repo.bot_hold(bot.id).unwrap(), None);
    assert_eq!(repo.bot_chat_hold(sibling).unwrap(), None);
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Pending)
    );
    assert_eq!(
        op.progress().left_in_place,
        vec![BotLeftInPlace::Pages, BotLeftInPlace::RunRecords]
    );
    // A whole-Bot completion never completes it.
    assert!(repo.complete_bot_trash(scope, op.operation, 22).is_err());
    assert_eq!(
        repo.complete_bot_chat_trash(scope, op.operation, 22),
        Err(BotError::Conflict),
        "it settles first"
    );
    settle(&mut repo, scope, op.operation, false);
    let done = repo
        .complete_bot_chat_trash(scope, op.operation, 23)
        .unwrap();
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    // The main conversation is in Trash; its association, and the Bot, stay.
    assert_eq!(
        trashed_under(&repo, bot.main_chat),
        Some(op.operation.to_storage())
    );
    assert!(repo.bot_chat_in_trash(bot.main_chat).unwrap());
    assert_eq!(revision(&repo, bot.id), start);
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().main_chat, bot.main_chat);
    assert_eq!(repo.bot_for_chat(bot.main_chat).unwrap(), Some(bot.id));
    assert_eq!(trashed_under(&repo, sibling), None);
    // Restore: the revision fences it; then exactly that row goes.
    assert_eq!(
        repo.restore_bot_chat(&restore(
            &bot,
            owner,
            BotLifecycleKind::RestoreChat,
            Some(bot.main_chat),
            op.operation,
            start.0 + 1
        )),
        Err(BotError::RevisionConflict)
    );
    // Naming another conversation than the Trash's is refused.
    assert!(
        repo.restore_bot_chat(&restore(
            &bot,
            owner,
            BotLifecycleKind::RestoreChat,
            Some(sibling),
            op.operation,
            start.0
        ))
        .is_err()
    );
    let request = restore(
        &bot,
        owner,
        BotLifecycleKind::RestoreChat,
        Some(bot.main_chat),
        op.operation,
        start.0,
    );
    let restored = repo.restore_bot_chat(&request).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert_eq!(trashed_under(&repo, bot.main_chat), None);
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(revision(&repo, bot.id), start);
    assert_eq!(repo.restore_bot_chat(&request).unwrap(), restored);
    // The completed Trash it reversed says so, and keeps when it completed.
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage.token(), "reversed");
    assert_eq!(
        (original.refusal, original.completed_at_ms),
        (None, Some(23))
    );
}

#[test]
fn a_parked_conversation_trash_is_reversed_by_its_own_restore() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let start = revision(&repo, bot.id).0;
    let op = repo
        .capture_bot_lifecycle(&chat_trash_capture(&bot, owner, bot.main_chat, start))
        .unwrap();
    repo.park_bot_lifecycle(scope, op.operation, BotLifecycleRefusal::WorkRunning, 21)
        .unwrap();
    assert_eq!(
        repo.bot_chat_hold(bot.main_chat).unwrap(),
        Some(BotHold::Parked)
    );
    // A whole-Bot Restore cannot name a conversation's Trash.
    assert_eq!(
        repo.restore_trashed_bot(&restore(
            &bot,
            owner,
            BotLifecycleKind::Restore,
            None,
            op.operation,
            start
        )),
        Err(BotError::Invalid)
    );
    repo.restore_bot_chat(&restore(
        &bot,
        owner,
        BotLifecycleKind::RestoreChat,
        Some(bot.main_chat),
        op.operation,
        start,
    ))
    .unwrap();
    assert_eq!(repo.bot_chat_hold(bot.main_chat).unwrap(), None);
    assert_eq!(trashed_under(&repo, bot.main_chat), None);
    assert!(!repo.resume_bot_lifecycle(scope, op.operation, 22).unwrap());
    // Its own row says it was reversed, and why it had parked, and the
    // per-conversation barrier lookup no longer finds it.
    let original = repo
        .bot_lifecycle_operation(scope, op.operation)
        .unwrap()
        .unwrap();
    assert_eq!(original.stage.token(), "reversed");
    assert_eq!(original.refusal, Some(BotLifecycleRefusal::WorkRunning));
    assert_eq!(repo.open_chat_lifecycle(bot.main_chat).unwrap(), None);
}
