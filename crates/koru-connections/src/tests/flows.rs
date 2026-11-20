// SPDX-License-Identifier: MPL-2.0
//! Connect, sign in again, choose more files, disconnect, and what the start
//! of the service finishes.
use super::*;
use koru_oauth_credentials::CredentialStore;

fn stores(h: &Harness) -> usize {
    h.credentials
        .touched()
        .iter()
        .filter(|(operation, _)| *operation == "store")
        .count()
}
fn live(h: &Harness) -> Vec<Connection> {
    h.store.with(|r| r.live_connections(h.owner)).unwrap()
}

#[test]
fn m5_connect_flow_connects_stores_the_credential_in_its_own_slot_and_replays() {
    let h = Harness::new();
    let (request, consent) = h.begin("Work");
    // The exact retry returns the same consent and asks the vendor nothing.
    match h
        .service
        .begin_connect(h.owner, request, "Work", REDIRECT)
        .unwrap()
    {
        TripStart::Consent { consent: again, .. } => assert_eq!(again, consent),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.vendor.calls(FakeCall::AuthorizationRequest), 1);
    // The same request with another name is another command.
    assert_eq!(
        h.service
            .begin_connect(h.owner, request, "Other", REDIRECT)
            .unwrap_err(),
        ConnectionError::Conflict
    );
    // One connect at a time per person.
    assert_eq!(
        h.service
            .begin_connect(h.owner, BotRequestId::mint(), "Second", REDIRECT)
            .unwrap_err(),
        ConnectionError::Busy
    );
    assert!(live(&h).is_empty());
    assert_eq!(stores(&h), 0);

    // A callback that is not this trip's is refused; the trip keeps waiting.
    let stray = AuthorizationCallback {
        state: "not-the-state".into(),
        code: None,
    };
    assert_eq!(
        h.service
            .complete_trip(h.owner, request, stray)
            .unwrap_err(),
        ConnectionError::Invalid
    );
    let stranger = BotOwner {
        user_scope: UserScopeId::new(),
        device: h.owner.device,
    };
    assert_eq!(
        h.service
            .complete_trip(
                stranger,
                request,
                h.vendor.approve(&consent, SUBJECT_A, &["doc-1"])
            )
            .unwrap_err(),
        ConnectionError::Forbidden
    );

    let receipt = h
        .service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent, SUBJECT_A, &["doc-1", "doc-2"]),
        )
        .unwrap();
    assert_eq!(receipt.stage, ConnectionReceiptStage::Completed);
    let id = receipt.connection.unwrap();
    assert_eq!(receipt.planned_connection, Some(id));
    let connection = h.connection(id);
    assert_eq!(connection.state, ConnectionState::Connected);
    assert_eq!(connection.revision, 1);
    assert_eq!(connection.name, "Work");
    assert_eq!(connection.account_label, "MARK-LABEL-a@example.test");
    assert_eq!(connection.service_origin, FAKE_ORIGIN);
    assert_eq!(
        connection.account_subject_digest,
        account_subject_digest(connection.provider, SUBJECT_A)
    );
    let items = h.items(id);
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|i| i.state == ConnectionItemState::Available)
    );
    assert_eq!(h.item(id, "doc-1").title, "MARK-TITLE-1");

    // The refresh token, and only it, is in the connection's own slot, and
    // ai.db holds the digest of that slot's name.
    let slot = h.slot(id);
    assert_eq!(
        connection.credential_slot_digest,
        credential_slot_digest(&slot)
    );
    let stored = h.stored(id).unwrap();
    assert!(stored.starts_with("SECRET-refresh-"), "{stored}");
    assert_eq!(
        h.credentials.touched(),
        vec![("store", slot.account().to_owned())]
    );
    assert_eq!(h.changes.kinds(), vec![ConnectionChange::Connected]);
    assert_eq!(h.changes.all()[0].connection, id);
    assert_eq!(h.changes.all()[0].vendor_revoke, None);

    // Completing again and beginning again answer what was retained.
    assert_eq!(
        h.service
            .complete_trip(h.owner, request, h.vendor.deny(&consent))
            .unwrap(),
        receipt
    );
    match h
        .service
        .begin_connect(h.owner, request, "Work", REDIRECT)
        .unwrap()
    {
        TripStart::Finished(again) => assert_eq!(again, receipt),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.vendor.calls(FakeCall::CompleteAuthorization), 1);
    assert_eq!(live(&h).len(), 1);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
}

