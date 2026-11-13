// SPDX-License-Identifier: MPL-2.0
//! Synthetic prepared evidence exercises persistence, not Store verification.
use super::*;
use koru_delegate_core as d;
mod capture;
mod memory;
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn fixture() -> (Repository, Bot, BotOwner) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = super::super::tests::fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), HASH, None)
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    (repo, bot, owner)
}
fn knowledge(bot: &Bot, owner: BotOwner) -> PreparedBotKnowledge {
    PreparedBotKnowledge {
        owner,
        bot: bot.id,
        request: BotRequestId::mint(),
        expected_revision: bot.revision,
        pages: vec![BotPage {
            workspace: bot.home_workspace,
            document: DocumentId::new(),
        }],
    }
}
fn seed() -> BotMemorySeed {
    BotMemorySeed {
        title: "Memory".into(),
        hash: [7; 32],
    }
}
fn prepared(repo: &mut Repository, bot: &Bot, owner: BotOwner) -> BotMemoryCreation {
    repo.prepare_bot_memory_creation(
        owner,
        bot.id,
        BotRequestId::mint(),
        bot.revision,
        &seed(),
        HASH,
        2,
    )
    .unwrap()
}
fn confirmation(c: &BotMemoryCreation) -> BotMemoryConfirmation {
    BotMemoryConfirmation {
        identity: c.identity.clone(),
        original_edit_revision: vec![1, 2, 3],
        live_edit_revision: vec![1, 2, 3],
    }
}
fn receipt_count(repo: &Repository) -> u32 {
    repo.connection
        .query_row("SELECT count(*) FROM bot_command_receipts", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn library_namespace_is_shared_both_directions() {
    let (mut repo, bot, owner) = fixture();
    let mut input = knowledge(&bot, owner);
    let legacy = BotRequestId::mint();
    repo.create_bot_chat(
        &bot,
        ProfileId::new(),
        legacy,
        HASH,
        Timestamp::from_millis(2),
    )
    .unwrap();
    input.request = legacy;
    assert_eq!(
        repo.set_bot_knowledge(&input, HASH, 3),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.prepare_bot_memory_creation(owner, bot.id, legacy, 1, &seed(), HASH, 3),
        Err(BotError::Conflict)
    );
    input.request = BotRequestId::mint();
    let stored = repo.set_bot_knowledge(&input, HASH, 3).unwrap();
    let count = receipt_count(&repo);
    assert_eq!(repo.set_bot_knowledge(&input, HASH, 4).unwrap(), stored);
    assert_eq!(receipt_count(&repo), count);
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, 2);
    assert_eq!(
        repo.bot_receipt(owner.user_scope, input.request, HASH),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.save_bot(&bot, ProfileId::new(), input.request, HASH, None),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.prepare_bot_pause(owner, bot.id, input.request, 2, true, HASH, 4),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.prepare_bot_memory_creation(owner, bot.id, input.request, 2, &seed(), HASH, 4),
        Err(BotError::Conflict)
    );
    let mut changed = input.clone();
    changed.pages.clear();
    assert_eq!(
        repo.set_bot_knowledge(&changed, HASH, 4),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.set_bot_knowledge(&input, OTHER_HASH, 4),
        Err(BotError::Conflict)
    );
    let current = repo.bot(bot.id).unwrap().unwrap();
    let c = prepared(&mut repo, &current, owner);
    assert_eq!(
        repo.bot_receipt(owner.user_scope, c.identity.creation_request, HASH),
        Err(BotError::Conflict)
    );
    let mut other = super::super::tests::fixture_bot();
    other.user_scope = owner.user_scope;
    other.owner_device = owner.device;
    repo.save_bot(&other, ProfileId::new(), BotRequestId::mint(), HASH, None)
        .unwrap();
    let mut collision = knowledge(&other, owner);
    collision.request = input.request;
    assert_eq!(
        repo.set_bot_knowledge(&collision, HASH, 5),
        Err(BotError::Conflict)
    );
}

#[test]
fn knowledge_atomic_owner_revision_and_binding_fences() {
    let (mut repo, bot, owner) = fixture();
    let input = knowledge(&bot, owner);
    let before = repo.bot(bot.id).unwrap().unwrap();
    let count = receipt_count(&repo);
    for foreign in [
        BotOwner {
            user_scope: UserScopeId::new(),
            ..owner
        },
        BotOwner {
            device: DeviceId::new(),
            ..owner
        },
    ] {
        let mut bad = input.clone();
        bad.owner = foreign;
        assert_eq!(
            repo.set_bot_knowledge(&bad, HASH, 3),
            Err(BotError::Forbidden)
        );
        assert_eq!(repo.bot_library(foreign, bot.id), Err(BotError::Forbidden));
    }
    let mut stale = input.clone();
    stale.expected_revision = 2;
    assert_eq!(
        repo.set_bot_knowledge(&stale, HASH, 3),
        Err(BotError::RevisionConflict)
    );
    let mut foreign = input.clone();
    foreign.pages[0].workspace = WorkspaceId::new();
    assert_eq!(
        repo.set_bot_knowledge(&foreign, HASH, 3),
        Err(BotError::Invalid)
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap(), before);
    assert_eq!(receipt_count(&repo), count);
    let pause = repo
        .prepare_bot_pause(owner, bot.id, BotRequestId::mint(), 1, true, HASH, 3)
        .unwrap();
    let mut current = input.clone();
    current.expected_revision = 2;
    assert_eq!(
        repo.set_bot_knowledge(&current, HASH, 4),
        Err(BotError::Settling)
    );
    repo.acknowledge_bot_pause(
        owner,
        bot.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        5,
    )
    .unwrap();
    repo.set_bot_knowledge(&current, HASH, 6).unwrap();
    let after = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(after.policy_revision, before.policy_revision);
    assert_eq!(after.selected_pages, before.selected_pages);
    assert!(after.paused);
    let mut clear = knowledge(&after, owner);
    clear.pages.clear();
    repo.set_bot_knowledge(&clear, HASH, 7).unwrap();
    assert!(
        repo.bot_library(owner, bot.id)
            .unwrap()
            .knowledge
            .is_empty()
    );
    let current_bot = repo.bot(bot.id).unwrap().unwrap();
    let c = prepared(&mut repo, &current_bot, owner);
    let binding = repo
        .bind_bot_memory(&confirmation(&c), BotMemoryState::Prepared, 8)
        .unwrap();
    let current_bot = repo.bot(bot.id).unwrap().unwrap();
    let mut forbidden = knowledge(&current_bot, owner);
    forbidden.pages = vec![binding.page];
    let count = receipt_count(&repo);
    assert_eq!(
        repo.set_bot_knowledge(&forbidden, HASH, 9),
        Err(BotError::Forbidden)
    );
    assert_eq!(receipt_count(&repo), count);
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(repo.bot_library(owner, bot.id).is_ok());
    assert_eq!(
        repo.set_bot_knowledge(&forbidden, HASH, 9),
        Err(BotError::Forbidden)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::NotFound));
}

