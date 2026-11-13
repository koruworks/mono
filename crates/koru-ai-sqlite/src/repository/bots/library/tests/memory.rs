// SPDX-License-Identifier: MPL-2.0
use super::*;
mod cas;
mod recovery;

#[test]
fn exact_memory_creation_receipt_is_read_only_and_binds_alias_body_before_current_revision() {
    let (mut repo, bot, owner) = fixture();
    let original = prepared(&mut repo, &bot, owner);
    let alias = BotRequestId::mint();
    assert_eq!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 1, &seed(), OTHER_HASH)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.bot_library(owner, bot.id).unwrap().creation,
        Some(original.clone())
    );
    repo.prepare_bot_memory_creation(owner, bot.id, alias, 1, &seed(), OTHER_HASH, 3)
        .unwrap();
    repo.bind_bot_memory(&confirmation(&original), original.observed(), 4)
        .unwrap();
    let bound = repo.bot_library(owner, bot.id).unwrap().creation.unwrap();
    assert_eq!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 1, &seed(), OTHER_HASH)
            .unwrap(),
        Some(bound.clone())
    );
    assert!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 2, &seed(), OTHER_HASH)
            .is_err()
    );
    assert!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 1, &seed(), HASH)
            .is_err()
    );
    let mut changed = seed();
    changed.title = "Changed".into();
    assert!(
        repo.bot_memory_creation_receipt(
            owner,
            bot.id,
            original.identity.creation_request,
            1,
            &changed,
            HASH
        )
        .is_err()
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 1, &seed(), OTHER_HASH)
            .unwrap(),
        Some(bound)
    );
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(
        repo.bot_memory_creation_receipt(owner, bot.id, alias, 1, &seed(), OTHER_HASH)
            .is_err()
    );
}

