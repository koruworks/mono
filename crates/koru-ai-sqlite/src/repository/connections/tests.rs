// SPDX-License-Identifier: MPL-2.0
//! The connection writer on an in-memory ai.db. No vendor, no credential
//! store, no worker.
use super::*;

pub(crate) fn owner() -> BotOwner {
    BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
    }
}
fn fingerprint(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}
fn picked(n: u8) -> PickedConnectionItem {
    PickedConnectionItem {
        item_ref: format!("MARK-REF-{n}"),
        item_ref_digest: [n; 32],
        identity_digest: [n.wrapping_add(100); 32],
        title: format!("MARK-TITLE-{n}"),
    }
}
pub(crate) fn account(n: u8) -> ConnectedAccount {
    ConnectedAccount {
        provider: ProviderToken::parse(vendors::QUALIFIED[0]).unwrap(),
        service_origin: "https://vendor.example".into(),
        name: format!("Name {n}"),
        account_subject_digest: [n; 32],
        account_label: format!("MARK-LABEL-{n}@example.test"),
        scopes: vec!["scope.read".into()],
        credential_slot_digest: [n.wrapping_add(50); 32],
        items: vec![picked(1), picked(2)],
    }
}
/// A connected connection, through the two writer calls.
pub(crate) fn connect(repo: &mut Repository, owner: BotOwner, n: u8) -> Connection {
    let request = BotRequestId::mint();
    let planned = ConnectionId::mint();
    let account = account(n);
    repo.begin_connect(owner, request, &fingerprint(n), planned, &account.name, 10)
        .unwrap();
    let connection = repo.complete_connect(owner, request, &account, 11).unwrap();
    assert_eq!(connection.id, planned);
    connection
}
fn count(repo: &Repository, table: &str) -> i64 {
    repo.connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn m5_connection_receipts_connect_is_planned_replayed_and_completed_once() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let (request, planned) = (BotRequestId::mint(), ConnectionId::mint());
    let account = account(1);
    let begun = repo
        .begin_connect(owner, request, &fingerprint(1), planned, &account.name, 10)
        .unwrap();
    let ConnectionCommandAdmission::Prepared(receipt) = &begun else {
        panic!("{begun:?}")
    };
    assert_eq!(receipt.planned_connection, Some(planned));
    assert_eq!(receipt.connection, None);
    assert_eq!(receipt.stage, ConnectionReceiptStage::Prepared);
    // The planned id names the slot before any connection row exists.
    assert_eq!(count(&repo, "connections"), 0);
    assert_eq!(repo.connection_by_id(owner, planned).unwrap(), None);
    // The exact retry returns what was retained; another fingerprint, or
    // another command under the same request, is a conflict.
    assert_eq!(
        repo.begin_connect(owner, request, &fingerprint(1), planned, &account.name, 99)
            .unwrap(),
        ConnectionCommandAdmission::Retained(receipt.clone())
    );
    assert_eq!(
        repo.begin_connect(owner, request, &fingerprint(2), planned, &account.name, 10),
        Err(ConnectionError::Conflict)
    );
    assert_eq!(
        repo.begin_disconnect(owner, request, &fingerprint(1), planned, 1, 10)
            .map(|_| ()),
        Err(ConnectionError::Conflict)
    );
    // One connect at a time per person; another person is not held.
    assert_eq!(
        repo.begin_connect(
            owner,
            BotRequestId::mint(),
            &fingerprint(3),
            ConnectionId::mint(),
            "Other",
            10
        ),
        Err(ConnectionError::Busy)
    );
    // A planned id is used once.
    let other = self::owner();
    assert_eq!(
        repo.begin_connect(
            other,
            BotRequestId::mint(),
            &fingerprint(3),
            planned,
            "Other",
            10
        ),
        Err(ConnectionError::Conflict)
    );
    assert!(matches!(
        repo.begin_connect(
            other,
            BotRequestId::mint(),
            &fingerprint(3),
            ConnectionId::mint(),
            "Other",
            10
        )
        .unwrap(),
        ConnectionCommandAdmission::Prepared(_)
    ));
    assert_eq!(repo.prepared_connection_commands(0, 64).unwrap().len(), 2);

    let connection = repo.complete_connect(owner, request, &account, 20).unwrap();
    assert_eq!(connection.id, planned);
    assert_eq!(connection.owner, owner);
    assert_eq!(connection.state, ConnectionState::Connected);
    assert_eq!(connection.revision, 1);
    assert_eq!(connection.name, account.name);
    assert_eq!(connection.account_label, account.account_label);
    assert_eq!(connection.scopes, account.scopes);
    assert_eq!(connection.vendor_revoke, None);
    assert_eq!(
        (connection.connected_at_ms, connection.refreshed_at_ms),
        (20, None)
    );
    let items = repo.connection_items(owner, planned).unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|i| i.state == ConnectionItemState::Available
                && i.connection == planned
                && i.changed_at_ms.is_none())
    );
    let done = repo.connection_receipt(owner, request).unwrap().unwrap();
    assert_eq!(done.stage, ConnectionReceiptStage::Completed);
    assert_eq!(
        (done.connection, done.result_revision),
        (Some(planned), Some(1))
    );
    // Completing again, and beginning again, return the same things.
    assert_eq!(
        repo.complete_connect(owner, request, &account, 30).unwrap(),
        connection
    );
    assert_eq!(
        repo.begin_connect(owner, request, &fingerprint(1), planned, &account.name, 30)
            .unwrap(),
        ConnectionCommandAdmission::Retained(done.clone())
    );
    assert_eq!(count(&repo, "connections"), 1);
    assert_eq!(count(&repo, "connection_items"), 2);
    // A finished command cannot be failed.
    assert_eq!(
        repo.fail_connection_command(owner, request, ConnectionFailure::Canceled, 40),
        Err(ConnectionError::Conflict)
    );
    // Another person cannot read any of it.
    assert_eq!(
        repo.connection_by_id(other, planned),
        Err(ConnectionError::Forbidden)
    );
    assert_eq!(
        repo.connection_items(other, planned),
        Err(ConnectionError::Forbidden)
    );
    assert_eq!(repo.connection_receipt(other, request).unwrap(), None);
    assert!(repo.live_connections(other).unwrap().is_empty());
    assert_eq!(repo.live_connections(owner).unwrap(), vec![connection]);
}

