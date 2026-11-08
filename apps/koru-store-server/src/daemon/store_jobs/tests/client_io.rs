// SPDX-License-Identifier: MPL-2.0

//! Actual Client I/O -> bounded worker command -> shared Store -> network Ack.
use super::*;
use koru_network_iroh::cloud::{client_io, records};
use std::num::NonZeroUsize;
use tokio::{
    io::{AsyncReadExt, DuplexStream, ReadHalf, WriteHalf},
    time::{Duration, Instant},
};

mod reconcile;

type IoClient = client_io::Client<'static, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn client_pair(
    f: &Fixture,
    worker: StoreWorkerClient,
) -> (IoClient, DuplexStream, Stream, MemoryBudget) {
    client_pair_with_policy(f, worker, transfer::MAX_PAYLOAD, |_| {
        transfer::Decision::Defer
    })
}

fn client_pair_with_policy(
    f: &Fixture,
    worker: StoreWorkerClient,
    automatic_limit: usize,
    permission: impl FnMut(&transfer::Start) -> transfer::Decision + Send + 'static,
) -> (IoClient, DuplexStream, Stream, MemoryBudget) {
    use koru_replication::cloud::transfer::ClaimReferences;
    client_pair_with_claims(
        f,
        worker,
        automatic_limit,
        permission,
        (ClaimReferences::None, ClaimReferences::None),
    )
}

/// [`client_pair_with_policy`] whose client and worker ends were admitted
/// with `claims`: which way a claim reference may cross each.
fn client_pair_with_claims(
    f: &Fixture,
    worker: StoreWorkerClient,
    automatic_limit: usize,
    permission: impl FnMut(&transfer::Start) -> transfer::Decision + Send + 'static,
    claims: (
        koru_replication::cloud::transfer::ClaimReferences,
        koru_replication::cloud::transfer::ClaimReferences,
    ),
) -> (IoClient, DuplexStream, Stream, MemoryBudget) {
    let budget = MemoryBudget::new(128 * 1024 * 1024);
    let accepted = PreparedSession {
        principal: f.scope.principal,
        generation: 1,
        expires_at_seconds: 600,
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([1; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
        claim_references: claims.0,
    };
    let server = Stream::new(
        &PreparedSession {
            accepted: accepted.accepted.clone(),
            claim_references: claims.1,
            ..accepted
        },
        f.scope,
        0,
        0,
        transfer::MAX_PAYLOAD,
        budget.clone(),
    )
    .unwrap();
    let (a, b) = tokio::io::duplex(4096);
    let (ar, aw) = tokio::io::split(a);
    let scope = f.scope;
    let client = client_io::Client::new(
        ar,
        aw,
        client_io::Config {
            accepted,
            scope,
            observed: Instant::now(),
            wall_millis: 0,
            automatic_limit,
            capacity: NonZeroUsize::new(4).unwrap(),
            budget: budget.clone(),
        },
        move |complete| {
            let reply = worker
                .submit_cloud(scope, complete, Timestamp::from_millis(2), |_, _, _| Ok(()))
                .map_err(|_| transfer::Error::Unavailable)?;
            Ok(async move {
                reply
                    .await
                    .map_err(|_| transfer::Error::Unavailable)?
                    .map_err(|e| match e {
                        Failure::Transfer(e) => e,
                        Failure::Store(_) => transfer::Error::Unavailable,
                    })
            })
        },
        |_| Ok(()),
        permission,
    )
    .unwrap();
    (client, b, server, budget)
}

async fn drive<F: Future>(client: &mut IoClient, work: F) -> F::Output {
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return result,
            event = client.next() => { assert!(event.is_ok()); }
        }
    }
}

