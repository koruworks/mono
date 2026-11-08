// SPDX-License-Identifier: MPL-2.0

//! Real client reconciliation and Store jobs against an in-memory remote Loro
//! replica using the standard carrier. Remote Ack is an explicit model, not a
//! PostgreSQL/disk-worker or physical-device durability claim.
use super::*;
use crate::daemon::cloud_sync;
use client_io::reconcile::{Answer, Event, Phase, Reconciler};
use koru_replication::cloud::transfer::{Admission, Control, Decision, Error};
use loro::{ExportMode, LoroDoc, VersionVector};

type SyncClient = Reconciler<'static, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

mod claim;
mod live;

fn map_failure<T>(result: Result<T, Failure>) -> Result<Answer<T>, Error> {
    match result {
        Ok(value) => Ok(Answer::Ready(value)),
        Err(Failure::Transfer(error)) => Err(error),
        Err(Failure::Store(_)) => Err(Error::Unavailable),
    }
}

fn bind(client: IoClient, worker: StoreWorkerClient, scope: Scope) -> SyncClient {
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
                .submit_cloud_export(request, Timestamp::from_millis(3), |_, _, _| Ok(()))
                .map_err(|_| Error::Unavailable)?;
            Ok(async move { map_failure(reply.await.map_err(|_| Error::Unavailable)?) })
        },
    )
}

struct RemoteResult {
    doc: LoroDoc,
    uploaded: usize,
    downloaded: usize,
    upload_deferred: bool,
    download_deferred: bool,
}

