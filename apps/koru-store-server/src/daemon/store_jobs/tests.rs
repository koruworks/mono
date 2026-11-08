// SPDX-License-Identifier: MPL-2.0

//! Real queue commands and Cloud carriers, without a runtime, disk or network.
//! Profile-only operations use the in-memory catalog; the workspace backend
//! is never opened. Workspace commit/retry durability is covered in SQLite.
use super::*;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_replication::cloud::{
    Bytes, HandshakeV1, PreparedSession, Version1,
    transfer::{self, MemoryBudget, Record, Stream, metadata},
};
use koru_store_core::{
    DocumentId, Locale, ProfileId, WorkspaceId,
    profile_units::{IdentityManifest, ProfileUnitRef},
};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::sync::mpsc;

mod client_io;
mod exports;

#[test]
fn eager_cloud_submission_owns_the_job_before_its_reply_is_polled() {
    let mut f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let reply = client
        .submit_cloud(
            f.scope,
            f.completed(&budget),
            Timestamp::from_millis(2),
            |_, _, _| Ok(()),
        )
        .unwrap();
    assert_eq!(receiver.len(), 1, "submission is not hidden in a future");
    drop(reply); // Never polled.
    assert!(budget.used() >= f.bytes.len());
    execute(&mut receiver, &mut f.store);
    assert_eq!(f.name(), "After");
    assert_eq!(budget.used(), 0);
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn queue(capacity: usize) -> (StoreWorkerClient, mpsc::Receiver<WorkerCommand>) {
    let (requests, receiver) = mpsc::channel(capacity);
    (StoreWorkerClient { requests }, receiver)
}

fn execute(receiver: &mut mpsc::Receiver<WorkerCommand>, store: &mut crate::WorkerStore) {
    let WorkerCommand::Store(task) = receiver.try_recv().unwrap() else {
        panic!("expected the existing Store command");
    };
    task(Ok(store));
}

struct Fixture {
    store: crate::WorkerStore,
    profile: ProfileId,
    scope: Scope,
    unit: Unit,
    bytes: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        // Construction only retains this empty path. No workspace is created,
        // opened or queried by the Profile-only operations below.
        let mut store = crate::WorkerStore::open(
            SqliteCatalog::in_memory().unwrap(),
            SqliteBackend::new(std::path::PathBuf::new()),
        )
        .unwrap();
        let profile = store
            .create_profile(
                "Before",
                None,
                None,
                Locale::default(),
                Timestamp::from_millis(1),
            )
            .unwrap()
            .id;
        let state = store
            .profile_unit_states(profile)
            .unwrap()
            .into_iter()
            .find(|state| matches!(state.unit, ProfileUnitRef::Identity { .. }))
            .unwrap();
        let identity = IdentityManifest::from_snapshot(&state.snapshot).unwrap();
        identity.set_name("After").unwrap();
        let bytes = identity.export_snapshot().unwrap();
        let profile_id = CloudId::from_bytes(profile.as_uuid().into_bytes());
        let scope = Scope {
            principal: CloudReplicaPrincipalV1::Profile {
                profile_id,
                lineage_id: CloudId::from_bytes([4; 16]),
                logical_replica_id: CloudId::from_bytes([5; 16]),
            },
            profile: profile_id,
            eligibility_workspace: WorkspaceId::new(),
            manifest: DocumentId::new(),
        };
        Self {
            store,
            profile,
            scope,
            unit: Unit::Identity { profile_id },
            bytes,
        }
    }

    fn completed(&self, budget: &MemoryBudget) -> Completed {
        let accepted = PreparedSession {
            principal: self.scope.principal,
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
        let mut sender = Stream::new(
            &accepted,
            self.scope,
            0,
            0,
            transfer::MAX_PAYLOAD,
            MemoryBudget::new(64 * 1024 * 1024),
        )
        .unwrap();
        let mut receiver = Stream::new(
            &accepted,
            self.scope,
            0,
            0,
            transfer::MAX_PAYLOAD,
            budget.clone(),
        )
        .unwrap();
        let sp = sender.initial_policy(0).unwrap();
        let rp = receiver.initial_policy(0).unwrap();
        sender.receive_control(rp, 0, 0, |_| Ok(())).unwrap();
        receiver.receive_control(sp, 0, 0, |_| Ok(())).unwrap();
        let (start, _) = sender
            .begin_outgoing(self.unit, [11; 8], self.bytes.clone(), 1, 1, |_| Ok(()))
            .unwrap();
        receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
        while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
            let encoded = transfer::encode_record(record.record()).unwrap();
            let Record::Peer(frame) = transfer::decode_record(encoded[4], &encoded[5..]).unwrap()
            else {
                panic!("expected payload");
            };
            receiver.receive_peer(frame, 2, |_| Ok(())).unwrap();
        }
        receiver.handoff(3, |_| Ok(()), |done| done).unwrap()
    }

    fn name(&self) -> String {
        assert_eq!(self.store.open_workspaces(), 0);
        self.store.profile(self.profile).unwrap().name
    }
}

