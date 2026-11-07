// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_replication::{
    cloud::{Bytes, HandshakeV1, Version1},
    protocol::PeerFrame,
};
use koru_store_core::{DocumentId, WorkspaceId};
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

type TestClient = Client<'static, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

mod claims;

fn config(limit: usize, budget: MemoryBudget) -> Config {
    let profile_id = CloudId::from_bytes(koru_store_core::ProfileId::new().as_uuid().into_bytes());
    let principal = CloudReplicaPrincipalV1::Profile {
        profile_id,
        lineage_id: CloudId::from_bytes([4; 16]),
        logical_replica_id: CloudId::from_bytes([5; 16]),
    };
    Config {
        accepted: PreparedSession {
            principal,
            generation: 1,
            expires_at_seconds: 600,
            accepted: HandshakeV1::SessionAccepted {
                version: Version1,
                session_id: Bytes([1; 32]),
                generation: 1,
                expires_at_seconds: 600,
            },
            claim_references: koru_replication::cloud::transfer::ClaimReferences::None,
        },
        scope: Scope {
            principal,
            profile: profile_id,
            eligibility_workspace: WorkspaceId::new(),
            manifest: DocumentId::new(),
        },
        observed: Instant::now(),
        wall_millis: 0,
        automatic_limit: limit,
        capacity: NonZeroUsize::new(4).unwrap(),
        budget,
    }
}

async fn drive<F: Future>(client: &mut TestClient, work: F) -> F::Output {
    tokio::pin!(work);
    loop {
        tokio::select! {
            output = &mut work => return output,
            event = client.next() => { assert!(event.is_ok()); }
        }
    }
}

pub(super) async fn pair() -> (
    TestClient,
    ReadHalf<DuplexStream>,
    WriteHalf<DuplexStream>,
    Stream,
    Scope,
    MemoryBudget,
) {
    pair_with_workspace(false).await
}

pub(super) async fn pair_with_workspace(
    workspace: bool,
) -> (
    TestClient,
    ReadHalf<DuplexStream>,
    WriteHalf<DuplexStream>,
    Stream,
    Scope,
    MemoryBudget,
) {
    use koru_replication::cloud::transfer::ClaimReferences;
    pair_with_claims(workspace, ClaimReferences::None, ClaimReferences::None).await
}