async fn remote(
    mut peer: DuplexStream,
    mut stream: Stream,
    scope: Scope,
    doc: LoroDoc,
    budget: MemoryBudget,
    hold_upload: bool,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> RemoteResult {
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
    if hold_upload {
        let policy = stream.change_policy(transfer::RESTRICTED_LIMIT, 0).unwrap();
        records::write(&mut peer, &Record::Control(policy), deadline, &budget)
            .await
            .unwrap();
    }
    // Exercise a content-free wake without treating it as coverage or payload.
    records::write(
        &mut peer,
        &Record::Peer(koru_replication::protocol::PeerFrame::SyncRequest),
        deadline,
        &budget,
    )
    .await
    .unwrap();
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
    let known = request
        .known_version
        .as_ref()
        .map(|v| VersionVector::decode(v).unwrap())
        .unwrap_or_default();
    let needs_upload = !doc.oplog_vv().includes_vv(&known);
    let needs_download = request.needs_payload(&doc.oplog_vv().encode()).unwrap();
    records::write(
        &mut peer,
        &request
            .response(scope, doc.oplog_vv().encode())
            .unwrap()
            .unwrap(),
        deadline,
        &budget,
    )
    .await
    .unwrap();
    let mut result = RemoteResult {
        doc,
        uploaded: 0,
        downloaded: 0,
        upload_deferred: false,
        download_deferred: false,
    };
    let mut download_done = !needs_download;
    let mut upload_done = !needs_upload;
    if needs_download {
        let bytes = if known.is_empty() {
            result.doc.export(ExportMode::snapshot())
        } else {
            result.doc.export(ExportMode::updates(&known))
        }
        .unwrap();
        let (start, admission) = stream
            .begin_outgoing(request.unit, [25; 8], bytes, 0, 0, |_| Ok(()))
            .unwrap();
        records::write(&mut peer, &Record::Control(start), deadline, &budget)
            .await
            .unwrap();
        let send = if admission == Admission::AwaitDecision {
            let (_, Record::Control(decision), _) = records::read(&mut peer, deadline, &budget)
                .await
                .unwrap()
                .into_parts()
            else {
                panic!("decision")
            };
            match stream.receive_control(decision, 0, 0, |_| Ok(())).unwrap() {
                Admission::Accepted => true,
                Admission::Deferred => {
                    result.download_deferred = true;
                    download_done = true;
                    false
                }
                _ => panic!("decision outcome"),
            }
        } else {
            true
        };
        if send {
            while let Some(record) = stream.next_outgoing(0, |_| Ok(())).unwrap() {
                records::write(&mut peer, record.record(), deadline, &budget)
                    .await
                    .unwrap();
            }
            stream.outbound_written(0).unwrap();
        }
    }
    while !upload_done || !download_done {
        let received = records::read(&mut peer, deadline, &budget).await.unwrap();
        match received.into_parts().1 {
            Record::Control(control) => {
                let start = if let Control::TransferStart(start) = &control {
                    Some(start.clone())
                } else {
                    None
                };
                if stream.receive_control(control, 0, 0, |_| Ok(())).unwrap()
                    == Admission::AwaitDecision
                {
                    let start = start.unwrap();
                    let decision = stream
                        .decide(&start, Decision::Defer, 0, |_| Ok(()))
                        .unwrap();
                    records::write(&mut peer, &Record::Control(decision), deadline, &budget)
                        .await
                        .unwrap();
                    result.upload_deferred = true;
                    upload_done = true;
                }
            }
            Record::Peer(frame) => {
                let (_, frame) = stream.receive_peer_owned(frame, 0, |_| Ok(())).unwrap();
                if let Some(frame) = frame {
                    assert!(
                        matches!(metadata::decode(scope, frame, 1).unwrap().as_slice(),
                        [metadata::Metadata::Ack { unit, batch, ok: true }] if *unit == request.unit && *batch == [25; 8])
                    );
                    assert!(!download_done);
                    result.downloaded += 1;
                    download_done = true;
                }
                if stream.status().incoming_complete {
                    let complete = stream.handoff(0, |_| Ok(()), |v| v).unwrap();
                    let status = result.doc.import(&complete.bytes).unwrap();
                    assert!(status.pending.is_none());
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
                    result.uploaded += 1;
                    upload_done = true;
                }
            }
        }
    }
    let _ = (&mut shutdown).await;
    result
}

async fn exchange(local_edit: bool, remote_edit: bool) {
    exercise(local_edit, remote_edit, false, false, false).await;
}

async fn exercise(
    local_edit: bool,
    remote_edit: bool,
    large: bool,
    hold_upload: bool,
    hold_download: bool,
) {
    let mut f = Fixture::new();
    let remote_doc = LoroDoc::new();
    remote_doc
        .import(
            &f.store
                .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
                .unwrap(),
        )
        .unwrap();
    if local_edit {
        f.store
            .apply_profile_unit_updates(
                ProfileUnitRef::Identity { profile: f.profile },
                &[f.bytes.clone()],
            )
            .unwrap();
    }
    if remote_edit {
        remote_doc.set_peer_id(901).unwrap();
        remote_doc
            .get_map("meta")
            .insert("purpose", "Remote purpose")
            .unwrap();
        remote_doc.commit();
    }
    if large {
        let mut state = 0x12345678u32;
        let purpose: String = (0..350_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from(32 + (state % 95) as u8)
            })
            .collect();
        if local_edit {
            let local = LoroDoc::new();
            local
                .import(
                    &f.store
                        .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
                        .unwrap(),
                )
                .unwrap();
            local.set_peer_id(902).unwrap();
            let before = local.oplog_vv();
            local
                .get_map("meta")
                .insert("purpose", purpose.as_str())
                .unwrap();
            local.commit();
            let bytes = local.export(ExportMode::updates(&before)).unwrap();
            assert!(bytes.len() > transfer::RESTRICTED_LIMIT);
            f.store
                .apply_profile_unit_updates(
                    ProfileUnitRef::Identity { profile: f.profile },
                    &[bytes],
                )
                .unwrap();
        } else {
            let before = remote_doc.oplog_vv();
            remote_doc
                .get_map("meta")
                .insert("purpose", purpose.as_str())
                .unwrap();
            remote_doc.commit();
            assert!(
                remote_doc
                    .export(ExportMode::updates(&before))
                    .unwrap()
                    .len()
                    > transfer::RESTRICTED_LIMIT
            );
        }
    }
    let (worker, mut requests) = queue(4);
    let limit = if hold_download {
        transfer::RESTRICTED_LIMIT
    } else {
        transfer::MAX_PAYLOAD
    };
    let (client, peer, server, budget) =
        client_pair_with_policy(&f, worker.clone(), limit, |_| Decision::Defer);
    let mut sync = bind(client, worker, f.scope);
    sync.start(f.unit).unwrap();
    assert_eq!(sync.status().phase, Phase::Reading);
    assert!(matches!(sync.start(f.unit), Err(Error::Unavailable)));
    let (done, shutdown) = tokio::sync::oneshot::channel();
    let driver = async {
        let mut settled = false;
        let mut hints = 0;
        let mut uploaded = 0;
        let mut downloaded = 0;
        let mut deferred = 0;
        loop {
            let status = sync.status();
            if settled
                && !status.transport.committing
                && !status.transport.writing
                && status.transport.queued_controls == 0
            {
                break;
            }
            tokio::select! {
                event = sync.next() => match event.unwrap() {
                    Event::Settled { unit, upload_deferred, download_deferred, .. } => {
                        assert_eq!(unit, f.unit);
                        assert_eq!(upload_deferred, hold_upload);
                        assert_eq!(download_deferred, hold_download);
                        settled = true;
                    }
                    Event::RemoteHint => hints += 1,
                    Event::Uploaded { .. } => uploaded += 1,
                    Event::Downloaded(_) => downloaded += 1,
                    Event::Deferred { start, upload } => {
                        assert_eq!(start.unit, f.unit);
                        assert!(start.total_length > transfer::RESTRICTED_LIMIT);
                        assert!(if upload { hold_upload } else { hold_download });
                        deferred += 1;
                    }
                    Event::Refused { unit, code } => {
                        panic!("no Store job refuses here: {unit:?} {code}")
                    }
                    Event::Progress => {}
                },
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    task(Ok(&mut f.store));
                }
            }
        }
        assert_eq!(hints, 1);
        assert_eq!(uploaded, usize::from(local_edit && !hold_upload));
        assert_eq!(downloaded, usize::from(remote_edit && !hold_download));
        assert_eq!(
            deferred,
            usize::from(hold_upload) + usize::from(hold_download)
        );
        done.send(()).unwrap();
    };
    let server = remote(
        peer,
        server,
        f.scope,
        remote_doc,
        budget.clone(),
        hold_upload,
        shutdown,
    );
    let (_, result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(driver, server)
    })
    .await
    .expect("bounded exchange");
    assert_eq!(result.uploaded, usize::from(local_edit && !hold_upload));
    assert_eq!(
        result.downloaded,
        usize::from(remote_edit && !hold_download)
    );
    assert_eq!(result.upload_deferred, hold_upload);
    assert_eq!(result.download_deferred, hold_download);
    let local = LoroDoc::new();
    local
        .import(
            &f.store
                .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
                .unwrap(),
        )
        .unwrap();
    if hold_upload || hold_download {
        assert_ne!(
            local.oplog_vv(),
            result.doc.oplog_vv(),
            "held bytes were not imported"
        );
    } else {
        assert_eq!(local.oplog_vv(), result.doc.oplog_vv());
        assert_eq!(local.get_deep_value(), result.doc.get_deep_value());
    }
    assert_eq!(sync.status().phase, Phase::Idle);
    assert_eq!(f.store.open_workspaces(), 0);
    drop(sync);
    assert_eq!(budget.used(), 0);
}

