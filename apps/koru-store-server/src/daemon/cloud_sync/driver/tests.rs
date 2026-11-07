// SPDX-License-Identifier: MPL-2.0

//! The driver at its ports: a Store composition in memory, an in-memory Cloud
//! on the real carrier, and a Store executor the test can pause. TECHNICAL
//! §16's items for this plan — a driver stopped mid-upload leaves no carrier
//! state, and an unlisted unit is fetched and accepted under its derived
//! candidate id — are proven here at port level; Cloud MCP Plan 2.3 proves
//! them again through the host.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_replication::cloud::transfer::{Decision, MemoryBudget, Start};
use koru_store_core::{
    DocumentId, Locale, ProfileAccountBinding, ProfileId, Timestamp, WorkspaceId,
};
use sha2::{Digest, Sha256};

use super::*;
use crate::daemon::cloud_sync::{NoDownloads, Plan, Stage};
use crate::daemon::fake_cloud::{FakeCloud, Sessions, document_unit, workspace_scope};
use crate::daemon::memory::MemoryStore;
use crate::daemon::{StoreReply, WorkerError};

type Job = Box<dyn FnOnce(&mut crate::WorkerStore) + Send>;

/// A Store executor that runs jobs on its own task, and that the test can
/// pause: a job submitted while it is paused is queued, owned and not yet run,
/// exactly the state a fence finds on a busy actor.
#[derive(Clone)]
struct Pausable {
    store: Arc<MemoryStore>,
    queue: Arc<Mutex<VecDeque<Job>>>,
    paused: Arc<AtomicBool>,
}