#[test]
fn m5_connect_flow_cancel_denial_timeout_and_vendor_failures_connect_nothing() {
    let h = Harness::new();
    let failed = |request: BotRequestId, failure: ConnectionFailure| {
        let receipt = h.receipt(request);
        assert_eq!(receipt.stage, ConnectionReceiptStage::Failed, "{failure:?}");
        assert_eq!(receipt.failure, Some(failure));
        assert_eq!(receipt.connection, None);
        assert!(live(&h).is_empty());
        // Nothing was ever stored, so there is nothing to clear or revoke.
        assert_eq!(stores(&h), 0);
        assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    };
    // Canceled by the person.
    let (request, consent) = h.begin("Work");
    let canceled = h.service.cancel_trip(h.owner, request).unwrap();
    assert_eq!(canceled.failure, Some(ConnectionFailure::Canceled));
    failed(request, ConnectionFailure::Canceled);
    // A late callback finds the retained failure and connects nothing.
    assert_eq!(
        h.service
            .complete_trip(
                h.owner,
                request,
                h.vendor.approve(&consent, SUBJECT_A, &["doc-1"])
            )
            .unwrap(),
        canceled
    );
    failed(request, ConnectionFailure::Canceled);

    // The person did not consent.
    let (request, consent) = h.begin("Work");
    h.service
        .complete_trip(h.owner, request, h.vendor.deny(&consent))
        .unwrap();
    failed(request, ConnectionFailure::ConsentDenied);

    // Five minutes pass: the service's upkeep ends the trip.
    let (request, consent) = h.begin("Work");
    h.clock.advance(CONSENT_WAIT);
    assert_eq!(h.service.drive(), 0);
    h.clock.advance(Duration::from_secs(1));
    assert_eq!(h.service.drive(), 1);
    failed(request, ConnectionFailure::TimedOut);
    // And a callback that arrives after the wait, before any upkeep ran.
    let (request, consent_late) = h.begin("Work");
    h.clock.advance(CONSENT_WAIT + Duration::from_secs(1));
    h.service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent_late, SUBJECT_A, &["doc-1"]),
        )
        .unwrap();
    failed(request, ConnectionFailure::TimedOut);
    let _ = consent;

    // The vendor fails the exchange, each in its own closed reason.
    for (error, failure) in [
        (
            VendorError::Unavailable,
            ConnectionFailure::VendorUnavailable,
        ),
        (
            VendorError::Unconfirmed,
            ConnectionFailure::VendorUnavailable,
        ),
        (VendorError::Rejected, ConnectionFailure::VendorRejected),
        (VendorError::RateLimited, ConnectionFailure::VendorRejected),
        (VendorError::ConsentDenied, ConnectionFailure::ConsentDenied),
    ] {
        let (request, consent) = h.begin("Work");
        h.vendor.fail(FakeCall::CompleteAuthorization, error);
        h.service
            .complete_trip(
                h.owner,
                request,
                h.vendor.approve(&consent, SUBJECT_A, &["doc-1"]),
            )
            .unwrap();
        failed(request, failure);
    }
    // A code the vendor does not know (a forged callback with the right
    // state) is rejected.
    let (request, consent) = h.begin("Work");
    let mut forged = h.vendor.deny(&consent);
    forged.code = koru_oauth_credentials::SecretString::new("SECRET-forged").ok();
    h.service.complete_trip(h.owner, request, forged).unwrap();
    failed(request, ConnectionFailure::VendorRejected);

    // The credential store cannot be written: nothing is connected.
    let (request, consent) = h.begin("Work");
    h.credentials.set_locked(true);
    h.service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent, SUBJECT_A, &["doc-1"]),
        )
        .unwrap();
    h.credentials.set_locked(false);
    let receipt = h.receipt(request);
    assert_eq!(
        receipt.failure,
        Some(ConnectionFailure::CredentialStoreUnavailable)
    );
    assert!(live(&h).is_empty());

    // A build with no registration says so at once, and the person is free
    // to try again afterwards.
    h.vendor.unconfigure();
    let request = BotRequestId::mint();
    match h
        .service
        .begin_connect(h.owner, request, "Work", REDIRECT)
        .unwrap()
    {
        TripStart::Finished(receipt) => {
            assert_eq!(receipt.failure, Some(ConnectionFailure::NotConfigured))
        }
        other => panic!("{other:?}"),
    }
    failed_without_store_count(&h, request);
}

