// SPDX-License-Identifier: MPL-2.0
use super::super::tests::fixture_bot;
use super::*;
use std::collections::BTreeSet;
mod migration;
mod plans;

const HASH: &str = "abababababababababababababababababababababababababababababababab";

fn setup() -> (Repository, Bot, BotOwner) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    (repo, bot, owner)
}

fn capture(bot: &Bot, owner: BotOwner, kind: BotLifecycleKind) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind,
        chat: kind.names_chat().then_some(bot.main_chat),
        fingerprint: HASH.into(),
        expected_revision: bot.revision,
        source: None,
        members: vec![
            LifecycleMemberDraft {
                subject: LifecycleSubject::Bot,
                reason: BotLifecycleReason::Bot,
                prior_state: BotLifecyclePriorState::Live,
                intended: BotLifecycleIntended::Hide,
                retained: None,
            },
            LifecycleMemberDraft {
                subject: LifecycleSubject::Chat(bot.main_chat),
                reason: BotLifecycleReason::OwnedConversation,
                prior_state: BotLifecyclePriorState::Live,
                intended: BotLifecycleIntended::Hide,
                retained: None,
            },
        ]
        .into_iter()
        .filter(|m| !(kind.names_chat() && m.subject == LifecycleSubject::Bot))
        .collect(),
        now: 10,
    }
}

/// The `IN (...)` list of `column`'s CHECK in `table`'s stored definition.
pub(super) fn check_list(conn: &Connection, table: &str, column: &str) -> BTreeSet<String> {
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )
        .unwrap();
    let marker = format!("CHECK({column} IN (");
    let start = sql
        .find(&marker)
        .unwrap_or_else(|| panic!("{table}.{column} has no closed CHECK"))
        + marker.len();
    let end = start + sql[start..].find(')').unwrap();
    let list: BTreeSet<String> = sql[start..end]
        .split(',')
        .map(|v| v.trim().trim_matches('\'').to_owned())
        .collect();
    assert_eq!(
        sql.matches(&marker).count(),
        1,
        "{table}.{column} has one vocabulary"
    );
    list
}

fn tokens<T: Copy>(all: &[T], token: fn(T) -> &'static str) -> BTreeSet<String> {
    all.iter().map(|v| token(*v).to_owned()).collect()
}

#[test]
fn every_stored_vocabulary_equals_its_one_rust_enum() {
    let repo = Repository::in_memory().unwrap();
    let c = &repo.connection;
    let cases: Vec<(&str, &str, BTreeSet<String>)> = vec![
        (
            "bots",
            "lifecycle",
            tokens(BotLifecycle::ALL, BotLifecycle::token),
        ),
        (
            "bot_chats",
            "purpose",
            tokens(BotChatPurpose::ALL, BotChatPurpose::token),
        ),
        (
            "bot_lifecycle_intents",
            "kind",
            tokens(BotLifecycleKind::ALL, BotLifecycleKind::token),
        ),
        (
            "bot_lifecycle_intents",
            "prior_lifecycle",
            tokens(BotLifecycle::ALL, BotLifecycle::token),
        ),
        (
            "bot_lifecycle_intents",
            "policy_barrier_state",
            tokens(
                BotLifecycleBarrierState::ALL,
                BotLifecycleBarrierState::token,
            ),
        ),
        (
            "bot_lifecycle_intents",
            "stage",
            tokens(BotLifecycleStage::ALL, BotLifecycleStage::token),
        ),
        (
            "bot_lifecycle_intents",
            "refusal",
            tokens(BotLifecycleRefusal::ALL, BotLifecycleRefusal::token),
        ),
        (
            "bot_lifecycle_members",
            "kind",
            tokens(BotLifecycleMemberKind::ALL, BotLifecycleMemberKind::token),
        ),
        (
            "bot_lifecycle_members",
            "owner",
            tokens(BotLifecycleOwner::ALL, BotLifecycleOwner::token),
        ),
        (
            "bot_lifecycle_members",
            "reason",
            tokens(BotLifecycleReason::ALL, BotLifecycleReason::token),
        ),
        (
            "bot_lifecycle_members",
            "prior_state",
            tokens(BotLifecyclePriorState::ALL, BotLifecyclePriorState::token),
        ),
        (
            "bot_lifecycle_members",
            "intended",
            tokens(BotLifecycleIntended::ALL, BotLifecycleIntended::token),
        ),
        (
            "bot_lifecycle_members",
            "stage",
            tokens(BotLifecycleMemberStage::ALL, BotLifecycleMemberStage::token),
        ),
        (
            "bot_lifecycle_members",
            "retained_reason",
            tokens(BotLifecycleRetained::ALL, BotLifecycleRetained::token),
        ),
        (
            "bot_purge_tombstones",
            "subject_kind",
            tokens(BotTombstoneSubject::ALL, BotTombstoneSubject::token),
        ),
        (
            "bot_purge_tombstones",
            "chat_purpose",
            tokens(BotChatPurpose::ALL, BotChatPurpose::token),
        ),
    ];
    for (table, column, rust) in cases {
        assert_eq!(check_list(c, table, column), rust, "{table}.{column}");
    }
    // Every member kind's owner is one the stored vocabulary allows, and the
    // repository never offers a member to an owner that cannot act on it.
    for subject in [LifecycleSubject::Bot, LifecycleSubject::Chat(ChatId::new())] {
        assert_eq!(subject.owner(), BotLifecycleOwner::Ai);
    }
}

