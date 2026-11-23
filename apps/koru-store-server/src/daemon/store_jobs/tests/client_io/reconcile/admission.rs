// SPDX-License-Identifier: MPL-2.0

//! Remembered upload admission holds across passes, against a worker end that
//! refuses on metadata. Real reconciler, real Store jobs and the standard
//! carrier; the worker is a script, and the 60-second cycle is a test clock.
use super::*;
use client_io::reconcile::UploadAdmissionHolds;
use koru_account_core::cloud_admission::Reason;
use koru_replication::protocol::PeerFrame;
use std::sync::atomic::{AtomicI64, Ordering};

const CYCLE: i64 = 60_000;

fn holds() -> (UploadAdmissionHolds, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(1_700_000_000_000));
    let read = now.clone();
    (
        UploadAdmissionHolds::with_clock(move || read.load(Ordering::SeqCst)),
        now,
    )
}

/// A Store one local edit ahead of the replica Cloud holds.
fn edited() -> (Fixture, LoroDoc) {
    let mut f = Fixture::new();
    let cloud = local(&mut f);
    f.store
        .apply_profile_unit_updates(
            ProfileUnitRef::Identity { profile: f.profile },
            &[f.bytes.clone()],
        )
        .unwrap();
    (f, cloud)
}

fn local(f: &mut Fixture) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.import(
        &f.store
            .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
            .unwrap(),
    )
    .unwrap();
    doc
}

/// The worker end of one dataset stream.
struct Worker {
    peer: DuplexStream,
    stream: Stream,
    scope: Scope,
    budget: MemoryBudget,
    doc: LoroDoc,
    starts: usize,
}

fn session(
    f: &Fixture,
    doc: LoroDoc,
    holds: &UploadAdmissionHolds,
    deciding: bool,
) -> (SyncClient, Worker, mpsc::Receiver<WorkerCommand>) {
    let (store, requests) = queue(4);
    let (client, peer, stream, budget) = client_pair(f, store.clone());
    let worker = Worker {
        peer,
        stream: stream.decision_required(deciding).unwrap(),
        scope: f.scope,
        budget,
        doc,
        starts: 0,
    };
    let sync = bind(client, store, f.scope).with_admission_holds(holds.clone());
    (sync, worker, requests)
}

impl Worker {
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    async fn read(&mut self) -> Record {
        records::read(&mut self.peer, Self::deadline(), &self.budget)
            .await
            .unwrap()
            .into_parts()
            .1
    }

    async fn write(&mut self, record: &Record) {
        records::write(&mut self.peer, record, Self::deadline(), &self.budget)
            .await
            .unwrap();
    }

    async fn open(&mut self) {
        let policy = self.stream.initial_policy(0).unwrap();
        self.write(&Record::Control(policy)).await;
        let Record::Control(policy) = self.read().await else {
            panic!("policy")
        };
        self.stream
            .receive_control(policy, 0, 0, |_| Ok(()))
            .unwrap();
    }

    /// Answer the unit's join, and send whatever the client lacks. True when
    /// a download was sent, whose Ack the caller then expects.
    async fn join(&mut self) -> bool {
        let Record::Peer(frame) = self.read().await else {
            panic!("join")
        };
        let mut messages = metadata::decode(self.scope, frame, 1).unwrap();
        let metadata::Metadata::Request(request) = messages.remove(0) else {
            panic!("request")
        };
        let version = self.doc.oplog_vv();
        let download = request.needs_payload(&version.encode()).unwrap();
        let known = request
            .known_version
            .as_ref()
            .map(|v| VersionVector::decode(v).unwrap())
            .unwrap_or_default();
        let response = request
            .response(self.scope, version.encode())
            .unwrap()
            .unwrap();
        self.write(&response).await;
        if download {
            let bytes = self.doc.export(ExportMode::updates(&known)).unwrap();
            let (start, admission) = self
                .stream
                .begin_outgoing(request.unit, [25; 8], bytes, 0, 0, |_| Ok(()))
                .unwrap();
            assert_ne!(admission, Admission::AwaitDecision);
            self.write(&Record::Control(start)).await;
            while let Some(record) = self.stream.next_outgoing(0, |_| Ok(())).unwrap() {
                records::write(
                    &mut self.peer,
                    record.record(),
                    Self::deadline(),
                    &self.budget,
                )
                .await
                .unwrap();
            }
            self.stream.outbound_written(0).unwrap();
        }
        download
    }