fn failed_without_store_count(h: &Harness, request: BotRequestId) {
    assert_eq!(h.receipt(request).stage, ConnectionReceiptStage::Failed);
    assert!(live(h).is_empty());
}

#[test]
fn m5_connect_flow_the_token_is_in_the_planned_slot_before_the_row_and_cleared_if_the_row_fails() {
    let h = Harness::new();
    let (request, consent) = h.begin("Work");
    let planned = h.receipt(request).planned_connection.unwrap();
    h.store.fail_complete.store(true, Ordering::SeqCst);
    let receipt = h
        .service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent, SUBJECT_A, &["doc-1"]),
        )
        .unwrap();
    assert_eq!(receipt.failure, Some(ConnectionFailure::Interrupted));
    // The credential went into the slot named from the planned id, with no
    // connection row in existence, and was cleared when the row failed.
    let slot = h.slot(planned).account().to_owned();
    assert_eq!(
        h.credentials.touched(),
        vec![("store", slot.clone()), ("clear", slot)]
    );
    assert_eq!(h.stored(planned), None);
    assert!(live(&h).is_empty());
    assert_eq!(h.connection_or_none(planned), None);
    // The tokens of the failed connect were dropped, not revoked.
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    // The same account connects afterwards.
    h.store.fail_complete.store(false, Ordering::SeqCst);
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    assert_ne!(connection.id, planned);
    assert!(h.stored(connection.id).is_some());
}

impl Harness {
    pub(super) fn connection_or_none(&self, id: ConnectionId) -> Option<Connection> {
        self.store.with(|r| r.connection(self.owner, id)).unwrap()
    }
    pub(super) fn expire(&self, id: ConnectionId) -> Connection {
        self.store
            .with(|r| r.needs_sign_in(self.owner, id, 5))
            .unwrap()
    }
    pub(super) fn sign_in_again(
        &self,
        id: ConnectionId,
        subject: &str,
    ) -> ConnectionCommandReceipt {
        let request = BotRequestId::mint();
        let revision = self.connection(id).revision;
        let TripStart::Consent { consent, .. } = self
            .service
            .begin_sign_in_again(self.owner, request, id, revision, REDIRECT)
            .unwrap()
        else {
            panic!("the sign-in did not begin")
        };
        self.service
            .complete_trip(
                self.owner,
                request,
                self.vendor.approve(&consent, subject, &[]),
            )
            .unwrap()
    }
}

