// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_api::cloud_changes::{Changes, Dataset};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn local_commit_reconciles_again_on_the_same_stream_then_fence_stops_it() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(4);
    let changes = Arc::new(Changes::default());
    let allowed = Arc::new(AtomicBool::new(true));
    let passes = Arc::new(AtomicUsize::new(0));
    let budget = MemoryBudget::new(64 * 1024 * 1024);
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
        claim_references: koru_replication::cloud::transfer::ClaimReferences::None,
    };
    let server = Stream::new(
        &accepted,
        f.scope,
        0,
        0,
        transfer::MAX_PAYLOAD,
        budget.clone(),
    )
    .unwrap();
    let (a, peer) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(a);
    let (done, shutdown) = tokio::sync::oneshot::channel();
    let gate = allowed.clone();
    let progress_gate = allowed.clone();
    let progress_changes = changes.clone();
    let progress_passes = passes.clone();
    let profile = f.profile;
    let driver = async {
        let work = cloud_sync::subscribe_dataset(
            reader,
            writer,
            cloud_sync::Session {
                accepted,
                scope: f.scope,
                automatic_limit: transfer::MAX_PAYLOAD,
                capacity: NonZeroUsize::new(4).unwrap(),
                budget: budget.clone(),
                observed: Instant::now(),
                wall_millis: 0,
            },
            cloud_sync::Plan::Profile {
                profile_id: profile,
                private: false,
                personal_workspaces: vec![],
            },
            worker,
            Arc::new(move |_| {
                if gate.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(koru_replication::cloud::Refusal::ReplicaFenced)
                }
            }),
            Arc::new(cloud_sync::NoDownloads),
            Arc::new(std::sync::Mutex::new(cloud_sync::Deferrals::new())),
            |_| Decision::Accept,
            move |progress| {
                if let cloud_sync::Progress::PassFinished(outcome) = progress {
                    assert_eq!(outcome.settled.len(), 1);
                    let count = progress_passes.fetch_add(1, Ordering::AcqRel) + 1;
                    if count == 1 {
                        // Hundreds of changes remain one wake, including a change
                        // after the pass's read but before its idle receiver polls.
                        for _ in 0..100 {
                            progress_changes.changed(Dataset::Profile(profile));
                        }
                    } else {
                        progress_gate.store(false, Ordering::Release);
                    }
                }
            },
        );
        tokio::pin!(work);
        let result = loop {
            tokio::select! {
                result = &mut work => break result,
                command = requests.recv() => match command.unwrap() {
                    WorkerCommand::Store(task) => task(Ok(&mut f.store)),
                    WorkerCommand::WatchCloud { dataset, reply } => { let _ = reply.send(changes.subscribe(dataset)); }
                    _ => panic!("unexpected job"),
                },
            }
        };
        assert!(matches!(
            result,
            Err(Error::Authority(
                koru_replication::cloud::Refusal::ReplicaFenced
            ))
        ));
        let _ = done.send(());
    };
    let scope = f.scope;
    let remote = agreeing_remote(peer, server, scope, budget.clone(), shutdown);
    let (_, joined) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(driver, remote)
    })
    .await
    .expect("woken by the commit, not the 60-second recovery timer");
    assert_eq!(passes.load(Ordering::Acquire), 2);
    assert_eq!(joined, vec![f.unit, f.unit]);
}