#[tokio::test]
async fn unchanged_unit_finishes_without_payload_or_false_upload() {
    exchange(false, false).await;
}
#[tokio::test]
async fn local_edit_is_exported_and_acknowledged() {
    exchange(true, false).await;
}
#[tokio::test]
async fn remote_edit_is_downloaded_and_checked_after_store_commit() {
    exchange(false, true).await;
}
#[tokio::test]
async fn concurrent_edits_converge_through_both_directions() {
    exchange(true, true).await;
}

#[tokio::test]
async fn large_upload_completes_when_remote_policy_allows_it() {
    exercise(true, false, true, false, false).await;
}

#[tokio::test]
async fn remote_upload_deferral_is_reported_without_waiting_for_an_ack() {
    exercise(true, false, true, true, false).await;
}

#[tokio::test]
async fn local_download_deferral_settles_as_held_not_as_converged() {
    exercise(false, true, true, false, true).await;
}

#[tokio::test]
async fn canceled_next_wait_keeps_the_original_store_read_job() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (client, _peer, server, budget) = client_pair(&f, worker.clone());
    let mut sync = bind(client, worker, f.scope);
    sync.start(f.unit).unwrap();
    for _ in 0..2 {
        std::future::poll_fn(|cx| {
            loop {
                let mut wait = Box::pin(sync.next());
                match wait.as_mut().poll(cx) {
                    Poll::Ready(Ok(Event::Progress)) => {}
                    Poll::Pending => return Poll::Ready(()),
                    _ => panic!("unexpected progress before Store execution"),
                }
            }
        })
        .await;
        assert_eq!(sync.status().phase, Phase::Reading);
        assert_eq!(
            requests.len(),
            1,
            "canceled waits never enqueue another job"
        );
    }
    drop(sync);
    execute(&mut requests, &mut f.store);
    drop(server);
    assert_eq!(budget.used(), 0);
}

