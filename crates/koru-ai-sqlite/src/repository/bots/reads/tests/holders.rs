// SPDX-License-Identifier: MPL-2.0
//! The completed, unreversed Archive or Trash that holds an archived or
//! trashed Bot, read from the owner's own journal (QUESTIONS O44 interim).
use super::*;

fn owner_of(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

/// One journal row as the driver leaves it, written straight into the table
/// so each case states exactly the history it means. `source` names the
/// operation a Restore reverses.
fn op(
    repo: &Repository,
    owner: BotOwner,
    bot: BotId,
    kind: BotLifecycleKind,
    stage: BotLifecycleStage,
    source: Option<BotRequestId>,
    at: i64,
) -> BotRequestId {
    let operation = BotRequestId::mint();
    let completed = (stage == BotLifecycleStage::Completed).then_some(at);
    let refusal = (stage == BotLifecycleStage::Incomplete).then_some("work_running");
    repo.connection
        .execute(
            "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,chat_id,fingerprint,expected_revision,source_operation_id,prior_lifecycle,stage,refusal,requested_at_ms,updated_at_ms,completed_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,'archived',?9,?10,?11,?11,?12)",
            params![
                owner.user_scope.to_storage(),
                operation.to_storage(),
                bot.to_storage(),
                owner.device.to_storage(),
                kind.token(),
                kind.names_chat().then(|| ChatId::new().to_storage()),
                HASH,
                source.map(BotRequestId::to_storage),
                stage.token(),
                refusal,
                at,
                completed,
            ],
        )
        .unwrap();
    operation
}

fn holder(repo: &Repository, bot: &Bot, lifecycle: BotLifecycle) -> Option<BotLifecycleHolder> {
    repo.bot_lifecycle_holders(owner_of(bot), &[(bot.id, lifecycle)])
        .unwrap()
        .remove(&bot.id)
}

#[test]
fn archive_then_trash_then_restore_then_unarchive_names_the_right_holder_each_time() {
    use BotLifecycleKind::*;
    use BotLifecycleStage::*;
    let (repo, bot, _) = setup();
    let owner = owner_of(&bot);
    assert_eq!(holder(&repo, &bot, BotLifecycle::Active), None);
    // Archived.
    let archive = op(&repo, owner, bot.id, Archive, Completed, None, 10);
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Archived),
        Some(BotLifecycleHolder {
            request_id: archive,
            kind: Archive,
            completed_at_ms: 10
        })
    );
    // Trashed while archived: the Trash holds it, the Archive still stands.
    let trash = op(&repo, owner, bot.id, Trash, Completed, None, 20);
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Trashed).map(|h| (h.request_id, h.kind)),
        Some((trash, Trash))
    );
    // That Trash restored: back in the Archive, which holds it again.
    op(&repo, owner, bot.id, Restore, Completed, Some(trash), 30);
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Archived).map(|h| (h.request_id, h.kind)),
        Some((archive, Archive))
    );
    // Unarchived: active, nothing holds it, and a reversed Trash is never
    // named even if the caller asks as though the Bot were trashed.
    op(&repo, owner, bot.id, Restore, Completed, Some(archive), 40);
    assert_eq!(holder(&repo, &bot, BotLifecycle::Active), None);
    assert_eq!(holder(&repo, &bot, BotLifecycle::Archived), None);
    assert_eq!(holder(&repo, &bot, BotLifecycle::Trashed), None);
    // Archived again: the new Archive, never the reversed one.
    let again = op(&repo, owner, bot.id, Archive, Completed, None, 50);
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Archived).map(|h| h.request_id),
        Some(again)
    );
}

#[test]
fn unfinished_operations_and_conversation_trash_never_hold_a_bot() {
    use BotLifecycleKind::*;
    use BotLifecycleStage::*;
    let (repo, bot, _) = setup();
    let owner = owner_of(&bot);
    let archive = op(&repo, owner, bot.id, Archive, Completed, None, 10);
    // A parked Trash of the archived Bot, and a conversation's own Trash.
    op(&repo, owner, bot.id, Trash, Incomplete, None, 20);
    op(&repo, owner, bot.id, TrashChat, Completed, None, 30);
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Archived).map(|h| h.request_id),
        Some(archive)
    );
    assert_eq!(holder(&repo, &bot, BotLifecycle::Trashed), None);
}

#[test]
fn another_owners_or_devices_operations_never_appear() {
    use BotLifecycleKind::*;
    use BotLifecycleStage::*;
    let (repo, bot, _) = setup();
    let owner = owner_of(&bot);
    // Another scope, and another device of the same scope, wrote a completed
    // Archive naming this Bot's ID.
    let stranger = BotOwner {
        user_scope: UserScopeId::new(),
        device: owner.device,
    };
    let other_device = BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
    };
    op(&repo, stranger, bot.id, Archive, Completed, None, 10);
    op(&repo, other_device, bot.id, Archive, Completed, None, 11);
    assert_eq!(holder(&repo, &bot, BotLifecycle::Archived), None);
    // A Bot not asked about is not answered, even the owner's own.
    let other = BotId::mint();
    op(&repo, owner, other, Archive, Completed, None, 12);
    let mine = op(&repo, owner, bot.id, Archive, Completed, None, 13);
    let found = repo
        .bot_lifecycle_holders(owner, &[(bot.id, BotLifecycle::Archived)])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[&bot.id].request_id, mine);
    // One read answers several Bots at once.
    let both = repo
        .bot_lifecycle_holders(
            owner,
            &[
                (bot.id, BotLifecycle::Archived),
                (other, BotLifecycle::Archived),
            ],
        )
        .unwrap();
    assert_eq!(both.len(), 2);
}

#[test]
fn the_holder_read_walks_only_the_owners_journal_through_its_primary_key() {
    use BotLifecycleKind::*;
    use BotLifecycleStage::*;
    let (repo, bot, _) = setup();
    let owner = owner_of(&bot);
    // Thousands of other owners' completed operations, and a little of this
    // owner's own history.
    for n in 0..3000 {
        let stranger = BotOwner {
            user_scope: UserScopeId::new(),
            device: owner.device,
        };
        op(&repo, stranger, BotId::mint(), Archive, Completed, None, n);
    }
    let archive = op(&repo, owner, bot.id, Archive, Completed, None, 5000);
    let found = plan(
        &repo.connection,
        BOT_LIFECYCLE_HOLDERS_SQL,
        &[
            &owner.user_scope.to_storage(),
            &owner.device.to_storage(),
            &format!("[\"{}\"]", bot.id.to_storage()),
        ],
    );
    assert!(
        found.contains(
            "SEARCH i USING INDEX sqlite_autoindex_bot_lifecycle_intents_1 (user_scope_id=?"
        ),
        "{found}"
    );
    assert!(found.contains("bot_lifecycle_source"), "{found}");
    assert!(!found.contains("SCAN i"), "{found}");
    assert_eq!(
        holder(&repo, &bot, BotLifecycle::Archived).map(|h| h.request_id),
        Some(archive)
    );
}