impl Pausable {
    fn new(store: Arc<MemoryStore>) -> Self {
        let jobs = Self {
            store,
            queue: Arc::default(),
            paused: Arc::default(),
        };
        let runner = jobs.clone();
        tokio::spawn(async move {
            loop {
                if !runner.paused.load(Ordering::SeqCst) {
                    runner.drain();
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        jobs
    }

    fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    fn queued(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    fn drain(&self) {
        loop {
            let Some(job) = self.queue.lock().unwrap().pop_front() else {
                return;
            };
            self.store.with_store(job).unwrap();
        }
    }
}

impl StoreJobs for Pausable {
    fn submit_store<R, F>(&self, task: F) -> Result<StoreReply<R>, WorkerError>
    where
        R: Send + 'static,
        F: FnOnce(&mut crate::WorkerStore) -> R + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.queue.lock().unwrap().push_back(Box::new(move |store| {
            let _ = reply.send(Ok(task(store)));
        }));
        Ok(StoreReply::from_channel(response))
    }

    fn watch_cloud(
        &self,
        dataset: koru_store_api::cloud_changes::Dataset,
    ) -> impl Future<Output = Result<koru_store_api::cloud_changes::Subscription, WorkerError>> + Send
    {
        std::future::ready(Ok(self.store.watch_cloud(dataset)))
    }
}

/// The permission and progress a test driver runs with.
type TestParts<J> = Parts<J, fn(&Start) -> Decision, fn(Progress)>;

fn accept(_: &Start) -> Decision {
    Decision::Accept
}

fn parts<J: StoreJobs>(
    jobs: J,
    workspace: WorkspaceId,
    stage: Stage,
    authorize: Authorize,
) -> TestParts<J> {
    Parts {
        plan: Plan::Workspace {
            workspace_id: workspace,
        },
        stage,
        jobs,
        authorize,
        downloads: Arc::new(NoDownloads),
        deferrals: Arc::default(),
        permission: accept,
        progress: |_| {},
        uploads: None,
    }
}

fn anyone() -> Authorize {
    Arc::new(|_| Ok(()))
}

/// A Workspace with one document that has content, on a device's Store.
struct Device {
    store: Arc<MemoryStore>,
    profile: ProfileId,
    workspace: WorkspaceId,
    manifest: DocumentId,
    document: DocumentId,
}

fn device() -> Device {
    let store = Arc::new(MemoryStore::open("device").unwrap());
    let (profile, workspace, manifest, document) = store
        .with_store(|store| {
            let at = Timestamp::now();
            let profile = store.create_profile("Person", None, None, Locale::default(), at)?;
            let workspace = store.create_workspace(profile.id, "Garden", at)?;
            let manifest = store.manifest_id(workspace.id)?;
            let (document, _) =
                store.create_document(workspace.id, "Planting", "page", None, None, at)?;
            let doc = loro::LoroDoc::new();
            doc.import(&store.snapshot(workspace.id, document)?)
                .unwrap();
            doc.get_text("probe")
                .insert(0, &"kauri ".repeat(64))
                .unwrap();
            doc.commit();
            store.apply_update(
                workspace.id,
                document,
                doc.export(loro::ExportMode::all_updates()).unwrap(),
                at,
                koru_store_core::ports::UpdateOrigin::Store,
            )?;
            store.flush(workspace.id, document, at)?;
            Ok::<_, koru_store_core::Error>((profile.id, workspace.id, manifest, document))
        })
        .unwrap()
        .unwrap();
    Device {
        store,
        profile,
        workspace,
        manifest,
        document,
    }
}

/// An empty replica of the device's Workspace: its Profile and the
/// identity-only bootstrap, and no document.
async fn replica(device: &Device) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::open("replica").unwrap());
    let profile = device.profile;
    store
        .with_store(move |store| {
            store.provision_account_profile(
                profile,
                ProfileAccountBinding::from_identity_uuid(uuid::Uuid::new_v4()).unwrap(),
                "Person",
                Locale::default(),
                Timestamp::now(),
            )
        })
        .unwrap()
        .unwrap();
    crate::cloud_driver::ensure_workspace(
        &store.client(),
        device.profile,
        device.workspace,
        device.manifest,
    )
    .await
    .unwrap();
    store
}

async fn passes(driver: &Driver, at_least: u64) {
    let mut status = driver.status();
    tokio::time::timeout(Duration::from_secs(10), async {
        while status.borrow_and_update().passes < at_least {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("the pass finished");
}

async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Upload the device's Workspace to Cloud and stop.
async fn uploaded(device: &Device, cloud: &FakeCloud) {
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, MemoryBudget::new(64 << 20), 1),
        parts(
            device.store.client(),
            device.workspace,
            Stage::Full,
            anyone(),
        ),
    );
    passes(&driver, 1).await;
    driver.stop().await;
}

/// TECHNICAL §16: **stopping the driver mid-upload leaves no carrier state**
/// that a rebuilt replica inherits.
///
/// An upload is in flight at the worker, unacknowledged, when the driver is
/// stopped. `stop` returns only once the task has exited, and by then the
/// carrier's reservations are all released and the worker sees the stream
/// close. Anything the executor was still holding runs after the stop and
/// refuses as fenced: it never asks the caller's authority and carries
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_mid_upload_leaves_no_carrier_state() {
    let device = device();
    let cloud = FakeCloud::default();
    cloud.hold_uploads(true);
    let budget = MemoryBudget::new(64 << 20);
    let jobs = Pausable::new(Arc::clone(&device.store));
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let authorize: Authorize = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, budget.clone(), 1),
        parts(jobs.clone(), device.workspace, Stage::Full, authorize),
    );

    // The manifest's upload reaches the worker whole and is held there.
    tokio::time::timeout(Duration::from_secs(10), cloud.upload_held())
        .await
        .expect("an upload is in flight");
    assert!(
        budget.used() > 0,
        "the carrier holds the upload's accounting"
    );
    jobs.pause(true);

    driver.stop().await;
    assert_eq!(budget.used(), 0, "every carrier reservation is released");
    eventually("the worker sees the stream close", || {
        cloud.log.lock().unwrap().sessions_closed == 1
    })
    .await;
    {
        let log = cloud.log.lock().unwrap();
        assert!(log.uploaded.is_empty(), "nothing was acknowledged");
        assert_eq!(log.held.len(), 1);
    }

    // Whatever the executor still holds runs after the stop and refuses.
    let before = asked.load(Ordering::SeqCst);
    jobs.drain();
    jobs.pause(false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        asked.load(Ordering::SeqCst),
        before,
        "a job run after the stop never reached the caller's authority"
    );
    assert_eq!(jobs.queued(), 0);
    assert_eq!(budget.used(), 0, "and it carried nothing");
}

/// A Store job the executor was handed before the stop and runs after it is
/// refused, and nothing reaches the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_queued_before_the_stop_refuses_after_it() {
    let device = device();
    let cloud = FakeCloud::default();
    let budget = MemoryBudget::new(64 << 20);
    let jobs = Pausable::new(Arc::clone(&device.store));
    jobs.pause(true);
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let authorize: Authorize = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, budget.clone(), 1),
        parts(jobs.clone(), device.workspace, Stage::Full, authorize),
    );
    // The pass's first Store job — the manifest's version read — is queued.
    eventually("a queued job", || jobs.queued() > 0).await;
    let before = asked.load(Ordering::SeqCst);
    driver.stop().await;
    jobs.drain();
    jobs.pause(false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(asked.load(Ordering::SeqCst), before, "refused as fenced");
    assert_eq!(budget.used(), 0);
    let log = cloud.log.lock().unwrap();
    assert!(log.joined.is_empty() && log.uploaded.is_empty(), "{log:?}");
}