#[tokio::test(start_paused = true)]
async fn ready_store_reply_cannot_extend_the_original_initialization_deadline() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (client, _peer, _server, _budget) = client_pair(&f, worker.clone());
    let mut sync = bind(client, worker, f.scope);
    sync.start(f.unit).unwrap();
    tokio::time::advance(Duration::from_millis(transfer::CONTROL_WAIT_MS)).await;
    execute(&mut requests, &mut f.store);
    assert!(matches!(sync.next().await, Err(Error::Timeout)));
    assert_eq!(sync.status().phase, Phase::Idle);
}

#[tokio::test]
async fn canceled_reconciliation_keeps_join_metadata_with_the_accepted_export_job() {
    let mut f = Fixture::new();
    let (worker, mut requests) = queue(1);
    let (client, mut peer, mut server, budget) = client_pair(&f, worker.clone());
    let scope = f.scope;
    let mut sync = bind(client, worker, scope);
    sync.start(f.unit).unwrap();
    let reach_export = async {
        while sync.status().phase != Phase::Exporting {
            tokio::select! {
                event = sync.next() => { assert!(event.is_ok()); },
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    task(Ok(&mut f.store));
                }
            }
        }
    };
    let setup = async {
        let deadline = Instant::now() + Duration::from_secs(10);
        records::write(
            &mut peer,
            &Record::Control(server.initial_policy(0).unwrap()),
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
        server.receive_control(policy, 0, 0, |_| Ok(())).unwrap();
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
        let response = request
            .response(scope, VersionVector::new().encode())
            .unwrap()
            .unwrap();
        records::write(&mut peer, &response, deadline, &budget)
            .await
            .unwrap();
        peer
    };
    let (_, _peer) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(reach_export, setup)
    })
    .await
    .unwrap();
    assert_eq!(requests.len(), 1);
    drop(sync);
    drop(server);
    assert!(
        budget.used() > transfer::MAX_PAYLOAD,
        "queued export retains decoded metadata as well as its payload allowance"
    );
    execute(&mut requests, &mut f.store);
    assert_eq!(budget.used(), 0);
}

/// The catalog driver over the same in-memory remote: a Profile plan settles
/// its Identity unit through the eager Store ports and reports the hint.
#[tokio::test]
async fn dataset_driver_settles_a_profile_plan_and_reports_the_remote_hint() {
    use crate::daemon::cloud_sync::Plan;
    let mut f = Fixture::new();
    let remote_doc = LoroDoc::new();
    remote_doc
        .import(
            &f.store
                .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
                .unwrap(),
        )
        .unwrap();
    remote_doc.set_peer_id(903).unwrap();
    remote_doc
        .get_map("meta")
        .insert("purpose", "Driver purpose")
        .unwrap();
    remote_doc.commit();
    let (worker, mut requests) = queue(4);
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
    let mut seen = Vec::new();
    let driver = async {
        let pass = cloud_sync::reconcile_dataset(
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
            Plan::Profile {
                profile_id: f.profile,
                private: false,
                personal_workspaces: Vec::new(),
            },
            worker.clone(),
            Arc::new(|_| Ok(())),
            Arc::new(cloud_sync::NoDownloads),
            Arc::new(std::sync::Mutex::new(cloud_sync::Deferrals::new())),
            |_| Decision::Defer,
            |event| seen.push(format!("{event:?}")),
        );
        tokio::pin!(pass);
        let outcome = loop {
            tokio::select! {
                result = &mut pass => break result.unwrap(),
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    task(Ok(&mut f.store));
                }
            }
        };
        done.send(()).unwrap();
        outcome
    };
    let server = remote(
        peer,
        server,
        f.scope,
        remote_doc,
        budget.clone(),
        false,
        shutdown,
    );
    let (outcome, result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(driver, server)
    })
    .await
    .expect("bounded exchange");
    assert_eq!(outcome.settled, vec![f.unit]);
    assert!(outcome.deferred.is_empty());
    assert!(outcome.remote_hint);
    assert_eq!(result.downloaded, 1);
    assert_eq!(result.uploaded, 0);
    assert!(seen.iter().any(|event| event.starts_with("Started")));
    assert!(seen.iter().any(|event| event.starts_with("Downloaded")));
    assert!(seen.iter().any(|event| event.starts_with("Settled")));
    let local = LoroDoc::new();
    local
        .import(
            &f.store
                .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(local.get_deep_value(), result.doc.get_deep_value());
    assert_eq!(f.store.open_workspaces(), 0);
}