#[test]
fn memory_candidate_transitions_preserve_identity() {
    let (mut repo, bot, owner) = fixture();
    let c = prepared(&mut repo, &bot, owner);
    for reason in [
        BotMemoryRefusal::OwnerUnavailable,
        BotMemoryRefusal::StoreUnavailable,
    ] {
        assert_eq!(
            repo.transition_bot_memory_creation(
                &c.identity,
                c.observed(),
                BotMemoryTransition::Disagreement(reason),
                3
            ),
            Err(BotError::Invalid)
        );
        assert_eq!(
            repo.bot_library(owner, bot.id).unwrap().creation,
            Some(c.clone())
        );
    }
    let alias = BotRequestId::mint();
    assert_eq!(
        repo.prepare_bot_memory_creation(owner, bot.id, alias, 1, &seed(), HASH, 3)
            .unwrap(),
        c
    );
    let alias_json: String = repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [alias.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    let mut current = c.clone();
    for transition in [
        BotMemoryTransition::ReadFailure,
        BotMemoryTransition::Unknown,
        BotMemoryTransition::Disagreement(BotMemoryRefusal::DocumentChanged),
        BotMemoryTransition::Abandon,
        BotMemoryTransition::Retry,
    ] {
        current = repo
            .transition_bot_memory_creation(&c.identity, current.observed(), transition, 4)
            .unwrap();
        assert_eq!(current.identity, c.identity);
    }
    assert_eq!(current.state, BotMemoryState::Prepared);
    assert_eq!(
        repo.transition_bot_memory_creation(
            &c.identity,
            BotMemoryObserved {
                state: BotMemoryState::Conflict,
                ..current.observed()
            },
            BotMemoryTransition::Retry,
            5
        ),
        Err(BotError::Conflict)
    );
    let evidence = confirmation(&c);
    let binding = repo
        .bind_bot_memory(&evidence, current.observed(), 6)
        .unwrap();
    assert_eq!(
        repo.bind_bot_memory(&evidence, current.observed(), 7),
        Err(BotError::Conflict)
    );
    let bound = repo.bot_library(owner, bot.id).unwrap().creation.unwrap();
    assert_eq!(
        repo.bind_bot_memory(&evidence, bound.observed(), 7)
            .unwrap(),
        binding
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().revision, 2);
    let after = repo
        .prepare_bot_memory_creation(
            owner,
            bot.id,
            c.identity.creation_request,
            1,
            &seed(),
            HASH,
            8,
        )
        .unwrap();
    assert_eq!(after.state, BotMemoryState::Bound);
    assert_eq!(
        repo.prepare_bot_memory_creation(owner, bot.id, alias, 1, &seed(), HASH, 8)
            .unwrap(),
        after
    );
    assert_eq!(
        repo.transition_bot_memory_creation(
            &c.identity,
            after.observed(),
            BotMemoryTransition::Abandon,
            8
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.connection
            .query_row(
                "SELECT count(*) FROM bot_memory_creation_intents",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
    let alias_after: (String, String) = repo
        .connection
        .query_row(
            "SELECT outcome_json,stage FROM bot_command_receipts WHERE request_id=?1",
            [alias.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(alias_after, (alias_json, "prepared".into()));
    assert_eq!(
        repo.bot_library(owner, bot.id).unwrap().binding,
        Some(binding)
    );
}

#[test]
fn memory_bind_rechecks_current_canonical_state() {
    for disagreement in [
        "owner",
        "home",
        "lifecycle",
        "role",
        "knowledge",
        "evidence",
    ] {
        let (mut repo, bot, owner) = fixture();
        let c = prepared(&mut repo, &bot, owner);
        let mut evidence = confirmation(&c);
        match disagreement {
            "owner" => {
                repo.connection
                    .execute(
                        "UPDATE bots SET owner_device_id=?1 WHERE bot_id=?2",
                        params![DeviceId::new().to_storage(), bot.id.to_storage()],
                    )
                    .unwrap();
            }
            "home" => {
                repo.connection
                    .execute(
                        "UPDATE bots SET home_workspace_id=?1 WHERE bot_id=?2",
                        params![WorkspaceId::new().to_storage(), bot.id.to_storage()],
                    )
                    .unwrap();
            }
            "lifecycle" => {
                repo.connection
                    .execute(
                        "UPDATE bots SET lifecycle='archived' WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            "role" => {
                repo.connection
                    .execute(
                        "UPDATE bots SET role_document_id=?1 WHERE bot_id=?2",
                        params![c.identity.candidate.to_storage(), bot.id.to_storage()],
                    )
                    .unwrap();
            }
            "knowledge" => {
                let mut input = knowledge(&bot, owner);
                input.pages[0].document = c.identity.candidate;
                repo.set_bot_knowledge(&input, HASH, 3).unwrap();
            }
            "evidence" => evidence.live_edit_revision.push(4),
            _ => unreachable!(),
        }
        let revision: u64 = repo
            .connection
            .query_row(
                "SELECT revision FROM bots WHERE bot_id=?1",
                [bot.id.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            repo.bind_bot_memory(&evidence, c.observed(), 4).is_err(),
            "{disagreement}"
        );
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
        assert_eq!(
            repo.connection
                .query_row("SELECT state FROM bot_memory_creation_intents", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "prepared"
        );
        assert_eq!(
            repo.connection
                .query_row("SELECT count(*) FROM bot_memory_bindings", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            0
        );
    }
    let (mut repo, bot, owner) = fixture();
    let c = prepared(&mut repo, &bot, owner);
    let mut changed = bot.clone();
    changed.revision = 2;
    changed.config.name = "Human changed name".into();
    repo.save_bot(
        &changed,
        ProfileId::new(),
        BotRequestId::mint(),
        OTHER_HASH,
        Some(1),
    )
    .unwrap();
    repo.bind_bot_memory(&confirmation(&c), c.observed(), 4)
        .unwrap();
    let read = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(read.revision, 3);
    assert_eq!(read.config.name, changed.config.name);
}

#[test]
fn memory_pending_scan_is_owner_bound_and_keyset() {
    let (mut repo, bot, owner) = fixture();
    let mut identities = Vec::new();
    for i in 0..19 {
        let mut next = super::super::super::tests::fixture_bot();
        next.user_scope = owner.user_scope;
        next.owner_device = owner.device;
        repo.save_bot(&next, ProfileId::new(), BotRequestId::mint(), HASH, None)
            .unwrap();
        let c = repo
            .prepare_bot_memory_creation(owner, next.id, BotRequestId::mint(), 1, &seed(), HASH, i)
            .unwrap();
        identities.push(c);
    }
    let foreign = prepared(&mut repo, &bot, owner);
    repo.transition_bot_memory_creation(
        &foreign.identity,
        foreign.observed(),
        BotMemoryTransition::Abandon,
        1,
    )
    .unwrap();
    repo.bind_bot_memory(&confirmation(&identities[0]), identities[0].observed(), 20)
        .unwrap();
    let first = repo.pending_bot_memory(owner, None, 16).unwrap();
    assert_eq!(first.creations.len(), 16);
    let second = repo
        .pending_bot_memory(owner, first.next.as_ref(), 16)
        .unwrap();
    assert_eq!(second.creations.len(), 2);
    assert!(second.next.is_none());
    let ids = first
        .creations
        .iter()
        .chain(&second.creations)
        .map(|c| c.identity.bot)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 18);
    for other in [
        BotOwner {
            user_scope: UserScopeId::new(),
            ..owner
        },
        BotOwner {
            device: DeviceId::new(),
            ..owner
        },
    ] {
        assert!(
            repo.pending_bot_memory(other, None, 16)
                .unwrap()
                .creations
                .is_empty()
        );
        assert_eq!(
            repo.pending_bot_memory(other, first.next.as_ref(), 16),
            Err(BotError::Invalid)
        );
    }
    let plan:String=repo.connection.query_row("EXPLAIN QUERY PLAN SELECT request_id FROM bot_memory_creation_intents INDEXED BY bot_memory_creation_pending WHERE state IN ('prepared','unconfirmed') AND (updated_at_ms,request_id)>(1,'') ORDER BY updated_at_ms,request_id LIMIT 17",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("bot_memory_creation_pending"));
}