/// The candidate id a hosted creation derives (TECHNICAL §5.6):
/// `SHA-256("koru:app-connection:create:v1" ‖ connection ‖ workspace ‖ key)`,
/// with the version-4 and variant bits set.
fn candidate(connection: uuid::Uuid, workspace: WorkspaceId, key: &str) -> DocumentId {
    let mut hash = Sha256::new();
    hash.update(b"koru:app-connection:create:v1");
    hash.update(connection.as_bytes());
    hash.update(workspace.as_uuid().as_bytes());
    hash.update(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    let id = DocumentId::from_uuid(uuid::Builder::from_random_bytes(bytes).into_uuid());
    // What every Store id has to be, and what its parser checks.
    assert_eq!(DocumentId::parse(&id.to_storage()), Some(id));
    id
}

/// TECHNICAL §16: **the driver fetches a unit the manifest does not name, and
/// the Store accepts it under the derived candidate id.**
///
/// One candidate is in Cloud — a creation whose seed was uploaded and whose
/// manifest entry never was — and arrives whole, without being listed. The
/// other is absent, which is an answer and not an error, and the Store then
/// seeds a document under that very id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetches_an_unlisted_unit_and_the_store_accepts_its_candidate_id() {
    let device = device();
    let cloud = FakeCloud::default();
    uploaded(&device, &cloud).await;
    let connection = uuid::Uuid::new_v4();
    let present = candidate(connection, device.workspace, "create-1");
    let absent = candidate(connection, device.workspace, "create-2");
    assert_ne!(present, absent);
    // The earlier attempt's seed, in Cloud under its candidate id.
    let mut seed = koru_core::WorkspaceDoc::create(
        koru_core::WorkspaceId::from_uuid(device.workspace.as_uuid()),
        "Seeded",
    )
    .unwrap();
    seed.insert(
        koru_core::Parent::Root,
        koru_core::Node::document("Seeded").unwrap(),
    )
    .unwrap();
    let seed = seed.export_snapshot().unwrap();
    cloud.put(document_unit(device.workspace, present), &seed);

    let replica = replica(&device).await;
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, MemoryBudget::new(64 << 20), 1),
        parts(replica.client(), device.workspace, Stage::Full, anyone()),
    );
    passes(&driver, 1).await;

    assert_eq!(
        driver.fetch(document_unit(device.workspace, present)).await,
        Ok(Fetched::Present)
    );
    let workspace = device.workspace;
    let held = replica
        .with_store(move |store| store.snapshot(workspace, present))
        .unwrap()
        .unwrap();
    let held_doc = loro::LoroDoc::new();
    held_doc.import(&held).unwrap();
    let cloud_doc = cloud
        .unit(document_unit(device.workspace, present))
        .unwrap();
    assert_eq!(
        held_doc.get_deep_value(),
        cloud_doc.get_deep_value(),
        "the Store holds Cloud's content under the candidate id"
    );
    let listed = replica
        .with_store(move |store| store.documents(workspace))
        .unwrap()
        .unwrap();
    assert!(
        listed.iter().all(|entry| entry.id != present),
        "fetched, not listed: the manifest does not name it"
    );

    assert_eq!(
        driver.fetch(document_unit(device.workspace, absent)).await,
        Ok(Fetched::Absent)
    );
    // Absent is an empty version, and the Store seeds under the same id.
    let seeded = replica
        .with_store(move |store| {
            store.create_seeded_document(
                workspace,
                "Seeded",
                "page",
                None,
                None,
                "app-connection",
                "create-2",
                "request-hash",
                Some(absent),
                seed,
                Timestamp::now(),
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(seeded.document, absent);
    assert!(!seeded.replayed);

    // What may not be fetched is refused without touching the carrier.
    let manifest = document_unit(device.workspace, device.manifest);
    assert_eq!(driver.fetch(manifest).await, Err(FetchError::NotADocument));
    let elsewhere = document_unit(WorkspaceId::new(), DocumentId::new());
    assert_eq!(driver.fetch(elsewhere).await, Err(FetchError::NotADocument));
    driver.stop().await;
}

/// TECHNICAL §5.3's stages: a session admitted at the manifest stage carries
/// the manifest and nothing else, and advancing opens the next session at the
/// full stage, which carries the documents.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_manifest_stage_carries_the_manifest_alone_until_advanced() {
    let device = device();
    let cloud = FakeCloud::default();
    uploaded(&device, &cloud).await;
    let joined_before = cloud.log.lock().unwrap().joined.len();

    let replica = replica(&device).await;
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let sessions = Sessions::new(cloud.clone(), scope, MemoryBudget::new(64 << 20), 2);
    let asked = Arc::clone(&sessions.asked);
    let driver = Driver::spawn(
        sessions,
        parts(
            replica.client(),
            device.workspace,
            Stage::Manifest,
            anyone(),
        ),
    );
    passes(&driver, 1).await;
    assert_eq!(driver.status().borrow().stage, Stage::Manifest);
    let manifest = document_unit(device.workspace, device.manifest);
    let document = document_unit(device.workspace, device.document);
    {
        let log = cloud.log.lock().unwrap();
        assert_eq!(
            &log.joined[joined_before..],
            &[manifest],
            "the manifest alone"
        );
    }
    // The manifest names the document; its body has not come.
    let workspace = device.workspace;
    let listed = replica
        .with_store(move |store| store.documents(workspace))
        .unwrap()
        .unwrap();
    assert!(listed.iter().any(|entry| entry.id == device.document));
    assert_eq!(
        driver.fetch(document).await,
        Err(FetchError::ManifestStage),
        "no document body at the manifest stage"
    );

    driver.advance(Stage::Full).unwrap();
    passes(&driver, 2).await;
    assert_eq!(*asked.lock().unwrap(), vec![Stage::Manifest, Stage::Full]);
    assert_eq!(driver.status().borrow().stage, Stage::Full);
    assert!(
        cloud.log.lock().unwrap().downloaded.contains(&document),
        "the full stage brought the document"
    );
    driver.stop().await;
}

/// A device's gateway: one bridged stream and no other. When that session
/// ends the driver ends, with the session's own result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_once_source_ends_with_its_session() {
    let device = device();
    let cloud = FakeCloud::default();
    cloud.close_at_once(true);
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let opened = cloud.session(scope, MemoryBudget::new(64 << 20));
    let driver = Driver::spawn(
        OnceSource::new(opened.reader, opened.writer, opened.session),
        parts(
            device.store.client(),
            device.workspace,
            Stage::Full,
            anyone(),
        ),
    );
    let ended = tokio::time::timeout(Duration::from_secs(10), driver.join())
        .await
        .expect("the driver ends with its only session");
    assert!(ended.is_err(), "a closed stream is the session's error");
}