/// A remote that agrees with whatever the device already holds: every join is
/// answered with the version the device advertised, so nothing transfers in
/// either direction and each unit settles at once. That is the right model for
/// a test about **which units a pass reaches**, because it adds no payload of
/// its own to confuse the question. It answers with the units it was joined
/// for, which is the whole evidence the test needs from the wire.
async fn agreeing_remote(
    mut peer: DuplexStream,
    mut stream: Stream,
    scope: Scope,
    budget: MemoryBudget,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Vec<Unit> {
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
    let mut joined = Vec::new();
    loop {
        let received = tokio::select! {
            _ = &mut shutdown => break,
            received = records::read(&mut peer, deadline, &budget) => received,
        };
        let Ok(received) = received else { break };
        let Record::Peer(frame) = received.into_parts().1 else {
            continue;
        };
        for message in metadata::decode(scope, frame, 1).unwrap() {
            let metadata::Metadata::Request(request) = message else {
                continue;
            };
            joined.push(request.unit);
            let version = request
                .known_version
                .clone()
                .unwrap_or_else(|| VersionVector::new().encode());
            let response = request.response(scope, version).unwrap().unwrap();
            records::write(&mut peer, &response, deadline, &budget)
                .await
                .unwrap();
        }
    }
    joined
}

/// One whole pass over an Identity-and-Private Profile plan against
/// [`agreeing_remote`].
///
/// `refuse_job` names a Store job — counted from one, in the order the pass
/// submits them — to run against `absent`, a Store that holds no Profile at
/// all. That is a real refusal through the real port: `profile_unit_version`
/// answers `profile_not_found`, which is `Class::NotFound` and so a statement
/// about the unit rather than the session. Nothing is stubbed and no error is
/// injected into the carrier.
async fn agreeing_pass(
    f: &mut Fixture,
    absent: &mut crate::WorkerStore,
    refuse_job: Option<usize>,
    deferrals: &cloud_sync::DeferralLedger,
) -> (cloud_sync::Outcome, Vec<Unit>) {
    use crate::daemon::cloud_sync::Plan;
    let (worker, mut requests) = queue(4);
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
    let driver = async {
        let pass = cloud_sync::reconcile_dataset(
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
            Plan::Profile {
                profile_id: f.profile,
                private: true,
                personal_workspaces: Vec::new(),
            },
            worker.clone(),
            Arc::new(|_| Ok(())),
            Arc::new(cloud_sync::NoDownloads),
            Arc::clone(deferrals),
            |_| Decision::Defer,
            |_| {},
        );
        tokio::pin!(pass);
        let mut job = 0usize;
        let outcome = loop {
            tokio::select! {
                result = &mut pass => break result.unwrap(),
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    job += 1;
                    if refuse_job == Some(job) {
                        task(Ok(absent));
                    } else {
                        task(Ok(&mut f.store));
                    }
                }
            }
        };
        done.send(()).unwrap();
        outcome
    };
    let joined = agreeing_remote(peer, server, f.scope, budget.clone(), shutdown);
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(driver, joined)
    })
    .await
    .expect("bounded exchange")
}

/// A Store that holds no Profile: the refusal lever the tests below use.
fn absent_store() -> crate::WorkerStore {
    crate::WorkerStore::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::new(std::path::PathBuf::new()),
    )
    .unwrap()
}

