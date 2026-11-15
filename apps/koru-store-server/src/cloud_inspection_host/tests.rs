// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{DocumentId, ProfileId, WorkspaceId};
use koru_store_wire::cloud_document::CloudDocumentSelection;
use std::sync::atomic::{AtomicBool, Ordering};

fn fixture() -> (
    Arc<InspectionHub>,
    Owner,
    InspectionRequest,
    mpsc::Receiver<Command>,
    Arc<AtomicBool>,
) {
    let hub = InspectionHub::new(tokio::runtime::Handle::current());
    let account = CloudId::from_bytes(Uuid::new_v4().into_bytes());
    let owner = Owner {
        control_session: Uuid::new_v4(),
        owner_scope: 7,
        account,
        account_generation: 11,
    };
    let scope = crate::daemon::fake_cloud::workspace_scope(
        WorkspaceId::new(),
        ProfileId::new(),
        DocumentId::new(),
    );
    let CloudReplicaPrincipalV1::Workspace {
        workspace_id,
        incarnation_id,
        ..
    } = scope.principal
    else {
        panic!()
    };
    let request = InspectionRequest {
        selection: CloudDocumentSelection {
            account_id: account.to_string(),
            workspace_id: workspace_id.to_string(),
            incarnation_id: incarnation_id.to_string(),
            cloud_unit_id: Uuid::new_v4().to_string(),
            document_id: Uuid::new_v4().to_string(),
            source_head_digest: "ab".repeat(32),
        },
        action: CloudDocumentAction::Inspect,
        can_write: true,
    };
    let (commands, receiver) = mpsc::channel(1);
    let live = Arc::new(AtomicBool::new(true));
    let current = live.clone();
    let weak = Arc::downgrade(&hub);
    lock(&hub.sessions).insert(
        Uuid::new_v4(),
        Registered {
            session: Session {
                control_session: owner.control_session,
                account,
                account_generation: 11,
                scope,
            },
            commands,
            live: Arc::new(move || {
                // Exercise the lock-order guarantee at every callback callsite.
                let hub = weak.upgrade().unwrap();
                assert!(hub.slot.try_lock().is_ok());
                assert!(hub.sessions.try_lock().is_ok());
                current.load(Ordering::Acquire)
            }),
        },
    );
    (hub, owner, request, receiver, live)
}
async fn drained(hub: &InspectionHub) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while hub.admission.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn cancel_before_start_ack_keeps_admission_until_detached_work_releases_it() {
    let (hub, owner, request, mut commands, _) = fixture();
    let token = Uuid::new_v4();
    hub.start(owner, token, request.clone()).unwrap();
    let Command::Inspect(mut job) = commands.recv().await.unwrap() else {
        panic!()
    };
    let detached = job.retention.clone();
    hub.cancel(owner, token);
    assert!(*job.cancelled.borrow_and_update());
    drop(job);
    assert!(hub.start(owner, Uuid::new_v4(), request.clone()).is_err());
    drop(detached);
    drained(&hub).await;
    let next = hub.start(owner, Uuid::new_v4(), request).unwrap();
    hub.cancel(owner, next);
}
#[tokio::test]
async fn duplicate_start_foreign_poll_and_old_account_generation_cannot_retarget() {
    let (hub, owner, request, mut commands, _) = fixture();
    assert!(
        hub.start(
            Owner {
                account_generation: 10,
                ..owner
            },
            Uuid::new_v4(),
            request.clone()
        )
        .is_err()
    );
    let token = Uuid::new_v4();
    hub.start(owner, token, request.clone()).unwrap();
    assert!(hub.start(owner, token, request).is_err());
    let Command::Inspect(job) = commands.recv().await.unwrap() else {
        panic!()
    };
    job.reply
        .send(Ok(CloudDocumentOutcome::Removed))
        .ok()
        .unwrap();
    drop(job.retention);
    assert!(matches!(
        hub.poll(
            Owner {
                owner_scope: 8,
                ..owner
            },
            token
        ),
        InspectionPoll::Failed { .. }
    ));
    tokio::task::yield_now().await;
    assert!(matches!(hub.poll(owner, token), InspectionPoll::Removed));
    assert!(matches!(
        hub.poll(owner, token),
        InspectionPoll::Failed { .. }
    ));
}
#[tokio::test]
async fn revoked_or_closed_session_discards_even_a_completed_result() {
    let (hub, owner, request, mut commands, live) = fixture();
    let token = hub.start(owner, Uuid::new_v4(), request.clone()).unwrap();
    let Command::Inspect(job) = commands.recv().await.unwrap() else {
        panic!()
    };
    job.reply
        .send(Ok(CloudDocumentOutcome::Removed))
        .ok()
        .unwrap();
    drop(job.retention);
    tokio::task::yield_now().await;
    live.store(false, Ordering::Release);
    hub.revalidate();
    assert!(lock(&hub.slot).is_none());
    assert!(matches!(
        hub.poll(owner, token),
        InspectionPoll::Failed { .. }
    ));
    live.store(true, Ordering::Release);
    drained(&hub).await;
    let token = hub.start(owner, Uuid::new_v4(), request).unwrap();
    hub.close_session(owner.control_session);
    assert!(matches!(
        hub.poll(owner, token),
        InspectionPoll::Failed { .. }
    ));
}
#[tokio::test(start_paused = true)]
async fn unpolled_timeout_expires_without_retaining_a_slot_forever() {
    let (hub, owner, request, mut commands, _) = fixture();
    let token = hub.start(owner, Uuid::new_v4(), request).unwrap();
    let Command::Inspect(mut job) = commands.recv().await.unwrap() else {
        panic!()
    };
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(16)).await;
    tokio::task::yield_now().await;
    assert!(*job.cancelled.borrow_and_update());
    drop(job);
    tokio::time::advance(Duration::from_secs(15)).await;
    tokio::task::yield_now().await;
    assert!(lock(&hub.slot).is_none());
    assert!(matches!(
        hub.poll(owner, token),
        InspectionPoll::Failed { .. }
    ));
}