#[test]
fn archive_capture_is_one_transaction_with_its_pause_barrier() {
    let (mut repo, bot, owner) = setup();
    let request = capture(&bot, owner, BotLifecycleKind::Archive);
    let captured = repo.capture_bot_lifecycle(&request).unwrap();
    assert_eq!(captured.stage, BotLifecycleStage::Captured);
    assert_eq!(captured.prior_lifecycle, BotLifecycle::Active);
    assert_eq!(captured.barrier_revision, Some(bot.revision + 1));
    assert_eq!(
        captured.policy_barrier,
        Some(BotLifecycleBarrierState::Pending)
    );
    assert_eq!(captured.members.len(), 2);
    assert!(
        captured
            .members
            .iter()
            .all(|m| m.child_request.is_none() && m.stage == BotLifecycleMemberStage::Pending)
    );
    let stored = repo.bot(bot.id).unwrap().unwrap();
    assert!(stored.paused);
    assert_eq!(stored.revision, bot.revision + 1);
    // The barrier is the existing pause receipt, recovered by the existing owner path.
    let pause = captured.pause_request.unwrap();
    assert!(matches!(
        repo.bot_admission_receipt(owner, bot.id, pause).unwrap(),
        Some(BotAdmissionReceipt::Pause(_))
    ));
    // Exact replay returns the capture; a changed request conflicts.
    assert_eq!(repo.capture_bot_lifecycle(&request).unwrap(), captured);
    let mut changed = request.clone();
    changed.fingerprint = "cd".repeat(32);
    assert_eq!(
        repo.capture_bot_lifecycle(&changed),
        Err(BotError::Conflict)
    );
    // One unfinished operation per Bot: a second identity loses.
    let mut second = capture(&bot, owner, BotLifecycleKind::Trash);
    second.expected_revision = bot.revision + 1;
    assert_eq!(repo.capture_bot_lifecycle(&second), Err(BotError::Conflict));
    let (scope, operation) = (owner.user_scope, request.operation);
    assert_eq!(
        repo.resumable_bot_lifecycle(None, 16).unwrap(),
        vec![(10, scope, operation)]
    );
}