    /// The client's next record: an upload start, or the Ack of a download.
    /// Anything else, a body frame above all, fails the stream state here.
    async fn next(&mut self) -> Option<transfer::Start> {
        match self.read().await {
            Record::Control(Control::TransferStart(start)) => {
                self.starts += 1;
                Some(start)
            }
            Record::Control(other) => panic!("unexpected control: {other:?}"),
            Record::Peer(frame) => {
                let (_, frame) = self
                    .stream
                    .receive_peer_owned(frame, 0, |_| Ok(()))
                    .unwrap();
                assert!(matches!(
                    metadata::decode(self.scope, frame.expect("metadata"), 1)
                        .unwrap()
                        .as_slice(),
                    [metadata::Metadata::Ack { ok: true, .. }]
                ));
                None
            }
        }
    }

    async fn start(&mut self) -> transfer::Start {
        self.next().await.expect("the upload start")
    }

    async fn quiet(&mut self) {
        let silent = tokio::time::timeout(
            Duration::from_millis(250),
            records::read(&mut self.peer, Self::deadline(), &self.budget),
        )
        .await
        .is_err();
        assert!(silent, "the client wrote a record nothing permitted");
    }

    /// Refuse on the start's metadata alone: no body was reserved or read.
    async fn refuse(&mut self, start: transfer::Start, reason: Reason) {
        self.stream
            .reject_start_admission(start, 0, 0, |_| Ok(()))
            .unwrap();
        assert_eq!(self.stream.status().incoming_bytes, 0);
        self.write(&Record::Peer(PeerFrame::Error {
            code: reason.code().into(),
        }))
        .await;
    }

    async fn accept(&mut self, start: transfer::Start) {
        assert_eq!(
            self.stream
                .receive_control(Control::TransferStart(start.clone()), 0, 0, |_| Ok(()))
                .unwrap(),
            Admission::AwaitDecision
        );
        let accept = self
            .stream
            .decide(&start, Decision::Accept, 0, |_| Ok(()))
            .unwrap();
        self.write(&Record::Control(accept)).await;
        while !self.stream.status().incoming_complete {
            let Record::Peer(frame) = self.read().await else {
                panic!("only the accepted body follows")
            };
            self.stream
                .receive_peer_owned(frame, 0, |_| Ok(()))
                .unwrap();
        }
        let complete = self.stream.handoff(0, |_| Ok(()), |v| v).unwrap();
        assert!(self.doc.import(&complete.bytes).unwrap().pending.is_none());
        let ack = self
            .scope
            .committed_ack(complete.unit, complete.batch_id)
            .unwrap();
        self.write(&ack).await;
    }
}

#[derive(Debug, Default)]
struct Seen {
    held: Vec<Reason>,
    uploaded: usize,
    downloaded: usize,
    upload_deferred: Option<bool>,
}

/// One pass over the unit, with the worker's script for it running beside.
async fn cycle<T>(
    sync: &mut SyncClient,
    f: &mut Fixture,
    requests: &mut mpsc::Receiver<WorkerCommand>,
    worker: impl Future<Output = T>,
) -> (Seen, T) {
    sync.start(f.unit).unwrap();
    tokio::pin!(worker);
    let mut seen = Seen::default();
    let mut done = None;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = sync.status();
            if done.is_some()
                && seen.upload_deferred.is_some()
                && !status.transport.committing
                && !status.transport.writing
                && status.transport.queued_controls == 0
            {
                break;
            }
            tokio::select! {
                result = &mut worker, if done.is_none() => done = Some(result),
                event = sync.next() => match event.unwrap() {
                    Event::Settled { upload_deferred, download_deferred, .. } => {
                        assert!(!download_deferred);
                        seen.upload_deferred = Some(upload_deferred);
                    }
                    Event::AdmissionHeld { unit, reason } => {
                        assert_eq!(unit, f.unit);
                        seen.held.push(reason);
                    }
                    Event::Uploaded { .. } => seen.uploaded += 1,
                    Event::Downloaded(_) => seen.downloaded += 1,
                    Event::Refused { unit, code } => {
                        panic!("no Store job refuses here: {unit:?} {code}")
                    }
                    Event::Deferred { .. } => panic!("nothing defers here"),
                    Event::RemoteHint | Event::Progress => {}
                },
                command = requests.recv() => {
                    let WorkerCommand::Store(task) = command.unwrap() else { panic!("Store job") };
                    task(Ok(&mut f.store));
                }
            }
        }
    })
    .await
    .expect("bounded cycle");
    (seen, done.unwrap())
}