async fn submit(
    client: &mut IoClient,
    peer: &mut DuplexStream,
    server: &mut Stream,
    requests: &mut mpsc::Receiver<WorkerCommand>,
    f: &Fixture,
    budget: &MemoryBudget,
) -> WorkerCommand {
    drive(client, async {
        let deadline = Instant::now() + Duration::from_secs(10);
        records::write(
            peer,
            &Record::Control(server.initial_policy(0).unwrap()),
            deadline,
            budget,
        )
        .await
        .unwrap();
        let received = records::read(peer, deadline, budget).await.unwrap();
        let (_, Record::Control(policy), _) = received.into_parts() else {
            panic!();
        };
        server.receive_control(policy, 0, 0, |_| Ok(())).unwrap();
        let (start, admission) = server
            .begin_outgoing(f.unit, [11; 8], f.bytes.clone(), 0, 0, |_| Ok(()))
            .unwrap();
        records::write(peer, &Record::Control(start), deadline, budget)
            .await
            .unwrap();
        if admission == transfer::Admission::AwaitDecision {
            let response = records::read(peer, deadline, budget).await.unwrap();
            let (_, Record::Control(decision), _) = response.into_parts() else {
                panic!();
            };
            assert_eq!(
                server.receive_control(decision, 0, 0, |_| Ok(())).unwrap(),
                transfer::Admission::Accepted
            );
        }
        while let Some(record) = server.next_outgoing(0, |_| Ok(())).unwrap() {
            records::write(peer, record.record(), deadline, budget)
                .await
                .unwrap();
        }
        requests.recv().await.unwrap()
    })
    .await
}