#[test]
fn m5_wrong_account_sign_in_again_as_another_account_changes_nothing_and_revokes_nothing() {
    let h = Harness::new();
    let connected = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let token = h.stored(connected.id).unwrap();
    // Signing in again is for a connection that needs it.
    assert_eq!(
        h.service
            .begin_sign_in_again(h.owner, BotRequestId::mint(), connected.id, 1, REDIRECT)
            .unwrap_err(),
        ConnectionError::Unavailable
    );
    let expired = h.expire(connected.id);
    assert_eq!(expired.state, ConnectionState::NeedsSignIn);

    // The person signs in as another account.
    let stores_before = stores(&h);
    let receipt = h.sign_in_again(connected.id, SUBJECT_B);
    assert_eq!(receipt.stage, ConnectionReceiptStage::Failed);
    assert_eq!(receipt.failure, Some(ConnectionFailure::WrongAccount));
    // The account was compared before anything was stored: the slot still
    // holds the connection's own credential, untouched.
    assert_eq!(stores(&h), stores_before);
    assert_eq!(h.stored(connected.id).unwrap(), token);
    assert_eq!(h.connection(connected.id), expired);
    // The other sign-in's tokens were dropped. Nothing was revoked.
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    assert!(!h.vendor.revoked(SUBJECT_B));

    // The right account restores it, under the same id and name.
    let receipt = h.sign_in_again(connected.id, SUBJECT_A);
    assert_eq!(receipt.stage, ConnectionReceiptStage::Completed);
    let restored = h.connection(connected.id);
    assert_eq!(restored.state, ConnectionState::Connected);
    assert_eq!(restored.revision, expired.revision + 1);
    assert_eq!(restored.name, "Work");
    assert_eq!(
        restored.credential_slot_digest,
        connected.credential_slot_digest
    );
    let renewed = h.stored(connected.id).unwrap();
    assert_ne!(renewed, token);
    assert!(renewed.starts_with("SECRET-refresh-"));
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    assert_eq!(
        h.changes.kinds(),
        vec![ConnectionChange::Connected, ConnectionChange::SignedIn]
    );
    // And it reads again.
    let effect = h.open(connected.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
}

#[test]
fn m5_wrong_account_a_duplicate_account_and_a_live_other_account_are_never_revoked() {
    let h = Harness::new();
    let work = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let other = h.connect("Other", SUBJECT_B, &["doc-b"]);
    let stores_before = stores(&h);

    // The same account a second time, under another name.
    let (request, consent) = h.begin("Second");
    let receipt = h
        .service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent, SUBJECT_A, &["doc-2"]),
        )
        .unwrap();
    assert_eq!(receipt.failure, Some(ConnectionFailure::AccountInUse));
    assert_eq!(receipt.connection, None);
    // Decided before anything was stored, in any slot.
    assert_eq!(stores(&h), stores_before);
    assert_eq!(live(&h).len(), 2);

    // The first connection needs sign-in, and the person signs in as the
    // account of the other live connection.
    h.expire(work.id);
    let receipt = h.sign_in_again(work.id, SUBJECT_B);
    assert_eq!(receipt.failure, Some(ConnectionFailure::WrongAccount));
    assert_eq!(stores(&h), stores_before);

    // In all of it the vendor was never asked to revoke anything, so the
    // other connection, which holds that account, still works.
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    assert!(!h.vendor.revoked(SUBJECT_A) && !h.vendor.revoked(SUBJECT_B));
    h.expire_access();
    let effect = h.open(other.id, "doc-b");
    assert_eq!(Harness::text(h.read(&effect)), "MARK-CONTENT b");
}

#[test]
fn m5_connect_flow_choosing_files_again_adds_them_and_needs_the_same_account() {
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let add = |subject: &str, picked: &[&str]| {
        let request = BotRequestId::mint();
        let revision = h.connection(connection.id).revision;
        let TripStart::Consent { consent, .. } = h
            .service
            .begin_add_items(h.owner, request, connection.id, revision, REDIRECT)
            .unwrap()
        else {
            panic!("the trip did not begin")
        };
        h.service
            .complete_trip(
                h.owner,
                request,
                h.vendor.approve(&consent, subject, picked),
            )
            .unwrap()
    };
    let token = h.stored(connection.id).unwrap();
    let receipt = add(SUBJECT_B, &["doc-b"]);
    assert_eq!(receipt.failure, Some(ConnectionFailure::WrongAccount));
    assert_eq!(h.items(connection.id).len(), 1);
    assert_eq!(h.stored(connection.id).unwrap(), token);

    let receipt = add(SUBJECT_A, &["doc-2"]);
    assert_eq!(receipt.stage, ConnectionReceiptStage::Completed);
    assert_eq!(receipt.result_revision, Some(2));
    assert_eq!(h.items(connection.id).len(), 2);
    assert_eq!(h.connection(connection.id).revision, 2);
    assert_eq!(
        h.changes.kinds(),
        vec![ConnectionChange::Connected, ConnectionChange::ItemsChanged]
    );
    let effect = h.open(connection.id, "doc-2");
    assert_eq!(Harness::text(h.read(&effect)), "MARK-CONTENT two");
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
}