/// A pass in which the worker answers the join and the client offers nothing.
async fn held_cycle(
    sync: &mut SyncClient,
    f: &mut Fixture,
    requests: &mut mpsc::Receiver<WorkerCommand>,
    worker: &mut Worker,
) {
    let (seen, ()) = cycle(sync, f, requests, async {
        assert!(!worker.join().await);
        worker.quiet().await;
    })
    .await;
    assert!(seen.held.is_empty());
    assert_eq!(seen.uploaded, 0);
    assert_eq!(seen.upload_deferred, Some(true));
}

/// A pass in which the client offers its start and the worker refuses it.
async fn refused_cycle(
    sync: &mut SyncClient,
    f: &mut Fixture,
    requests: &mut mpsc::Receiver<WorkerCommand>,
    worker: &mut Worker,
    reason: Reason,
    open: bool,
) {
    let (seen, ()) = cycle(sync, f, requests, async {
        if open {
            worker.open().await;
        }
        assert!(!worker.join().await);
        let start = worker.start().await;
        // The whole body fits one frame, and none of it follows the start.
        worker.quiet().await;
        worker.refuse(start, reason).await;
        worker.quiet().await;
    })
    .await;
    assert_eq!(seen.held, [reason]);
    assert_eq!(seen.uploaded, 0);
    assert_eq!(seen.upload_deferred, Some(true));
}

#[tokio::test]
async fn a_resource_hold_offers_one_start_per_cycle_and_no_body_until_accepted() {
    let (mut f, cloud) = edited();
    let (holds, clock) = holds();
    let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, true);
    let reason = Reason::StoredDocumentBytes;

    refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, true).await;
    assert!(sync.upload_decision_required());
    assert_eq!(worker.starts, 1);
    for retry in 2..=3 {
        // Any number of passes inside the cycle offer nothing at all.
        held_cycle(&mut sync, &mut f, &mut requests, &mut worker).await;
        clock.fetch_add(CYCLE - 1, Ordering::SeqCst);
        held_cycle(&mut sync, &mut f, &mut requests, &mut worker).await;
        assert_eq!(worker.starts, retry - 1);
        // The cycle elapses: exactly one start, refused again unchanged.
        clock.fetch_add(1, Ordering::SeqCst);
        refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, false).await;
        assert_eq!(worker.starts, retry);
        assert_eq!(holds.reason(f.unit), Some(reason));
    }
    assert_eq!(worker.stream.status().incoming_bytes, 0);
    assert!(!worker.doc.oplog_vv().includes_vv(&local(&mut f).oplog_vv()));

    // The resource recovers: the next cycle's one start is accepted, and only
    // then does that attempt's body cross. The hold ends at the receipt.
    clock.fetch_add(CYCLE, Ordering::SeqCst);
    let (seen, ()) = cycle(&mut sync, &mut f, &mut requests, async {
        assert!(!worker.join().await);
        let start = worker.start().await;
        worker.quiet().await;
        worker.accept(start).await;
    })
    .await;
    assert!(seen.held.is_empty());
    assert_eq!(seen.uploaded, 1);
    assert_eq!(seen.upload_deferred, Some(false));
    assert_eq!(worker.starts, 4);
    assert!(!holds.active());
    assert!(worker.doc.oplog_vv().includes_vv(&local(&mut f).oplog_vv()));
    assert_eq!(f.name(), "After");
}

#[tokio::test]
async fn quota_and_oversized_holds_get_no_timer_driven_attempt() {
    for reason in [Reason::AccountQuota, Reason::FileAllowance] {
        let (mut f, cloud) = edited();
        let (holds, clock) = holds();
        let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, true);
        refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, true).await;
        for _ in 0..3 {
            clock.fetch_add(10 * CYCLE, Ordering::SeqCst);
            held_cycle(&mut sync, &mut f, &mut requests, &mut worker).await;
        }
        assert_eq!(worker.starts, 1);
        assert_eq!(holds.reason(f.unit), Some(reason));
        // The refused edit is still here, and still only here.
        assert_eq!(f.name(), "After");
        assert!(!worker.doc.oplog_vv().includes_vv(&local(&mut f).oplog_vv()));
    }
}

