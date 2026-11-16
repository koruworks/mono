// SPDX-License-Identifier: MPL-2.0
//! One read of one picked item, under the delegate's obligations: when the
//! vendor may be called, when content may be handed over, and what is
//! recorded when something goes wrong.
use super::*;
use koru_delegate_core::{
    PrivateEffectRefusal as Refusal, PrivateEffectStage as Stage,
    PrivateEffectTransition as Transition,
};
use koru_oauth_credentials::CredentialStore;

fn connected() -> (Harness, Connection) {
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1", "doc-2"]);
    h.journal.clear();
    (h, connection)
}
fn position(journal: &[String], prefix: &str) -> Option<usize> {
    journal.iter().position(|entry| entry.starts_with(prefix))
}
fn refusal(outcome: ReadOutcome) -> Refusal {
    match outcome {
        ReadOutcome::Refused(code) => code,
        other => panic!("not a refusal: {other:?}"),
    }
}

#[test]
fn m5_private_read_marks_dispatch_before_the_vendor_and_delivers_once() {
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let outcome = h.read(&effect);
    assert_eq!(Harness::text(outcome), CONTENT_1);
    // The dispatch mark was answered `dispatched` before the vendor was
    // called, and the receipt followed the call.
    let journal = h.journal.entries();
    assert_eq!(
        journal,
        vec![
            "ledger:Dispatched->Dispatched".to_owned(),
            "vendor:ReadItem".to_owned(),
            format!(
                "ledger:Confirmed {{ outcome_bytes: {}, withheld: false }}->Confirmed",
                CONTENT_1.len()
            ),
        ]
    );
    let row = h.ledger.row(&effect.key);
    assert_eq!(row.stage, Stage::Confirmed);
    assert_eq!(row.outcome_bytes, Some(CONTENT_1.len() as u32));
    assert_eq!(row.withheld, Some(false));
    assert_eq!(h.service.open_obligations(), 0);
    assert_eq!(h.vendor.content_reads(), 1);

    // The license is spent. The same reply offered again reaches the ledger,
    // which refuses the mark on a final row: the vendor is not called.
    assert!(matches!(h.read(&effect), ReadOutcome::Unknown));
    // A later reply for the same effect says the read happened and its
    // reply is gone. It is not performed again.
    assert!(matches!(
        h.read(&h.ledger.row(&effect.key)),
        ReadOutcome::ResultUnavailable
    ));
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
    assert_eq!(h.vendor.content_reads(), 1);
    assert_eq!(h.service.open_obligations(), 0);

    // An effect another process opened is not this one's to perform.
    let foreign = h.ledger.open(
        h.proposal(connection.id, "doc-1"),
        koru_delegate_core::PrivateDispatcherId::mint(),
    );
    assert!(matches!(h.read(&foreign), ReadOutcome::NotOurs));
    assert_eq!(h.ledger.row(&foreign.key).stage, Stage::Prepared);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);

    // One dispatcher identity for the whole process, whoever asks.
    let second = h.another_service(h.context.clone());
    assert_eq!(second.dispatcher(), h.service.dispatcher());
    assert_eq!(process_dispatcher(), process_dispatcher());
    assert_eq!(effect.dispatcher, process_dispatcher());
}

#[test]
fn m5_private_read_long_content_is_cut_at_a_character_and_says_so() {
    let (h, connection) = connected();
    // Two-byte characters across the limit: the cut may not split one.
    let long = "é".repeat(READ_LIMIT_BYTES);
    h.vendor.add_item("doc-2", SUBJECT_A, "MARK-TITLE-2", &long);
    let effect = h.open(connection.id, "doc-2");
    let ReadOutcome::Delivered(content) = h.read(&effect) else {
        panic!("not delivered")
    };
    assert!(content.truncated());
    assert_eq!(content.total_bytes(), long.len() as u64);
    assert_eq!(content.text().len(), READ_LIMIT_BYTES);
    assert!(long.starts_with(content.text()));
    assert_eq!(
        h.ledger.row(&effect.key).outcome_bytes,
        Some(READ_LIMIT_BYTES as u32)
    );
    // The bound is the type's own too, whatever an adapter returns.
    let over = PrivateReadContent::new(ItemContent {
        text: format!("a{}", "é".repeat(READ_LIMIT_BYTES)),
        truncated: false,
        total_bytes: 0,
    });
    assert_eq!(over.text().len(), READ_LIMIT_BYTES - 1);
    assert!(over.truncated());
    assert!(over.total_bytes() >= over.text().len() as u64);
}

