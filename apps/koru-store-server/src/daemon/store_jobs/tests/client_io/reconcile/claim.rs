// SPDX-License-Identifier: MPL-2.0

//! The upload context through the Reconciler (TECHNICAL §5.4 item 4): a
//! Store port's export answer carries an `UploadContext` for its unit, the
//! Reconciler hands it to the carrier, and the claim reference in it is what
//! the worker's handoff holds. A hosted replica's Store port attaches it the
//! way [`bind_with`] does, from the effect on that unit; a device's never
//! does. The unit here is a Profile unit only because the fixture's Store
//! holds one: the carrier checks the session, not the unit (DS-D46).

use super::*;
use koru_replication::cloud::transfer::{ClaimReferences, Completed, UploadContext};

const CLAIM: [u8; 32] = [0x3c; 32];

/// [`bind`] whose export port answers every export with `context` attached.
fn bind_with(
    client: IoClient,
    worker: StoreWorkerClient,
    scope: Scope,
    context: UploadContext,
) -> SyncClient {
    let versions = worker.clone();
    Reconciler::new(
        client,
        move |unit| {
            let reply = versions
                .submit_store(move |store| {
                    client_store::version(
                        store,
                        scope,
                        unit,
                        Timestamp::from_millis(3),
                        |_, _, _| Ok(()),
                    )
                })
                .map_err(|_| Error::Unavailable)?;
            Ok(async move { map_failure(reply.await.map_err(|_| Error::Unavailable)?) })
        },
        move |request| {
            let reply = worker
                .submit_store(move |store| {
                    request
                        .run(store, Timestamp::from_millis(3), |_, _, _| Ok(()))
                        .map(|export| export.map(|export| export.with_upload(context)))
                })
                .map_err(|_| Error::Unavailable)?;
            Ok(async move { map_failure(reply.await.map_err(|_| Error::Unavailable)?) })
        },
    )
}

/// The worker's end of one pass: it answers the join with Cloud's empty
/// version, takes the upload that follows, acknowledges it, and answers the
/// handoff. The stream is returned so it stays open until the pass settles.
async fn worker_end(
    mut peer: DuplexStream,
    mut stream: Stream,
    scope: Scope,
    budget: MemoryBudget,
) -> (Completed, DuplexStream) {
    let deadline = Instant::now() + Duration::from_secs(10);
    records::write(
        &mut peer,
        &Record::Control(stream.initial_policy(0).unwrap()),
        deadline,
        &budget,
    )
    .await
    .unwrap();
    let (_, Record::Control(policy), _) = records::read(&mut peer, deadline, &budget)
        .await
        .unwrap()
        .into_parts()
    else {
        panic!("policy")
    };
    stream.receive_control(policy, 0, 0, |_| Ok(())).unwrap();
    let (_, Record::Peer(frame), _) = records::read(&mut peer, deadline, &budget)
        .await
        .unwrap()
        .into_parts()
    else {
        panic!("join")
    };
    let mut messages = metadata::decode(scope, frame, 1).unwrap();
    let metadata::Metadata::Request(request) = messages.remove(0) else {
        panic!("request")
    };
    records::write(
        &mut peer,
        &request
            .response(scope, VersionVector::default().encode())
            .unwrap()
            .unwrap(),
        deadline,
        &budget,
    )
    .await
    .unwrap();
    loop {
        match records::read(&mut peer, deadline, &budget)
            .await
            .unwrap()
            .into_parts()
            .1
        {
            Record::Control(control) => {
                stream.receive_control(control, 0, 0, |_| Ok(())).unwrap();
            }
            Record::Peer(frame) => {
                stream.receive_peer(frame, 0, |_| Ok(())).unwrap();
            }
        }
        if stream.status().incoming_complete {
            let complete = stream.handoff(0, |_| Ok(()), |v| v).unwrap();
            records::write(
                &mut peer,
                &scope
                    .committed_ack(complete.unit, complete.batch_id)
                    .unwrap(),
                deadline,
                &budget,
            )
            .await
            .unwrap();
            return (complete, peer);
        }
    }
}

/// One pass of the fixture's unit over a session admitted with `claims`,
/// whose Store port attaches `context`. Answers the worker's handoff.
async fn pass(claims: (ClaimReferences, ClaimReferences), context: UploadContext) -> Completed {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(4);
    let (client, peer, server, budget) = client_pair_with_claims(
        &f,
        worker.clone(),
        transfer::MAX_PAYLOAD,
        |_| Decision::Defer,
        claims,
    );
    let mut sync = bind_with(client, worker, f.scope, context);
    sync.start(f.unit).unwrap();
    let unit = f.unit;
    let driver = async {
        loop {
            tokio::select! {
                event = sync.next() => {
                    if let Event::Settled { unit: settled, .. } = event.unwrap() {
                        assert_eq!(settled, unit);
                        break;
                    }
                }
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    task(Ok(&mut f.store));
                }
            }
        }
    };
    let ((), (complete, _peer)) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(driver, worker_end(peer, server, f.scope, budget.clone()))
    })
    .await
    .expect("bounded pass");
    assert_eq!(complete.unit, unit);
    complete
}

#[tokio::test]
async fn a_store_ports_upload_context_rides_through_the_reconciler_to_the_worker() {
    let complete = pass(
        (ClaimReferences::Sent, ClaimReferences::Received),
        UploadContext { claim: Some(CLAIM) },
    )
    .await;
    assert_eq!(complete.claim, Some(CLAIM));
}

#[tokio::test]
async fn a_devices_pass_uploads_without_a_reference() {
    let complete = pass(
        (ClaimReferences::None, ClaimReferences::None),
        UploadContext::default(),
    )
    .await;
    assert_eq!(complete.claim, None);
}