#[tokio::test]
async fn a_worker_without_the_mode_gets_no_remembered_resource_retry_and_still_serves_reads() {
    let (mut f, cloud) = edited();
    let (holds, clock) = holds();
    let reason = Reason::PhysicalBytes;
    let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, true);
    refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, true).await;
    let cloud = worker.doc;
    drop((sync, requests));

    // The carrier is renewed against an older worker: its policy has no
    // decision field. Somebody else's edit is waiting there to be read.
    cloud.set_peer_id(901).unwrap();
    cloud
        .get_map("meta")
        .insert("purpose", "Remote purpose")
        .unwrap();
    cloud.commit();
    let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, false);
    clock.fetch_add(5 * CYCLE, Ordering::SeqCst);
    let (seen, ()) = cycle(&mut sync, &mut f, &mut requests, async {
        worker.open().await;
        assert!(worker.join().await);
        // The download's Ack is the only thing this client writes.
        assert!(worker.next().await.is_none());
        worker.quiet().await;
    })
    .await;
    assert!(!sync.upload_decision_required());
    assert!(seen.held.is_empty());
    assert_eq!((seen.uploaded, seen.downloaded), (0, 1));
    assert_eq!(seen.upload_deferred, Some(true));
    // The read was imported by the Store beside the retained local edit.
    assert!(local(&mut f).oplog_vv().includes_vv(&worker.doc.oplog_vv()));
    assert_eq!(f.name(), "After");

    // No cycle, however long, turns silence from this worker into permission,
    // and the merged download did not make the refused edit a new candidate.
    for _ in 0..2 {
        clock.fetch_add(CYCLE, Ordering::SeqCst);
        held_cycle(&mut sync, &mut f, &mut requests, &mut worker).await;
    }
    assert_eq!(worker.starts, 0);
    assert_eq!(holds.reason(f.unit), Some(reason));
    assert!(!worker.doc.oplog_vv().includes_vv(&local(&mut f).oplog_vv()));
}

#[tokio::test]
async fn a_refused_resource_retry_crosses_a_download_that_still_commits_and_acks() {
    let (mut f, cloud) = edited();
    let (holds, clock) = holds();
    let reason = Reason::StagingBytes;
    let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, true);
    refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, true).await;

    worker.doc.set_peer_id(901).unwrap();
    worker
        .doc
        .get_map("meta")
        .insert("purpose", "Remote purpose")
        .unwrap();
    worker.doc.commit();
    clock.fetch_add(CYCLE, Ordering::SeqCst);
    let (seen, ()) = cycle(&mut sync, &mut f, &mut requests, async {
        assert!(worker.join().await);
        // The timed start and the download's Ack arrive in either order.
        let mut acked = false;
        let mut refused = false;
        while !(acked && refused) {
            match worker.next().await {
                Some(start) => {
                    worker.refuse(start, reason).await;
                    refused = true;
                }
                None => acked = true,
            }
        }
        worker.quiet().await;
    })
    .await;
    assert_eq!(seen.held, [reason]);
    assert_eq!((seen.uploaded, seen.downloaded), (0, 1));
    assert_eq!(seen.upload_deferred, Some(true));
    assert_eq!(worker.starts, 2);
    assert!(local(&mut f).oplog_vv().includes_vv(&worker.doc.oplog_vv()));
    assert_eq!(f.name(), "After");

    // The stream stayed open, and the rejected edit is not offered again.
    held_cycle(&mut sync, &mut f, &mut requests, &mut worker).await;
    assert_eq!(worker.starts, 2);
    assert_eq!(holds.reason(f.unit), Some(reason));
}

#[tokio::test]
async fn a_hold_ends_once_cloud_already_holds_the_refused_edit() {
    let (mut f, cloud) = edited();
    let (holds, _clock) = holds();
    let reason = Reason::AccountQuota;
    let (mut sync, mut worker, mut requests) = session(&f, cloud, &holds, true);
    refused_cycle(&mut sync, &mut f, &mut requests, &mut worker, reason, true).await;

    // The same edit reaches Cloud another way. Nothing is left to offer, so
    // nothing keeps the owner's account being read on this unit's behalf.
    let snapshot = local(&mut f).export(ExportMode::Snapshot).unwrap();
    worker.doc.import(&snapshot).unwrap();
    let (seen, ()) = cycle(&mut sync, &mut f, &mut requests, async {
        assert!(!worker.join().await);
        worker.quiet().await;
    })
    .await;
    assert!(seen.held.is_empty());
    assert_eq!(seen.upload_deferred, Some(false));
    assert_eq!(worker.starts, 1);
    assert!(!holds.active());
}