#[test]
fn m5_connection_receipts_a_failed_connect_leaves_no_row_and_frees_the_person() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    for (n, failure) in [
        ConnectionFailure::Canceled,
        ConnectionFailure::TimedOut,
        ConnectionFailure::Interrupted,
        ConnectionFailure::ConsentDenied,
        ConnectionFailure::VendorRejected,
        ConnectionFailure::VendorUnavailable,
        ConnectionFailure::CredentialStoreUnavailable,
        ConnectionFailure::NotConfigured,
    ]
    .into_iter()
    .enumerate()
    {
        let request = BotRequestId::mint();
        repo.begin_connect(
            owner,
            request,
            &fingerprint(n as u8),
            ConnectionId::mint(),
            "Work",
            10,
        )
        .unwrap();
        let failed = repo
            .fail_connection_command(owner, request, failure, 20)
            .unwrap();
        assert_eq!(failed.stage, ConnectionReceiptStage::Failed);
        assert_eq!(failed.failure, Some(failure));
        assert_eq!((failed.connection, failed.result_revision), (None, None));
        // The same reason again is the same receipt; another is a conflict,
        // and so is completing it.
        assert_eq!(
            repo.fail_connection_command(owner, request, failure, 30)
                .unwrap(),
            failed
        );
        assert_eq!(
            repo.fail_connection_command(owner, request, ConnectionFailure::ItemLimit, 30),
            Err(ConnectionError::Conflict)
        );
        assert_eq!(
            repo.complete_connect(owner, request, &account(1), 30),
            Err(ConnectionError::Conflict)
        );
    }
    assert_eq!(count(&repo, "connections"), 0);
    assert!(repo.prepared_connection_commands(0, 64).unwrap().is_empty());
    assert_eq!(
        repo.fail_connection_command(owner, BotRequestId::mint(), ConnectionFailure::Canceled, 1),
        Err(ConnectionError::NotFound)
    );
}