/// The units beside a refused one still sync.
///
/// This is the outage: one unit a Store job would not serve ended the pass for
/// everything else in the dataset, on every pass, for as long as the condition
/// lasted. The refused unit here is the **first** one the plan names, so
/// nothing after it would run at all if the refusal still ended the pass.
#[tokio::test]
async fn a_refused_unit_does_not_stop_the_units_beside_it() {
    let mut f = Fixture::new();
    // A Private unit with real content, so the neighbour is an ordinary unit
    // rather than an absent one.
    f.store
        .set_dictionary_word(f.profile, "en", "koru", true)
        .unwrap();
    let mut absent = absent_store();
    let deferrals: cloud_sync::DeferralLedger =
        Arc::new(std::sync::Mutex::new(cloud_sync::Deferrals::new()));
    // Job 1 is the Identity unit's version read, the first act of the first
    // unit in the plan.
    let (outcome, joined) = agreeing_pass(&mut f, &mut absent, Some(1), &deferrals).await;

    let private = Unit::Private {
        profile_id: CloudId::from_bytes(f.profile.as_uuid().into_bytes()),
    };
    assert_eq!(
        outcome.refused,
        vec![(f.unit, koru_store_core::errors::Code::ProfileNotFound)],
        "the Store's own code survives to the caller"
    );
    assert_eq!(outcome.deferred, vec![f.unit]);
    assert_eq!(
        outcome.settled,
        vec![private],
        "the unit beside the refused one reconciled"
    );
    assert!(
        outcome.unavailable.is_empty(),
        "one refusal is not yet a verdict"
    );
    assert_eq!(
        joined,
        vec![private],
        "the carrier survived the refusal and carried the next unit"
    );
}

/// A unit that keeps being refused stops being silent.
///
/// Silent permanent deferral looks like success, which is worse than the loud
/// stop it replaced: the pass would report itself finished forever while one
/// unit never synced. The bound is `MAX_CONSECUTIVE_REFUSALS`, and the third
/// pass is where it is crossed.
#[tokio::test]
async fn a_unit_refused_on_every_pass_becomes_unavailable_rather_than_silent() {
    let mut f = Fixture::new();
    f.store
        .set_dictionary_word(f.profile, "en", "koru", true)
        .unwrap();
    let mut absent = absent_store();
    let deferrals: cloud_sync::DeferralLedger =
        Arc::new(std::sync::Mutex::new(cloud_sync::Deferrals::new()));
    let private = Unit::Private {
        profile_id: CloudId::from_bytes(f.profile.as_uuid().into_bytes()),
    };

    for pass in 1..=cloud_sync::MAX_CONSECUTIVE_REFUSALS {
        let (outcome, _) = agreeing_pass(&mut f, &mut absent, Some(1), &deferrals).await;
        assert_eq!(outcome.settled, vec![private], "pass {pass} still ran");
        assert_eq!(outcome.refused.len(), 1, "pass {pass} refused the Identity");
        if pass < cloud_sync::MAX_CONSECUTIVE_REFUSALS {
            assert!(
                outcome.unavailable.is_empty(),
                "pass {pass} is still inside the bound, so a transient rides it out"
            );
        } else {
            assert_eq!(
                outcome.unavailable,
                vec![(f.unit, koru_store_core::errors::Code::ProfileNotFound)],
                "the bound is crossed and the unit is named out loud"
            );
        }
    }
}

/// A unit that is served again starts its run over: consecutive means
/// consecutive, or one refusal a week apart would eventually condemn a unit
/// that is working.
#[tokio::test]
async fn a_unit_that_syncs_again_clears_its_run_of_refusals() {
    let mut f = Fixture::new();
    f.store
        .set_dictionary_word(f.profile, "en", "koru", true)
        .unwrap();
    let mut absent = absent_store();
    let deferrals: cloud_sync::DeferralLedger =
        Arc::new(std::sync::Mutex::new(cloud_sync::Deferrals::new()));

    for _ in 0..2 {
        let (outcome, _) = agreeing_pass(&mut f, &mut absent, Some(1), &deferrals).await;
        assert!(outcome.unavailable.is_empty());
    }
    // A pass where nothing is refused.
    let (outcome, _) = agreeing_pass(&mut f, &mut absent, None, &deferrals).await;
    assert!(outcome.refused.is_empty());
    assert_eq!(outcome.settled.len(), 2);
    // ...and the run starts again from nothing rather than at the bound.
    let (outcome, _) = agreeing_pass(&mut f, &mut absent, Some(1), &deferrals).await;
    assert_eq!(outcome.refused.len(), 1);
    assert!(
        outcome.unavailable.is_empty(),
        "the third refusal overall is the first of its run"
    );
}
