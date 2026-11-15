// SPDX-License-Identifier: MPL-2.0

//! The desktop's composition of the blob transfer engine.
//!
//! **Here and never in the window.** The window has no account, no Store lock
//! and no business holding a 2 GiB file; what it has is a document on screen
//! and a `GET /v1/workspaces/{id}/blobs/{hash}` when it needs a picture.
//! Everything below runs in the Store/Agent background service, which already
//! holds the Store, already runs the document pass, and is already where a
//! Cloud stream lands.
//!
//! ```text
//!   koru-agent                    this process
//!   ──────────                    ────────────
//!   the account          ──►  replace_cloud_blob_access (control 7)
//!                                     │
//!   the window           ──►  GET /blobs/{hash}  ──►  peer hydration
//!                                     │                    │ parks
//!                                     └────────────────────┴──►  BlobEngine
//!                                                                  │
//!   a durable commit     ──►  document_changed ──► reconcile ──►  Cloud
//! ```
//!
//! Five things are composed and nothing else is invented:
//!
//! - **The walk** reads `projection.db` — its own connection, the same
//!   sweep-first discipline as the attribute and task ports — and hands the
//!   rows to `koru-blob-sync`'s one canonical rule. The read model does not
//!   get a second copy of the rule and this file does not get one either.
//! - **The Store port** is the existing `Store` surface: `verify_blob`,
//!   `begin_blob_upload`/`finish_blob_upload`, `blob`. The download's object
//!   is published *before* its association because that is what those two
//!   calls do in that order, and they are in one port method so a caller
//!   cannot reverse them.
//! - **The account** arrives over control 7 and is read per pass, never
//!   cached: a token expires and an account is switched while a transfer runs.
//! - **The announcement** wakes whatever is waiting for that picture. On this
//!   platform the thing waiting is the window's own pending `GET`, because
//!   that request already blocks on hydration — so a blob a background pass
//!   fetched completes a request the window made for its own reasons, and the
//!   picture appears without a second round trip.
//! - **The triggers** are §10's, and the one that fires most often —
//!   a durable document change — is gated on the hash set having actually
//!   moved, inside the engine, before this file is entered.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use koru_account_core::workspace_authorization::Region;
use koru_blob_sync::{
    BlobDownloads, BlobEngine, BlobPorts, BlobStorePort, BlobTransferStatus, ByteOrigins,
    CloudAccess, CloudAccessFacts, CoverageReport, CoverageRequest, DocumentSample, DocumentState,
    HttpControlPlane, HttpObjectPlane, LiveAuthorization, LocalBlob, PortFuture, SampledDocument,
    TransferError, Trigger, WalkDocument, WalkNode,
};
use koru_projection::{Projections, worker::Sweeps};
use koru_store_api::state::{BlobHydrationFailure, BlobHydrationService};
use koru_store_core::{
    DocumentId, WorkspaceId,
    errors::Code,
    model::{BlobHash, Timestamp},
};
use koru_sync_control::ControlHost;

mod admission;

use super::StoreWorkerClient;

// ------------------------------------------------------------------- walk

/// The canonical walk's input, read from the projection.
///
/// Its own connection to `projection.db`, like the attribute and task ports:
/// WAL lets a reader answer without taking the projector's mutex or the
/// Store's authoritative lock, and a walk is exactly the kind of read that
/// must not be behind either.
pub struct ProjectionWalk {
    projections: Mutex<Projections>,
    sweeps: Sweeps,
}

impl ProjectionWalk {
    pub fn open(root: &std::path::Path, sweeps: Sweeps) -> Option<Arc<Self>> {
        match Projections::open(root) {
            Ok(projections) => Some(Arc::new(Self {
                projections: Mutex::new(projections),
                sweeps,
            })),
            Err(error) => {
                tracing::error!("the blob reference projection did not open: {error}");
                None
            }
        }
    }

    /// **One read**, with each document's state beside its references.
    ///
    /// Every row the projection returns belongs to a listed document, which is
    /// what makes `durable` true without a predicate anybody could forget: a
    /// permanently removed document has no listing entry to join to. The
    /// *state* beside it is the version the read model projected those rows
    /// from, and the two arrive together out of one deferred read transaction —
    /// which is what lets Cloud coverage say *these pictures belong to that
    /// document at that state* rather than to two reads taken a moment apart.
    /// **The address one file document holds** (D259 §4).
    ///
    /// One indexed row on this crate's own projection connection, and no
    /// sweep request: a file somebody can see in the tree has been projected,
    /// and asking for a sweep per selection would put a write behind every
    /// click. An unprojected file answers `None`, which the pane says as
    /// *not measured* rather than as a fact about the bytes.
    fn file_blob(&self, document: DocumentId) -> Option<(String, Option<String>)> {
        self.projections
            .lock()
            .ok()?
            .file_blob(document)
            .ok()
            .flatten()
    }

    fn sample(&self, workspace: WorkspaceId) -> Result<DocumentSample, TransferError> {
        // A Workspace nothing has touched since start-up is cold, and a cold
        // read model answers "no references" — which is indistinguishable from
        // a Workspace with no pictures. Asking for a sweep first is what the
        // other read ports do, for the same reason.
        self.sweeps.request(workspace);
        let rows = self
            .projections
            .lock()
            .map_err(|_| TransferError::Local(Code::ProjectionUnavailable))?
            .blob_reference_sample(workspace)
            .map_err(|_| TransferError::Local(Code::ProjectionUnavailable))?;
        Ok(DocumentSample::new(
            rows.into_iter()
                .filter_map(|row| {
                    // A listing row whose id will not parse is a row this build
                    // cannot say anything about. Dropping it silently would
                    // shrink the sample; it is left out and the walk's own
                    // unreadable counter is not the place for it either, so the
                    // honest effect is that its references never appear —
                    // which cannot happen, because the same id produced them.
                    let document = DocumentId::parse(&row.document)?;
                    Some(SampledDocument {
                        document,
                        state: row.state.map(DocumentState::from_bytes),
                        content: WalkDocument {
                            durable: true,
                            trashed: row.trashed,
                            nodes: row
                                .references
                                .into_iter()
                                .map(|(blob, mime)| WalkNode {
                                    blob: Some(blob),
                                    mime,
                                })
                                .collect(),
                        },
                    })
                })
                .collect(),
        ))
    }
}

// ------------------------------------------------------------------ store

/// The Store, behind the worker that owns it.
pub struct StoreBlobs {
    worker: StoreWorkerClient,
    walk: Arc<ProjectionWalk>,
    root: PathBuf,
}

impl StoreBlobs {
    fn refusal(error: koru_store_core::Error) -> TransferError {
        TransferError::Local(error.code())
    }

    fn worker_gone(_: super::WorkerError) -> TransferError {
        TransferError::Local(Code::StoreUnreachable)
    }
}