#[test]
fn m5_private_read_no_vendor_call_unless_the_mark_was_answered_dispatched() {
    // Remove access before the read: the mark is refused as a reply.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.ledger.remove_access();
    assert_eq!(refusal(h.read(&effect)), Refusal::AccessDenied);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    let row = h.ledger.row(&effect.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::AccessDenied))
    );
    assert_eq!(h.service.open_obligations(), 0);

    // The delegate does not answer the mark for the whole budget.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.ledger.unavailable(1000);
    assert_eq!(refusal(h.read(&effect)), Refusal::Unavailable);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    assert_eq!(h.vendor.content_reads(), 0);
    // It waited on the back-off, 250 ms doubling, inside its one budget.
    let slept = h.clock.slept();
    assert_eq!(
        &slept[..6],
        &[250, 500, 1000, 2000, 4000, 8000].map(Duration::from_millis)
    );
    assert!(slept.iter().sum::<Duration>() < Duration::from_secs(30));
    // The row is still open and this process still owes it the truth.
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Prepared);
    assert_eq!(h.service.open_obligations(), 1);
    // The delegate comes back: the next upkeep records what was told.
    h.ledger.available();
    assert_eq!(h.service.drive(), 1);
    let row = h.ledger.row(&effect.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::Unavailable))
    );
    assert_eq!(h.service.open_obligations(), 0);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);

    // The mark commits and its reply is lost: the same mark is sent again,
    // and the vendor is called only after the reply that says `dispatched`.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.ledger.lose_next_reply();
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    let journal = h.journal.entries();
    assert_eq!(journal[0], "ledger:Dispatched->Unavailable");
    assert_eq!(journal[1], "ledger:Dispatched->Dispatched");
    assert_eq!(position(&journal, "vendor:ReadItem"), Some(2));
}

#[test]
fn m5_private_read_content_is_dropped_without_deliver_or_without_a_caller() {
    // Remove access while the read is in flight: the read finishes at the
    // vendor, the receipt answers `deliver: false`, the content is dropped.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let ledger = h.ledger.clone();
    h.vendor.on_read(move || ledger.remove_access());
    assert!(matches!(h.read(&effect), ReadOutcome::Withheld));
    assert_eq!(h.vendor.content_reads(), 1);
    let row = h.ledger.row(&effect.key);
    assert_eq!((row.stage, row.withheld), (Stage::Confirmed, Some(false)));

    // The caller is gone when the content is ready: nobody receives it.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let outcome = h.service.read(h.owner, &effect, &Gone(true), h.budget());
    assert!(matches!(outcome, ReadOutcome::CallerGone));
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Confirmed);
    assert_eq!(h.service.open_obligations(), 0);

    // Another process's dispatcher closed the row while the vendor was
    // answering: the late receipt is stale and the content is dropped.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let (ledger, key) = (h.ledger.clone(), effect.key.clone());
    h.vendor
        .on_read(move || ledger.resolve_for_another_dispatcher(&key));
    assert!(matches!(h.read(&effect), ReadOutcome::Unknown));
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Unknown);
    assert_eq!(h.service.open_obligations(), 0);
    assert!(matches!(
        h.ledger.advances().last(),
        Some((Transition::Confirmed { .. }, Err(LedgerError::Stale)))
    ));

    // Disconnect while the read is in flight: the revision moved, so the
    // read is recorded as withheld and its content is dropped.
    let (h, connection) = connected();
    // A vendor whose revoke does not end a call already in flight.
    h.vendor.set_revoke_supported(false);
    let effect = h.open(connection.id, "doc-1");
    let (service, owner, id) = (h.service.clone(), h.owner, connection.id);
    let once = AtomicBool::new(false);
    h.vendor.on_read(move || {
        if !once.swap(true, Ordering::SeqCst) {
            service
                .disconnect(owner, BotRequestId::mint(), id, 1)
                .unwrap();
        }
    });
    assert!(matches!(h.read(&effect), ReadOutcome::Withheld));
    let row = h.ledger.row(&effect.key);
    assert_eq!((row.stage, row.withheld), (Stage::Confirmed, Some(true)));
    assert_eq!(
        h.connection(connection.id).state,
        ConnectionState::Disconnected
    );

    // A read that is replayed while it is being performed is in progress:
    // nothing more is sent anywhere.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let (service, owner, ledger, key) = (
        h.service.clone(),
        h.owner,
        h.ledger.clone(),
        effect.key.clone(),
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let budget = h.budget();
    h.vendor.on_read(move || {
        let replay = service.read(owner, &ledger.row(&key), &PresentCaller, budget);
        record.lock().unwrap().push(format!("{replay:?}"));
    });
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(*seen.lock().unwrap(), vec!["InProgress".to_owned()]);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
}