#[test]
fn library_tables_reject_invalid_rows_and_corrupt_reads() {
    let (mut repo, bot, owner) = fixture();
    let input = knowledge(&bot, owner);
    repo.set_bot_knowledge(&input, HASH, 2).unwrap();
    for (ordinal, workspace, doc) in [
        (16, bot.home_workspace, DocumentId::new()),
        (1, WorkspaceId::new(), DocumentId::new()),
        (1, bot.home_workspace, input.pages[0].document),
    ] {
        assert!(repo.connection.execute("INSERT INTO bot_knowledge_pages(bot_id,workspace_id,document_id,ordinal,added_at_ms) VALUES(?1,?2,?3,?4,1)",params![bot.id.to_storage(),workspace.to_storage(),doc.to_storage(),ordinal]).is_err());
    }
    repo.connection
        .execute(
            "UPDATE bot_knowledge_pages SET ordinal=2 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::Storage));
    repo.connection
        .execute(
            "UPDATE bot_knowledge_pages SET ordinal=0,document_id='invalid' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(repo.bot_library(owner, bot.id), Err(BotError::Storage));
}

#[test]
fn library_writes_obey_pending_policy_and_lifecycle() {
    for lifecycle_hold in [false, true] {
        let (mut repo, bot, owner) = fixture();
        let c = prepared(&mut repo, &bot, owner);
        if lifecycle_hold {
            let op = repo
                .capture_bot_lifecycle(&LifecycleCapture {
                    owner,
                    bot: bot.id,
                    operation: BotRequestId::mint(),
                    kind: BotLifecycleKind::Archive,
                    chat: None,
                    fingerprint: HASH.into(),
                    expected_revision: 1,
                    source: None,
                    now: 3,
                    members: vec![LifecycleMemberDraft {
                        subject: BotLifecycleSubject::Bot,
                        reason: BotLifecycleReason::Bot,
                        prior_state: BotLifecyclePriorState::Live,
                        intended: BotLifecycleIntended::Hide,
                        retained: None,
                    }],
                })
                .unwrap();
            // Settle the pause itself so the still-open lifecycle hold is the
            // independent refusal this case exercises.
            let BotAdmissionReceipt::Pause(pause) = repo
                .bot_admission_receipt(owner, bot.id, op.pause_request.unwrap())
                .unwrap()
                .unwrap()
            else {
                panic!("pause receipt expected")
            };
            repo.acknowledge_bot_pause(
                owner,
                bot.id,
                pause.intent.barrier.request,
                &pause.intent.barrier,
                4,
            )
            .unwrap();
        } else {
            let mut desired = bot.clone();
            desired.revision = 2;
            desired.policy_revision = 2;
            desired.access = BotAccess::Workspace;
            let barrier = d::BotPolicyBarrier {
                key: d::BotPolicyBarrierKey {
                    owner: d::BotOwner {
                        user_scope: owner.user_scope,
                        device: owner.device,
                    },
                    bot: bot.id,
                    request: d::NativePolicyRequestId::from(BotRequestId::mint()),
                },
                config_revision: 2,
                policy_revision: 2,
                policy: bot_native_policy(&desired),
                effects_allowed: true,
            };
            repo.prepare_bot_access(
                &desired,
                ProfileId::new(),
                BotRequestId::mint(),
                HASH,
                1,
                &barrier,
            )
            .unwrap();
        }
        let revision: u64 = repo
            .connection
            .query_row(
                "SELECT revision FROM bots WHERE bot_id=?1",
                [bot.id.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        let mut input = knowledge(&bot, owner);
        input.expected_revision = revision;
        let count = receipt_count(&repo);
        assert!(repo.set_bot_knowledge(&input, HASH, 5).is_err());
        assert!(
            repo.prepare_bot_memory_creation(
                owner,
                bot.id,
                BotRequestId::mint(),
                revision,
                &seed(),
                HASH,
                5
            )
            .is_err()
        );
        assert!(
            repo.bind_bot_memory(&confirmation(&c), BotMemoryState::Prepared, 5)
                .is_err()
        );
        assert_eq!(receipt_count(&repo), count);
        assert_eq!(
            repo.connection
                .query_row(
                    "SELECT revision FROM bots WHERE bot_id=?1",
                    [bot.id.to_storage()],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            revision
        );
    }
}