fn disconnect(h: &Harness, id: ConnectionId) -> (BotRequestId, Disconnected) {
    let request = BotRequestId::mint();
    let revision = h.connection(id).revision;
    let done = h
        .service
        .disconnect(h.owner, request, id, revision)
        .unwrap();
    (request, done)
}

#[test]
fn m5_disconnect_fences_revokes_clears_and_leaves_a_tombstone() {
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let effect_before = h.open(connection.id, "doc-1");
    h.journal.clear();
    let (request, done) = disconnect(&h, connection.id);
    assert_eq!(done.connection.state, ConnectionState::Disconnected);
    assert_eq!(done.connection.vendor_revoke, Some(VendorRevoke::Confirmed));
    assert_eq!(done.connection.revision, 3);
    assert_eq!(done.receipt.stage, ConnectionReceiptStage::Completed);
    assert_eq!(done.receipt.result_revision, Some(3));
    // The vendor ended the grant, and the credential is gone.
    assert!(h.vendor.revoked(SUBJECT_A));
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 1);
    assert_eq!(h.stored(connection.id), None);
    assert!(live(&h).is_empty());
    assert_eq!(
        h.changes.kinds(),
        vec![
            ConnectionChange::Connected,
            ConnectionChange::Disconnecting,
            ConnectionChange::Disconnected
        ]
    );
    let notices = h.changes.all();
    assert_eq!(notices[1].revision, 2);
    assert_eq!(notices[1].vendor_revoke, None);
    assert_eq!(notices[2].vendor_revoke, Some(VendorRevoke::Confirmed));
    // The same request again answers the same thing and asks nobody.
    let again = h
        .service
        .disconnect(h.owner, request, connection.id, 1)
        .unwrap();
    assert_eq!(again.connection, done.connection);
    assert_eq!(again.receipt, done.receipt);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 1);
    // A read planned before the Disconnect is refused before the vendor is
    // called, and so is any later one.
    assert!(matches!(
        h.read(&effect_before),
        ReadOutcome::Refused(koru_delegate_core::PrivateEffectRefusal::Revoked)
    ));
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    // The tombstone admits no command.
    assert_eq!(
        h.service
            .disconnect(h.owner, BotRequestId::mint(), connection.id, 3)
            .unwrap_err(),
        ConnectionError::Unavailable
    );
    // Connecting the same account again is a new connection.
    let fresh = h.connect("Work", SUBJECT_A, &["doc-1"]);
    assert_ne!(fresh.id, connection.id);
    assert_ne!(h.slot(fresh.id).account(), h.slot(connection.id).account());
}