#[test]
fn m5_dispatcher_obligation_what_the_caller_was_told_is_what_is_recorded() {
    // The vendor answered, and the delegate stops answering before the
    // receipt: the call says unknown and drops the content.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let ledger = h.ledger.clone();
    h.vendor.on_read(move || ledger.unavailable(1000));
    assert!(matches!(h.read(&effect), ReadOutcome::Unknown));
    assert_eq!(h.vendor.content_reads(), 1);
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Dispatched);
    assert_eq!(h.service.open_obligations(), 1);
    // For as long as the delegate is silent the same receipt is sent again,
    // on the back-off: 250 ms doubling, never more than 32 s apart.
    let mut waits = Vec::new();
    assert_eq!(h.service.drive(), 1);
    for _ in 0..10 {
        let mut waited = Duration::ZERO;
        while h.service.drive() == 0 {
            h.clock.advance(Duration::from_millis(250));
            waited += Duration::from_millis(250);
            assert!(
                waited <= ADVANCE_BACKOFF_MAX,
                "the back-off grew past its cap"
            );
        }
        waits.push(waited);
    }
    assert_eq!(
        waits[..8],
        [250, 500, 1000, 2000, 4000, 8000, 16000, 32000].map(Duration::from_millis)
    );
    assert_eq!(waits[9], ADVANCE_BACKOFF_MAX);
    // The delegate comes back. What is recorded is unknown, never confirmed.
    h.ledger.available();
    h.clock.advance(ADVANCE_BACKOFF_MAX);
    assert_eq!(h.service.drive(), 1);
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Unknown);
    assert_eq!(h.service.open_obligations(), 0);
    assert!(
        !h.ledger
            .advances()
            .iter()
            .any(
                |(transition, answer)| matches!(transition, Transition::Confirmed { .. })
                    && answer.is_ok()
            )
    );
    // Unknown is final: a replay says so and nothing is sent again.
    assert!(matches!(
        h.read(&h.ledger.row(&effect.key)),
        ReadOutcome::Unknown
    ));
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
    assert_eq!(h.service.drive(), 0);

    // The receipt committed and only its reply was lost: the caller is
    // still told unknown, gets no content, and the late `unknown` finds a
    // final row and is dropped.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    let ledger = h.ledger.clone();
    h.vendor.on_read(move || {
        ledger.lose_next_reply();
        ledger.unavailable(1000);
    });
    assert!(matches!(h.read(&effect), ReadOutcome::Unknown));
    h.ledger.available();
    assert_eq!(h.service.drive(), 1);
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Confirmed);
    assert_eq!(h.service.open_obligations(), 0);

    // A refusal that could not be recorded in time is told and recorded as
    // that refusal.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.vendor.delete_item("doc-1");
    let ledger = h.ledger.clone();
    h.vendor.on_read(move || ledger.unavailable(1000));
    assert_eq!(refusal(h.read(&effect)), Refusal::ItemUnavailable);
    h.ledger.available();
    assert_eq!(h.service.drive(), 1);
    let row = h.ledger.row(&effect.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::ItemUnavailable))
    );
}