#[test]
fn a_late_member_failure_rolls_back_intent_barrier_and_revision() {
    let (mut repo, bot, owner) = setup();
    repo.connection
        .execute_batch("CREATE TRIGGER refuse_member BEFORE INSERT ON bot_lifecycle_members WHEN NEW.ordinal=1 BEGIN SELECT RAISE(ABORT,'fixture'); END;")
        .unwrap();
    let request = capture(&bot, owner, BotLifecycleKind::Trash);
    assert_eq!(repo.capture_bot_lifecycle(&request), Err(BotError::Storage));
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    assert_eq!(
        repo.bot_lifecycle_operation(owner.user_scope, request.operation)
            .unwrap(),
        None
    );
    let pauses: i64 = repo
        .connection
        .query_row(
            "SELECT count(*) FROM bot_command_receipts WHERE command_kind='pause'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pauses, 0);
    repo.connection
        .execute_batch("DROP TRIGGER refuse_member")
        .unwrap();
    assert!(repo.capture_bot_lifecycle(&request).is_ok());
}

#[test]
fn stale_foreign_and_malformed_captures_refuse_before_writing() {
    let (mut repo, bot, owner) = setup();
    let mut stale = capture(&bot, owner, BotLifecycleKind::Archive);
    stale.expected_revision += 1;
    assert_eq!(
        repo.capture_bot_lifecycle(&stale),
        Err(BotError::RevisionConflict)
    );
    let mut foreign = capture(&bot, owner, BotLifecycleKind::Archive);
    foreign.owner.device = DeviceId::from_uuid(uuid::Uuid::new_v4());
    assert_eq!(
        repo.capture_bot_lifecycle(&foreign),
        Err(BotError::Forbidden)
    );
    let mut sourceless = capture(&bot, owner, BotLifecycleKind::Restore);
    sourceless.members.clear();
    assert_eq!(
        repo.capture_bot_lifecycle(&sourceless),
        Err(BotError::Invalid)
    );
    let mut wrong_member = capture(&bot, owner, BotLifecycleKind::Archive);
    wrong_member.members.push(LifecycleMemberDraft {
        subject: LifecycleSubject::OwnedRole {
            workspace: bot.config.role.workspace,
            document: bot.config.role.document,
        },
        reason: BotLifecycleReason::RoleCreationReceipt,
        prior_state: BotLifecyclePriorState::Live,
        intended: BotLifecycleIntended::Hide,
        retained: None,
    });
    assert_eq!(
        repo.capture_bot_lifecycle(&wrong_member),
        Err(BotError::Invalid)
    );
    let mut other_chat = capture(&bot, owner, BotLifecycleKind::TrashChat);
    other_chat.chat = Some(ChatId::new());
    assert_eq!(
        repo.capture_bot_lifecycle(&other_chat),
        Err(BotError::Invalid)
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    let intents: i64 = repo
        .connection
        .query_row("SELECT count(*) FROM bot_lifecycle_intents", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(intents, 0);
}

#[test]
fn a_conversation_barrier_needs_no_pause_and_only_its_own_reversal_may_follow() {
    let (mut repo, bot, owner) = setup();
    let mut trash = capture(&bot, owner, BotLifecycleKind::TrashChat);
    trash.members.push(LifecycleMemberDraft {
        subject: LifecycleSubject::RunReference {
            run: RunId::mint(),
            workspace: bot.home_workspace,
            document: None,
            chat: Some(bot.main_chat),
        },
        reason: BotLifecycleReason::RunOfConversation,
        prior_state: BotLifecyclePriorState::Live,
        intended: BotLifecycleIntended::Hide,
        retained: None,
    });
    let captured = repo.capture_bot_lifecycle(&trash).unwrap();
    assert_eq!(captured.pause_request, None);
    assert_eq!(captured.barrier_revision, None);
    assert!(!repo.bot(bot.id).unwrap().unwrap().paused);
    // Only the delegate/Store members carry a minted child request.
    assert!(captured.members[0].child_request.is_none());
    assert!(captured.members[1].child_request.is_some());
    assert_eq!(
        repo.open_chat_lifecycle(bot.main_chat).unwrap(),
        Some((owner.user_scope, trash.operation))
    );
    // A reversal of a moving operation must wait for it to settle or park.
    let mut restore = capture(&bot, owner, BotLifecycleKind::RestoreChat);
    restore.members.clear();
    restore.source = Some(trash.operation);
    assert_eq!(
        repo.capture_bot_lifecycle(&restore),
        Err(BotError::Conflict)
    );
    repo.connection
        .execute(
            "UPDATE bot_lifecycle_intents SET stage='incomplete',refusal='owner_unavailable' WHERE operation_id=?1",
            [trash.operation.to_storage()],
        )
        .unwrap();
    // Parked: nothing else may start, but its own reversal may.
    let mut archive = capture(&bot, owner, BotLifecycleKind::Archive);
    archive.expected_revision = bot.revision;
    assert_eq!(
        repo.capture_bot_lifecycle(&archive),
        Err(BotError::Conflict)
    );
    let restored = repo.capture_bot_lifecycle(&restore).unwrap();
    assert_eq!(restored.source, Some(trash.operation));
    assert!(restored.members.is_empty());
    // An operation is reversed at most once.
    let mut again = restore.clone();
    again.operation = BotRequestId::mint();
    assert_eq!(repo.capture_bot_lifecycle(&again), Err(BotError::Conflict));
}

#[test]
fn stored_constraints_refuse_rows_the_enums_could_not_write() {
    let (mut repo, bot, owner) = setup();
    let captured = repo
        .capture_bot_lifecycle(&capture(&bot, owner, BotLifecycleKind::TrashChat))
        .unwrap();
    let insert = |sql: &str| {
        repo.connection
            .execute(
                sql,
                params![
                    owner.user_scope.to_storage(),
                    captured.operation.to_storage()
                ],
            )
            .is_err()
    };
    // A chat member without its chat, and a delegate member without a child request.
    assert!(insert(
        "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,reason,prior_state,intended,stage,updated_at_ms) VALUES(?1,?2,9,'chat','ai','owned_conversation','live','hide','pending',1)"
    ));
    assert!(insert(
        "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,run_id,workspace_id,reason,prior_state,intended,stage,updated_at_ms) VALUES(?1,?2,9,'run_reference','delegate','r','w','run_of_conversation','live','hide','pending',1)"
    ));
    // Retained needs its reason; an unknown token is refused.
    assert!(insert(
        "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,chat_id,reason,prior_state,intended,stage,updated_at_ms) VALUES(?1,?2,9,'chat','ai','c','owned_conversation','live','retain','retained',1)"
    ));
    assert!(insert(
        "INSERT INTO bot_lifecycle_members(user_scope_id,operation_id,ordinal,kind,owner,chat_id,reason,prior_state,intended,stage,updated_at_ms) VALUES(?1,?2,9,'chat','ai','c','owned_conversation','live','hide','later',1)"
    ));
    // A completed stage needs its time, and an incomplete one its refusal.
    assert!(
        repo.connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage='completed' WHERE operation_id=?1",
                [captured.operation.to_storage()],
            )
            .is_err()
    );
    assert!(
        repo.connection
            .execute(
                "UPDATE bot_lifecycle_intents SET stage='incomplete' WHERE operation_id=?1",
                [captured.operation.to_storage()],
            )
            .is_err()
    );
}