#[test]
fn m5_disconnect_says_what_the_vendor_said_and_disconnects_locally_either_way() {
    // The vendor has no revoke call.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    h.vendor.set_revoke_supported(false);
    let (_, done) = disconnect(&h, connection.id);
    assert_eq!(done.connection.state, ConnectionState::Disconnected);
    assert_eq!(
        done.connection.vendor_revoke,
        Some(VendorRevoke::Unsupported)
    );
    assert_eq!(h.stored(connection.id), None);

    // The revoke fails, in every way a vendor call can.
    for error in [
        VendorError::Unavailable,
        VendorError::Unconfirmed,
        VendorError::Rejected,
        VendorError::InvalidGrant,
    ] {
        let h = Harness::new();
        let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
        h.vendor.fail(FakeCall::Revoke, error);
        let (_, done) = disconnect(&h, connection.id);
        assert_eq!(
            done.connection.state,
            ConnectionState::Disconnected,
            "{error}"
        );
        assert_eq!(
            done.connection.vendor_revoke,
            Some(VendorRevoke::Unconfirmed)
        );
        assert_eq!(h.stored(connection.id), None);
        assert_eq!(h.vendor.calls(FakeCall::Revoke), 1);
    }

    // There is no credential to revoke with.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    h.credentials.clear(&h.slot(connection.id)).unwrap();
    let (_, done) = disconnect(&h, connection.id);
    assert_eq!(
        done.connection.vendor_revoke,
        Some(VendorRevoke::Unconfirmed)
    );
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
}

#[test]
fn m5_disconnect_with_a_locked_credential_store_stays_fenced_and_can_be_repeated() {
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    h.credentials.set_locked(true);
    let (request, done) = disconnect(&h, connection.id);
    assert_eq!(done.receipt.stage, ConnectionReceiptStage::Failed);
    assert_eq!(
        done.receipt.failure,
        Some(ConnectionFailure::CredentialStoreUnavailable)
    );
    // Fenced: nothing is read, and the credential was neither revoked nor
    // cleared.
    assert_eq!(done.connection.state, ConnectionState::Disconnecting);
    assert_eq!(done.connection.revision, 2);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    assert!(h.stored(connection.id).is_some());
    // The failed request stays failed.
    let again = h
        .service
        .disconnect(h.owner, request, connection.id, 1)
        .unwrap();
    assert_eq!(again.receipt, done.receipt);
    // The store opens: a new request finishes the Disconnect.
    h.credentials.set_locked(false);
    let (_, done) = disconnect(&h, connection.id);
    assert_eq!(done.connection.state, ConnectionState::Disconnected);
    assert_eq!(done.connection.vendor_revoke, Some(VendorRevoke::Confirmed));
    assert_eq!(h.stored(connection.id), None);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 1);
}

#[test]
fn m5_disconnect_interrupted_at_each_step_is_finished_at_the_next_start() {
    // After step 1: fenced, the credential still there.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let request = BotRequestId::mint();
    h.store
        .with(|r| r.begin_disconnect(h.owner, request, &"d".repeat(64), connection.id, 1, 9))
        .unwrap();
    let later = h.another_service(h.context.clone());
    assert_eq!(
        later.recover().unwrap(),
        Recovered {
            disconnects_finished: 1,
            ..Recovered::default()
        }
    );
    let gone = h.connection(connection.id);
    assert_eq!(gone.state, ConnectionState::Disconnected);
    assert_eq!(gone.vendor_revoke, Some(VendorRevoke::Confirmed));
    assert!(h.vendor.revoked(SUBJECT_A));
    assert_eq!(h.stored(connection.id), None);
    assert_eq!(h.receipt(request).stage, ConnectionReceiptStage::Completed);
    // A second start finds nothing left to do.
    assert_eq!(later.recover().unwrap(), Recovered::default());

    // After step 3: revoked and cleared, the tombstone not written. The
    // secret is gone, so what the vendor holds cannot be confirmed again.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let request = BotRequestId::mint();
    h.store
        .with(|r| r.begin_disconnect(h.owner, request, &"d".repeat(64), connection.id, 1, 9))
        .unwrap();
    h.credentials.clear(&h.slot(connection.id)).unwrap();
    let later = h.another_service(h.context.clone());
    assert_eq!(later.recover().unwrap().disconnects_finished, 1);
    let gone = h.connection(connection.id);
    assert_eq!(gone.state, ConnectionState::Disconnected);
    assert_eq!(gone.vendor_revoke, Some(VendorRevoke::Unconfirmed));

    // The store is locked at the next start: it is left for the one after.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let request = BotRequestId::mint();
    h.store
        .with(|r| r.begin_disconnect(h.owner, request, &"d".repeat(64), connection.id, 1, 9))
        .unwrap();
    h.credentials.set_locked(true);
    let later = h.another_service(h.context.clone());
    assert_eq!(later.recover().unwrap().deferred, 1);
    assert_eq!(
        h.connection(connection.id).state,
        ConnectionState::Disconnecting
    );
    assert_eq!(h.receipt(request).stage, ConnectionReceiptStage::Prepared);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    h.credentials.set_locked(false);
    assert_eq!(later.recover().unwrap().disconnects_finished, 1);
    assert_eq!(
        h.connection(connection.id).state,
        ConnectionState::Disconnected
    );
    assert_eq!(h.receipt(request).stage, ConnectionReceiptStage::Completed);
}