#[test]
fn m5_dispatcher_obligation_a_panicked_call_is_finished_after_its_deadline() {
    // The adapter panics during the vendor call, after the dispatch mark.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.vendor.panic_on_read(true);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.read(&effect)));
    assert!(panicked.is_err());
    // The obligation is the service's, not the dead call's.
    assert_eq!(h.service.open_obligations(), 1);
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Dispatched);
    // Until the call's deadline has passed nobody guesses.
    assert_eq!(h.service.drive(), 0);
    h.clock.advance(Duration::from_secs(29));
    assert_eq!(h.service.drive(), 0);
    h.clock.advance(Duration::from_secs(2));
    assert_eq!(h.service.drive(), 1);
    assert_eq!(h.ledger.row(&effect.key).stage, Stage::Unknown);
    assert_eq!(h.service.open_obligations(), 0);
    // The service still works afterwards.
    h.vendor.panic_on_read(false);
    let next = h.open(connection.id, "doc-2");
    assert_eq!(Harness::text(h.read(&next)), "MARK-CONTENT two");

    // A panic before the mark was sent: the vendor was never called, and
    // the row ends refused as not dispatched.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    h.store.panic_on_item.store(true, Ordering::SeqCst);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.read(&effect)));
    assert!(panicked.is_err());
    h.store.panic_on_item.store(false, Ordering::SeqCst);
    assert_eq!(h.service.open_obligations(), 1);
    assert_eq!(h.service.drive(), 0);
    h.clock.advance(Duration::from_secs(31));
    assert_eq!(h.service.drive(), 1);
    let row = h.ledger.row(&effect.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::NotDispatched))
    );
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    assert_eq!(h.service.open_obligations(), 0);
}

#[test]
fn m5_dispatcher_obligation_an_own_open_row_nobody_is_performing_is_closed() {
    let (h, connection) = connected();
    // The reply that carried `execute` was lost: all this process ever sees
    // is the replay, `reconcile_only`, of a `prepared` row it owns.
    let lost = h.open(connection.id, "doc-1");
    let replay = h.ledger.row(&lost.key);
    assert_eq!(
        replay.disposition,
        koru_delegate_core::PrivateEffectDisposition::ReconcileOnly
    );
    assert_eq!(refusal(h.read(&replay)), Refusal::NotDispatched);
    let row = h.ledger.row(&lost.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::NotDispatched))
    );
    // Nothing was read, and the refusal is final.
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    assert_eq!(
        refusal(h.read(&h.ledger.row(&lost.key))),
        Refusal::NotDispatched
    );
    assert_eq!(h.ledger.advances().len(), 1);

    // An own row at `dispatched` that no call here accounts for cannot be
    // called not dispatched: it is recorded as unknown.
    let stray = h.open(connection.id, "doc-2");
    h.ledger
        .advance(&stray.key, process_dispatcher(), &Transition::Dispatched {})
        .unwrap();
    assert!(matches!(
        h.read(&h.ledger.row(&stray.key)),
        ReadOutcome::Unknown
    ));
    assert_eq!(h.ledger.row(&stray.key).stage, Stage::Unknown);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
}

#[test]
fn m5_credential_faults_locked_missing_another_context_and_expired() {
    // Locked: the store cannot be read. Refused from `prepared`; the
    // connection is not changed.
    let (h, connection) = connected();
    h.expire_access();
    h.credentials.set_locked(true);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::CredentialLocked);
    assert_eq!(h.connection(connection.id), connection);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 0);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    assert_eq!(
        h.ledger.advances(),
        vec![(
            Transition::Refused {
                code: Refusal::CredentialLocked
            },
            Ok(Stage::Refused)
        )]
    );
    // Unlocked, a new read goes through.
    h.credentials.set_locked(false);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);

    // Missing: the slot is empty. The connection needs sign-in; nothing of
    // it is removed.
    let (h, connection) = connected();
    h.expire_access();
    h.credentials.clear(&h.slot(connection.id)).unwrap();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::CredentialMissing);
    let lapsed = h.connection(connection.id);
    assert_eq!(lapsed.state, ConnectionState::NeedsSignIn);
    assert_eq!(lapsed.revision, 2);
    assert_eq!(h.items(connection.id).len(), 2);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    assert_eq!(
        h.changes.kinds(),
        vec![ConnectionChange::Connected, ConnectionChange::NeedsSignIn]
    );
    // A read planned against it afterwards is refused before anything.
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::CredentialExpired);

    // Another credential context (another root): the slot digest differs,
    // so the credential is reported missing and no slot is read at all.
    let (h, connection) = connected();
    let elsewhere = h.another_service(context("/isolated/second-root", h.owner));
    let touched = h.credentials.touched().len();
    let effect = h.open(connection.id, "doc-1");
    let outcome = elsewhere.read(h.owner, &effect, &PresentCaller, h.budget());
    assert_eq!(refusal(outcome), Refusal::CredentialMissing);
    assert_eq!(h.credentials.touched().len(), touched);
    assert!(h.stored(connection.id).is_some());
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);

    // Expired or revoked at the vendor: the refresh is refused.
    let (h, connection) = connected();
    h.vendor.invalidate(SUBJECT_A);
    h.expire_access();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::CredentialExpired);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 1);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
    let lapsed = h.connection(connection.id);
    assert_eq!(
        (lapsed.state, lapsed.revision),
        (ConnectionState::NeedsSignIn, 2)
    );
    // The stored credential is left where it is until the person signs in.
    assert!(h.stored(connection.id).is_some());
    assert_eq!(h.vendor.calls(FakeCall::Revoke), 0);
}