#[test]
fn m5_connection_rows_an_account_and_a_name_are_live_once_and_eight_is_the_limit() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let first = connect(&mut repo, owner, 1);
    // The same account again: nothing is written, and the caller records why.
    let request = BotRequestId::mint();
    let mut again = account(1);
    again.name = "Another name".into();
    repo.begin_connect(
        owner,
        request,
        &fingerprint(9),
        ConnectionId::mint(),
        &again.name,
        10,
    )
    .unwrap();
    assert_eq!(
        repo.complete_connect(owner, request, &again, 20),
        Err(ConnectionError::AccountInUse)
    );
    assert_eq!(count(&repo, "connections"), 1);
    assert_eq!(count(&repo, "connection_items"), 2);
    assert_eq!(
        repo.connection_receipt(owner, request)
            .unwrap()
            .unwrap()
            .stage,
        ConnectionReceiptStage::Prepared
    );
    repo.fail_connection_command(owner, request, ConnectionFailure::AccountInUse, 21)
        .unwrap();
    // The same name is refused before a receipt is written.
    assert_eq!(
        repo.begin_connect(
            owner,
            BotRequestId::mint(),
            &fingerprint(9),
            ConnectionId::mint(),
            &first.name,
            10
        ),
        Err(ConnectionError::NameInUse)
    );
    // Another person may hold the same account and the same name.
    let other = self::owner();
    connect(&mut repo, other, 1);
    for n in 2..=8 {
        connect(&mut repo, owner, n);
    }
    assert_eq!(
        repo.live_connections(owner).unwrap().len(),
        CONNECTION_LIMIT
    );
    let receipts = count(&repo, "connection_command_receipts");
    assert_eq!(
        repo.begin_connect(
            owner,
            BotRequestId::mint(),
            &fingerprint(9),
            ConnectionId::mint(),
            "Ninth",
            10
        ),
        Err(ConnectionError::ConnectionLimit)
    );
    assert_eq!(count(&repo, "connection_command_receipts"), receipts);
    // Malformed input never reaches a table.
    for (name, print) in [
        ("", fingerprint(9)),
        (" x", fingerprint(9)),
        ("ok", "zz".repeat(32)),
    ] {
        assert_eq!(
            repo.begin_connect(
                other,
                BotRequestId::mint(),
                &print,
                ConnectionId::mint(),
                name,
                10
            ),
            Err(ConnectionError::Invalid)
        );
    }
}

fn expire(repo: &mut Repository, owner: BotOwner, connection: &Connection) -> Connection {
    let expired = repo
        .connection_needs_sign_in(owner, connection.id, 30)
        .unwrap();
    assert_eq!(expired.state, ConnectionState::NeedsSignIn);
    assert_eq!(expired.revision, connection.revision + 1);
    expired
}