impl BlobStorePort for StoreBlobs {
    fn sample_documents(
        &self,
        workspace: WorkspaceId,
    ) -> PortFuture<'_, Result<DocumentSample, TransferError>> {
        let walk = self.walk.clone();
        Box::pin(async move {
            // Off the caller's thread: a sample is SQLite work, and the task
            // that answers a person never waits on the world.
            tokio::task::spawn_blocking(move || walk.sample(workspace))
                .await
                .unwrap_or(Err(TransferError::Local(Code::Internal)))
        })
    }

    fn object_present(&self, hash: BlobHash) -> PortFuture<'_, Result<bool, TransferError>> {
        // One lookup: since B1 there is one object store per root, so
        // "anywhere on this device" is a single `exists`. The signature was
        // frozen for exactly this, and nothing here moved when the layout did.
        //
        // It cannot see a blob still sitting in a pre-B1 Workspace directory —
        // it has no Workspace to scope a legacy search by — so on a store not
        // yet read since the upgrade this can answer "no" for bytes that are
        // physically present. `workspace_blob` below is the scoped question
        // and it does promote.
        Box::pin(async move {
            self.worker
                .with_store_async(move |store| store.object_present(&hash).map_err(Self::refusal))
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn workspace_blob(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<Option<LocalBlob>, TransferError>> {
        Box::pin(async move {
            self.worker
                .with_store_async(move |store| {
                    // **Associated *and* present**, and no caller infers the
                    // second from the first. A row without bytes is a
                    // Workspace that knows about a picture it does not hold.
                    if !store
                        .workspace_has_blob(workspace, &hash)
                        .map_err(Self::refusal)?
                    {
                        return Ok(None);
                    }
                    match store.blob(workspace, &hash) {
                        Ok(blob) => Ok(Some(LocalBlob {
                            hash: blob.hash,
                            size: blob.size,
                            mime: blob.mime,
                            filename: blob.filename,
                        })),
                        Err(error) if error.code() == Code::BlobNotFound => Ok(None),
                        Err(error) => Err(Self::refusal(error)),
                    }
                })
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn verify_object(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<(), TransferError>> {
        Box::pin(async move {
            self.worker
                .with_store_async(move |store| {
                    // Re-hashes and quarantines a rotted object by copying it
                    // aside — never by removing it, because the document still
                    // references it.
                    store.verify_blob(workspace, &hash).map_err(Self::refusal)
                })
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn open_blob(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<Box<dyn std::io::Read + Send>, TransferError>> {
        Box::pin(async move {
            // §9's `open_blob`, straight through. The handle outlives the
            // Store borrow, so the lock is released before the first chunk is
            // read — and `get_blob` would have returned the whole file as a
            // `Vec`, which is the mistake the signature exists to avoid.
            self.worker
                .with_store_async(move |store| {
                    store.open_blob(workspace, &hash).map_err(Self::refusal)
                })
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn install_downloaded(
        &self,
        workspace: WorkspaceId,
        blob: LocalBlob,
        staged: PathBuf,
    ) -> PortFuture<'_, Result<(), TransferError>> {
        Box::pin(async move {
            // 1. An owned pending upload, so the Store lock is not held while
            //    a gigabyte is read off the disk.
            let pending = self
                .worker
                .with_store_async(move |store| {
                    store.begin_blob_upload(workspace).map_err(Self::refusal)
                })
                .await
                .map_err(Self::worker_gone)??;

            // 2. **The object.** Streamed in fixed buffers on a blocking
            //    thread, and published atomically by `finish`, which re-hashes
            //    what it wrote — so the address this returns is the address the
            //    bytes have, not the one anybody claimed.
            let expected = blob.hash.clone();
            let completed = tokio::task::spawn_blocking(move || {
                use std::io::Read as _;
                let mut pending = pending;
                let mut file = std::fs::File::open(&staged)
                    .map_err(|_| TransferError::Local(Code::StorageFailed))?;
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    let read = file
                        .read(&mut buffer)
                        .map_err(|_| TransferError::Local(Code::StorageFailed))?;
                    if read == 0 {
                        break;
                    }
                    pending
                        .write_chunk(&buffer[..read])
                        .map_err(StoreBlobs::refusal)?;
                }
                pending.finish().map_err(StoreBlobs::refusal)
            })
            .await
            .unwrap_or(Err(TransferError::Local(Code::Internal)))?;
            if completed.hash != expected || completed.size != blob.size {
                return Err(TransferError::Local(Code::BlobChecksumMismatch));
            }

            // 3. **The association, last.** Until this row exists a metadata
            //    lookup answers "not here", which is the truth; the reverse
            //    order would have it answer "here" over partial bytes.
            self.worker
                .with_store_async(move |store| {
                    store
                        .finish_blob_upload(
                            workspace,
                            completed,
                            &blob.mime,
                            blob.filename.as_deref(),
                            Timestamp::now(),
                            // The origin device is whoever *first* held the
                            // bytes, and this device received them. Stamping
                            // ourselves would rewrite provenance on every hop.
                            None,
                        )
                        .map(|_| ())
                        .map_err(Self::refusal)
                })
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn release_blob(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> PortFuture<'_, Result<u64, TransferError>> {
        Box::pin(async move {
            self.worker
                .with_store_async(move |store| {
                    // The Store checks the association and keeps it; whether
                    // the bytes were safe to drop was the engine's question,
                    // answered before this was called.
                    store.release_blob(workspace, &hash).map_err(Self::refusal)
                })
                .await
                .map_err(Self::worker_gone)?
        })
    }

    fn staging_root(&self) -> PathBuf {
        self.root.join("blobs").join("staging")
    }
}

// ------------------------------------------------------- the transport mode

/// **§6's gate, forwarded to the Store and decided nowhere else.**
///
/// The peer path asks `Store::admit_transport` through
/// `LiveReplicationStore::admit_peer_blob_transfer`; this is the same call with
/// the other `TransportPath`. Neither re-derives the ladder, the Profile
/// ceiling or the fence, which is what makes "both transports ask one question"
/// a fact about the code rather than a hope.
///
/// It runs on the Store worker, which serializes it behind whatever job is in
/// front of it. That is the right cost: the answer has to be the committed one,
/// and a Workspace's transfers are one at a time anyway.
pub struct StoreModes {
    worker: StoreWorkerClient,
}

impl koru_blob_sync::TransportModes for StoreModes {
    fn admit(
        &self,
        workspace: WorkspaceId,
        path: koru_store_core::TransportPath,
    ) -> PortFuture<'_, Result<(), TransferError>> {
        Box::pin(async move {
            self.worker
                .with_store_async(move |store| {
                    store.admit_transport(
                        koru_store_core::TransportSubject::Workspace(workspace),
                        path,
                    )
                })
                .await
                // A worker that has stopped is not a mode decision, and
                // answering "permitted" because nobody could be asked would be
                // the one failure this gate exists to prevent.
                .map_err(StoreBlobs::worker_gone)?
                .map(|_| ())
                .map_err(|error| match error.code() {
                    // `Forbidden` out of `admit_transport` is the mode itself
                    // refusing, which since B5 is a hold with a name rather
                    // than one word shared with every other pause.
                    koru_store_core::errors::Code::Forbidden => {
                        TransferError::NotPermitted(koru_blob_sync::TransferHold::TransportMode)
                    }
                    _ => StoreBlobs::refusal(error),
                })
        })
    }
}

// ------------------------------------------------------------- the peer arm

/// **§8's peer-first candidate on the desktop.**
///
/// The engine asks for one bounded range; this forwards it to whichever
/// replication runtime this process has, and the runtime applies D246's
/// selected-path gate per candidate. `None` means no authorized peer on a
/// direct path is offering these bytes right now, which the engine reads as
/// "go to Cloud" rather than as a failure.
///
/// **No iroh type crosses into the engine**, and none crosses back: what
/// returns is bytes, a length and the metadata a peer already advertised.
pub struct RuntimePeers {
    runtime: crate::sync_control::ReplicationRuntime,
}

impl koru_blob_sync::PeerBlobs for RuntimePeers {
    fn read_range(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
        offset: u64,
        max: u64,
    ) -> PortFuture<'_, Result<Option<koru_blob_sync::PeerChunk>, TransferError>> {
        Box::pin(async move {
            Ok(self
                .runtime
                .read_blob_range(workspace, hash, offset, max)
                .await
                .map(|range| koru_blob_sync::PeerChunk {
                    bytes: range.bytes,
                    total: range.total,
                    mime: range.mime,
                    filename: range.filename,
                }))
        })
    }
}

// ----------------------------------------------------------------- access

/// The account's reach into the control plane, read from the control host on
/// every pass.
///
/// **Never cached.** A token expires and an account is switched while a
/// transfer runs, and a cached one is a request this device knows will be
/// refused.
pub struct ControlAccess {
    host: Arc<Mutex<ControlHost>>,
    /// The same fact, installed **in process** by a host that holds the
    /// account itself.
    ///
    /// Control 7 exists because on the desktop the account lives in
    /// `koru-agent` and the Store lives here (D257). A phone has both in one
    /// process and no control session to sequence, so it writes the value
    /// straight in. The *shape* is the same value and the same `validate`, so
    /// neither platform gets a second idea of what blob access is — and the
    /// same expiry rule applies, because a token this host knows is dead is a
    /// request it should not make.
    local: Mutex<Option<koru_sync_control::cloud::CloudBlobAccessV1>>,
    /// The compiled-in per-region set, plus whatever local stack this device
    /// was explicitly told about. **Built here, from this device's own build
    /// or environment**, and never from the control message: where a person's
    /// file may be sent is a fact the device holds.
    ///
    /// The desktop reads `KORU_CLOUD_BLOB_BYTE_ORIGINS` when the engine is
    /// composed. **Android has no path to an environment variable** — its
    /// configuration arrives as `BuildConfig` fields compiled from
    /// `.env.local`, and nothing sets a variable in that process — so a phone
    /// installs the same string through
    /// [`BlobSync::set_development_origins`]. One value, one meaning, one
    /// check: both go through [`ByteOrigins::with_development_origins`], so
    /// neither platform can acquire a second idea of where bytes may be sent.
    development: Mutex<Option<String>>,
}

impl ControlAccess {
    /// Install the account's reach into the control plane from inside this
    /// process. Shape-checked exactly as the control command is; a value that
    /// could never work is refused here rather than discovered as a `401`.
    /// Answers whether access *changed*: there was none live before, or the
    /// bearer is a different one. The caller runs the passes that were
    /// refused for want of it.
    ///
    /// **A different bearer counts, not only a first one.** A phone restores
    /// its stored token at start and refreshes it a moment later; the first
    /// pass ran on the stale one, the region answered 401, and every
    /// Workspace was stamped as having no access. The fresh token then
    /// arrived into a slot that already held a live-looking value, and
    /// nothing ran.
    fn install(
        &self,
        access: koru_sync_control::cloud::CloudBlobAccessV1,
    ) -> Result<bool, &'static str> {
        access.validate().map_err(|error| error.code())?;
        let now = now_millis();
        let mut local = self.local.lock().unwrap_or_else(|error| error.into_inner());
        let unchanged = local
            .as_ref()
            .is_some_and(|held| held.live_at(now) && held.access_token == access.access_token);
        *local = Some(access);
        Ok(!unchanged)
    }

    /// Install this device's local-stack allowance from inside the process,
    /// for a host that has no environment to read one from.
    ///
    /// **Checked by the allowlist itself, before it is kept.** A value that
    /// will not parse — or that names a public host — is refused here and
    /// nothing is stored, so the compiled-in production set stands exactly as
    /// it does on a desktop whose variable was refused. The difference is only
    /// that a phone says so once, at the moment it was configured, rather than
    /// on every pass: a build is configured once and a person reading logcat
    /// after a failed transfer should not have to wait for a pass to be told
    /// their setting was never in force.
    fn install_development_origins(&self, raw: &str) -> Result<(), koru_blob_sync::OriginRefused> {
        // The region is irrelevant to the check — `with_development_origins`
        // only ever adds — so this validates against the empty set rather than
        // guessing at a region the account has not yet named.
        ByteOrigins::none().with_development_origins(raw)?;
        *self
            .development
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(raw.to_owned());
        Ok(())
    }

    fn development_origins(&self) -> Option<String> {
        self.development
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn origins(&self, region: Region) -> ByteOrigins {
        let origins = ByteOrigins::for_region(region);
        let development = self
            .development
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        match development.as_deref() {
            Some(raw) => origins
                .with_development_origins(raw)
                .unwrap_or_else(|refused| {
                    // An argument that is not understood is not run: the local
                    // allowance is dropped and the production set stands, loudly.
                    tracing::warn!(
                        target: "koru_blob_sync",
                        event = "cloud.blob.development_origins_refused",
                        code = refused.message(),
                    );
                    ByteOrigins::for_region(region)
                }),
            None => origins,
        }
    }
}

impl CloudAccess for ControlAccess {
    fn access(&self, _workspace: WorkspaceId) -> Option<CloudAccessFacts> {
        let now = now_millis();
        // The in-process value first: a host that installed one holds the
        // account, so there is no control session to have installed the other.
        // On the desktop this is always empty and the control host answers.
        let local = self
            .local
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|access| access.live_at(now))
            .cloned();
        let access = match local {
            Some(access) => access,
            None => {
                let host = self.host.lock().unwrap_or_else(|error| error.into_inner());
                host.cloud_blob_access(now)?.clone()
            }
        };
        let region = match access.region.as_str() {
            "eu" => Region::Eu,
            _ => Region::Us,
        };
        let api_origin = url::Url::parse(&access.api_origin).ok()?;
        Some(CloudAccessFacts {
            api_origin,
            bearer: access.access_token.clone(),
            region,
            byte_origins: self.origins(region),
        })
    }
}

// ----------------------------------------------------------------- policy

/// The network policy, as a host installs it.
///
/// **Permissive until told otherwise**, which is what a desktop on mains power
/// and Ethernet means and is also the only safe default: a gate that refused
/// while nobody had answered would stop Cloud blobs on every installation, the
/// way asking `transport_admission` before B8's initializer would.
#[derive(Default)]
pub struct HostPolicy {
    installed: Mutex<Option<Arc<dyn koru_blob_sync::BlobPolicy>>>,
}

impl HostPolicy {
    fn installed(&self) -> Option<Arc<dyn koru_blob_sync::BlobPolicy>> {
        self.installed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl koru_blob_sync::BlobPolicy for HostPolicy {
    fn transfers_permitted(&self, workspace: WorkspaceId) -> bool {
        self.installed()
            .is_none_or(|policy| policy.transfers_permitted(workspace))
    }

    fn hold(&self, workspace: WorkspaceId) -> koru_blob_sync::TransferHold {
        self.installed()
            .map(|policy| policy.hold(workspace))
            .unwrap_or_default()
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

// -------------------------------------------------------------- downloads

/// The announcement seam, and the thing on this platform that is waiting.
///
/// `watching` is asked before bytes move and decides **order**, never whether:
/// a referenced blob is fetched whether or not anybody is looking, or coverage
/// would depend on which tab was open.
///
/// `arrived` wakes every request parked on that hash. The window's own
/// `GET /blobs/{hash}` blocks on hydration, so a blob a background pass
/// fetched completes a request the window made for its own reasons — the
/// picture appears without a second round trip, and **after durability, never
/// before**.
#[derive(Default)]
pub struct BlobArrivals {
    waiters: Mutex<HashMap<(WorkspaceId, String), Arc<tokio::sync::Notify>>>,
    rooms: Option<Arc<koru_store_api::rooms::Rooms>>,
    /// Which documents reference which hash, as the engine learned it from
    /// durable changes. Shared with the engine rather than recomputed: the
    /// engine already holds it to answer §10's second trigger.
    documents: Mutex<HashMap<(WorkspaceId, String), Vec<DocumentId>>>,
    /// A second listener, installed by an embedding host whose surfaces are
    /// not in this process's rooms.
    ///
    /// **Android has no room registry** (D242): its surfaces re-read the Store
    /// through the shim, so the announcement §11 owes has to reach them by
    /// their own path. Consulted beside the rooms rather than instead of them,
    /// so a host that has both keeps both.
    observer: Mutex<Option<Arc<dyn BlobDownloads>>>,
}

impl BlobArrivals {
    pub fn new(rooms: Arc<koru_store_api::rooms::Rooms>) -> Arc<Self> {
        Arc::new(Self {
            waiters: Mutex::new(HashMap::new()),
            rooms: Some(rooms),
            documents: Mutex::new(HashMap::new()),
            observer: Mutex::new(None),
        })
    }

    fn observer(&self) -> Option<Arc<dyn BlobDownloads>> {
        self.observer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn key(workspace: WorkspaceId, hash: &BlobHash) -> (WorkspaceId, String) {
        (workspace, hash.as_str().to_owned())
    }

    /// A handle that resolves when this hash becomes durable and associated.
    ///
    /// Taken **before** the caller checks the Store, or an arrival between the
    /// check and the wait is an arrival nobody hears. `Notify` stores no
    /// permit, so a caller that misses the wake-up still has to check the
    /// Store — which every caller here does, because the pass it is waiting
    /// on also answers.
    ///
    /// An entry is removed when the blob arrives, so what is held is one
    /// small value per hash this device has asked for and not yet got:
    /// bounded by the Workspace's own missing set, which is the same order as
    /// everything else the engine holds.
    pub fn waiter(&self, workspace: WorkspaceId, hash: &BlobHash) -> Arc<tokio::sync::Notify> {
        self.waiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(Self::key(workspace, hash))
            .or_insert_with(|| Arc::new(tokio::sync::Notify::new()))
            .clone()
    }

    /// Remember which documents name a hash, so `watching` can ask the rooms.
    pub fn record(&self, workspace: WorkspaceId, document: DocumentId, hashes: &[BlobHash]) {
        let mut held = self
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        held.retain(|_, documents| {
            documents.retain(|held| *held != document);
            !documents.is_empty()
        });
        for hash in hashes {
            held.entry(Self::key(workspace, hash))
                .or_default()
                .push(document);
        }
    }
}

impl BlobDownloads for BlobArrivals {
    fn watching(&self, workspace: WorkspaceId, hash: &BlobHash) -> bool {
        if self
            .observer()
            .is_some_and(|observer| observer.watching(workspace, hash))
        {
            return true;
        }
        let Some(rooms) = self.rooms.as_ref() else {
            return false;
        };
        let held = self
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        held.get(&Self::key(workspace, hash))
            .is_some_and(|documents| {
                documents.iter().any(|document| {
                    rooms.subscribers(&koru_store_api::rooms::Room::Document(*document)) > 0
                })
            })
    }

    fn arrived(&self, workspace: WorkspaceId, hash: &BlobHash) {
        // Identity, never bytes. At blob sizes the alternative is a 2 GiB
        // `Vec` on the Store dispatcher, which on a phone is a kill.
        let waiter = self
            .waiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&Self::key(workspace, hash));
        if let Some(waiter) = waiter {
            waiter.notify_waiters();
        }
        // After durability, never before — and the host's own path gets the
        // same call at the same moment, not a second one later.
        if let Some(observer) = self.observer() {
            observer.arrived(workspace, hash);
        }
        tracing::debug!(
            target: "koru_blob_sync",
            event = "cloud.blob.arrived",
            workspace = %workspace,
            hash = %hash,
        );
    }
}

// -------------------------------------------------------------- hydration

/// The window's `GET /blobs/{hash}`, extended to Cloud.
///
/// The peer path is asked first and keeps every property it had. Cloud is the
/// fallback, which is the order §8 prescribes for Cloud mode and is also the
/// order a person would want: the device across the room before the device
/// across the ocean. This is §10's sixth trigger — an explicit request for a
/// visible missing blob. The **background** pass's peer-first ordering and the
/// progress the two paths share are B8's, and are composed below through the
/// engine's `peers` port rather than here.
pub struct CloudFallbackHydration {
    peers: Option<Arc<dyn BlobHydrationService>>,
    engine: Arc<BlobEngine>,
    arrivals: Arc<BlobArrivals>,
    store: Arc<dyn BlobStorePort>,
}

impl BlobHydrationService for CloudFallbackHydration {
    fn hydrate(
        &self,
        workspace: WorkspaceId,
        hash: BlobHash,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), BlobHydrationFailure>> + Send + 'static>,
    > {
        let peers = self.peers.clone();
        let engine = self.engine.clone();
        let arrivals = self.arrivals.clone();
        let store = self.store.clone();
        Box::pin(async move {
            // Taken before anything is asked, or an arrival between the ask
            // and the wait is an arrival nobody hears.
            let waiter = arrivals.waiter(workspace, &hash);
            let peer_failure = match peers {
                Some(peers) => match peers.hydrate(workspace, hash.clone()).await {
                    Ok(()) => {
                        // **The count has to learn about this.** The bytes came
                        // from a device across the room and never passed
                        // through the engine, so its tally still says the file
                        // is absent — and since D260 the next pass may be a
                        // long time coming. The same request-triggered pass the
                        // Cloud path runs, spawned the same way: it finds the
                        // file here, confirms Cloud has it, and the count moves.
                        engine.request(workspace, &hash);
                        let engine = engine.clone();
                        tokio::spawn(async move {
                            engine.reconcile(workspace, Trigger::Requested(hash)).await
                        });
                        return Ok(());
                    }
                    Err(failure) => Some(failure),
                },
                None => None,
            };
            // Somebody is looking at a placeholder: unpark it and put it first.
            engine.request(workspace, &hash);
            // **The pass runs as its own task and this request waits for its
            // own picture, not for the pass.** A Workspace with forty missing
            // blobs is one pass; holding an HTTP request open until the last
            // of them finished would make one person's placeholder wait on
            // somebody else's 2 GiB file. The requested hash goes first, so
            // the announcement usually arrives long before the pass ends —
            // and the pass carries on afterwards either way, because a
            // browser going away is not a reason to abandon a transfer.
            let running = {
                let engine = engine.clone();
                let hash = hash.clone();
                tokio::spawn(
                    async move { engine.reconcile(workspace, Trigger::Requested(hash)).await },
                )
            };
            let pass = tokio::select! {
                () = waiter.notified() => return Ok(()),
                finished = running => match finished {
                    Ok(pass) => pass,
                    Err(_) => return Err(BlobHydrationFailure {
                        code: Code::Internal.as_str().to_owned(),
                        retryable: true,
                        waiting_on: Vec::new(),
                        hold: None,
                    }),
                },
            };
            let arrived = store
                .workspace_blob(workspace, hash.clone())
                .await
                .ok()
                .flatten()
                .is_some();
            if arrived {
                return Ok(());
            }
            Err(hydration_failure(peer_failure, &pass))
        })
    }
}

/// **What a caller is told when the picture did not arrive** — and, when the
/// Cloud pass is holding, *why* (D259).
///
/// A named function rather than three lines inside the future, because the
/// rule in it is a product decision and was previously the wrong one: the
/// peer's answer was returned whole and the Cloud pass's own refusal was
/// discarded.
///
/// `blob_waiting_for_peer` says *nobody offering these bytes is online right
/// now*. A session hold says *this device will not fetch them at all, for
/// this Workspace*. **Both are true at once** whenever a Workspace has never
/// been promoted out of `local_only` and no peer is connected — which is the
/// state the hardware session was actually in — and the peer answer won, so
/// the placeholder sent a person to restart a machine they did not need.
///
/// The **code** stays the peer's, because it is what decides whether a caller
/// should come back; the **hold** is what a person reads, and it rides
/// beside the code rather than replacing it. Cloud sync being off for the
/// Profile is the same kind of answer without being a `TransferHold`, so it
/// is carried too.
fn hydration_failure(
    peer: Option<BlobHydrationFailure>,
    pass: &koru_blob_sync::Pass,
) -> BlobHydrationFailure {
    let code = pass
        .session_refusal
        .or_else(|| pass.parked.first().map(|(_, code)| *code));
    let mut failure = peer.unwrap_or(BlobHydrationFailure {
        code: code.unwrap_or(Code::BlobNotFound.as_str()).to_owned(),
        // A Cloud refusal about the session clears on a trigger, not on a
        // retry; one about this blob does not clear at all. The honest
        // answer to "should the caller come back" is therefore the one the
        // peer path would have given.
        retryable: false,
        waiting_on: Vec::new(),
        hold: None,
    });
    failure.hold = pass
        .session_hold
        .map(koru_blob_sync::TransferHold::as_str)
        .or_else(|| {
            pass.session_refusal
                .filter(|code| *code == koru_blob_sync::TransferError::NoAccess.code())
        })
        .map(str::to_owned);
    failure
}

// -------------------------------------------------------------- the feed

/// §10's second trigger, at its source.
///
/// A projection consumer beside the search and events feeds. It sees every
/// **durable** document change — the projector runs after a commit, not after
/// a keystroke — and records the hash set that document now references. The
/// question "did the Workspace's set move" is answered later, in the engine,
/// and only a `true` costs a walk.
///
/// **Coalescing by construction.** A paste of six pictures is six projections
/// and one drain; a document that changed twice before the drain is one entry,
/// because the entry is keyed by the document and the newest set is the true
/// one. That is what keeps a burst from becoming a burst of passes without a
/// timer anywhere.
#[derive(Default)]
pub struct BlobChanges {
    pending: Mutex<Vec<(WorkspaceId, DocumentId, Vec<BlobHash>)>>,
    wake: tokio::sync::Notify,
}

impl BlobChanges {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn push(&self, workspace: WorkspaceId, document: DocumentId, hashes: Vec<BlobHash>) {
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            pending.retain(|(_, held, _)| *held != document);
            pending.push((workspace, document, hashes));
        }
        self.wake.notify_one();
    }

    fn drain(&self) -> Vec<(WorkspaceId, DocumentId, Vec<BlobHash>)> {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

/// The consumer itself. Registered before the projector starts, like the
/// search and events feeds, and wired to an engine that does not exist yet —
/// which is why the two meet through [`BlobChanges`] rather than directly.
pub struct BlobChangeFeed {
    changes: Arc<BlobChanges>,
}

impl BlobChangeFeed {
    pub fn new(changes: Arc<BlobChanges>) -> Box<dyn koru_projection::Consumer> {
        Box::new(Self { changes })
    }
}

impl koru_projection::Consumer for BlobChangeFeed {
    fn name(&self) -> &str {
        "blob-references"
    }

    fn projected(&mut self, projection: &koru_projection::DocumentProjection) {
        // **Every node, including a binned one.** A reference in the
        // document's own bin is restorable, so its bytes are still owed — the
        // same rule the canonical walk applies, applied at its source so the
        // two cannot drift.
        let hashes: Vec<BlobHash> = projection
            .nodes
            .iter()
            .filter_map(|node| node.blob.as_deref())
            .filter_map(|blob| BlobHash::parse(blob).ok())
            .collect();
        self.changes
            .push(projection.workspace, projection.document, hashes);
    }

    fn removed(&mut self, workspace: WorkspaceId, document: DocumentId) {
        // Dropped, not binned: this document references nothing any more.
        self.changes.push(workspace, document, Vec::new());
    }
}

// ------------------------------------------------------------- the handle

/// What the daemon holds and the rest of the process talks to.
///
/// `Debug` is the empty one on purpose: everything inside is a handle, and the
/// account's bearer token is one field away. A `{:?}` added while debugging
/// something else is the one way a value like that escapes (D257).
#[derive(Clone)]
pub struct BlobSync {
    admission: Arc<admission::Observer>,
    engine: Arc<BlobEngine>,
    arrivals: Arc<BlobArrivals>,
    /// The two seams an embedding host fills after the engine exists: the
    /// account it holds itself, and the network policy its platform reads.
    access: Arc<ControlAccess>,
    policy: Arc<HostPolicy>,
    /// The document half of coverage, per Workspace, from the last Cloud pass.
    ///
    /// **Held so the two halves can converge.** The blob half moves on every
    /// reconciliation — a picture finishes uploading, a download lands — while
    /// the document half only moves when a Cloud pass runs. Without this, a
    /// Workspace whose last picture arrived two minutes after its documents
    /// did would keep reporting *incomplete* until the next pass, which is a
    /// Cloud icon that is wrong for as long as nothing happens.
    ///
    /// In memory, like everything else about a pass: a coverage input that
    /// survived a restart would start to look like a durable verdict about
    /// somebody's Workspace.
    documents: Arc<Mutex<HashMap<WorkspaceId, CoverageInputs>>>,
    /// **How a selected file becomes an address** (D259 §4).
    ///
    /// A workspace file is a document whose root node is the blob, and the
    /// read model already holds that row. The same connection the canonical
    /// walk reads through, because it is the same question asked at one
    /// document instead of at a Workspace.
    walk: Arc<ProjectionWalk>,
}

/// What one Workspace's coverage is computed from, between passes.
#[derive(Clone)]
struct CoverageInputs {
    incarnation: koru_device::cloud::CloudId,
    documents: koru_blob_sync::CoveredDocuments,
    /// Re-asked at every boundary, never cached: a revocation landing between
    /// two reconciliations is exactly what this must notice.
    authorization: LiveAuthorization,
}

impl std::fmt::Debug for BlobSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlobSync")
    }
}

impl BlobSync {
    /// Bind the shared background observer to this installed current permit.
    /// Current account/owner proof is completed by the owner-only usage read.
    pub fn bind_admission(
        &self,
        runtime: &tokio::runtime::Handle,
        permit: &koru_sync_control::cloud::CloudPermitV2,
        holds: koru_network_iroh::cloud::client_io::reconcile::UploadAdmissionHolds,
        live: LiveAuthorization,
    ) {
        self.admission.register(runtime, permit, holds, live);
    }

    /// Install the account's reach into the blob control plane from inside
    /// this process — the in-process counterpart of control 7's
    /// `replace_cloud_blob_access`, for a host that holds the account itself
    /// (D257: *the same engine must run in-process on Android where no
    /// boundary exists*).
    ///
    /// **Pushed every pass rather than on change**, exactly as the control
    /// command is, so the value carries how long it may be used without being
    /// confirmed again and a host that goes quiet stops making
    /// account-authenticated requests.
    ///
    /// `Ok(true)` when access arrived rather than merely renewed; see
    /// [`Self::access_arrived`] for what to do with that.
    pub fn install_blob_access(
        &self,
        access: koru_sync_control::cloud::CloudBlobAccessV1,
    ) -> Result<bool, &'static str> {
        self.access.install(access)
    }

    /// **Access arrived: lift the stamps it left.** Every pass that ran
    /// while there was no token ended on the no-access code and stamped its
    /// Workspace so; a placeholder reading that stamp does not even ask. Run
    /// a pass over each such Workspace now, rather than whenever its
    /// document session next comes round. Spawned, not awaited: the token
    /// was just handed over by an account pass, which must not wait on
    /// files.
    pub fn access_arrived(&self, runtime: &tokio::runtime::Handle) {
        let code = koru_blob_sync::TransferError::NoAccess.code();
        for workspace in self.engine.workspaces_pending(code) {
            let engine = Arc::clone(&self.engine);
            runtime.spawn(async move {
                engine
                    .reconcile(workspace, koru_blob_sync::Trigger::Enabled)
                    .await;
            });
        }
    }

    /// Name a local object worker this device may send bytes to, from inside
    /// the process — the in-process counterpart of
    /// `KORU_CLOUD_BLOB_BYTE_ORIGINS`, for a host with no environment.
    ///
    /// **This is a development allowance and it cannot become anything else.**
    /// The value goes through the one
    /// [`ByteOrigins::with_development_origins`] the desktop uses, which
    /// admits a loopback, a private-range or a link-local address and the name
    /// `localhost`, and nothing else. A value naming a public host is refused
    /// here, nothing is stored, and the compiled-in per-region set stands —
    /// which is the whole reason that set is compiled in rather than
    /// configured.
    ///
    /// Returns the refusal so the caller can say so out loud once; it is never
    /// a reason to fail whatever was configuring this device, because a
    /// mistyped development setting must not take the account down with it.
    pub fn set_development_origins(&self, raw: &str) -> Result<(), koru_blob_sync::OriginRefused> {
        self.access.install_development_origins(raw)
    }

    /// What development allowance this device is currently holding, if any.
    ///
    /// Exists so a test can assert the allowance is **still** installed after a
    /// second activation. It was not, on a phone: the install sat below an
    /// early return that every launch after the first takes, so the setting
    /// reached the engine once and never again, and the symptom was a transfer
    /// refusing its own stack's byte URL. A property with no reader is a
    /// property nothing can pin.
    pub fn development_origins(&self) -> Option<String> {
        self.access.development_origins()
    }

    /// The platform's network policy: metered, low-data, offline, and the
    /// person's cellular allowance (D50). Absent, transfers are permitted,
    /// which is what a desktop on Ethernet means.
    pub fn set_policy(&self, policy: Arc<dyn koru_blob_sync::BlobPolicy>) {
        *self
            .policy
            .installed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(policy);
    }

    /// A second announcement listener, for a host whose surfaces are not in
    /// this process's rooms — which is every Android surface (D242).
    pub fn observe_arrivals(&self, observer: Arc<dyn BlobDownloads>) {
        *self
            .arrivals
            .observer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(observer);
    }

    /// Somebody is looking at a placeholder: unpark the blob and put it at the
    /// front of the next pass. §10's sixth trigger, and D50's override.
    pub fn request(&self, workspace: WorkspaceId, hash: &BlobHash) {
        self.engine.request(workspace, hash);
    }

    /// A durable document change. Returns whether the Workspace's hash set
    /// moved — **the only thing that is a reason to reconcile**. A commit that
    /// changed a paragraph answers `false` here and costs no walk, no Store
    /// lock and no packet.
    pub fn document_changed(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        hashes: &[BlobHash],
    ) -> bool {
        self.arrivals.record(workspace, document, hashes);
        self.engine.document_changed(workspace, document, hashes)
    }

    /// **Cloud coverage for one Workspace**, over the documents a pass just
    /// carried and the pictures they name.
    ///
    /// The document half is remembered so that later reconciliations — a
    /// picture finishing its upload, a download landing — recompute the claim
    /// without waiting for another Cloud pass. The blob half, the sample and
    /// the boundary check are the engine's.
    pub async fn coverage(
        &self,
        workspace: WorkspaceId,
        incarnation: koru_device::cloud::CloudId,
        documents: koru_blob_sync::CoveredDocuments,
        authorization: LiveAuthorization,
    ) -> Option<CoverageReport> {
        self.expect_documents(workspace, incarnation, documents, authorization);
        self.recompute_coverage(workspace).await
    }

    /// Install the document half without computing anything.
    ///
    /// **What a Cloud pass calls first**, so the reconciliation that follows
    /// computes the claim once with the fresh half rather than once with the
    /// previous pass's and once with this one's. Two computations would be two
    /// samples and two rounds of control requests for one pass, and the first
    /// of them would be a claim about the Workspace as the *last* pass left it.
    pub fn expect_documents(
        &self,
        workspace: WorkspaceId,
        incarnation: koru_device::cloud::CloudId,
        documents: koru_blob_sync::CoveredDocuments,
        authorization: LiveAuthorization,
    ) {
        self.documents
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                workspace,
                CoverageInputs {
                    incarnation,
                    documents,
                    authorization,
                },
            );
    }

    /// Recompute the claim from the remembered document half, if there is one.
    async fn recompute_coverage(&self, workspace: WorkspaceId) -> Option<CoverageReport> {
        let inputs = self
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&workspace)
            .cloned()?;
        let report = self
            .engine
            .coverage(
                CoverageRequest {
                    workspace,
                    incarnation: inputs.incarnation,
                    documents: inputs.documents,
                },
                &inputs.authorization,
            )
            .await;
        match report {
            Ok(report) => {
                // **News, and the two numbers that are the point.** Said at
                // `info` when something is owed and at `debug` when nothing
                // is, because a line saying a Workspace is still complete is
                // one a reader could have predicted.
                if report.is_current() {
                    tracing::debug!(
                        target: "koru_blob_sync",
                        event = "cloud.blob.coverage",
                        workspace = %workspace,
                        state = report.state.as_str(),
                        documents_required = report.documents_required,
                    );
                } else {
                    tracing::info!(
                        target: "koru_blob_sync",
                        event = "cloud.blob.coverage",
                        workspace = %workspace,
                        state = report.state.as_str(),
                        documents_required = report.documents_required,
                        documents_missing = report.documents_missing,
                        blobs_referenced = report.blobs_referenced,
                        blobs_missing = report.blobs_missing,
                        blobs_uploading = report.blobs_uploading,
                        unreadable_references = report.unreadable_references,
                    );
                }
                Some(report)
            }
            Err(error) => {
                // A Local-only Workspace and a device with no account both
                // land here, and neither is a failure: there is no Cloud
                // coverage to compute, so none is claimed.
                tracing::debug!(
                    target: "koru_blob_sync",
                    event = "cloud.blob.coverage_unavailable",
                    workspace = %workspace,
                    code = error.code(),
                );
                None
            }
        }
    }

    pub async fn reconcile(&self, workspace: WorkspaceId, trigger: Trigger) {
        let pass = self.engine.reconcile(workspace, trigger.clone()).await;
        // Report news, not activity: a pass that moved nothing and refused
        // nothing is the ordinary case once two devices agree — and a pass
        // *held* by the network is the ordinary case on a phone that is on
        // cellular all afternoon, which is why a hold is not a refusal here
        // either. The status carries it, always live and free.
        let held = pass.session_hold.is_some_and(|hold| hold.is_pause());
        if pass.uploaded > 0 || pass.downloaded > 0 || (pass.session_refusal.is_some() && !held) {
            tracing::info!(
                target: "koru_blob_sync",
                event = "cloud.blob.pass_finished",
                workspace = %workspace,
                trigger = trigger.as_str(),
                uploaded = pass.uploaded,
                downloaded = pass.downloaded,
                already_durable = pass.already_durable,
                parked = pass.parked.len(),
                refused = pass.session_refusal.unwrap_or(""),
            );
        }
        // **The claim follows the bytes.** A pass that moved a picture changed
        // whether this Workspace is current, and saying so without waiting for
        // the next Cloud pass is the difference between a Cloud icon that is
        // right and one that is right eventually.
        if !pass.skipped {
            self.recompute_coverage(workspace).await;
        }
    }

    pub async fn recover(&self, workspace: WorkspaceId) {
        if let Err(error) = self.engine.recover(workspace).await {
            tracing::warn!(
                target: "koru_blob_sync",
                event = "cloud.blob.recovery_refused",
                workspace = %workspace,
                code = error.code(),
            );
        }
    }

    pub fn status(&self, workspace: WorkspaceId) -> BlobTransferStatus {
        self.engine.status(workspace)
    }

    /// **Clear local files**: every copy Cloud holds leaves this device, and
    /// every reference stays. The engine owns the safety rule; this reports
    /// the refusal codes a surface can act on.
    pub async fn release_local(
        &self,
        workspace: WorkspaceId,
    ) -> Result<koru_blob_sync::ReleaseReport, &'static str> {
        self.engine.release_local(workspace).await.map_err(|error| {
            // Worth a line: a person pressed this and nothing happened.
            tracing::warn!(
                target: "koru_blob_sync",
                event = "cloud.blob.release_refused",
                workspace = %workspace,
                code = error.code(),
            );
            error.code()
        })
    }

    pub fn engine(&self) -> &Arc<BlobEngine> {
        &self.engine
    }

    /// Drain durable document changes forever, reconciling the Workspaces
    /// whose hash set actually moved.
    ///
    /// **No timer.** It waits on a notification and does nothing between them,
    /// which is the difference between this and the `about_to_wait` walk that
    /// cost 26% of a core.
    pub async fn follow(self, changes: Arc<BlobChanges>) {
        loop {
            changes.wake.notified().await;
            let mut moved: Vec<WorkspaceId> = Vec::new();
            for (workspace, document, hashes) in changes.drain() {
                if self.document_changed(workspace, document, &hashes)
                    && !moved.contains(&workspace)
                {
                    moved.push(workspace);
                }
            }
            for workspace in moved {
                self.reconcile(workspace, Trigger::DocumentsChanged).await;
            }
        }
    }
}

/// The §15 status surface, shaped for the workspace debug report.
///
/// **Ids, counts and codes only**, which is what the engine already holds:
/// there is nothing to redact here because nothing private is kept.
impl koru_store_api::state::BlobTransferStatusService for BlobSync {
    fn status(&self, workspace: WorkspaceId) -> koru_store_wire::internal::BlobTransferReport {
        report(&self.engine.status(workspace))
    }

    fn release_local(
        &self,
        workspace: WorkspaceId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<koru_store_wire::internal::BlobReleaseReport, &'static str>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let released = BlobSync::release_local(self, workspace).await?;
            Ok(koru_store_wire::internal::BlobReleaseReport {
                referenced: released.referenced,
                released: released.released,
                bytes_freed: released.bytes_freed,
                kept: released.kept,
                not_held: released.not_held,
                failure_code: released.failure_code,
            })
        })
    }

    /// **Which documents wait on bytes Cloud does not hold**, as the engine's
    /// parks and the canonical walk say. Read-only: no park moves.
    fn waiting(&self, workspace: WorkspaceId) -> koru_store_api::state::WaitingFuture<'_> {
        Box::pin(async move {
            let waiting = self
                .engine
                .waiting(workspace)
                .await
                .map_err(|error| error.code())?;
            Ok(waiting
                .into_iter()
                .map(|entry| koru_store_api::state::WaitingFiles {
                    document: entry.document,
                    blobs: entry.blobs,
                })
                .collect())
        })
    }

    /// **One document's own Cloud state**, as the last coverage computation
    /// and one sample of the reference projection say. Read-only: no park
    /// moves and nothing reaches a region.
    fn document_coverage(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> koru_store_api::state::DocumentCoverageFuture<'_> {
        Box::pin(async move {
            let coverage = self
                .engine
                .document_coverage(workspace, document)
                .await
                .map_err(|error| error.code())?;
            Ok(koru_store_wire::internal::CloudDocumentCoverage {
                workspace: Some(workspace),
                document: Some(document),
                synced: coverage.synced,
                files: coverage
                    .files
                    .map(|files| koru_store_wire::internal::CloudDocumentFiles {
                        images_total: files.images_total,
                        images_synced: files.images_synced,
                        files_total: files.files_total,
                        files_synced: files.files_synced,
                    }),
            })
        })
    }

    /// **A person's pause** (D256), and then the state it left behind.
    ///
    /// Resuming reconciles: the trigger list has no "somebody pressed play",
    /// and `PolicyPermissive` is exactly what this is — a hold that was on the
    /// pass has come off. Pausing starts nothing, because there is nothing to
    /// start.
    fn set_paused(
        &self,
        workspace: WorkspaceId,
        paused: bool,
    ) -> koru_store_wire::internal::BlobTransferReport {
        let changed = match paused {
            true => self.engine.pause(workspace),
            false => self.engine.resume(workspace),
        };
        if changed {
            // News, not activity: this happens when a person presses
            // something, and never otherwise.
            tracing::info!(
                target: "koru_blob_sync",
                event = "cloud.blob.transfer_paused",
                workspace = %workspace,
                paused,
            );
        }
        if changed && !paused {
            // **Off the caller's thread.** This is an HTTP handler answering a
            // person, and a pass can be gigabytes; the request says "let them
            // go", not "wait for them to arrive".
            let sync = self.clone();
            tokio::spawn(async move {
                sync.reconcile(workspace, Trigger::PolicyPermissive).await;
            });
        }
        report(&self.engine.status(workspace))
    }

    /// **§10's first trigger, at last raised by something.**
    ///
    /// Until the Profile prompt existed, nothing in a running Koru said
    /// *"Cloud is on for this Workspace now"* — the engine had the trigger
    /// and no caller, so the first pass over a newly enabled Workspace was
    /// whatever happened to come next, and the pre-flight summary D256 asks
    /// for had nowhere to be published from.
    fn enabled(&self, workspace: WorkspaceId) {
        let sync = self.clone();
        tokio::spawn(async move {
            sync.reconcile(workspace, Trigger::Enabled).await;
        });
    }

    /// **One file's own context** (D259 §4).
    ///
    /// Two lookups and no probe: the local record the Store already holds —
    /// which answers *are the bytes here*, *how big*, *what type*, *what was
    /// it called* — and what the last coverage pass found on Cloud for this
    /// address. A selection is a pointer move, and a pointer move must not
    /// cost a round trip to a region.
    ///
    /// The Workspace's hold is repeated on the row because the pane
    /// explaining one file needs the reason beside the file rather than three
    /// lines further up.
    fn file(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<koru_store_wire::internal::BlobFileReport>>
                + Send
                + '_,
        >,
    > {
        let engine = self.engine.clone();
        let walk = self.walk.clone();
        Box::pin(async move {
            // The read model turns the document a person selected into the
            // address it holds. A document with no blob reference is not a
            // file, and saying nothing is the honest answer for it — better
            // than a pane full of blanks under a name.
            let (hash, projected_mime) = tokio::task::spawn_blocking(move || walk.file_blob(file))
                .await
                .ok()
                .flatten()?;
            let hash = BlobHash::parse(&hash).ok()?;
            let facts = engine.file(workspace, hash).await;
            Some(koru_store_wire::internal::BlobFileReport {
                hash: facts.hash.as_str().to_owned(),
                size: facts.local.as_ref().map(|blob| blob.size),
                // The local record's media type where there is one, and the
                // read model's otherwise: a file this device has never held
                // still has a type the document recorded when it was made,
                // and withholding it would leave a blank where a fact is.
                mime: facts
                    .local
                    .as_ref()
                    .map(|blob| blob.mime.clone())
                    .or(projected_mime),
                filename: facts.local.as_ref().and_then(|blob| blob.filename.clone()),
                here: facts.local.is_some(),
                cloud: facts.cloud.map(|cloud| cloud.as_str().to_owned()),
                hold: facts.hold.map(str::to_owned),
            })
        })
    }
}

/// One status, shaped for the wire. Both readers share it so a field added to
/// one cannot go missing from the other.
fn report(status: &BlobTransferStatus) -> koru_store_wire::internal::BlobTransferReport {
    koru_store_wire::internal::BlobTransferReport {
        referenced: status.referenced,
        present: status.present,
        durable: status.durable,
        uploading: status.uploading,
        downloading: status.downloading,
        missing: status.missing,
        bytes_complete: status.bytes_complete,
        bytes_total: status.bytes_total,
        pending_reason: status.pending_reason.map(str::to_owned),
        last_failure_code: status.last_failure_code.map(str::to_owned),
        direction: status.direction.map(|direction| {
            match direction {
                koru_blob_sync::Direction::Upload => "upload",
                koru_blob_sync::Direction::Download => "download",
            }
            .to_owned()
        }),
        path: status.path.map(|path| path.as_str().to_owned()),
        object_host: status.object_host.clone(),
        oldest_retry_at_millis: status.oldest_retry_at_millis,
        unreadable_references: status.unreadable_references,
        walks: status.walks,
        passes: status.passes,
        passes_skipped: status.passes_skipped,
        paused: status.paused,
        preflight: status
            .preflight
            .map(|pre| koru_store_wire::internal::BlobPreflightReport {
                files: pre.files,
                upload_files: pre.upload_files,
                upload_bytes: pre.upload_bytes,
                download_files: pre.download_files,
                unreadable: pre.unreadable,
                at_millis: pre.at_millis,
            }),
        // **B4's claim, carried through the helper B10 introduced.** The two
        // landed in the same hour and the merge nearly dropped this: the
        // refactor to `report` was written against a status that had no
        // coverage on it yet, so the route would have compiled, answered, and
        // silently stopped telling anyone whether the pictures were there.
        coverage: status.coverage.as_ref().map(|coverage| {
            koru_store_wire::internal::BlobCoverageReport {
                state: coverage.state.as_str().to_owned(),
                current: coverage.is_current(),
                documents_required: coverage.documents_required,
                documents_missing: coverage.documents_missing,
                blobs_referenced: coverage.blobs_referenced,
                blobs_durable: coverage.blobs_durable,
                blobs_uploading: coverage.blobs_uploading,
                blobs_missing: coverage.blobs_missing,
                unreadable_references: coverage.unreadable_references,
                sample: coverage.sample.short(),
            }
        }),
    }
}

/// Compose the engine for this process.
///
/// Returns `None` when the read model would not open: a device whose
/// projection is unavailable cannot answer *which blobs does this Workspace
/// have*, and reconciling against a guess would upload the wrong set.
#[allow(clippy::too_many_arguments)]
pub(crate) fn install(
    root: &std::path::Path,
    sweeps: Sweeps,
    worker: StoreWorkerClient,
    host: Arc<Mutex<ControlHost>>,
    rooms: Arc<koru_store_api::rooms::Rooms>,
    peers: Option<Arc<dyn BlobHydrationService>>,
    replication: Option<crate::sync_control::ReplicationRuntime>,
    gate_transport_modes: bool,
) -> Option<(BlobSync, Arc<CloudFallbackHydration>)> {
    let walk = ProjectionWalk::open(root, sweeps)?;
    // **Only a process whose rows have been promoted may ask the gate.**
    // Migration 12 seeds every row `local_only`; the desktop's initializer
    // raises them on the control request that carried the evidence, and a
    // process without one would refuse every picture forever. See
    // `WorkerConfig::gate_transport_modes`.
    let modes: Arc<dyn koru_blob_sync::TransportModes> = if gate_transport_modes {
        Arc::new(StoreModes {
            worker: worker.clone(),
        })
    } else {
        Arc::new(koru_blob_sync::AlwaysAdmitted)
    };
    let store: Arc<dyn BlobStorePort> = Arc::new(StoreBlobs {
        worker,
        walk: walk.clone(),
        root: root.to_path_buf(),
    });
    let arrivals = BlobArrivals::new(rooms);
    let access = Arc::new(ControlAccess {
        host,
        local: Mutex::new(None),
        // The desktop's path. A phone has no environment and installs the
        // same string through `BlobSync::set_development_origins`.
        development: Mutex::new(koru_env::value(
            koru_env::name::KORU_CLOUD_BLOB_BYTE_ORIGINS,
        )),
    });
    let policy = Arc::new(HostPolicy::default());
    let control = match HttpControlPlane::new() {
        Ok(control) => Arc::new(control),
        Err(_) => {
            tracing::error!("the blob control plane client did not build");
            return None;
        }
    };
    let objects = match HttpObjectPlane::new() {
        Ok(objects) => Arc::new(objects),
        Err(_) => {
            tracing::error!("the blob object plane client did not build");
            return None;
        }
    };
    let engine = BlobEngine::new(
        BlobPorts {
            store: store.clone(),
            access: access.clone(),
            // **Not the transport-mode gate** — `modes` beside it is. This
            // port carries §10's sixth trigger, the network policy: metered,
            // low-data, offline. A phone installs a real one (B5) and a
            // desktop on Ethernet does not, so it is permissive until a host
            // supplies one. The *ceiling* this port will eventually enforce
            // is **still owed on the desktop**: B10 built the person's pause
            // and the pre-flight summary and did not build a network ceiling,
            // so nothing calls `set_policy` outside `koru-mobile-host`.
            policy: policy.clone(),
            // **§6's fourth point, live** (B8). Safe to ask on a desktop, and
            // only there, because the Profile prompt in the Manage tab is a
            // way out of migration 12's seeded `local_only` that a person can
            // reach (D258). That is D254's order with a person where the
            // evidence used to be: no gate without a promotion path.
            modes,
            // **§8, peer first.** A process with no replication runtime says
            // so out loud rather than by omission.
            peers: match replication {
                Some(runtime) => Arc::new(RuntimePeers { runtime }),
                None => Arc::new(koru_blob_sync::NoPeerBlobs),
            },
            downloads: arrivals.clone(),
        },
        control.clone(),
        objects,
    );
    let hydration = Arc::new(CloudFallbackHydration {
        peers,
        engine: engine.clone(),
        arrivals: arrivals.clone(),
        store,
    });
    Some((
        BlobSync {
            admission: admission::Observer::new(&engine, access.clone(), control),
            engine,
            arrivals,
            access,
            policy,
            documents: Arc::new(Mutex::new(HashMap::new())),
            walk,
        },
        hydration,
    ))
}

#[cfg(test)]
mod tests;
