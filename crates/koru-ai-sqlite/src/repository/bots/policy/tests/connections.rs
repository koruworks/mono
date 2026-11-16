// SPDX-License-Identifier: MPL-2.0
//! A Bot's connection grants (ai.db 25, M5): part of its policy, replaced as
//! a set by an access change that states them, and left alone by one that
//! does not.
use super::*;
use crate::repository::connections::tests::connect;
use koru_ai_core::connections::*;

/// One access change of `bot` to `desired`, prepared and acknowledged as the
/// worker does it: the barrier is built from the desired Bot's own policy.
fn change(repo: &mut Repository, desired: &Bot, profile: ProfileId, n: u8) -> BotAccessReceipt {
    let barrier = barrier_of(desired, n);
    let request = BotRequestId::mint();
    let receipt = repo
        .prepare_bot_access(
            desired,
            profile,
            request,
            &format!("{n:02x}").repeat(32),
            desired.revision - 1,
            &barrier,
        )
        .unwrap();
    assert!(!receipt.completed);
    repo.acknowledge_bot_access(
        owner(desired),
        desired.id,
        request,
        &BotPolicyBarrierReceipt {
            barrier,
            state: BotPolicyBarrierState::Effective,
        },
        desired.updated_at_ms,
    )
    .unwrap()
}
fn barrier_of(bot: &Bot, n: u8) -> BotPolicyBarrier {
    BotPolicyBarrier {
        key: koru_delegate_core::BotPolicyBarrierKey {
            owner: koru_delegate_core::BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            request: format!("12345678-1234-4234-8234-1234567890{n:02x}")
                .parse()
                .unwrap(),
        },
        config_revision: bot.revision,
        policy_revision: bot.policy_revision,
        policy: bot_native_policy(bot),
        effects_allowed: true,
    }
}
/// The next access change of the Bot as it is stored now.
fn next(repo: &Repository, bot: BotId, at: i64) -> Bot {
    let mut desired = repo.bot(bot).unwrap().unwrap();
    desired.revision += 1;
    desired.policy_revision += 1;
    desired.updated_at_ms = at;
    desired
}
fn granted_at(repo: &Repository, bot: BotId) -> Vec<(String, i64)> {
    repo.connection
        .prepare("SELECT connection_id,granted_at_ms FROM bot_connection_grants WHERE bot_id=?1 ORDER BY connection_id")
        .unwrap()
        .query_map([bot.to_storage()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn m5_grant_rows_an_access_change_that_omits_connections_removes_no_grant() {
    let (mut repo, bot, profile, _, _) = fixture();
    let first = connect(&mut repo, owner(&bot), 1);
    let second = connect(&mut repo, owner(&bot), 2);
    // Grant both.
    let mut desired = next(&repo, bot.id, 100);
    desired.connections = [first.read_grant(), second.read_grant()].into();
    let receipt = change(&mut repo, &desired, profile, 1);
    assert_eq!(receipt.bot.connections, desired.connections);
    // The barrier the delegate installs carries the grants.
    assert_eq!(
        receipt.barrier.unwrap().policy.connections,
        desired.connections
    );
    let stored = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(stored.connections, desired.connections);
    assert_eq!(bot_native_policy(&stored).connections, desired.connections);
    assert_eq!(
        repo.bot_policy_metadata(owner(&bot), bot.id)
            .unwrap()
            .policy(),
        bot_native_policy(&stored)
    );
    let given = granted_at(&repo, bot.id);
    assert_eq!(given.len(), 2);
    assert!(given.iter().all(|(_, at)| *at == 100));

    // A page-only change: the caller did not mention connections, so the
    // desired Bot keeps the set it read. Every grant stays, as it was given.
    let mut pages_only = next(&repo, bot.id, 200);
    pages_only.access = BotAccess::Default;
    assert_eq!(pages_only.connections, desired.connections);
    let receipt = change(&mut repo, &pages_only, profile, 2);
    assert_eq!(receipt.bot.connections, desired.connections);
    assert_eq!(granted_at(&repo, bot.id), given);
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().connections,
        desired.connections
    );

    // Present with one left out: Remove access. The kept grant keeps when it
    // was given, and repeating the same request changes nothing.
    let mut removed = next(&repo, bot.id, 300);
    removed.connections = [first.read_grant()].into();
    change(&mut repo, &removed, profile, 3);
    assert_eq!(
        granted_at(&repo, bot.id),
        vec![(first.id.to_storage(), 100)]
    );
    // Granting it again is a new grant, given now.
    let mut again = next(&repo, bot.id, 400);
    again.connections = [first.read_grant(), second.read_grant()].into();
    change(&mut repo, &again, profile, 4);
    let mut expected = vec![(first.id.to_storage(), 100), (second.id.to_storage(), 400)];
    expected.sort();
    assert_eq!(granted_at(&repo, bot.id), expected);
    // Present and empty removes them all.
    let mut none = next(&repo, bot.id, 500);
    none.connections.clear();
    let receipt = change(&mut repo, &none, profile, 5);
    assert!(granted_at(&repo, bot.id).is_empty());
    assert!(receipt.barrier.unwrap().policy.connections.is_empty());
    assert!(repo.bot(bot.id).unwrap().unwrap().connections.is_empty());
}

#[test]
fn m5_grant_rows_a_new_grant_needs_a_connected_connection_of_the_same_person() {
    let (mut repo, bot, profile, _, _) = fixture();
    let connected = connect(&mut repo, owner(&bot), 1);
    let foreign = connect(&mut repo, crate::repository::connections::tests::owner(), 2);
    let expired = connect(&mut repo, owner(&bot), 3);
    repo.connection_needs_sign_in(owner(&bot), expired.id, 50)
        .unwrap();
    let missing = ConnectionGrantRef {
        connection: ConnectionId::mint(),
        operation: PrivateOperationKind::ReadItem,
    };
    let receipts: i64 = repo
        .connection
        .query_row("SELECT count(*) FROM bot_command_receipts", [], |r| {
            r.get(0)
        })
        .unwrap();
    for refused in [foreign.read_grant(), expired.read_grant(), missing] {
        let mut desired = next(&repo, bot.id, 100);
        // A usable grant beside it does not carry the unusable one in.
        desired.connections = [connected.read_grant(), refused].into();
        let barrier = barrier_of(&desired, 1);
        assert_eq!(
            repo.prepare_bot_access(
                &desired,
                profile,
                BotRequestId::mint(),
                &"a".repeat(64),
                desired.revision - 1,
                &barrier
            ),
            Err(BotError::ConnectionUnavailable)
        );
        // Nothing of the change was kept: no grant, no revision, no receipt.
        assert!(granted_at(&repo, bot.id).is_empty());
        let current = repo.bot(bot.id).unwrap().unwrap();
        assert_eq!((current.revision, current.policy_revision), (1, 1));
        assert_eq!(
            repo.connection
                .query_row("SELECT count(*) FROM bot_command_receipts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            receipts
        );
    }
    // Seventeen grants are refused before anything is read.
    let mut many = next(&repo, bot.id, 100);
    many.connections = (0..17)
        .map(|_| ConnectionGrantRef {
            connection: ConnectionId::mint(),
            operation: PrivateOperationKind::ReadItem,
        })
        .collect();
    let barrier = barrier_of(&many, 1);
    assert_eq!(
        repo.prepare_bot_access(
            &many,
            profile,
            BotRequestId::mint(),
            &"a".repeat(64),
            1,
            &barrier
        ),
        Err(BotError::Invalid)
    );
}

#[test]
fn m5_grant_rows_a_kept_grant_survives_its_connection_and_authorizes_nothing_new() {
    let (mut repo, bot, profile, _, _) = fixture();
    let connection = connect(&mut repo, owner(&bot), 1);
    let mut desired = next(&repo, bot.id, 100);
    desired.connections = [connection.read_grant()].into();
    change(&mut repo, &desired, profile, 1);
    // Disconnect: the grant row is kept and points at a tombstone.
    let request = BotRequestId::mint();
    repo.begin_disconnect(owner(&bot), request, &"d".repeat(64), connection.id, 1, 150)
        .unwrap();
    repo.complete_disconnect(owner(&bot), request, VendorRevoke::Confirmed, 160)
        .unwrap();
    assert_eq!(granted_at(&repo, bot.id).len(), 1);
    // A later page-only change still keeps it: it was not mentioned.
    let pages_only = next(&repo, bot.id, 200);
    change(&mut repo, &pages_only, profile, 2);
    assert_eq!(
        granted_at(&repo, bot.id),
        vec![(connection.id.to_storage(), 100)]
    );
    // Connecting the same account again makes a new connection: the old
    // grant does not come back to life for it.
    let again = connect(&mut repo, owner(&bot), 1);
    assert_ne!(again.id, connection.id);
    let stored = repo.bot(bot.id).unwrap().unwrap();
    assert!(stored.connections.contains(&connection.read_grant()));
    assert!(!stored.connections.contains(&again.read_grant()));
    // Once removed, the tombstone cannot be granted again.
    let mut removed = next(&repo, bot.id, 300);
    removed.connections.clear();
    change(&mut repo, &removed, profile, 3);
    let mut back = next(&repo, bot.id, 400);
    back.connections = [connection.read_grant()].into();
    let barrier = barrier_of(&back, 4);
    assert_eq!(
        repo.prepare_bot_access(
            &back,
            profile,
            BotRequestId::mint(),
            &"b".repeat(64),
            back.revision - 1,
            &barrier
        ),
        Err(BotError::ConnectionUnavailable)
    );
}

#[test]
fn m5_grant_rows_only_an_access_change_moves_grants_and_stored_bots_keep_their_bytes() {
    let (mut repo, bot, profile, request, barrier) = fixture();
    // The fixture's own access change, which knows nothing of connections,
    // stores a Bot and a barrier with no such field: the bytes of ai.db 24.
    repo.prepare_bot_access(&bot, profile, request, &"a".repeat(64), 1, &barrier)
        .unwrap();
    let stored: String = repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [request.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains("connections"), "{stored}");
    repo.acknowledge_bot_access(
        owner(&bot),
        bot.id,
        request,
        &BotPolicyBarrierReceipt {
            barrier,
            state: BotPolicyBarrierState::Effective,
        },
        2,
    )
    .unwrap();
    let connection = connect(&mut repo, owner(&bot), 1);
    // A configuration change cannot carry a grant in.
    let mut renamed = repo.bot(bot.id).unwrap().unwrap();
    renamed.revision += 1;
    renamed.config.name = "renamed".into();
    renamed.connections = [connection.read_grant()].into();
    assert_eq!(
        repo.save_bot(
            &renamed,
            profile,
            BotRequestId::mint(),
            "rename",
            Some(renamed.revision - 1)
        ),
        Err(BotError::Invalid)
    );
    // A Bot is not created holding one.
    let mut fresh = crate::repository::bots::tests::fixture_bot();
    fresh.user_scope = bot.user_scope;
    fresh.owner_device = bot.owner_device;
    fresh.connections = [connection.read_grant()].into();
    assert_eq!(
        repo.save_bot(&fresh, profile, BotRequestId::mint(), "create", None),
        Err(BotError::Invalid)
    );
    assert!(granted_at(&repo, bot.id).is_empty());
    // With a grant, the stored Bot and barrier carry it, and a configuration
    // change afterwards leaves it where it is.
    let mut desired = next(&repo, bot.id, 100);
    desired.connections = [connection.read_grant()].into();
    let receipt = change(&mut repo, &desired, profile, 1);
    let stored: String = repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [receipt.request.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored.matches("\"connections\"").count(), 2, "{stored}");
    let mut renamed = repo.bot(bot.id).unwrap().unwrap();
    renamed.revision += 1;
    renamed.config.name = "renamed".into();
    renamed.updated_at_ms = 500;
    let saved = repo
        .save_bot(
            &renamed,
            profile,
            BotRequestId::mint(),
            "rename",
            Some(renamed.revision - 1),
        )
        .unwrap();
    assert_eq!(saved.connections, desired.connections);
    assert_eq!(
        granted_at(&repo, bot.id),
        vec![(connection.id.to_storage(), 100)]
    );
}

#[test]
fn m5_grant_rows_the_routine_source_reads_the_bots_grants_and_gives_a_routine_none() {
    let (mut repo, bot, profile, _, _) = fixture();
    let connection = connect(&mut repo, owner(&bot), 1);
    let mut desired = next(&repo, bot.id, 100);
    desired.connections = [connection.read_grant()].into();
    change(&mut repo, &desired, profile, 1);
    let source = repo
        .routine_creation_source_for_test(owner(&bot), bot.id)
        .unwrap();
    // The Bot's own access now, grants included.
    assert_eq!(source.policy, bot_native_policy(&desired));
    assert!(source.policy.valid());
}