#[test]
fn m5_connection_receipts_sign_in_again_restores_the_same_account_only() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let connected = connect(&mut repo, owner, 1);
    // Signing in again is for a connection that needs it.
    assert_eq!(
        repo.begin_sign_in_again(
            owner,
            BotRequestId::mint(),
            &fingerprint(2),
            connected.id,
            1,
            20
        ),
        Err(ConnectionError::Unavailable)
    );
    let expired = expire(&mut repo, owner, &connected);
    // Marking it again changes nothing.
    assert_eq!(
        repo.connection_needs_sign_in(owner, connected.id, 31)
            .unwrap(),
        expired
    );
    assert_eq!(
        repo.begin_sign_in_again(
            owner,
            BotRequestId::mint(),
            &fingerprint(2),
            connected.id,
            1,
            40
        ),
        Err(ConnectionError::RevisionConflict)
    );
    let request = BotRequestId::mint();
    let begun = repo
        .begin_sign_in_again(owner, request, &fingerprint(2), connected.id, 2, 40)
        .unwrap();
    // It is a connect receipt whose planned connection is the existing one.
    let receipt = begun.receipt();
    assert_eq!(receipt.kind, ConnectionCommandKind::Connect);
    assert_eq!(receipt.planned_connection, Some(connected.id));
    assert_eq!(receipt.connection, Some(connected.id));
    assert_eq!(receipt.expected_revision, Some(2));
    // While it is open, nothing else of this connection or person begins.
    assert_eq!(
        repo.begin_disconnect(
            owner,
            BotRequestId::mint(),
            &fingerprint(3),
            connected.id,
            2,
            41
        )
        .map(|_| ()),
        Err(ConnectionError::Busy)
    );
    assert_eq!(
        repo.begin_connect(
            owner,
            BotRequestId::mint(),
            &fingerprint(3),
            ConnectionId::mint(),
            "New",
            41
        ),
        Err(ConnectionError::Busy)
    );
    // Another account: nothing is written and the connection is unchanged.
    let mut wrong = account(1);
    wrong.account_subject_digest = [77; 32];
    assert_eq!(
        repo.complete_sign_in_again(owner, request, &wrong, 50),
        Err(ConnectionError::WrongAccount)
    );
    assert_eq!(
        repo.connection_by_id(owner, connected.id).unwrap().unwrap(),
        expired
    );
    // A fresh connect's completion does not complete a sign-in again.
    assert_eq!(
        repo.complete_connect(owner, request, &account(1), 50),
        Err(ConnectionError::Conflict)
    );
    let failed = repo
        .fail_connection_command(owner, request, ConnectionFailure::WrongAccount, 51)
        .unwrap();
    assert_eq!(failed.failure, Some(ConnectionFailure::WrongAccount));
    assert_eq!(
        repo.connection_by_id(owner, connected.id).unwrap().unwrap(),
        expired
    );

    // The right account, with a refreshed label and one more picked item.
    let request = BotRequestId::mint();
    repo.begin_sign_in_again(owner, request, &fingerprint(4), connected.id, 2, 60)
        .unwrap();
    let mut same = account(1);
    same.account_label = "MARK-LABEL-renamed@example.test".into();
    same.items = vec![picked(3)];
    let restored = repo
        .complete_sign_in_again(owner, request, &same, 70)
        .unwrap();
    assert_eq!(restored.state, ConnectionState::Connected);
    assert_eq!(restored.revision, 3);
    assert_eq!(restored.account_label, same.account_label);
    assert_eq!(restored.refreshed_at_ms, Some(70));
    assert_eq!(restored.id, connected.id);
    assert_eq!(restored.name, connected.name);
    assert_eq!(repo.connection_items(owner, connected.id).unwrap().len(), 3);
    assert_eq!(
        repo.complete_sign_in_again(owner, request, &same, 80)
            .unwrap(),
        restored
    );
    let done = repo.connection_receipt(owner, request).unwrap().unwrap();
    assert_eq!(done.result_revision, Some(3));
    // Recording a refresh is not a state change.
    repo.connection_refreshed(owner, connected.id, 90).unwrap();
    let refreshed = repo.connection_by_id(owner, connected.id).unwrap().unwrap();
    assert_eq!(
        (refreshed.revision, refreshed.refreshed_at_ms),
        (3, Some(90))
    );
}