#[test]
fn m5_refresh_expiry_rotation_single_flight_and_a_rotation_that_cannot_be_stored() {
    // Within its life the access token from sign-in is used as it is.
    let (h, connection) = connected();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 0);
    // After it, one refresh, recorded on the row without moving its revision.
    h.expire_access();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 1);
    let refreshed = h.connection(connection.id);
    assert_eq!(refreshed.revision, 1);
    assert!(refreshed.refreshed_at_ms.is_some());
    let effect = h.open(connection.id, "doc-2");
    assert_eq!(Harness::text(h.read(&effect)), "MARK-CONTENT two");
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 1);

    // Rotation: the new refresh token replaces the old one in the same slot.
    let (h, connection) = connected();
    let before = h.stored(connection.id).unwrap();
    h.vendor.set_rotation(true);
    h.expire_access();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    let rotated = h.stored(connection.id).unwrap();
    assert_ne!(rotated, before);
    assert_eq!(
        h.connection(connection.id).state,
        ConnectionState::Connected
    );
    // The rotated token is the one that works next time.
    h.expire_access();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 2);

    // A rotated token that cannot be stored: this read goes ahead with the
    // access token it has, and the connection then needs sign-in.
    let (h, connection) = connected();
    h.vendor.set_rotation(true);
    h.expire_access();
    h.credentials.set_store_refused(true);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    let lapsed = h.connection(connection.id);
    assert_eq!(
        (lapsed.state, lapsed.revision),
        (ConnectionState::NeedsSignIn, 2)
    );
    assert_eq!(
        h.changes.kinds(),
        vec![ConnectionChange::Connected, ConnectionChange::NeedsSignIn]
    );

    // Two reads at once after expiry: one refresh, and both read.
    let (h, connection) = connected();
    h.expire_access();
    let first = h.open(connection.id, "doc-1");
    let second = h.open(connection.id, "doc-2");
    let budget = h.budget();
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| h.service.read(h.owner, &first, &PresentCaller, budget));
        let two = scope.spawn(|| h.service.read(h.owner, &second, &PresentCaller, budget));
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(Harness::text(one), CONTENT_1);
    assert_eq!(Harness::text(two), "MARK-CONTENT two");
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 1);
}

#[test]
fn m5_destination_a_changed_identity_refuses_before_content_and_asks_for_a_fresh_grant() {
    let (h, connection) = connected();
    // The picked file now belongs to another account.
    h.vendor.transfer("doc-1", SUBJECT_B);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::DestinationChanged);
    // The vendor was asked, and produced no content.
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
    assert_eq!(h.vendor.content_reads(), 0);
    let row = h.ledger.row(&effect.key);
    assert_eq!(
        (row.stage, row.refusal),
        (Stage::Refused, Some(Refusal::DestinationChanged))
    );
    // The item is marked, the revision moved, the other item is untouched.
    let changed = h.item(connection.id, "doc-1");
    assert_eq!(changed.state, ConnectionItemState::Changed);
    assert!(changed.changed_at_ms.is_some());
    assert_eq!(
        h.item(connection.id, "doc-2").state,
        ConnectionItemState::Available
    );
    assert_eq!(h.connection(connection.id).revision, 2);
    assert_eq!(
        h.changes.kinds(),
        vec![ConnectionChange::Connected, ConnectionChange::ItemsChanged]
    );
    // It is not read again, and the vendor is not even asked.
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::DestinationChanged);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
    // The other item still reads.
    let effect = h.open(connection.id, "doc-2");
    assert_eq!(Harness::text(h.read(&effect)), "MARK-CONTENT two");

    // The person picks it again: the fresh grant, under its new identity.
    let request = BotRequestId::mint();
    let TripStart::Consent { consent, .. } = h
        .service
        .begin_add_items(h.owner, request, connection.id, 2, REDIRECT)
        .unwrap()
    else {
        panic!("the trip did not begin")
    };
    h.service
        .complete_trip(
            h.owner,
            request,
            h.vendor.approve(&consent, SUBJECT_A, &["doc-1"]),
        )
        .unwrap();
    let again = h.item(connection.id, "doc-1");
    assert_eq!(again.state, ConnectionItemState::Available);
    assert_eq!(again.id, changed.id);
    assert_ne!(again.identity_digest, changed.identity_digest);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(h.vendor.content_reads(), 2);
}