/// [`pair_with_workspace`] whose client and worker ends were admitted with
/// `client` and `worker`: which way a claim reference may cross each.
pub(super) async fn pair_with_claims(
    workspace: bool,
    client: koru_replication::cloud::transfer::ClaimReferences,
    worker: koru_replication::cloud::transfer::ClaimReferences,
) -> (
    TestClient,
    ReadHalf<DuplexStream>,
    WriteHalf<DuplexStream>,
    Stream,
    Scope,
    MemoryBudget,
) {
    let budget = MemoryBudget::new(128 * 1024 * 1024);
    let mut config = config(transfer::RESTRICTED_LIMIT, budget.clone());
    config.accepted.claim_references = client;
    if workspace {
        let principal = CloudReplicaPrincipalV1::Workspace {
            workspace_id: CloudId::from_bytes(
                config.scope.eligibility_workspace.as_uuid().into_bytes(),
            ),
            incarnation_id: CloudId::from_bytes(uuid::Uuid::new_v4().into_bytes()),
            logical_replica_id: CloudId::from_bytes(uuid::Uuid::new_v4().into_bytes()),
        };
        config.scope.principal = principal;
        config.accepted.principal = principal;
    }
    let scope = config.scope;
    let worker_end = PreparedSession {
        accepted: config.accepted.accepted.clone(),
        claim_references: worker,
        ..config.accepted
    };
    let mut stream = Stream::new(
        &worker_end,
        scope,
        0,
        0,
        transfer::MAX_PAYLOAD,
        budget.clone(),
    )
    .unwrap();
    let (a, b) = tokio::io::duplex(4096);
    let (ar, aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    let mut client = Client::new(
        ar,
        aw,
        config,
        |_| Ok(async { Err(Error::Unavailable) }),
        |_| Ok(()),
        |_| Decision::Defer,
    )
    .unwrap();
    drive(&mut client, async {
        records::write(
            &mut bw,
            &Record::Control(stream.initial_policy(0).unwrap()),
            Instant::now() + Duration::from_secs(10),
            &budget,
        )
        .await
        .unwrap();
        let received = records::read(&mut br, Instant::now() + Duration::from_secs(10), &budget)
            .await
            .unwrap();
        let (_, Record::Control(policy), _) = received.into_parts() else {
            panic!();
        };
        stream.receive_control(policy, 0, 0, |_| Ok(())).unwrap();
    })
    .await;
    while client.status().transfer.peer_generation.is_none() || client.policy_pending != 0 {
        client.next().await.unwrap();
    }
    (client, br, bw, stream, scope, budget)
}

#[tokio::test]
async fn canceled_event_wait_keeps_partial_metadata_read_while_join_is_written() {
    let (mut client, mut reader, mut writer, _, scope, budget) = pair().await;
    let record = transfer::encode_record(&Record::Peer(PeerFrame::SyncRequest)).unwrap();
    writer.write_all(&record[..2]).await.unwrap();
    std::future::poll_fn(|cx| {
        let mut wait = Box::pin(client.next());
        assert!(wait.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    client
        .join(
            Unit::Identity {
                profile_id: scope.profile,
            },
            Vec::new(),
        )
        .unwrap();
    let request = drive(
        &mut client,
        records::read(
            &mut reader,
            Instant::now() + Duration::from_secs(10),
            &budget,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(request.record(), Record::Peer(_)));
    writer.write_all(&record[2..]).await.unwrap();
    loop {
        if let Event::Metadata(messages) = client.next().await.unwrap() {
            assert!(matches!(
                messages.values.as_slice(),
                [Metadata::SyncRequest]
            ));
            break;
        }
    }
}

#[tokio::test]
async fn restricted_download_is_deferred_without_payload_or_store_handoff() {
    let (mut client, mut reader, mut writer, mut stream, scope, budget) = pair().await;
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    // Opaque declared payload: no bytes are transmitted or imported in this test.
    let (start, admission) = stream
        .begin_outgoing(
            unit,
            [9; 8],
            vec![1; transfer::RESTRICTED_LIMIT + 1],
            0,
            0,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(admission, Admission::AwaitDecision);
    records::write(
        &mut writer,
        &Record::Control(start),
        Instant::now() + Duration::from_secs(10),
        &budget,
    )
    .await
    .unwrap();
    loop {
        if let Event::Deferred(start) = client.next().await.unwrap() {
            assert_eq!(start.unit, unit);
            assert_eq!(start.total_length, 200001);
            break;
        }
    }
    assert_eq!(client.status().transfer.incoming_bytes, 0);
    assert!(!client.status().committing);
    let response = drive(
        &mut client,
        records::read(
            &mut reader,
            Instant::now() + Duration::from_secs(10),
            &budget,
        ),
    )
    .await
    .unwrap();
    let (_, Record::Control(decision), _) = response.into_parts() else {
        panic!();
    };
    assert_eq!(
        stream.receive_control(decision, 0, 0, |_| Ok(())).unwrap(),
        Admission::Deferred
    );
    assert_eq!(stream.status().outgoing_bytes, 0);
}

#[tokio::test]
async fn idle_equal_limit_policy_change_is_published_before_new_reads() {
    let (mut client, mut reader, _, mut stream, _, budget) = pair().await;
    client.change_policy(transfer::RESTRICTED_LIMIT).unwrap();
    assert_eq!(client.status().transfer.local_generation, 2);
    let response = drive(
        &mut client,
        records::read(
            &mut reader,
            Instant::now() + Duration::from_secs(10),
            &budget,
        ),
    )
    .await
    .unwrap();
    let (_, Record::Control(policy), _) = response.into_parts() else {
        panic!();
    };
    assert_eq!(
        stream.receive_control(policy, 0, 0, |_| Ok(())).unwrap(),
        Admission::Policy
    );
    assert_eq!(stream.status().peer_generation, Some(2));
}

#[tokio::test]
async fn upload_waits_for_exact_remote_ack_and_keeps_retained_guard() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (mut client, mut reader, mut writer, mut stream, scope, budget) = pair().await;
    let dropped = Arc::new(AtomicBool::new(false));
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    let doc = loro::LoroDoc::new();
    doc.get_text("fixture").insert(0, "upload").unwrap();
    let bytes = doc.export(loro::ExportMode::Snapshot).unwrap();
    client
        .upload(unit, bytes.clone(), Guard(dropped.clone()))
        .unwrap();
    let done = drive(&mut client, async {
        loop {
            let received = records::read(
                &mut reader,
                Instant::now() + Duration::from_secs(10),
                &budget,
            )
            .await
            .unwrap();
            match received.into_parts().1 {
                Record::Control(control) => {
                    stream.receive_control(control, 0, 0, |_| Ok(())).unwrap();
                }
                Record::Peer(frame) => {
                    stream.receive_peer(frame, 0, |_| Ok(())).unwrap();
                }
            }
            if stream.status().incoming_complete {
                break stream.handoff(0, |_| Ok(()), |v| v).unwrap();
            }
        }
    })
    .await;
    assert_eq!(done.bytes, bytes);
    assert!(!dropped.load(Ordering::SeqCst));
    // Explicit simulated peer receipt: real Store persistence is separately
    // exercised by the Store-server integration tests, not claimed here.
    let ack = scope.committed_ack(done.unit, done.batch_id).unwrap();
    records::write(
        &mut writer,
        &ack,
        Instant::now() + Duration::from_secs(10),
        &budget,
    )
    .await
    .unwrap();
    loop {
        if let Event::Uploaded {
            unit: received,
            batch,
        } = client.next().await.unwrap()
        {
            assert_eq!(received, unit);
            assert_eq!(batch, done.batch_id);
            break;
        }
    }
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn initialization_deadline_does_not_restart_after_canceled_wait() {
    let budget = MemoryBudget::new(128 * 1024 * 1024);
    let (reader, _peer) = tokio::io::duplex(4096);
    let mut client = Client::new(
        reader,
        Vec::<u8>::new(),
        config(200000, budget.clone()),
        |_| Ok(async { Err(Error::Unavailable) }),
        |_| Ok(()),
        |_| Decision::Defer,
    )
    .unwrap();
    assert!(matches!(client.next().await.unwrap(), Event::Progress));
    tokio::time::advance(Duration::from_secs(11)).await;
    assert!(matches!(client.next().await, Err(Error::Timeout)));
    drop(client);
    assert_eq!(budget.used(), 0);
}

#[tokio::test]
async fn control_queue_is_bounded_and_cancellation_releases_it() {
    let (mut client, _, _, _, _, budget) = pair().await;
    for _ in 0..4 {
        client.change_policy(200000).unwrap();
    }
    assert!(matches!(
        client.change_policy(200000),
        Err(Error::Unavailable)
    ));
    assert_eq!(client.status().queued_controls, 0);
    assert_eq!(budget.used(), 0);
}

#[tokio::test]
async fn unknown_ack_is_refused_with_the_exact_error_and_no_upload_completion() {
    let (mut client, mut reader, mut writer, _, scope, budget) = pair().await;
    let ack = scope
        .committed_ack(
            Unit::Identity {
                profile_id: scope.profile,
            },
            [99; 8],
        )
        .unwrap();
    records::write(
        &mut writer,
        &ack,
        Instant::now() + Duration::from_secs(10),
        &budget,
    )
    .await
    .unwrap();
    assert!(matches!(client.next().await, Err(Error::Invalid)));
    let denial = records::read(
        &mut reader,
        Instant::now() + Duration::from_secs(10),
        &budget,
    )
    .await
    .unwrap();
    assert!(
        matches!(denial.record(), Record::Peer(PeerFrame::Error { code })
        if code == "sync_transfer_invalid")
    );
}

#[tokio::test]
async fn policy_change_before_handoff_discards_the_unfinished_download() {
    let (mut client, _, mut writer, mut stream, scope, budget) = pair().await;
    let (start, _) = stream
        .begin_outgoing(
            Unit::Identity {
                profile_id: scope.profile,
            },
            [12; 8],
            vec![1; 100],
            0,
            0,
            |_| Ok(()),
        )
        .unwrap();
    records::write(
        &mut writer,
        &Record::Control(start),
        Instant::now() + Duration::from_secs(10),
        &budget,
    )
    .await
    .unwrap();
    while client.stream.incoming_deadline().is_none() {
        client.next().await.unwrap();
    }
    assert!(matches!(
        client.change_policy(transfer::RESTRICTED_LIMIT),
        Err(Error::PolicyChanged)
    ));
    assert!(!client.status().committing);
    drop((client, stream));
    assert_eq!(budget.used(), 0);
}