#[test]
fn m5_connection_receipts_add_items_adds_restores_and_bounds() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let connection = connect(&mut repo, owner, 1);
    let first = repo
        .connection_items(owner, connection.id)
        .unwrap()
        .into_iter()
        .find(|item| item.item_ref_digest == [1; 32])
        .unwrap();
    // A read found another identity: the item is not read again.
    let changed = repo.connection_item_changed(owner, first.id, 20).unwrap();
    assert_eq!(changed.revision, 2);
    let (_, item) = repo.connection_item(owner, first.id).unwrap().unwrap();
    assert_eq!(item.state, ConnectionItemState::Changed);
    assert_eq!(item.changed_at_ms, Some(20));
    assert_eq!(
        repo.connection_item_changed(owner, first.id, 21).unwrap(),
        changed
    );
    // By reference, across the person's live connections.
    let provider = connection.provider;
    assert_eq!(
        repo.connection_items_by_ref(owner, provider, &first.item_ref_digest)
            .unwrap()
            .len(),
        1
    );
    assert!(
        repo.connection_items_by_ref(self::owner(), provider, &first.item_ref_digest)
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        repo.begin_add_items(
            owner,
            BotRequestId::mint(),
            &fingerprint(5),
            connection.id,
            1,
            30
        ),
        Err(ConnectionError::RevisionConflict)
    );
    let request = BotRequestId::mint();
    repo.begin_add_items(owner, request, &fingerprint(5), connection.id, 2, 30)
        .unwrap();
    // Picking the changed item again is the fresh grant: its new identity.
    let mut again = picked(1);
    again.identity_digest = [200; 32];
    let added = repo
        .complete_add_items(owner, request, &[again.clone(), picked(7)], 40)
        .unwrap();
    assert_eq!(added.revision, 3);
    let (_, item) = repo.connection_item(owner, first.id).unwrap().unwrap();
    assert_eq!(item.state, ConnectionItemState::Available);
    assert_eq!(item.identity_digest, [200; 32]);
    assert_eq!(item.changed_at_ms, None);
    assert_eq!(item.item_ref, first.item_ref);
    assert_eq!(
        repo.connection_items(owner, connection.id).unwrap().len(),
        3
    );
    assert_eq!(
        repo.complete_add_items(owner, request, &[picked(8)], 50)
            .unwrap(),
        added
    );
    assert_eq!(
        repo.connection_items(owner, connection.id).unwrap().len(),
        3
    );

    // The bound: 256 items per connection. Nothing of the trip is kept.
    let request = BotRequestId::mint();
    repo.begin_add_items(owner, request, &fingerprint(6), connection.id, 3, 60)
        .unwrap();
    let many: Vec<_> = (0..254u16)
        .map(|n| {
            let mut digest = [9; 32];
            digest[..2].copy_from_slice(&n.to_be_bytes());
            PickedConnectionItem {
                item_ref: format!("many-{n}"),
                item_ref_digest: digest,
                identity_digest: [1; 32],
                title: "t".into(),
            }
        })
        .collect();
    assert_eq!(
        repo.complete_add_items(owner, request, &many, 70),
        Err(ConnectionError::ItemLimit)
    );
    assert_eq!(
        repo.connection_items(owner, connection.id).unwrap().len(),
        3
    );
    let failed = repo
        .fail_connection_command(owner, request, ConnectionFailure::ItemLimit, 71)
        .unwrap();
    assert_eq!(failed.failure, Some(ConnectionFailure::ItemLimit));
    assert_eq!(
        repo.connection_by_id(owner, connection.id)
            .unwrap()
            .unwrap()
            .revision,
        3
    );
}