#[tokio::test]
async fn consumed_carrier_keeps_its_session_until_result_delivery_or_revocation() {
    for revoked in [false, true] {
        let (hub, owner, request, mut commands, live) = fixture();
        let id = *lock(&hub.sessions).keys().next().unwrap();
        let registration = Registration {
            hub: Arc::downgrade(&hub),
            id,
        };
        let token = hub.start(owner, Uuid::new_v4(), request).unwrap();
        let Command::Inspect(job) = commands.recv().await.unwrap() else {
            panic!()
        };
        job.reply
            .send(Ok(CloudDocumentOutcome::Removed))
            .ok()
            .unwrap();
        drop(job.retention);
        tokio::task::yield_now().await;
        // This is the ordering both native host session tasks use before
        // reporting their consumed stream as finished and reconnecting.
        let completed = Arc::new(AtomicBool::new(false));
        let finished = completed.clone();
        let task = tokio::spawn(async move {
            registration.finish_delivery().await;
            finished.store(true, Ordering::Release);
        });
        tokio::task::yield_now().await;
        assert!(!completed.load(Ordering::Acquire));
        let unrelated = Registration {
            hub: Arc::downgrade(&hub),
            id: Uuid::new_v4(),
        };
        unrelated.finish_delivery().await;
        assert!(!completed.load(Ordering::Acquire));
        if revoked {
            live.store(false, Ordering::Release);
            hub.revalidate();
            assert!(matches!(
                hub.poll(owner, token),
                InspectionPoll::Failed { .. }
            ));
        } else {
            assert!(matches!(hub.poll(owner, token), InspectionPoll::Removed));
        }
        timeout_join(task).await;
        assert!(completed.load(Ordering::Acquire));
    }
}

async fn timeout_join(task: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}