#[test]
fn m5_private_read_every_vendor_failure_has_its_one_recorded_outcome() {
    let cases: [(VendorError, Option<Refusal>); 8] = [
        (VendorError::NotFound, Some(Refusal::ItemUnavailable)),
        (VendorError::Forbidden, Some(Refusal::ItemUnavailable)),
        (VendorError::Rejected, Some(Refusal::VendorRejected)),
        (VendorError::RateLimited, Some(Refusal::VendorRejected)),
        (VendorError::Unauthorized, Some(Refusal::VendorRejected)),
        (VendorError::Unavailable, Some(Refusal::Unavailable)),
        (VendorError::InvalidGrant, Some(Refusal::CredentialExpired)),
        // The request left and nothing trustworthy came back.
        (VendorError::Unconfirmed, None),
    ];
    for (error, expected) in cases {
        let (h, connection) = connected();
        let effect = h.open(connection.id, "doc-1");
        h.vendor.fail_after_send(FakeCall::ReadItem, error);
        let outcome = h.read(&effect);
        let row = h.ledger.row(&effect.key);
        match expected {
            Some(code) => {
                assert_eq!(refusal(outcome), code, "{error}");
                assert_eq!((row.stage, row.refusal), (Stage::Refused, Some(code)));
            }
            None => {
                assert!(matches!(outcome, ReadOutcome::Unknown), "{error}");
                assert_eq!(row.stage, Stage::Unknown);
            }
        }
        // One call, whatever came of it. No retry, and none on a replay.
        assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1, "{error}");
        let _ = h.read(&h.ledger.row(&effect.key));
        assert_eq!(h.vendor.calls(FakeCall::ReadItem), 1);
        assert_eq!(h.ledger.row(&effect.key).stage, row.stage);
        assert_eq!(h.service.open_obligations(), 0);
        // The item row is kept as it was: "unavailable" is not a stored state.
        assert_eq!(
            h.item(connection.id, "doc-1").state,
            ConnectionItemState::Available
        );
    }

    // The item is really gone, or no longer shared.
    let (h, connection) = connected();
    h.vendor.delete_item("doc-1");
    h.vendor.withdraw(SUBJECT_A, "doc-2");
    for reference in ["doc-1", "doc-2"] {
        let effect = h.open(connection.id, reference);
        assert_eq!(refusal(h.read(&effect)), Refusal::ItemUnavailable);
    }
    assert_eq!(h.vendor.content_reads(), 0);
    assert_eq!(h.connection(connection.id), connection);

    // The vendor forgot the access token: this read is refused and not
    // repeated; the next one refreshes and reads.
    let (h, connection) = connected();
    h.vendor.drop_access_tokens();
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(refusal(h.read(&effect)), Refusal::VendorRejected);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 0);
    let effect = h.open(connection.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    assert_eq!(h.vendor.calls(FakeCall::Refresh), 1);

    // What the plan bound no longer matches ai.db: refused before anything.
    let (h, connection) = connected();
    let mut effect = h.open(connection.id, "doc-1");
    effect.operation.account = DecisionDigest::from_bytes([9; 32]);
    assert_eq!(refusal(h.read(&effect)), Refusal::WrongAccount);
    let mut effect = h.open(connection.id, "doc-1");
    effect.operation.connection_revision = DecisionRevision::new(7).unwrap();
    assert_eq!(refusal(h.read(&effect)), Refusal::Revoked);
    let mut effect = h.open(connection.id, "doc-1");
    effect.operation.item = ConnectionItemId::mint();
    assert_eq!(refusal(h.read(&effect)), Refusal::ItemUnavailable);
    // Another person's effect for this connection.
    let effect = h.open(connection.id, "doc-1");
    let stranger = BotOwner {
        user_scope: UserScopeId::new(),
        device: h.owner.device,
    };
    let outcome = h
        .service
        .read(stranger, &effect, &PresentCaller, h.budget());
    assert_eq!(refusal(outcome), Refusal::Revoked);
    assert_eq!(h.vendor.calls(FakeCall::ReadItem), 0);
}