#[test]
fn m5_connection_receipts_disconnect_fences_first_and_the_tombstone_is_final() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let connection = connect(&mut repo, owner, 1);
    assert_eq!(
        repo.begin_disconnect(
            owner,
            BotRequestId::mint(),
            &fingerprint(1),
            connection.id,
            9,
            20
        )
        .map(|_| ()),
        Err(ConnectionError::RevisionConflict)
    );
    let request = BotRequestId::mint();
    let (begun, fenced) = repo
        .begin_disconnect(owner, request, &fingerprint(1), connection.id, 1, 20)
        .unwrap();
    // The fence and the receipt are one commit: from here nothing is read.
    assert!(matches!(begun, ConnectionCommandAdmission::Prepared(_)));
    assert_eq!(fenced.state, ConnectionState::Disconnecting);
    assert_eq!(fenced.revision, 2);
    assert_eq!(
        repo.begin_disconnect(owner, request, &fingerprint(1), connection.id, 1, 99)
            .unwrap(),
        (
            ConnectionCommandAdmission::Retained(begun.receipt().clone()),
            fenced.clone()
        )
    );
    // The credential store could not be cleared: the command fails, the
    // connection stays fenced, and the command can be repeated.
    repo.fail_connection_command(
        owner,
        request,
        ConnectionFailure::CredentialStoreUnavailable,
        30,
    )
    .unwrap();
    assert_eq!(
        repo.connection_by_id(owner, connection.id)
            .unwrap()
            .unwrap(),
        fenced
    );
    assert_eq!(
        repo.connection_needs_sign_in(owner, connection.id, 31)
            .unwrap(),
        fenced
    );
    assert_eq!(
        repo.begin_add_items(
            owner,
            BotRequestId::mint(),
            &fingerprint(2),
            connection.id,
            2,
            32
        ),
        Err(ConnectionError::Unavailable)
    );
    let request = BotRequestId::mint();
    let (_, still) = repo
        .begin_disconnect(owner, request, &fingerprint(2), connection.id, 2, 40)
        .unwrap();
    assert_eq!(still, fenced);
    let gone = repo
        .complete_disconnect(owner, request, VendorRevoke::Unconfirmed, 50)
        .unwrap();
    assert_eq!(gone.state, ConnectionState::Disconnected);
    assert_eq!(gone.vendor_revoke, Some(VendorRevoke::Unconfirmed));
    assert_eq!((gone.revision, gone.disconnected_at_ms), (3, Some(50)));
    assert_eq!(
        repo.complete_disconnect(owner, request, VendorRevoke::Confirmed, 60)
            .unwrap(),
        gone
    );
    let done = repo.connection_receipt(owner, request).unwrap().unwrap();
    assert_eq!(done.result_revision, Some(3));
    // The tombstone is read, never listed as live, and admits no command.
    assert!(repo.live_connections(owner).unwrap().is_empty());
    assert_eq!(
        repo.connection_by_id(owner, connection.id)
            .unwrap()
            .unwrap(),
        gone
    );
    assert_eq!(
        repo.begin_disconnect(
            owner,
            BotRequestId::mint(),
            &fingerprint(3),
            connection.id,
            3,
            70
        )
        .map(|_| ()),
        Err(ConnectionError::Unavailable)
    );
    assert_eq!(
        repo.begin_sign_in_again(
            owner,
            BotRequestId::mint(),
            &fingerprint(3),
            connection.id,
            3,
            70
        ),
        Err(ConnectionError::Unavailable)
    );
    assert_eq!(
        repo.connection_needs_sign_in(owner, connection.id, 70)
            .unwrap(),
        gone
    );
    // The items are kept with their connection.
    assert_eq!(
        repo.connection_items(owner, connection.id).unwrap().len(),
        2
    );
    // The same account connects again as a new connection with a new id.
    let again = connect(&mut repo, owner, 1);
    assert_ne!(again.id, connection.id);
    assert_eq!(again.revision, 1);
    assert_eq!(repo.live_connections(owner).unwrap(), vec![again]);
}