#[test]
fn m5_connect_flow_an_interrupted_connect_leaves_no_untracked_secret() {
    let h = Harness::new();
    // A connection that needs sign-in, with a sign-in again a previous
    // process began; and a fresh connect of another person that the same
    // process had stored a token for when it died.
    let existing = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let token = h.stored(existing.id).unwrap();
    let expired = h.expire(existing.id);
    let again = BotRequestId::mint();
    h.store
        .with(|r| {
            r.begin_sign_in_again(
                h.owner,
                again,
                &"a".repeat(64),
                existing.id,
                expired.revision,
                9,
            )
        })
        .unwrap();
    let other = BotOwner {
        user_scope: UserScopeId::new(),
        device: h.owner.device,
    };
    let (fresh, planned) = (BotRequestId::mint(), ConnectionId::mint());
    h.store
        .with(|r| r.begin_connect(other, fresh, &"b".repeat(64), planned, "Fresh", 9))
        .unwrap();
    let orphan = h.slot(planned);
    h.credentials
        .store(
            &orphan,
            koru_oauth_credentials::SecretString::new("SECRET-orphan").unwrap(),
        )
        .unwrap();

    // The credential store is locked at the first start: the fresh connect
    // is kept, so its slot is found again; the sign-in again needs no slot
    // work and is failed.
    h.credentials.set_locked(true);
    let later = h.another_service(h.context.clone());
    assert_eq!(
        later.recover().unwrap(),
        Recovered {
            trips_interrupted: 1,
            deferred: 1,
            ..Recovered::default()
        }
    );
    assert!(h.credentials.peek(&orphan).is_some());
    h.credentials.set_locked(false);
    assert_eq!(
        later.recover().unwrap(),
        Recovered {
            trips_interrupted: 1,
            slots_cleared: 1,
            ..Recovered::default()
        }
    );
    // The planned slot is empty and both commands failed as interrupted.
    assert!(h.credentials.peek(&orphan).is_none());
    for (owner, request) in [(h.owner, again), (other, fresh)] {
        let receipt = h
            .store
            .with(|r| r.receipt(owner, request))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.failure, Some(ConnectionFailure::Interrupted));
    }
    // The existing connection's own credential was left alone.
    assert_eq!(h.stored(existing.id).unwrap(), token);
    assert_eq!(h.connection(existing.id), expired);
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
    // A trip this process holds is not one a previous process left.
    let (live_request, _) = h.begin("Another");
    assert_eq!(h.service.recover().unwrap(), Recovered::default());
    assert_eq!(
        h.receipt(live_request).stage,
        ConnectionReceiptStage::Prepared
    );
    // A begin whose trip no process holds any more fails it as interrupted.
    match later
        .begin_connect(h.owner, live_request, "Another", REDIRECT)
        .unwrap()
    {
        TripStart::Finished(receipt) => {
            assert_eq!(receipt.failure, Some(ConnectionFailure::Interrupted))
        }
        other => panic!("{other:?}"),
    }
}