#[tokio::test]
async fn actual_client_withholds_ack_until_queued_store_commit() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (mut client, mut peer, mut server, budget) = client_pair(&f, worker);
    let command = submit(
        &mut client,
        &mut peer,
        &mut server,
        &mut requests,
        &f,
        &budget,
    )
    .await;
    assert_eq!(f.name(), "Before");
    assert!(client.status().committing);
    assert!(budget.used() >= f.bytes.len());
    // No bytes are waiting on the peer: the client has not made a success Ack.
    std::future::poll_fn(|cx| {
        let mut probe = Box::pin(peer.read_u8());
        assert!(probe.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let WorkerCommand::Store(task) = command else {
        panic!();
    };
    task(Ok(&mut f.store));
    assert_eq!(f.name(), "After");
    let received = drive(
        &mut client,
        records::read(&mut peer, Instant::now() + Duration::from_secs(10), &budget),
    )
    .await
    .unwrap();
    let (_, Record::Peer(frame), _) = received.into_parts() else {
        panic!();
    };
    assert!(
        matches!(metadata::decode(f.scope, frame, 1).unwrap().as_slice(),
        [metadata::Metadata::Ack { unit, batch, ok: true }] if *unit == f.unit && *batch == [11; 8])
    );
}

#[tokio::test]
async fn large_private_download_accepts_exact_start_then_commits_through_store() {
    use koru_store_core::profile_units::PrivateManifest;
    let mut f = Fixture::new();
    let state = f
        .store
        .profile_unit_states(f.profile)
        .unwrap()
        .into_iter()
        .find(|s| matches!(s.unit, ProfileUnitRef::Private { .. }))
        .unwrap();
    let private = PrivateManifest::from_snapshot(&state.snapshot).unwrap();
    let mut random = 0x12345678u32;
    let mut words = 0;
    loop {
        for _ in 0..500 {
            let word: String = (0..32)
                .map(|_| {
                    random ^= random << 13;
                    random ^= random >> 17;
                    random ^= random << 5;
                    char::from(b'a' + (random % 26) as u8)
                })
                .collect();
            private.add_dictionary_word("en", &word).unwrap();
            words += 1;
        }
        f.bytes = private.export_snapshot().unwrap();
        if f.bytes.len() > transfer::RESTRICTED_LIMIT {
            break;
        }
        assert!(words < 20000);
    }
    f.unit = Unit::Private {
        profile_id: f.scope.profile,
    };
    let length = f.bytes.len();
    let unit = f.unit;
    let decided = Arc::new(AtomicBool::new(false));
    let observed = decided.clone();
    let (worker, mut requests) = queue(1);
    let (mut client, mut peer, mut server, budget) =
        client_pair_with_policy(&f, worker, transfer::RESTRICTED_LIMIT, move |start| {
            assert_eq!(start.unit, unit);
            assert_eq!(start.total_length, length);
            assert!(!observed.swap(true, Ordering::SeqCst));
            transfer::Decision::Accept
        });
    let command = submit(
        &mut client,
        &mut peer,
        &mut server,
        &mut requests,
        &f,
        &budget,
    )
    .await;
    assert!(decided.load(Ordering::SeqCst));
    let WorkerCommand::Store(task) = command else {
        panic!();
    };
    task(Ok(&mut f.store));
    assert_eq!(
        f.store.personal_dictionary(f.profile).unwrap()["en"].len(),
        words
    );
    let received = drive(
        &mut client,
        records::read(&mut peer, Instant::now() + Duration::from_secs(10), &budget),
    )
    .await
    .unwrap();
    let (_, Record::Peer(frame), _) = received.into_parts() else {
        panic!();
    };
    assert!(
        matches!(metadata::decode(f.scope, frame, 1).unwrap().as_slice(),
        [metadata::Metadata::Ack { unit: actual, batch, ok: true }]
        if *actual == unit && *batch == [11; 8])
    );
}

#[tokio::test]
async fn policy_change_after_actual_queue_handoff_does_not_cancel_the_commit() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (mut client, mut peer, mut server, budget) = client_pair(&f, worker);
    let command = submit(
        &mut client,
        &mut peer,
        &mut server,
        &mut requests,
        &f,
        &budget,
    )
    .await;
    client.change_policy(transfer::RESTRICTED_LIMIT).unwrap();
    let WorkerCommand::Store(task) = command else {
        panic!();
    };
    task(Ok(&mut f.store));
    let mut policy_seen = false;
    loop {
        let received = drive(
            &mut client,
            records::read(&mut peer, Instant::now() + Duration::from_secs(10), &budget),
        )
        .await
        .unwrap();
        match received.into_parts().1 {
            Record::Control(control) => {
                assert!(!policy_seen);
                assert!(matches!(
                    control,
                    transfer::Control::TransferPolicy {
                        policy_generation: 2,
                        ..
                    }
                ));
                policy_seen = true;
            }
            Record::Peer(frame) => {
                assert!(policy_seen);
                assert!(matches!(
                    metadata::decode(f.scope, frame, 1).unwrap().as_slice(),
                    [metadata::Metadata::Ack { ok: true, .. }]
                ));
                break;
            }
        }
    }
    assert_eq!(f.name(), "After");
}

#[tokio::test]
async fn canceled_client_keeps_accepted_store_work_and_original_reservation() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (mut client, mut peer, mut server, budget) = client_pair(&f, worker);
    let command = submit(
        &mut client,
        &mut peer,
        &mut server,
        &mut requests,
        &f,
        &budget,
    )
    .await;
    drop(client);
    drop(server);
    assert!(budget.used() >= f.bytes.len());
    assert_eq!(f.name(), "Before");
    let WorkerCommand::Store(task) = command else {
        panic!();
    };
    task(Ok(&mut f.store));
    assert_eq!(f.name(), "After");
    assert_eq!(budget.used(), 0);
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.is_empty(), "canceled network owner sends no late Ack");
}

#[tokio::test]
async fn queue_failure_closes_client_without_success_ack() {
    let f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (mut client, mut peer, mut server, budget) = client_pair(&f, worker);
    let command = submit(
        &mut client,
        &mut peer,
        &mut server,
        &mut requests,
        &f,
        &budget,
    )
    .await;
    let WorkerCommand::Store(task) = command else {
        panic!();
    };
    task(Err(WorkerError::Poisoned));
    assert!(matches!(
        client.next().await,
        Err(transfer::Error::Unavailable)
    ));
    assert_eq!(f.name(), "Before");
    drop(client);
    drop(server);
    assert_eq!(budget.used(), 0);
    let denial = records::read(&mut peer, Instant::now() + Duration::from_secs(10), &budget)
        .await
        .unwrap();
    assert!(matches!(denial.record(), Record::Peer(
        koru_replication::protocol::PeerFrame::Error { code }
    ) if code == "sync_unavailable"));
}