#[test]
fn m5_connection_receipts_prepared_commands_are_paged_for_recovery() {
    let mut repo = Repository::in_memory().unwrap();
    let (first, second) = (owner(), owner());
    let connected = connect(&mut repo, first, 1);
    let expired = expire(&mut repo, first, &connected);
    let other = connect(&mut repo, second, 2);
    // An interrupted fresh connect, an interrupted sign-in again, and an
    // interrupted disconnect, of two people.
    let fresh = BotRequestId::mint();
    let planned = ConnectionId::mint();
    repo.begin_connect(second, fresh, &fingerprint(1), planned, "Fresh", 40)
        .unwrap();
    let again = BotRequestId::mint();
    repo.begin_sign_in_again(
        first,
        again,
        &fingerprint(2),
        connected.id,
        expired.revision,
        41,
    )
    .unwrap();
    let disconnect = BotRequestId::mint();
    repo.begin_disconnect(second, disconnect, &fingerprint(3), other.id, 1, 42)
        .unwrap();
    let page = repo.prepared_connection_commands(0, 2).unwrap();
    assert_eq!(page.len(), 2);
    let rest = repo.prepared_connection_commands(page[1].0, 2).unwrap();
    assert_eq!(rest.len(), 1);
    let all: Vec<_> = page.into_iter().chain(rest).map(|(_, r)| r).collect();
    let by = |request| all.iter().find(|r| r.request == request).unwrap();
    // What the service needs to decide: which slot to clear and which not.
    assert_eq!(by(fresh).planned_connection, Some(planned));
    assert_eq!(by(fresh).connection, None);
    assert_eq!(by(fresh).owner, second);
    // A sign-in again plans the existing connection: its slot is left alone.
    assert_eq!(by(again).planned_connection, Some(connected.id));
    assert_eq!(by(again).connection, Some(connected.id));
    assert_eq!(by(disconnect).kind, ConnectionCommandKind::Disconnect);
    assert_eq!(by(disconnect).connection, Some(other.id));
    // Failing the connects as interrupted empties the page of them.
    for (owner, request) in [(second, fresh), (first, again)] {
        repo.fail_connection_command(owner, request, ConnectionFailure::Interrupted, 50)
            .unwrap();
    }
    let left = repo.prepared_connection_commands(0, 64).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].1.request, disconnect);
    assert_eq!(
        repo.connection_by_id(first, connected.id).unwrap().unwrap(),
        expired
    );
}

#[test]
fn m5_connection_rows_private_text_is_stored_only_in_its_own_columns() {
    let mut repo = Repository::in_memory().unwrap();
    let owner = owner();
    let connection = connect(&mut repo, owner, 1);
    let request = BotRequestId::mint();
    repo.begin_disconnect(owner, request, &fingerprint(4), connection.id, 1, 20)
        .unwrap();
    repo.complete_disconnect(owner, request, VendorRevoke::Confirmed, 30)
        .unwrap();
    // Every text cell of the four tables, by column.
    let mut marked = std::collections::BTreeSet::new();
    for table in [
        "connections",
        "connection_items",
        "bot_connection_grants",
        "connection_command_receipts",
    ] {
        let columns: Vec<String> = repo
            .connection
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([table], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for column in columns {
            let hits: i64 = repo
                .connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM {table} WHERE CAST({column} AS TEXT) LIKE '%MARK%'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            if hits > 0 {
                marked.insert(format!("{table}.{column}"));
            }
        }
    }
    // The label, the titles and the vendor's references, and nowhere else:
    // no receipt holds any of them.
    assert_eq!(
        marked.into_iter().collect::<Vec<_>>(),
        [
            "connection_items.item_ref",
            "connection_items.title",
            "connections.account_label"
        ]
    );
    // Nothing printed about a row or a refusal carries them either.
    let printed = format!(
        "{:?} {:?} {:?}",
        repo.connection_by_id(owner, connection.id).unwrap(),
        repo.connection_items(owner, connection.id).unwrap(),
        repo.connection_receipt(owner, request).unwrap()
    );
    assert!(!printed.contains("MARK"), "{printed}");
}