#[test]
fn cloud_job_returns_ack_only_after_catalog_commit() {
    let mut f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let expected_scope = f.scope;
    let expected_unit = f.unit;
    let mut job = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        move |_, scope, unit| {
            assert_eq!(scope.principal, expected_scope.principal);
            assert_eq!(unit, expected_unit);
            Ok(())
        },
    ));
    assert!(poll(job.as_mut()).is_pending());
    assert_eq!(f.name(), "Before");
    assert!(budget.used() >= f.bytes.len());
    execute(&mut receiver, &mut f.store);
    assert_eq!(f.name(), "After");
    assert_eq!(budget.used(), 0);
    let Poll::Ready(Ok(Ok(receipt))) = poll(job.as_mut()) else {
        panic!("missing receipt");
    };
    assert_eq!(receipt.unit(), f.unit);
    let Record::Peer(frame) = receipt.acknowledgement() else {
        panic!("missing Ack");
    };
    let decoded = metadata::decode(f.scope, frame.clone(), 1).unwrap();
    assert!(matches!(decoded.as_slice(),
        [metadata::Metadata::Ack { unit, batch, ok: true }]
        if *unit == f.unit && *batch == [11; 8]));
    assert!(receiver.try_recv().is_err(), "exactly one Store job");
}

#[test]
fn canceled_waiter_does_not_cancel_accepted_cloud_commit_or_release_its_budget() {
    let mut f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let mut job = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| Ok(()),
    ));
    assert!(poll(job.as_mut()).is_pending());
    let charged = budget.used();
    drop(job);
    assert!(charged >= f.bytes.len());
    assert_eq!(budget.used(), charged);
    assert_eq!(f.name(), "Before");
    execute(&mut receiver, &mut f.store);
    assert_eq!(f.name(), "After");
    assert_eq!(budget.used(), 0);
}

#[test]
fn cloud_authority_is_checked_at_store_execution_even_after_waiter_cancellation() {
    let mut f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let allowed = Arc::new(AtomicBool::new(true));
    let checked = Arc::new(AtomicBool::new(false));
    let current = allowed.clone();
    let observed = checked.clone();
    let mut job = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        move |_, _, _| {
            observed.store(true, Ordering::SeqCst);
            if current.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(Refusal::CloudDisabled)
            }
        },
    ));
    assert!(poll(job.as_mut()).is_pending());
    assert!(!checked.load(Ordering::SeqCst));
    allowed.store(false, Ordering::SeqCst);
    drop(job);
    execute(&mut receiver, &mut f.store);
    assert!(checked.load(Ordering::SeqCst));
    assert_eq!(f.name(), "Before");
    assert_eq!(budget.used(), 0);
}

#[test]
fn full_and_closed_queue_refuse_cloud_jobs_and_release_unaccepted_payloads() {
    let f = Fixture::new();
    let (client, receiver) = queue(1);
    let mut occupying = Box::pin(client.with_store_async(|_| ()));
    assert!(poll(occupying.as_mut()).is_pending());
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let mut full = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| panic!("refused jobs must not run"),
    ));
    assert!(matches!(
        poll(full.as_mut()),
        Poll::Ready(Err(WorkerError::Saturated))
    ));
    assert_eq!(budget.used(), 0);
    drop(receiver);
    assert!(matches!(
        poll(occupying.as_mut()),
        Poll::Ready(Err(WorkerError::Stopped))
    ));
    let mut closed = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| panic!("closed queue must not execute"),
    ));
    assert!(matches!(
        poll(closed.as_mut()),
        Poll::Ready(Err(WorkerError::Stopped))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn unpolled_cloud_future_does_not_submit_work() {
    let f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let job = client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| Ok(()),
    );
    assert!(receiver.try_recv().is_err());
    assert!(budget.used() >= f.bytes.len());
    drop(job);
    assert_eq!(budget.used(), 0);
}

#[test]
fn worker_poison_and_transfer_refusal_remain_distinct() {
    let mut f = Fixture::new();
    let (client, mut receiver) = queue(1);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let mut poisoned = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| panic!("poisoned Store must not run authorization"),
    ));
    assert!(poll(poisoned.as_mut()).is_pending());
    let WorkerCommand::Store(task) = receiver.try_recv().unwrap() else {
        panic!();
    };
    task(Err(WorkerError::Poisoned));
    assert!(matches!(
        poll(poisoned.as_mut()),
        Poll::Ready(Err(WorkerError::Poisoned))
    ));
    assert_eq!(budget.used(), 0);
    let mut refused = Box::pin(client.receive_cloud(
        f.scope,
        f.completed(&budget),
        Timestamp::from_millis(2),
        |_, _, _| Err(Refusal::CloudDisabled),
    ));
    assert!(poll(refused.as_mut()).is_pending());
    execute(&mut receiver, &mut f.store);
    assert!(matches!(
        poll(refused.as_mut()),
        Poll::Ready(Ok(Err(Failure::Transfer(transfer::Error::Authority(
            Refusal::CloudDisabled
        )))))
    ));
    assert_eq!(budget.used(), 0);
    assert_eq!(f.name(), "Before");
}