/// Stopping a driver whose source never answers still returns.
#[tokio::test]
async fn stopping_while_waiting_for_a_session_returns() {
    struct Never;
    impl SessionSource for Never {
        type Reader = tokio::io::Empty;
        type Writer = tokio::io::Sink;
        fn open(
            &mut self,
            _stage: Stage,
        ) -> impl Future<Output = Option<Opened<Self::Reader, Self::Writer>>> + Send {
            std::future::pending()
        }
    }
    let store = MemoryStore::open("never").unwrap();
    let driver = Driver::spawn(
        Never,
        parts(store.client(), WorkspaceId::new(), Stage::Full, anyone()),
    );
    tokio::time::timeout(Duration::from_secs(5), driver.stop())
        .await
        .expect("stop returns");
}

/// A session that may not upload sends nothing, however far ahead of Cloud
/// the Store is: the manifest stage never uploads (the worker admits
/// downloads only there), and at the full stage the caller's rule — read
/// access, Identity — is asked before any export job exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_may_not_upload_sends_nothing() {
    for (stage, uploads) in [
        (Stage::Manifest, None),
        (
            Stage::Full,
            Some(Arc::new(|_| false) as crate::daemon::cloud_sync::Uploads),
        ),
    ] {
        let device = device();
        let cloud = FakeCloud::default();
        let budget = MemoryBudget::new(64 << 20);
        let scope = workspace_scope(device.workspace, device.profile, device.manifest);
        let driver = Driver::spawn(
            Sessions::new(cloud.clone(), scope, budget.clone(), 1),
            Parts {
                uploads,
                ..parts(device.store.client(), device.workspace, stage, anyone())
            },
        );
        passes(&driver, 1).await;
        driver.stop().await;
        let log = cloud.log.lock().unwrap();
        assert!(!log.joined.is_empty(), "{stage:?}: the units were joined");
        assert!(
            log.uploaded.is_empty() && log.held.is_empty(),
            "{stage:?}: {log:?}"
        );
        assert_eq!(budget.used(), 0);
    }
}

/// Cloud MCP Plan 2.3: **what Cloud holds, and why a session ended**, as a
/// host waiting on an acknowledgement reads them. An acknowledged upload is
/// `Confirmed` with a version that includes the local write; a refusal frame
/// ends the session and is reported, code and all, before the next opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_versions_and_session_ends_are_reported() {
    let device = device();
    let cloud = FakeCloud::default();
    let scope = workspace_scope(device.workspace, device.profile, device.manifest);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let unit = document_unit(device.workspace, device.document);
    let driver = Driver::spawn(
        Sessions::new(cloud.clone(), scope, MemoryBudget::new(64 << 20), 2),
        Parts {
            progress: move |event: Progress| match event {
                Progress::Confirmed { unit, version } => {
                    record.lock().unwrap().push((Some((unit, version)), None))
                }
                Progress::SessionEnded(error) => {
                    record.lock().unwrap().push((None, Some(error)));
                }
                _ => {}
            },
            plan: Plan::Workspace {
                workspace_id: device.workspace,
            },
            stage: Stage::Full,
            jobs: device.store.client(),
            authorize: anyone(),
            downloads: Arc::new(NoDownloads),
            deferrals: Arc::default(),
            permission: accept,
            uploads: None,
        },
    );
    passes(&driver, 1).await;
    let (workspace, document) = (device.workspace, device.document);
    let local = device
        .store
        .with_store(move |store| store.version(workspace, document))
        .unwrap()
        .unwrap();
    let confirmed = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(confirmed, _)| confirmed.clone())
        .find(|(confirmed, _)| *confirmed == unit)
        .map(|(_, version)| loro::VersionVector::decode(&version).unwrap())
        .expect("the uploaded document was confirmed");
    assert!(confirmed.includes_vv(&local), "Cloud holds the local write");

    // A refusal frame answering the next upload ends the session, and says so.
    cloud.refuse_uploads(Some("sync_unit_exists"));
    device
        .store
        .with_store(move |store| {
            let doc = loro::LoroDoc::new();
            doc.import(&store.snapshot(workspace, document)?).unwrap();
            let before = doc.oplog_vv();
            doc.get_text("probe").insert(0, "more ").unwrap();
            doc.commit();
            let at = Timestamp::now();
            store.apply_update(
                workspace,
                document,
                doc.export(loro::ExportMode::updates(&before)).unwrap(),
                at,
                koru_store_core::ports::UpdateOrigin::Store,
            )?;
            store.flush(workspace, document, at).map(|_| ())
        })
        .unwrap()
        .unwrap();
    eventually("the session's end was reported", || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|(_, ended)| *ended == Some(koru_replication::cloud::transfer::Error::UnitExists))
    })
    .await;
    driver.stop().await;
}
