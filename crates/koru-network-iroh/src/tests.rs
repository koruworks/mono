// SPDX-License-Identifier: MPL-2.0

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use koru_device::{
    DeviceDescriptorV1, DeviceId, DeviceSigningKey, RelationshipId, ReplicationEndpointSecret,
};
use koru_store_core::WorkspaceId;
use koru_store_core::{DocumentId, ProfileId};
use koru_sync_control::{AuthorizationSnapshot, AuthorizedPeer, ControlSessionId};

use super::*;
use crate::hydration::{BLOB_CHUNK_BYTES, MAX_BLOB_HYDRATION_BYTES};

mod profile_units;
mod redial;
mod root_catalog;
use profile_units::profile_identity;

struct EchoEnrollmentHandler {
    seen_remote: Arc<Mutex<Option<ReplicationEndpointId>>>,
}

impl EnrollmentHandler for EchoEnrollmentHandler {
    fn handle<'a>(&'a self, session: &'a mut EnrollmentSession) -> EnrollmentHandlerFuture<'a> {
        Box::pin(async move {
            *self.seen_remote.lock().unwrap() = Some(session.remote_endpoint());
            let frame = session
                .receive_frame()
                .await
                .map_err(|_| EnrollmentHandlerError::refused("invalid_frame"))?;
            session
                .send_frame(&frame)
                .await
                .map_err(|_| EnrollmentHandlerError::refused("send_failed"))
        })
    }
}

struct RefusingEnrollmentHandler;

impl EnrollmentHandler for RefusingEnrollmentHandler {
    fn handle<'a>(&'a self, session: &'a mut EnrollmentSession) -> EnrollmentHandlerFuture<'a> {
        Box::pin(async move {
            let _ = session.receive_frame().await;
            Err(EnrollmentHandlerError::refused("policy_refused"))
        })
    }
}

struct RecordingEnrollmentErrorHandler {
    errors: tokio::sync::mpsc::UnboundedSender<EnrollmentTransportError>,
}

struct CancellationEnrollmentHandler {
    started: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

struct CancellationGuard(Arc<AtomicBool>);

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl EnrollmentHandler for CancellationEnrollmentHandler {
    fn handle<'a>(&'a self, session: &'a mut EnrollmentSession) -> EnrollmentHandlerFuture<'a> {
        Box::pin(async move {
            let _guard = CancellationGuard(Arc::clone(&self.cancelled));
            session
                .receive_frame()
                .await
                .map_err(|_| EnrollmentHandlerError::refused("invalid_frame"))?;
            self.started.store(true, Ordering::SeqCst);
            std::future::pending::<Result<(), EnrollmentHandlerError>>().await
        })
    }
}

impl EnrollmentHandler for RecordingEnrollmentErrorHandler {
    fn handle<'a>(&'a self, session: &'a mut EnrollmentSession) -> EnrollmentHandlerFuture<'a> {
        Box::pin(async move {
            let error = session.receive_frame().await.unwrap_err();
            let _ = self.errors.send(error);
            Err(EnrollmentHandlerError::refused("invalid_frame"))
        })
    }
}

#[test]
fn enrollment_has_a_distinct_alpn_and_bounded_frames() {
    assert_ne!(ENROLLMENT_ALPN_V1, PAIRING_ALPN_V1);
    assert_ne!(ENROLLMENT_ALPN_V1, REPLICATION_ALPN_V1);
    let frame = koru_pairing::EnrollmentFrameV1::Error {
        code: "not_authorized".into(),
    };
    let bytes = encode_enrollment_frame(&frame).unwrap();
    assert_eq!(decode_enrollment_frame(&bytes).unwrap(), frame);
    let oversized_length = u32::try_from(MAX_ENROLLMENT_FRAME_BYTES + 1)
        .unwrap()
        .to_be_bytes();
    assert_eq!(
        decode_enrollment_frame(&oversized_length),
        Err(EnrollmentFrameError::Oversized)
    );
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        decode_enrollment_frame(&trailing),
        Err(EnrollmentFrameError::Malformed)
    );
    assert_eq!(
        encode_enrollment_frame(&koru_pairing::EnrollmentFrameV1::Error {
            code: "x".repeat(MAX_ENROLLMENT_FRAME_BYTES),
        }),
        Err(EnrollmentFrameError::Oversized)
    );
}

#[test]
fn enrollment_runtime_bounds_concurrent_sessions() {
    let hub = EnrollmentHub::new();
    let permits = (0..MAX_CONCURRENT_ENROLLMENT_SESSIONS)
        .map(|_| Arc::clone(&hub.sessions).try_acquire_owned().unwrap())
        .collect::<Vec<_>>();
    assert!(Arc::clone(&hub.sessions).try_acquire_owned().is_err());
    drop(permits);
    assert!(Arc::clone(&hub.sessions).try_acquire_owned().is_ok());
}

#[tokio::test]
async fn enrollment_runtime_round_trips_on_one_endpoint_bound_session_and_closes() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;

    let seen_remote = Arc::new(Mutex::new(None));
    b.set_enrollment_handler(Some(Arc::new(EchoEnrollmentHandler {
        seen_remote: Arc::clone(&seen_remote),
    })));
    let mut session = a
        .open_enrollment_at(b_endpoint.endpoint_id(), b.endpoint_addr().unwrap())
        .await
        .unwrap();
    assert_eq!(session.remote_endpoint(), b_endpoint.endpoint_id());
    let frame = koru_pairing::EnrollmentFrameV1::Error {
        code: "roundtrip".into(),
    };
    session.send_frame(&frame).await.unwrap();
    assert_eq!(session.receive_frame().await.unwrap(), frame);
    assert_eq!(*seen_remote.lock().unwrap(), Some(a_endpoint.endpoint_id()));
    assert_eq!(
        session.receive_frame().await,
        Err(EnrollmentTransportError::StreamFailed)
    );

    let mut capped = a
        .open_enrollment_at(b_endpoint.endpoint_id(), b.endpoint_addr().unwrap())
        .await
        .unwrap();
    capped.sent_frames = MAX_ENROLLMENT_FRAMES_PER_DIRECTION;
    assert_eq!(
        capped.send_frame(&frame).await,
        Err(EnrollmentTransportError::FrameLimit)
    );

    let wrong = ReplicationEndpointSecret::generate().endpoint_id();
    assert!(matches!(
        a.open_enrollment_at(wrong, b.endpoint_addr().unwrap())
            .await,
        Err(EnrollmentTransportError::EndpointMismatch)
    ));
    a.clear();
    b.clear();
}

#[tokio::test]
async fn enrollment_handler_can_refuse_without_falling_through() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    b.set_enrollment_handler(Some(Arc::new(RefusingEnrollmentHandler)));
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;

    let mut session = a
        .open_enrollment_at(b_endpoint.endpoint_id(), b.endpoint_addr().unwrap())
        .await
        .unwrap();
    session
        .send_frame(&koru_pairing::EnrollmentFrameV1::Error {
            code: "request".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        session.receive_frame().await.unwrap(),
        koru_pairing::EnrollmentFrameV1::Error {
            code: "policy_refused".into()
        }
    );
    assert_eq!(
        session.receive_frame().await,
        Err(EnrollmentTransportError::StreamFailed)
    );
    assert!(a.status().peers.is_empty());
    assert!(b.status().peers.is_empty());
    a.clear();
    b.clear();
}

#[tokio::test]
async fn enrollment_session_rejects_oversized_and_malformed_wire_frames() {
    let server_endpoint = ReplicationEndpointSecret::generate();
    let server = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    server.provision_endpoint(server_endpoint.secret_bytes());
    wait_until(|| server.endpoint_addr().is_some()).await;
    let (errors_tx, mut errors_rx) = tokio::sync::mpsc::unbounded_channel();
    server.set_enrollment_handler(Some(Arc::new(RecordingEnrollmentErrorHandler {
        errors: errors_tx,
    })));

    for (wire, expected) in [
        (
            u32::try_from(MAX_ENROLLMENT_FRAME_BYTES + 1)
                .unwrap()
                .to_be_bytes()
                .to_vec(),
            EnrollmentTransportError::OversizedFrame,
        ),
        (
            [1u32.to_be_bytes().as_slice(), b"{"].concat(),
            EnrollmentTransportError::MalformedFrame,
        ),
    ] {
        let client_secret = ReplicationEndpointSecret::generate();
        let client = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&client_secret.secret_bytes()))
            .alpns(vec![ENROLLMENT_ALPN_V1.to_vec()])
            .bind()
            .await
            .unwrap();
        let connection = client
            .connect(server.endpoint_addr().unwrap(), ENROLLMENT_ALPN_V1)
            .await
            .unwrap();
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        send.write_all(&wire).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), errors_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            expected
        );
        client.close().await;
    }
    assert!(server.status().peers.is_empty());
    server.clear();
}

#[tokio::test]
async fn enrollment_runtime_cancels_a_handler_at_the_session_deadline() {
    let server_secret = ReplicationEndpointSecret::generate();
    let server = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&server_secret.secret_bytes()))
        .alpns(vec![ENROLLMENT_ALPN_V1.to_vec()])
        .bind()
        .await
        .unwrap();
    let hub = Arc::new(EnrollmentHub::new());
    let started = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    *hub.handler.write().unwrap() = Some(Arc::new(CancellationEnrollmentHandler {
        started: Arc::clone(&started),
        cancelled: Arc::clone(&cancelled),
    }));
    let accepting = server.clone();
    let serving_hub = Arc::clone(&hub);
    let serving = tokio::spawn(async move {
        let incoming = accepting.accept().await.unwrap();
        let connection = incoming.accept().unwrap().await.unwrap();
        serve_enrollment_connection_with_timeout(
            connection,
            serving_hub,
            Duration::from_millis(50),
        )
        .await;
    });

    let client_secret = ReplicationEndpointSecret::generate();
    let client = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&client_secret.secret_bytes()))
        .bind()
        .await
        .unwrap();
    let connection = client
        .connect(server.addr(), ENROLLMENT_ALPN_V1)
        .await
        .unwrap();
    let (mut send, _recv) = connection.open_bi().await.unwrap();
    send.write_all(
        &encode_enrollment_frame(&koru_pairing::EnrollmentFrameV1::Error {
            code: "begin".into(),
        })
        .unwrap(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .expect("enrollment session was not cancelled")
        .unwrap();
    assert!(started.load(Ordering::SeqCst));
    assert!(cancelled.load(Ordering::SeqCst));
    client.close().await;
    server.close().await;
}

struct InventoryStore {
    workspace: WorkspaceId,
    manifest: DocumentId,
    documents: Mutex<BTreeSet<DocumentId>>,
    blobs: Mutex<HashMap<BlobHash, (koru_store_core::model::Blob, Vec<u8>)>>,
    /// Durable provenance, as the real store keeps it: the origin an
    /// advertisement named, first record wins.
    origins: Mutex<HashMap<BlobHash, String>>,
    blob_reads: AtomicUsize,
    blob_writes: AtomicUsize,
    /// Catalog requests served — one per sync pass a peer ran against us.
    catalog_reads: AtomicUsize,
    fail_history: AtomicBool,
    unit_versions: Mutex<BTreeMap<protocol::ProfileUnitRef, Vec<u8>>>,
    unit_events: Mutex<Vec<&'static str>>,
    workspace_ready: AtomicBool,
    stall_units: AtomicBool,
    /// This device's transport mode, as §6's second point reads it. Armed
    /// rather than asserted: a closed Workspace is a state, not a failure.
    peer_blobs_closed: AtomicBool,
    /// Protocol 6: the root units this device frames (address → version
    /// bytes, as `unit_versions` does for Profile units), its personal v2
    /// units, and the switches a test arms.
    root_units: Mutex<BTreeMap<protocol::CatalogUnitRef, Vec<u8>>>,
    personal_units: Mutex<BTreeMap<WorkspaceId, Vec<u8>>>,
    /// Whether a root placement is known here: provisioning for the same
    /// person's device is pending until a Workspace unit has been applied.
    placement_known: AtomicBool,
    /// The export boundary holds every root unit back.
    root_held: AtomicBool,
    /// This device's mode closes the peer path for the Workspace.
    peer_subject_blocked: AtomicBool,
}

impl InventoryStore {
    fn new(workspace: WorkspaceId, manifest: DocumentId) -> Self {
        Self {
            workspace,
            manifest,
            documents: Mutex::new([manifest].into_iter().collect()),
            blobs: Mutex::new(HashMap::new()),
            origins: Mutex::new(HashMap::new()),
            blob_reads: AtomicUsize::new(0),
            blob_writes: AtomicUsize::new(0),
            catalog_reads: AtomicUsize::new(0),
            fail_history: AtomicBool::new(false),
            unit_versions: Mutex::new(BTreeMap::new()),
            unit_events: Mutex::new(Vec::new()),
            workspace_ready: AtomicBool::new(true),
            stall_units: AtomicBool::new(false),
            peer_blobs_closed: AtomicBool::new(false),
            root_units: Mutex::new(BTreeMap::new()),
            personal_units: Mutex::new(BTreeMap::new()),
            placement_known: AtomicBool::new(true),
            root_held: AtomicBool::new(false),
            peer_subject_blocked: AtomicBool::new(false),
        }
    }

    fn add(&self, document: DocumentId) {
        self.documents.lock().unwrap().insert(document);
    }

    fn has(&self, document: DocumentId) -> bool {
        self.documents.lock().unwrap().contains(&document)
    }

    fn add_blob(&self, bytes: Vec<u8>) -> BlobHash {
        self.add_blob_with_mime(bytes, "image/png")
    }

    fn add_blob_with_mime(&self, bytes: Vec<u8>, mime: &str) -> BlobHash {
        let size = bytes.len() as u64;
        self.add_blob_announcing(bytes, mime, size)
    }

    /// `announced_size` is what the serving peer *says* the blob is, which is
    /// not necessarily what it hands over. The receiver refuses an oversized
    /// blob from its first chunk's metadata rather than after moving the bytes,
    /// so a test of that guard must be able to lie here.
    fn add_blob_announcing(&self, bytes: Vec<u8>, mime: &str, announced_size: u64) -> BlobHash {
        self.add_blob_full(bytes, mime, announced_size, None)
    }

    /// A blob whose record names the device that first stored it, as
    /// `Service::put_blob` writes it.
    fn add_blob_with_origin(&self, bytes: Vec<u8>, origin: &str) -> BlobHash {
        let size = bytes.len() as u64;
        self.add_blob_full(bytes, "image/png", size, Some(origin))
    }

    fn add_blob_full(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        announced_size: u64,
        origin: Option<&str>,
    ) -> BlobHash {
        let hash = BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(&bytes).to_hex()).unwrap();
        let mut attributes = std::collections::BTreeMap::new();
        if let Some(origin) = origin {
            attributes.insert(
                koru_store_core::model::blob_attr::ORIGIN_DEVICE.to_string(),
                koru_store_core::model::BlobAttribute::Text(origin.to_string()),
            );
        }
        let blob = koru_store_core::model::Blob {
            hash: hash.clone(),
            size: announced_size,
            mime: mime.into(),
            filename: None,
            created_at: koru_store_core::Timestamp::from_millis(1),
            attributes,
        };
        self.blobs
            .lock()
            .unwrap()
            .insert(hash.clone(), (blob, bytes));
        hash
    }

    fn advertised_origin(&self, hash: &BlobHash) -> Option<String> {
        self.origins.lock().unwrap().get(hash).cloned()
    }

    fn blob_bytes(&self, hash: &BlobHash) -> Option<Vec<u8>> {
        self.blobs
            .lock()
            .unwrap()
            .get(hash)
            .map(|(_, bytes)| bytes.clone())
    }
}

impl protocol::ReplicationStore for InventoryStore {
    fn admit_peer_blob_transfer(
        &self,
        _workspace: WorkspaceId,
    ) -> Result<(), protocol::StoreFailure> {
        if self.peer_blobs_closed.load(Ordering::Relaxed) {
            return Err(protocol::StoreFailure::new("mode_refuses_transport"));
        }
        Ok(())
    }

    fn profiles(
        &self,
        peer: &protocol::AuthenticatedPeer,
    ) -> Result<Vec<protocol::ProfileReplicaState>, protocol::StoreFailure> {
        peer.with_catalog_scope(|| {
            Ok(self
                .unit_versions
                .lock()
                .unwrap()
                .iter()
                .filter(|(unit, _)| peer.check_authorized_profile_unit(**unit).is_ok())
                .map(|(unit, version)| protocol::ProfileReplicaState {
                    unit: *unit,
                    version: version.clone(),
                    identity: matches!(unit, protocol::ProfileUnitRef::Identity { .. })
                        .then(profile_identity),
                })
                .collect())
        })
    }

    fn provision_profile(
        &self,
        peer: &protocol::AuthenticatedPeer,
        remote: &protocol::ProfileReplicaState,
    ) -> Result<(), protocol::StoreFailure> {
        peer.with_authorized_profile_unit(remote.unit, || {
            assert!(matches!(
                remote.unit,
                protocol::ProfileUnitRef::Identity { .. }
            ));
            remote.validate()
        })
    }

    fn profile_version(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::ProfileUnitRef,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        peer.with_authorized_profile_unit(unit, || {
            self.unit_versions
                .lock()
                .unwrap()
                .get(&unit)
                .cloned()
                .ok_or_else(|| protocol::StoreFailure::new("profile_unit_not_found"))
        })
    }

    fn profile_updates_since(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::ProfileUnitRef,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        self.profile_version(peer, unit)
    }

    fn apply_remote_profile_updates(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::ProfileUnitRef,
        updates: Vec<Vec<u8>>,
    ) -> Result<protocol::DurableAck, protocol::StoreFailure> {
        peer.with_authorized_profile_unit(unit, || {
            let kind = match unit {
                protocol::ProfileUnitRef::Identity { .. } => "identity",
                protocol::ProfileUnitRef::Private { .. } => "private",
                protocol::ProfileUnitRef::LegacyPrivate { .. } => "legacy",
                protocol::ProfileUnitRef::Personal { .. } => {
                    assert!(self.workspace_ready.load(Ordering::SeqCst));
                    "personal"
                }
            };
            self.unit_events.lock().unwrap().push(kind);
            if !self.stall_units.load(Ordering::SeqCst) {
                self.unit_versions
                    .lock()
                    .unwrap()
                    .insert(unit, updates[0].clone());
            }
            Ok(protocol::DurableAck {
                durable_sequence: None,
            })
        })
    }
    fn record_state_change(
        &self,
        event: &StateChangeEvent,
        _: i64,
    ) -> Result<StateChangeEvent, protocol::StoreFailure> {
        if self.fail_history.load(Ordering::SeqCst) {
            Err(protocol::StoreFailure::new("history_failed"))
        } else {
            Ok(event.clone())
        }
    }

    fn catalog(
        &self,
        peer: &protocol::AuthenticatedPeer,
    ) -> Result<Vec<protocol::WorkspaceCatalogState>, protocol::StoreFailure> {
        let profile = ProfileId::from_uuid(self.workspace.as_uuid());
        peer.with_catalog_scope(|| {
            self.catalog_reads.fetch_add(1, Ordering::SeqCst);
            Ok((self.workspace_ready.load(Ordering::SeqCst)
                && (peer.all_data
                    || peer.authorized_profiles.contains(&profile)
                    || peer.authorized_workspaces.contains(&self.workspace)))
            .then_some(protocol::WorkspaceCatalogState {
                workspace_id: self.workspace,
                profile_id: profile,
                // Blobs are in the digest: production digests move whenever a
                // document does, and a blob lands beside the document edit that
                // references it — so an inventory carrying the new advertisement
                // is only requested when this changes. A digest of documents alone
                // would let a pass skip the inventory that teaches the holder
                // directory.
                digest: vec![
                    self.documents.lock().unwrap().len() as u8,
                    self.blobs.lock().unwrap().len() as u8,
                ],
            })
            .into_iter()
            .collect())
        })
    }

    fn provision_replica(
        &self,
        peer: &protocol::AuthenticatedPeer,
        _: &protocol::ReplicaDescriptor,
    ) -> Result<(), protocol::StoreFailure> {
        // The same person's device provisions under a known root placement
        // only; a descriptor's Profile never stands in (§7.2).
        if peer.all_data && !self.placement_known.load(Ordering::SeqCst) {
            return Err(protocol::StoreFailure::new(protocol::PLACEMENT_PENDING));
        }
        self.workspace_ready.store(true, Ordering::SeqCst);
        self.unit_events.lock().unwrap().push("workspace");
        Ok(())
    }

    fn admit_peer_subject(&self, _: WorkspaceId) -> Result<(), protocol::StoreFailure> {
        if self.peer_subject_blocked.load(Ordering::SeqCst) {
            return Err(protocol::StoreFailure::new(
                protocol::MODE_REFUSES_TRANSPORT,
            ));
        }
        Ok(())
    }

    // ---- protocol 6

    fn bind_root_scope(
        &self,
        peer: &protocol::AuthenticatedPeer,
    ) -> Result<bool, protocol::StoreFailure> {
        Ok(peer.binds_root_scope())
    }

    fn publish_pending_root_overlays(&self) -> Result<usize, protocol::StoreFailure> {
        self.unit_events.lock().unwrap().push("publication_job");
        Ok(0)
    }

    /// This device's own reading of the subjects it holds, as the real Store
    /// answers it (`root_exchange::root_sync_admissions`): the same mode row
    /// that makes [`Self::admit_peer_subject`] refuse is what the pass seeds
    /// `PausedBySettings` from, so a double that refuses the one but stays
    /// silent on the other would let the status report the peer's reason for
    /// this device's own setting.
    fn root_sync_admissions(
        &self,
    ) -> Result<Vec<protocol::RootSubjectAdmission>, protocol::StoreFailure> {
        let allows_peer = !self.peer_subject_blocked.load(Ordering::SeqCst);
        Ok(vec![
            protocol::RootSubjectAdmission {
                subject: protocol::CatalogUnitRef::Profile {
                    profile: ProfileId::from_uuid(self.workspace.as_uuid()),
                },
                allows_peer: true,
                ceiling_mismatch: false,
                overlay_pending: false,
            },
            protocol::RootSubjectAdmission {
                subject: protocol::CatalogUnitRef::Workspace {
                    workspace: self.workspace,
                },
                allows_peer,
                ceiling_mismatch: false,
                overlay_pending: false,
            },
        ])
    }

    fn root_catalog(
        &self,
        peer: &protocol::AuthenticatedPeer,
    ) -> Result<protocol::RootCatalogInventory, protocol::StoreFailure> {
        peer.with_catalog_scope(|| {
            let units = self
                .root_units
                .lock()
                .unwrap()
                .iter()
                .filter(|(unit, _)| match unit {
                    protocol::CatalogUnitRef::Profile { profile } => {
                        peer.all_data || peer.authorized_profiles.contains(profile)
                    }
                    protocol::CatalogUnitRef::Workspace { workspace } => {
                        peer.all_data
                            || peer.authorized_workspaces.contains(workspace)
                            || peer
                                .authorized_profiles
                                .contains(&ProfileId::from_uuid(workspace.as_uuid()))
                    }
                })
                .map(|(unit, version)| protocol::RootUnitState {
                    unit: *unit,
                    version: version.clone(),
                })
                .collect();
            let personal = if peer.binds_root_scope() {
                self.personal_units
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(workspace, version)| protocol::PersonalUnitState {
                        workspace: *workspace,
                        version: version.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(protocol::RootCatalogInventory { units, personal })
        })
    }

    fn root_unit_version(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::CatalogUnitRef,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        peer.with_root_scope(|| {
            self.root_units
                .lock()
                .unwrap()
                .get(&unit)
                .cloned()
                .ok_or_else(|| protocol::StoreFailure::new(protocol::ROOT_UNIT_NOT_FOUND))
        })
    }

    fn root_unit_updates_since(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::CatalogUnitRef,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        peer.with_catalog_scope(|| {
            if self.root_held.load(Ordering::SeqCst) {
                return Err(protocol::StoreFailure::new(protocol::ROOT_UNIT_HELD));
            }
            self.root_units
                .lock()
                .unwrap()
                .get(&unit)
                .cloned()
                .ok_or_else(|| protocol::StoreFailure::new(protocol::ROOT_UNIT_NOT_FOUND))
        })
    }

    fn apply_remote_root_updates(
        &self,
        peer: &protocol::AuthenticatedPeer,
        unit: protocol::CatalogUnitRef,
        updates: Vec<Vec<u8>>,
        advertised: Option<&loro::VersionVector>,
    ) -> Result<protocol::RootJoin, protocol::StoreFailure> {
        peer.with_root_scope(|| {
            self.unit_events.lock().unwrap().push(match unit {
                protocol::CatalogUnitRef::Profile { .. } => "root_profile",
                protocol::CatalogUnitRef::Workspace { .. } => {
                    self.placement_known.store(true, Ordering::SeqCst);
                    "root_workspace"
                }
            });
            let frontier = updates[0].clone();
            self.root_units
                .lock()
                .unwrap()
                .insert(unit, frontier.clone());
            let reconciled = advertised.is_none_or(|advertised| {
                loro::VersionVector::decode(&frontier)
                    .is_ok_and(|frontier| frontier.includes_vv(advertised))
            });
            Ok(protocol::RootJoin::Applied {
                frontier,
                reconciled,
            })
        })
    }

    fn personal_version(
        &self,
        peer: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        peer.with_root_scope(|| {
            self.personal_units
                .lock()
                .unwrap()
                .get(&workspace)
                .cloned()
                .ok_or_else(|| protocol::StoreFailure::new(protocol::ROOT_UNIT_NOT_FOUND))
        })
    }

    fn personal_updates_since(
        &self,
        peer: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        self.personal_version(peer, workspace)
    }

    fn apply_remote_personal_updates(
        &self,
        peer: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        updates: Vec<Vec<u8>>,
        _: Option<&loro::VersionVector>,
    ) -> Result<protocol::RootJoin, protocol::StoreFailure> {
        peer.with_root_scope(|| {
            // Personal rows reference the Workspace row; the real Store
            // answers `workspace_not_found` until content bootstrap.
            if !self.workspace_ready.load(Ordering::SeqCst) {
                return Err(protocol::StoreFailure::new("workspace_not_found"));
            }
            self.unit_events.lock().unwrap().push("personal_v2");
            let frontier = updates[0].clone();
            self.personal_units
                .lock()
                .unwrap()
                .insert(workspace, frontier.clone());
            Ok(protocol::RootJoin::Applied {
                frontier,
                reconciled: true,
            })
        })
    }

    fn provision_document(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        document: DocumentId,
    ) -> Result<(), protocol::StoreFailure> {
        self.add(document);
        Ok(())
    }

    fn inventory(
        &self,
        peer: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<protocol::WorkspaceInventory, protocol::StoreFailure> {
        let profile = self.workspace_profile(workspace)?;
        peer.with_authorized_workspace_profile(workspace, profile, || {
            let mut replica = protocol::ReplicaDescriptor {
                profile_id: profile,
                profile_name: "PRIVATE PROFILE NAME".into(),
                profile_locale: "PRIVATE LOCALE".into(),
                profile_created_at_ms: 1,
                profile_icon: Some("PRIVATE ICON".into()),
                profile_color: Some("PRIVATE COLOR".into()),
                profile_kind: "PRIVATE KIND".into(),
                workspace_id: workspace,
                workspace_name: "Workspace".into(),
                workspace_created_at_ms: 1,
                workspace_icon: None,
                workspace_color: None,
                workspace_pinned: false,
                workspace_archived: false,
                workspace_deleted_at_ms: None,
                manifest_document: self.manifest,
            };
            if !peer.all_data {
                replica.redact_profile_metadata();
            }
            Ok(protocol::WorkspaceInventory {
                workspace_id: workspace,
                replica,
                documents: self
                    .documents
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|document| protocol::DocumentReplicaState {
                        document_id: *document,
                        room_id: if *document == self.manifest {
                            format!("workspace:{workspace}:manifest")
                        } else {
                            format!("document:{document}")
                        },
                        version: loro::VersionVector::new().encode(),
                        is_manifest: *document == self.manifest,
                    })
                    .collect(),
                blobs: self
                    .blobs
                    .lock()
                    .unwrap()
                    .values()
                    .map(|(blob, _)| blob.clone())
                    .collect(),
            })
        })
    }

    fn workspace_profile(
        &self,
        workspace: WorkspaceId,
    ) -> Result<ProfileId, protocol::StoreFailure> {
        (workspace == self.workspace && self.workspace_ready.load(Ordering::SeqCst))
            .then(|| ProfileId::from_uuid(self.workspace.as_uuid()))
            .ok_or_else(|| protocol::StoreFailure::new("workspace_not_found"))
    }

    fn locate(
        &self,
        _: &protocol::AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, protocol::StoreFailure> {
        Ok(self.has(document).then_some(self.workspace))
    }

    fn blob_origin(
        &self,
        _: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Option<String> {
        if workspace != self.workspace {
            return None;
        }
        // The held blob's own attributes first, then the advertisement table —
        // the same order as `Service::blob_origin`.
        if let Some((blob, _)) = self.blobs.lock().unwrap().get(hash)
            && let Some(origin) =
                blob.text_attribute(koru_store_core::model::blob_attr::ORIGIN_DEVICE)
        {
            return Some(origin.to_string());
        }
        self.origins.lock().unwrap().get(hash).cloned()
    }

    fn record_blob_origins(
        &self,
        _: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        blobs: &[koru_store_core::model::Blob],
    ) -> Result<usize, protocol::StoreFailure> {
        if workspace != self.workspace {
            return Err(protocol::StoreFailure::new("workspace_not_found"));
        }
        let mut recorded = 0;
        let mut origins = self.origins.lock().unwrap();
        for blob in blobs {
            let Some(origin) =
                blob.text_attribute(koru_store_core::model::blob_attr::ORIGIN_DEVICE)
            else {
                continue;
            };
            // A held blob's own attributes are the record; the table exists
            // for hashes with no local row. First record wins either way.
            if self.blobs.lock().unwrap().contains_key(&blob.hash) {
                continue;
            }
            if origins
                .insert(blob.hash.clone(), origin.to_string())
                .is_none()
            {
                recorded += 1;
            }
        }
        Ok(recorded)
    }

    fn manifest_id(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
    ) -> Result<DocumentId, protocol::StoreFailure> {
        Ok(self.manifest)
    }

    fn version(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        Ok(loro::VersionVector::new().encode())
    }

    fn updates_since(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        Ok(Vec::new())
    }

    fn read_blob_chunk(
        &self,
        _: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        offset: u64,
        max_bytes: u32,
    ) -> Result<protocol::BlobChunk, protocol::StoreFailure> {
        if workspace != self.workspace {
            return Err(protocol::StoreFailure::new("workspace_not_found"));
        }
        self.blob_reads.fetch_add(1, Ordering::Relaxed);
        let blobs = self.blobs.lock().unwrap();
        let (blob, bytes) = blobs
            .get(hash)
            .ok_or_else(|| protocol::StoreFailure::new("blob_not_found"))?;
        let start =
            usize::try_from(offset).map_err(|_| protocol::StoreFailure::new("invalid_offset"))?;
        if start > bytes.len() {
            return Err(protocol::StoreFailure::new("invalid_offset"));
        }
        let end = start.saturating_add(max_bytes as usize).min(bytes.len());
        Ok(protocol::BlobChunk {
            workspace_id: workspace,
            blob: blob.clone(),
            offset,
            bytes: bytes[start..end].to_vec(),
            complete: end == bytes.len(),
        })
    }

    fn store_remote_blob(
        &self,
        _: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
        blob: &koru_store_core::model::Blob,
        bytes: Vec<u8>,
    ) -> Result<(), protocol::StoreFailure> {
        if workspace != self.workspace {
            return Err(protocol::StoreFailure::new("workspace_not_found"));
        }
        let actual = BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(&bytes).to_hex())
            .map_err(|_| protocol::StoreFailure::new("blob_checksum_mismatch"))?;
        if actual != blob.hash || bytes.len() as u64 != blob.size {
            return Err(protocol::StoreFailure::new("blob_checksum_mismatch"));
        }
        self.blob_writes.fetch_add(1, Ordering::Relaxed);
        self.blobs
            .lock()
            .unwrap()
            .insert(blob.hash.clone(), (blob.clone(), bytes));
        Ok(())
    }

    fn apply_remote_updates(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
        _: Vec<Vec<u8>>,
    ) -> Result<protocol::DurableAck, protocol::StoreFailure> {
        Ok(protocol::DurableAck {
            durable_sequence: None,
        })
    }
}

struct ManifestStore {
    workspace: WorkspaceId,
    manifest_id: DocumentId,
    profile: ProfileId,
    manifest: Mutex<koru_store_core::Manifest>,
    documents: Vec<DocumentId>,
}

impl ManifestStore {
    fn pair(workspace: WorkspaceId, manifest_id: DocumentId) -> (Self, Self) {
        Self::pair_with_documents(workspace, manifest_id, 0)
    }

    fn pair_with_documents(
        workspace: WorkspaceId,
        manifest_id: DocumentId,
        document_count: usize,
    ) -> (Self, Self) {
        let first = koru_store_core::Manifest::create(
            workspace,
            "Original",
            koru_store_core::Timestamp::from_millis(1),
        )
        .unwrap();
        first.set_peer_id(1).unwrap();
        let snapshot = first.export_snapshot().unwrap();
        let second = koru_store_core::Manifest::from_snapshot(&snapshot).unwrap();
        second.set_peer_id(2).unwrap();
        let profile = ProfileId::new();
        let documents: Vec<_> = (0..document_count).map(|_| DocumentId::new()).collect();
        (
            Self {
                workspace,
                manifest_id,
                profile,
                manifest: Mutex::new(first),
                documents: documents.clone(),
            },
            Self {
                workspace,
                manifest_id,
                profile,
                manifest: Mutex::new(second),
                documents,
            },
        )
    }

    fn rename(&self, name: &str) {
        self.manifest.lock().unwrap().set_name(name).unwrap();
    }

    fn name(&self) -> Option<String> {
        self.manifest.lock().unwrap().name()
    }
}

impl protocol::ReplicationStore for ManifestStore {
    fn catalog(
        &self,
        _: &protocol::AuthenticatedPeer,
    ) -> Result<Vec<protocol::WorkspaceCatalogState>, protocol::StoreFailure> {
        Ok(vec![protocol::WorkspaceCatalogState {
            workspace_id: self.workspace,
            profile_id: self.profile,
            digest: self.manifest.lock().unwrap().version().encode(),
        }])
    }

    fn provision_replica(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: &protocol::ReplicaDescriptor,
    ) -> Result<(), protocol::StoreFailure> {
        Ok(())
    }

    fn provision_document(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<(), protocol::StoreFailure> {
        Ok(())
    }

    fn inventory(
        &self,
        _: &protocol::AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<protocol::WorkspaceInventory, protocol::StoreFailure> {
        let manifest = self.manifest.lock().unwrap();
        let mut documents = vec![protocol::DocumentReplicaState {
            document_id: self.manifest_id,
            room_id: format!("workspace:{workspace}:manifest"),
            version: manifest.version().encode(),
            is_manifest: true,
        }];
        documents.extend(
            self.documents
                .iter()
                .map(|document| protocol::DocumentReplicaState {
                    document_id: *document,
                    room_id: format!("document:{document}"),
                    version: loro::VersionVector::new().encode(),
                    is_manifest: false,
                }),
        );
        Ok(protocol::WorkspaceInventory {
            workspace_id: workspace,
            replica: protocol::ReplicaDescriptor {
                profile_id: self.profile,
                profile_name: "Profile".into(),
                profile_locale: "en".into(),
                profile_created_at_ms: 1,
                profile_icon: None,
                profile_color: None,
                profile_kind: String::new(),
                workspace_id: workspace,
                workspace_name: manifest.name().unwrap_or_default(),
                workspace_created_at_ms: 1,
                workspace_icon: None,
                workspace_color: None,
                workspace_pinned: false,
                workspace_archived: false,
                workspace_deleted_at_ms: None,
                manifest_document: self.manifest_id,
            },
            documents,
            blobs: Vec::new(),
        })
    }

    fn workspace_profile(&self, _: WorkspaceId) -> Result<ProfileId, protocol::StoreFailure> {
        Ok(self.profile)
    }

    fn locate(
        &self,
        _: &protocol::AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, protocol::StoreFailure> {
        Ok(
            (document == self.manifest_id || self.documents.contains(&document))
                .then_some(self.workspace),
        )
    }

    fn manifest_id(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
    ) -> Result<DocumentId, protocol::StoreFailure> {
        Ok(self.manifest_id)
    }

    fn version(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        if document == self.manifest_id {
            Ok(self.manifest.lock().unwrap().version().encode())
        } else {
            Ok(loro::VersionVector::new().encode())
        }
    }

    fn updates_since(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        document: DocumentId,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        if document != self.manifest_id {
            return Ok(Vec::new());
        }
        self.manifest
            .lock()
            .unwrap()
            .export_updates_since(version)
            .map_err(|_| protocol::StoreFailure::new("export"))
    }

    fn snapshot(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, protocol::StoreFailure> {
        if document != self.manifest_id {
            return Ok(Vec::new());
        }
        self.manifest
            .lock()
            .unwrap()
            .export_snapshot()
            .map_err(|_| protocol::StoreFailure::new("export"))
    }

    fn read_blob_chunk(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: &koru_store_core::model::BlobHash,
        _: u64,
        _: u32,
    ) -> Result<protocol::BlobChunk, protocol::StoreFailure> {
        Err(protocol::StoreFailure::new("blob_not_found"))
    }

    fn store_remote_blob(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        _: &koru_store_core::model::Blob,
        _: Vec<u8>,
    ) -> Result<(), protocol::StoreFailure> {
        Ok(())
    }

    fn apply_remote_updates(
        &self,
        _: &protocol::AuthenticatedPeer,
        _: WorkspaceId,
        document: DocumentId,
        updates: Vec<Vec<u8>>,
    ) -> Result<protocol::DurableAck, protocol::StoreFailure> {
        if document == self.manifest_id {
            self.manifest
                .lock()
                .unwrap()
                .import_frames(&updates)
                .map_err(|_| protocol::StoreFailure::new("import"))?;
        }
        Ok(protocol::DurableAck {
            durable_sequence: Some(1),
        })
    }
}

#[test]
fn connection_transitions_record_last_seen_without_claiming_trust_is_online() {
    let status = RwLock::new(NetworkStatus::default());
    let device = DeviceId::new();
    set_peer(
        &status,
        device,
        PeerConnectionState::Connected {
            path: PathKind::Relay,
        },
    );
    let connected_at = status.read().unwrap().last_seen_millis[&device];
    set_peer(&status, device, PeerConnectionState::BackingOff);
    let status = status.read().unwrap();
    assert_eq!(status.peers[&device], PeerConnectionState::BackingOff);
    assert!(status.last_seen_millis[&device] >= connected_at);
}

#[test]
fn change_status_is_outbound_while_reconciliation_status_is_inbound() {
    assert_eq!(
        sync_event_direction("change_debounce_reset"),
        "local_to_peer"
    );
    assert_eq!(sync_event_direction("change_acknowledged"), "local_to_peer");
    assert_eq!(sync_event_direction("catalog_received"), "peer_to_local");
}

/// **A reply that was never sent must not have consumed its payload.**
///
/// `take_pairing_events` drains the queue when the response is built, and the
/// frame is written afterwards — so a write that fails does not delay the
/// delivery, it destroys it. That is what turned a gossip batch too large for
/// a control frame into records nobody ever applied, rather than a retry.
/// Order matters as much as survival: a pairing event only means anything in
/// sequence, so restored events go back ahead of whatever was queued while the
/// reply was in flight.
#[tokio::test]
async fn events_taken_for_a_reply_that_failed_go_back_in_order() {
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    for code in ["first", "second", "third"] {
        runtime.pairing.event(PairingEvent::Failed {
            code: code.to_owned(),
        });
    }

    let taken = runtime.take_pairing_events();
    assert_eq!(taken.len(), 3);
    assert!(
        runtime.take_pairing_events().is_empty(),
        "taking is a drain, which is the whole reason the restore has to exist"
    );

    // What arrived while the reply was in flight, and must stay behind them.
    runtime.pairing.event(PairingEvent::Failed {
        code: "fourth".to_owned(),
    });
    runtime.restore_pairing_events(taken);

    let codes = runtime
        .take_pairing_events()
        .into_iter()
        .map(|event| match event {
            PairingEvent::Failed { code } => code,
            other => panic!("unexpected event: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(codes, ["first", "second", "third", "fourth"]);
    runtime.clear();
}

/// Restoring nothing is not the same as restoring an empty batch: every reply
/// that is not `PairingEvents` takes this path on a failed write.
#[tokio::test]
async fn restoring_no_events_leaves_the_queue_alone() {
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    runtime.pairing.event(PairingEvent::Failed {
        code: "kept".to_owned(),
    });
    runtime.restore_pairing_events(Vec::new());
    assert_eq!(runtime.take_pairing_events().len(), 1);
    runtime.clear();
}

#[tokio::test]
async fn provisioned_secret_is_the_endpoint_identity() {
    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    assert_eq!(runtime.status().endpoint_id, Some(secret.endpoint_id()));
    runtime.clear();
}

#[tokio::test]
async fn a_durable_change_hint_discovers_a_document_created_after_connect() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, manifest));
    let b_store = Arc::new(InventoryStore::new(workspace, manifest));
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    let document = DocumentId::new();
    a_store.add(document);
    a.notify_local_change();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b_store.has(document) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the connected peer did not rescan inventory");
    let received = b.status();
    assert!(received.state_events.iter().any(|event| {
        event.peer == a_id && event.kind == "document_created" && event.document == Some(document)
    }));
    assert_eq!(
        received
            .state_events
            .iter()
            .filter(|event| event.peer == a_id && event.kind == "catalog_reconciled")
            .count(),
        1,
        "the pass that discovered content records one catalog change"
    );
    let catalog = received
        .state_events
        .iter()
        .find(|event| event.peer == a_id && event.kind == "catalog_reconciled")
        .unwrap();
    assert!(
        catalog
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("document change")),
        "the catalog row says which category changed"
    );

    // A differing catalog is not itself a remote change. Make B locally ahead,
    // then hint it into pulling A again: the comparison has work to do, but it
    // imports nothing and must not add another catalog-history row.
    b_store.add(DocumentId::new());
    let before_sync = received
        .last_sync_event
        .as_ref()
        .map_or(0, |event| event.sequence);
    a.notify_local_change();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b
            .status()
            .last_sync_event
            .as_ref()
            .is_some_and(|event| event.sequence > before_sync && event.stage == "sync_complete")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the no-op catalog comparison did not complete");
    assert_eq!(
        b.status()
            .state_events
            .iter()
            .filter(|event| event.peer == a_id && event.kind == "catalog_reconciled")
            .count(),
        1,
        "a catalog comparison that imported nothing stays out of history"
    );
    a.clear();
    b.clear();
}

#[tokio::test]
async fn profile_scope_discovers_workspace_content_without_an_exact_workspace_grant() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let profile = ProfileId::from_uuid(workspace.as_uuid());
    let manifest = DocumentId::new();
    let future_document = DocumentId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, manifest));
    let b_store = Arc::new(InventoryStore::new(workspace, manifest));
    a_store.add(future_document);
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store);
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;

    let authorization = |local_device, peer_device, endpoint_id| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: peer_device,
            endpoint_id,
            all_data: false,
            authorized_profiles: vec![profile],
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    a.replace_authorization(authorization(a_id, b_id, b_endpoint.endpoint_id()));
    b.replace_authorization(authorization(b_id, a_id, a_endpoint.endpoint_id()));
    b.connect_peer_at(a_id, a.endpoint_addr().unwrap());

    tokio::time::timeout(Duration::from_secs(5), async {
        while !b_store.has(future_document) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Profile scope did not discover the workspace catalog row");
    assert!(
        b.status()
            .state_events
            .iter()
            .any(|event| event.document == Some(future_document))
    );

    a.clear();
    b.clear();
}

struct SessionIngressSlot {
    runtime: RwLock<Option<SessionRuntimeHandle>>,
}

#[derive(Default)]
struct ConnectionRecorder {
    active: Mutex<BTreeMap<u64, koru_network_edge::ConnectionMetadata>>,
}

impl ConnectionRecorder {
    fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
    }
}

impl koru_network_edge::NetworkIngress for ConnectionRecorder {
    fn admit(
        &self,
        _session: koru_network_edge::TransportSession,
    ) -> Result<(), koru_network_edge::EdgeError> {
        Ok(())
    }

    fn connected(
        &self,
        connection: koru_network_edge::ConnectionMetadata,
    ) -> Result<(), koru_network_edge::EdgeError> {
        self.active
            .lock()
            .unwrap()
            .insert(connection.connection_id, connection);
        Ok(())
    }

    fn disconnected(&self, connection_id: u64) {
        self.active.lock().unwrap().remove(&connection_id);
    }
}

impl SessionIngressSlot {
    fn new() -> Self {
        Self {
            runtime: RwLock::new(None),
        }
    }

    fn install(&self, runtime: SessionRuntimeHandle) {
        *self.runtime.write().unwrap() = Some(runtime);
    }

    fn runtime(&self) -> Result<SessionRuntimeHandle, koru_network_edge::EdgeError> {
        self.runtime
            .read()
            .unwrap()
            .clone()
            .ok_or(koru_network_edge::EdgeError::Closed)
    }
}

impl koru_network_edge::NetworkIngress for SessionIngressSlot {
    fn admit(
        &self,
        session: koru_network_edge::TransportSession,
    ) -> Result<(), koru_network_edge::EdgeError> {
        self.runtime()?.accept(session);
        Ok(())
    }

    fn connected(
        &self,
        connection: koru_network_edge::ConnectionMetadata,
    ) -> Result<(), koru_network_edge::EdgeError> {
        self.runtime()?
            .connection_event(koru_network_edge::ConnectionEvent::Connected(connection));
        Ok(())
    }

    fn disconnected(&self, connection_id: u64) {
        if let Ok(runtime) = self.runtime() {
            runtime.connection_event(koru_network_edge::ConnectionEvent::Disconnected {
                connection_id,
            });
        }
    }
}

#[derive(Clone)]
struct EdgeEgress(RuntimeHandle);

impl koru_network_edge::NetworkEgress for EdgeEgress {
    fn open(
        &self,
        target: koru_network_edge::NetworkTarget,
        protocol: koru_network_edge::ProtocolToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        koru_network_edge::TransportSession,
                        koru_network_edge::EdgeError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(self.0.open_product_session(target, protocol))
    }
}

#[derive(Clone, Default)]
struct PendingEgress {
    opens: Arc<AtomicUsize>,
}

impl PendingEgress {
    fn open_count(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

impl koru_network_edge::NetworkEgress for PendingEgress {
    fn open(
        &self,
        _target: koru_network_edge::NetworkTarget,
        _protocol: koru_network_edge::ProtocolToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        koru_network_edge::TransportSession,
                        koru_network_edge::EdgeError,
                    >,
                > + Send
                + '_,
        >,
    > {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

#[derive(Clone, Default)]
struct PendingTransport;

impl RequestTransport for PendingTransport {
    fn stable_id(&self) -> usize {
        73
    }

    fn request<'a>(
        &'a self,
        _frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(std::future::pending())
    }

    // A double for a connection on a LAN. None of these carries a blob frame,
    // so D246's gate is answered honestly rather than being left to a default
    // that a real transport could then inherit by forgetting.
    fn blob_path_admitted(&self) -> bool {
        true
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

#[derive(Clone)]
struct CatalogThenPendingInventory {
    workspace: WorkspaceId,
}

#[derive(Clone)]
struct CatalogWithPrivateProfileMetadata {
    workspace: WorkspaceId,
    profile: ProfileId,
}

impl RequestTransport for CatalogWithPrivateProfileMetadata {
    fn stable_id(&self) -> usize {
        75
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match frame {
                protocol::PeerFrame::CatalogRequest => Ok(protocol::PeerFrame::Catalog {
                    digest: vec![99],
                    profiles: vec![protocol::ProfileReplicaState {
                        unit: protocol::ProfileUnitRef::Private {
                            profile: self.profile,
                        },
                        version: loro::VersionVector::new().encode(),
                        identity: Some(protocol::ProfileIdentityState {
                            name: "PRIVATE PROFILE NAME".into(),
                            purpose: None,
                            avatar: None,
                            icon: None,
                            color: None,
                            kind: "PRIVATE KIND".into(),
                            account: Some("PRIVATE ACCOUNT".into()),
                            locale: "PRIVATE LOCALE".into(),
                            created_at_ms: 1,
                        }),
                    }],
                    workspaces: vec![protocol::WorkspaceCatalogState {
                        workspace_id: self.workspace,
                        profile_id: self.profile,
                        digest: vec![99],
                    }],
                }),
                other => panic!("metadata-bearing catalog reached {other:?}"),
            }
        })
    }

    // A double for a connection on a LAN. None of these carries a blob frame,
    // so D246's gate is answered honestly rather than being left to a default
    // that a real transport could then inherit by forgetting.
    fn blob_path_admitted(&self) -> bool {
        true
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

impl RequestTransport for CatalogThenPendingInventory {
    fn stable_id(&self) -> usize {
        74
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match frame {
                protocol::PeerFrame::CatalogRequest => Ok(protocol::PeerFrame::Catalog {
                    digest: vec![99],
                    profiles: Vec::new(),
                    workspaces: vec![protocol::WorkspaceCatalogState {
                        workspace_id: self.workspace,
                        profile_id: ProfileId::from_uuid(self.workspace.as_uuid()),
                        digest: vec![99],
                    }],
                }),
                protocol::PeerFrame::InventoryRequest { .. } => std::future::pending().await,
                // An all-data peer is asked for its root catalog before any
                // inventory (§7.2). This double holds no root units, which
                // makes the root exchange a no-op whatever the digests say,
                // so the pass reaches the inventory this test is about.
                protocol::PeerFrame::RootCatalogRequest => Ok(protocol::PeerFrame::RootCatalog {
                    digest: vec![99],
                    units: Vec::new(),
                    personal: Vec::new(),
                }),
                other => panic!("unexpected scripted request: {other:?}"),
            }
        })
    }

    // A double for a connection on a LAN. None of these carries a blob frame,
    // so D246's gate is answered honestly rather than being left to a default
    // that a real transport could then inherit by forgetting.
    fn blob_path_admitted(&self) -> bool {
        true
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

/// A peer that has the workspace in its catalog but has removed the Profile
/// that owns it — the shape of somebody deleting a Profile on one machine.
#[derive(Clone)]
struct CatalogThenProfileGone {
    workspace: WorkspaceId,
}

impl RequestTransport for CatalogThenProfileGone {
    fn stable_id(&self) -> usize {
        76
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match frame {
                protocol::PeerFrame::CatalogRequest => Ok(protocol::PeerFrame::Catalog {
                    digest: vec![99],
                    profiles: Vec::new(),
                    workspaces: vec![protocol::WorkspaceCatalogState {
                        workspace_id: self.workspace,
                        profile_id: ProfileId::from_uuid(self.workspace.as_uuid()),
                        digest: vec![99],
                    }],
                }),
                protocol::PeerFrame::InventoryRequest { .. } => Ok(protocol::PeerFrame::Error {
                    code: "profile_not_found".into(),
                }),
                // As above: no root units, so the root exchange neither
                // reschedules the pass nor contributes an error code, and
                // `profile_not_found` is still the one the test reads.
                protocol::PeerFrame::RootCatalogRequest => Ok(protocol::PeerFrame::RootCatalog {
                    digest: vec![99],
                    units: Vec::new(),
                    personal: Vec::new(),
                }),
                other => panic!("unexpected scripted request: {other:?}"),
            }
        })
    }

    // A double for a connection on a LAN. None of these carries a blob frame,
    // so D246's gate is answered honestly rather than being left to a default
    // that a real transport could then inherit by forgetting.
    fn blob_path_admitted(&self) -> bool {
        true
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

fn authenticated_test_peer(peer: &AuthorizedPeer) -> protocol::AuthenticatedPeer {
    protocol::AuthenticatedPeer {
        relationship_id: peer.relationship_id,
        device_id: peer.device_id,
        endpoint_id: peer.endpoint_id,
        all_data: peer.all_data,
        authorized_profiles: peer.authorized_profiles.iter().copied().collect(),
        authorized_workspaces: peer.workspaces.iter().copied().collect(),
        authorization_fence: protocol::AuthorizationFence::active(),
        display_name: peer.display_name.clone(),
        incompatible: false,
        peer_protocol: Some(protocol::PEER_PROTOCOL_VERSION),
    }
}

#[tokio::test]
async fn simultaneous_crossed_dials_settle_on_one_connection() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a_ingress = Arc::new(ConnectionRecorder::default());
    let b_ingress = Arc::new(ConnectionRecorder::default());
    let a =
        RuntimeHandle::spawn_with_opaque_ingress(IrohNetworkConfig::isolated(), a_ingress.clone());
    let b =
        RuntimeHandle::spawn_with_opaque_ingress(IrohNetworkConfig::isolated(), b_ingress.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    let snapshot = |local_device, device_id, endpoint_id| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id,
            endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    a.replace_authorization(snapshot(a_id, b_id, b_endpoint.endpoint_id()));
    b.replace_authorization(snapshot(b_id, a_id, a_endpoint.endpoint_id()));

    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    b.connect_peer_at(a_id, a.endpoint_addr().unwrap());
    wait_until(|| a_ingress.active_count() == 1 && b_ingress.active_count() == 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(a_ingress.active_count(), 1);
    assert_eq!(b_ingress.active_count(), 1);
    a.clear();
    b.clear();
}

#[tokio::test]
async fn stale_edge_disconnect_does_not_hide_a_replacement_connection() {
    let local = DeviceId::new();
    let peer_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let endpoint = ReplicationEndpointSecret::generate();
    let peer = AuthorizedPeer {
        relationship_id: relationship,
        device_id: peer_id,
        endpoint_id: endpoint.endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let edge = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let runtime = SessionRuntimeHandle::new(store, Arc::new(EdgeEgress(edge.clone())));
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![peer.clone()],
        local_build: None,
    });
    let metadata = |connection_id| koru_network_edge::ConnectionMetadata {
        connection_id,
        principal: device_principal(&peer, 1),
        path: koru_network_edge::PathMetadata {
            kind: PathKind::Direct,
            outbound: true,
            selected_is_ip: false,
        },
    };
    runtime.connection_event(koru_network_edge::ConnectionEvent::Connected(metadata(10)));
    runtime.connection_event(koru_network_edge::ConnectionEvent::Connected(metadata(11)));
    assert_eq!(
        runtime
            .sync_coordinator
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|state| state.connection_id),
        Some(11)
    );

    runtime
        .connection_event(koru_network_edge::ConnectionEvent::Disconnected { connection_id: 10 });
    assert!(matches!(
        runtime.status().peers.get(&peer_id),
        Some(PeerConnectionState::Connected { .. })
    ));
    assert_eq!(
        runtime
            .sync_coordinator
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|state| state.connection_id),
        Some(11),
        "the stale disconnect must not evict the replacement sync generation"
    );

    runtime
        .connection_event(koru_network_edge::ConnectionEvent::Disconnected { connection_id: 11 });
    assert_eq!(
        runtime.status().peers.get(&peer_id),
        Some(&PeerConnectionState::Disconnected)
    );
    edge.clear();
}

#[tokio::test(start_paused = true)]
async fn a_hung_catalog_request_times_out_instead_of_wedging_the_pass() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Hung catalog".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let peer = authenticated_test_peer(&authorized);
    let store: Arc<dyn protocol::ReplicationStore> =
        Arc::new(InventoryStore::new(workspace, manifest));
    let status = Arc::new(RwLock::new(NetworkStatus::default()));
    let result = pull_missing(
        PendingTransport,
        peer,
        store,
        Arc::clone(&status),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        1,
    )
    .await;

    assert!(result.retry_all);
    assert_eq!(result.retry_error_code.as_deref(), Some("timeout"));
    assert!(matches!(
        status.read().unwrap().last_sync_event.as_ref(),
        Some(event) if event.stage == "catalog_failed" && event.error_code.as_deref() == Some("timeout")
    ));
}

#[tokio::test]
async fn scoped_initiator_rejects_profile_metadata_before_digest_or_inventory() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::from_uuid(workspace.as_uuid());
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: false,
        authorized_profiles: vec![profile],
        workspaces: Vec::new(),
        via: None,
        display_name: Some("Malicious catalog".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let store: Arc<dyn protocol::ReplicationStore> =
        Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let result = pull_missing(
        CatalogWithPrivateProfileMetadata { workspace, profile },
        authenticated_test_peer(&authorized),
        store,
        Arc::new(RwLock::new(NetworkStatus::default())),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        1,
    )
    .await;

    assert!(result.retry_all);
    assert_eq!(
        result.retry_error_code.as_deref(),
        Some("unauthorized_catalog_row")
    );
}

#[tokio::test(start_paused = true)]
async fn a_hung_inventory_request_times_out() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Hung inventory".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let peer = authenticated_test_peer(&authorized);
    let store: Arc<dyn protocol::ReplicationStore> =
        Arc::new(InventoryStore::new(workspace, manifest));
    let status = Arc::new(RwLock::new(NetworkStatus::default()));
    let result = pull_missing(
        CatalogThenPendingInventory { workspace },
        peer,
        store,
        Arc::clone(&status),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        2,
    )
    .await;

    assert!(result.failed_workspaces.contains(&workspace));
    assert_eq!(result.retry_error_code.as_deref(), Some("timeout"));
    assert!(matches!(
        status.read().unwrap().last_sync_event.as_ref(),
        Some(event) if event.stage == "sync_incomplete" && event.error_code.as_deref() == Some("timeout")
    ));
}

/// **A Profile the peer has removed is skipped, not retried.**
///
/// The answer is settled on the other machine: asking again every thirty
/// seconds reaches the same refusal, and while the workspace stays in
/// `failed_workspaces` the pass never completes, so every healthy workspace
/// shared with that peer is dragged into the same loop. The error is still
/// remembered, so the condition is visible rather than silently dropped.
#[tokio::test(start_paused = true)]
async fn a_profile_the_peer_removed_is_skipped_rather_than_retried_for_ever() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Profile deleted there".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let peer = authenticated_test_peer(&authorized);
    let store: Arc<dyn protocol::ReplicationStore> =
        Arc::new(InventoryStore::new(workspace, manifest));
    let status = Arc::new(RwLock::new(NetworkStatus::default()));
    let result = pull_missing(
        CatalogThenProfileGone { workspace },
        peer,
        store,
        Arc::clone(&status),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        2,
    )
    .await;

    assert!(
        !result.failed_workspaces.contains(&workspace),
        "the workspace was queued for a retry that cannot succeed"
    );
    assert!(
        !result.retry_all,
        "one gone Profile rescheduled the catalog"
    );
    assert_eq!(
        result.retry_error_code.as_deref(),
        Some("profile_not_found"),
        "the condition stopped being visible"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hung_blob_chunk_times_out_and_stays_retryable() {
    let workspace = WorkspaceId::new();
    let bytes = b"never arrives";
    let hash =
        BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(bytes).to_hex()).expect("blob hash");
    let key = crate::hydration::BlobHydrationKey { workspace, hash };
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Hung blob".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let peer = authenticated_test_peer(&authorized);
    let store = InventoryStore::new(workspace, DocumentId::new());
    let (state, _) = watch::channel(BlobHydrationState::CheckingLocal);
    let error =
        crate::hydration::hydrate_blob_from_peer(&key, &peer, &PendingTransport, &store, &state)
            .await
            .expect_err("hung blob request must time out");

    assert_eq!(error.code, "timeout");
    assert!(error.retryable);
}

#[tokio::test]
async fn a_sync_request_arriving_before_its_connected_event_still_starts_a_pass() {
    let local = DeviceId::new();
    let workspace = WorkspaceId::new();
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Early session".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let principal = device_principal(&authorized, 1);
    let store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let egress = Arc::new(PendingEgress::default());
    let runtime = SessionRuntimeHandle::new(store, egress.clone());
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![authorized],
        local_build: None,
    });
    let (mut peer_session, mut accepted) = koru_network_edge::TransportSession::pair(
        principal.clone(),
        principal,
        replication_protocol(),
        4,
    );
    accepted.connection_id = 91;
    runtime.accept(accepted);
    send_edge_frame(&peer_session.send, &protocol::PeerFrame::SyncRequest)
        .await
        .expect("send sync request");
    assert_eq!(
        read_edge_frame(&mut peer_session.receive)
            .await
            .expect("sync ack"),
        protocol::PeerFrame::SyncAck
    );
    wait_until(|| egress.open_count() > 0).await;
}

#[tokio::test(start_paused = true)]
async fn a_hung_sync_announcement_records_failure() {
    let local = DeviceId::new();
    let workspace = WorkspaceId::new();
    let authorized = AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: Some("Hung announcement".into()),
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    let peer_id = authorized.device_id;
    let store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let egress = Arc::new(PendingEgress::default());
    let runtime = SessionRuntimeHandle::new(store, egress);
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![authorized.clone()],
        local_build: None,
    });
    runtime.connection_event(koru_network_edge::ConnectionEvent::Connected(
        koru_network_edge::ConnectionMetadata {
            connection_id: 92,
            principal: device_principal(&authorized, 1),
            path: koru_network_edge::PathMetadata {
                kind: PathKind::Direct,
                outbound: true,
                selected_is_ip: false,
            },
        },
    ));
    runtime.notify_local_change();
    tokio::task::yield_now().await;
    tokio::time::advance(CHANGE_ANNOUNCEMENT_DEBOUNCE).await;
    tokio::task::yield_now().await;
    tokio::time::advance(SYNC_REQUEST_TIMEOUT).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert!(matches!(
        runtime.status().last_sync_event,
        Some(event)
            if event.peer == peer_id
                && event.stage == "change_announcement_failed"
                && event.error_code.as_deref() == Some("sync_request")
    ));
}

#[tokio::test]
async fn opaque_iroh_edges_leave_replication_parsing_with_session_runtimes() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let document = DocumentId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, manifest));
    let b_store = Arc::new(InventoryStore::new(workspace, manifest));
    a_store.add(document);

    let a_ingress = Arc::new(SessionIngressSlot::new());
    let b_ingress = Arc::new(SessionIngressSlot::new());
    let a_edge =
        RuntimeHandle::spawn_with_opaque_ingress(IrohNetworkConfig::isolated(), a_ingress.clone());
    let b_edge =
        RuntimeHandle::spawn_with_opaque_ingress(IrohNetworkConfig::isolated(), b_ingress.clone());
    let a_sessions =
        SessionRuntimeHandle::new(a_store.clone(), Arc::new(EdgeEgress(a_edge.clone())));
    let b_sessions =
        SessionRuntimeHandle::new(b_store.clone(), Arc::new(EdgeEgress(b_edge.clone())));
    a_ingress.install(a_sessions.clone());
    b_ingress.install(b_sessions.clone());

    a_edge.provision_endpoint(a_endpoint.secret_bytes());
    b_edge.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a_edge.endpoint_addr().is_some() && b_edge.endpoint_addr().is_some()).await;
    let a_grant = AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    let b_grant = AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    a_sessions.replace_authorization(a_grant.clone());
    b_sessions.replace_authorization(b_grant.clone());
    a_edge.replace_authorization(a_grant);
    b_edge.replace_authorization(b_grant);
    a_edge.connect_peer_at(b_id, b_edge.endpoint_addr().unwrap());

    tokio::time::timeout(Duration::from_secs(10), async {
        while !b_store.has(document) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("opaque edge replication did not converge");
    assert!(matches!(
        b_sessions.status().peers.get(&a_id),
        Some(PeerConnectionState::Connected { .. })
    ));

    // A connection-time catalog pull is not enough: a durable edit made while
    // the same opaque edge remains connected must announce to the other side,
    // which then performs its own authoritative pull. Restarting either edge
    // must not be required to make live edits appear.
    let live_document = DocumentId::new();
    a_store.add(live_document);
    a_sessions.notify_local_change();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !b_store.has(live_document),
        "session-edge announcements must retain the trailing debounce"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !b_store.has(live_document) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a live edit required an edge restart before it converged");
    a_edge.clear();
    b_edge.clear();
}

#[tokio::test]
async fn durable_change_hints_wait_for_half_a_second_of_quiet() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, manifest));
    let b_store = Arc::new(InventoryStore::new(workspace, manifest));
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    let first = DocumentId::new();
    let second = DocumentId::new();
    a_store.add(first);
    a.notify_local_change();
    tokio::time::sleep(Duration::from_millis(300)).await;
    a_store.add(second);
    a.notify_local_change();
    wait_until(|| {
        a.status()
            .last_sync_event
            .is_some_and(|event| event.stage == "change_debounce_reset")
    })
    .await;

    // The second durable change resets the quiet period. A leading-edge timer
    // would already have sent the first document; a trailing debounce sends
    // one authoritative comparison after the final half-second of quiet.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b_store.has(first));
    assert!(!b_store.has(second));

    tokio::time::timeout(Duration::from_secs(3), async {
        while !b_store.has(first) || !b_store.has(second) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the quiet period did not announce the latest durable inventory");
    a.clear();
    b.clear();
}

#[tokio::test]
async fn a_fresh_hint_interrupts_a_long_failed_workspace_retry_delay() {
    let (wake, mut retry_wake) = watch::channel(0u64);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        wake.send_replace(1);
    });

    let hinted = tokio::time::timeout(
        Duration::from_secs(1),
        wait_for_retry_or_hint(Duration::from_secs(30), &mut retry_wake),
    )
    .await
    .expect("a fresh sync hint remained stuck behind retry backoff");
    assert!(hinted);
}

#[tokio::test]
async fn blob_hydration_waits_without_requests_when_no_peer_is_authorized() {
    let workspace = WorkspaceId::new();
    let store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let runtime = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), store.clone());
    let hash = store.add_blob(vec![7; 32]);
    store.blobs.lock().unwrap().remove(&hash);

    let ticket = runtime.hydrate_blob(workspace, hash);
    wait_for_hydration_state(ticket.subscribe(), |state| {
        matches!(
            state,
            BlobHydrationState::WaitingForPeer {
                reason: BlobWaitReason::NoLinkedDevices
            }
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(store.blob_reads.load(Ordering::Relaxed), 0);

    runtime.clear();
    assert_eq!(ticket.wait().await.unwrap_err().code, "runtime_closed");
}

/// A parked hydration answers the asker instead of holding the request — a
/// blob GET must not hang until an offline phone comes back — and the two
/// park reasons keep their own codes, because "wait for that device" and
/// "nothing is linked" are different sentences to a person. Parking is not
/// giving up: the same entry still completes once the holder connects.
#[tokio::test]
async fn a_parked_hydration_answers_the_asker_and_still_finishes_later() {
    // No linked devices at all: its own code.
    let workspace = WorkspaceId::new();
    let store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let runtime = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), store.clone());
    let hash = store.add_blob(vec![7; 32]);
    store.blobs.lock().unwrap().remove(&hash);
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.hydrate_blob(workspace, hash).wait_unless_parked(),
    )
    .await
    .expect("a parked hydration held the request open")
    .unwrap_err();
    assert_eq!(error.code, "no_linked_devices");
    assert!(error.retryable);
    runtime.clear();

    // A linked holder that is offline: waiting_for_peer — and connecting the
    // holder finishes the hydration the early answer left running.
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let b_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let bytes = vec![0x5a; 4096];
    let hash = a_store.add_blob(bytes.clone());
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        local_build: None,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        local_build: None,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
    });
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        b.hydrate_blob(workspace, hash.clone()).wait_unless_parked(),
    )
    .await
    .expect("a parked hydration held the request open")
    .unwrap_err();
    assert_eq!(error.code, "blob_waiting_for_peer");
    assert!(error.retryable);

    b.connect_peer_at(a_id, a.endpoint_addr().unwrap());
    tokio::time::timeout(
        Duration::from_secs(10),
        b.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("blob hydration remained waiting after the peer connected")
    .expect("blob hydration failed");
    assert_eq!(b_store.blob_bytes(&hash), Some(bytes));
    let hash_text = hash.to_string();
    assert!(b.status().state_events.iter().any(|event| {
        event.peer == a_id
            && event.kind == "blob_stored"
            && event.workspace == Some(workspace)
            && event.blob_hash.as_deref() == Some(hash_text.as_str())
    }));
    a.clear();
    b.clear();
}

#[test]
fn state_change_history_is_bounded_and_keeps_only_durable_categories() {
    let status = RwLock::new(NetworkStatus::default());
    let peer = DeviceId::new();
    for index in 0..(MAX_STATE_EVENTS + 3) {
        record_state_change(
            &status,
            None,
            peer,
            "document_updated",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(index),
        );
    }
    let status = status.into_inner().unwrap();
    assert_eq!(status.state_events.len(), MAX_STATE_EVENTS);
    assert_eq!(status.state_events.front().unwrap().items, Some(3));
    assert_eq!(
        status.state_events.back().unwrap().items,
        Some(MAX_STATE_EVENTS + 2)
    );
}

#[test]
fn a_history_write_failure_does_not_hide_the_applied_state_change() {
    let workspace = WorkspaceId::new();
    let store = InventoryStore::new(workspace, DocumentId::new());
    store.fail_history.store(true, Ordering::SeqCst);
    let status = RwLock::new(NetworkStatus::default());
    record_state_change(
        &status,
        Some(&store),
        DeviceId::new(),
        "document_updated",
        None,
        Some(workspace),
        None,
        None,
        None,
        None,
        Some(2),
    );
    let status = status.into_inner().unwrap();
    assert_eq!(status.state_events.len(), 1);
    assert_eq!(status.state_events[0].items, Some(2));
}

#[test]
fn catalog_change_detail_names_each_changed_category() {
    let changes = CatalogChanges {
        root: 1,
        profiles: 1,
        workspaces: 2,
        documents: 3,
    };
    assert_eq!(changes.total(), 7);
    assert_eq!(
        changes.summary(),
        "1 placement change, 1 profile change, 2 workspace changes, 3 document changes"
    );
}

#[test]
fn imported_operation_count_uses_loro_ids_not_network_frames() {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(7).unwrap();
    let before = doc.oplog_vv().encode();
    doc.get_text("text").insert(0, "abc").unwrap();
    doc.commit();
    let after = doc.oplog_vv().encode();
    assert_eq!(imported_op_count(&before, &after), Some(3));
}

#[tokio::test]
async fn offline_blob_hydration_wakes_once_and_deduplicates_a_multi_chunk_transfer() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let b_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let bytes = (0..(2 * BLOB_CHUNK_BYTES as usize + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let hash = a_store.add_blob(bytes.clone());
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });

    let first = b.hydrate_blob(workspace, hash.clone());
    wait_for_hydration_state(first.subscribe(), |state| {
        matches!(
            state,
            BlobHydrationState::WaitingForPeer {
                reason: BlobWaitReason::Offline { .. }
            }
        )
    })
    .await;
    let mut tickets = vec![first];
    tickets.extend((0..9).map(|_| b.hydrate_blob(workspace, hash.clone())));
    b.connect_peer_at(a_id, a.endpoint_addr().unwrap());

    for ticket in tickets {
        tokio::time::timeout(Duration::from_secs(5), ticket.wait())
            .await
            .expect("blob hydration remained waiting after the peer connected")
            .expect("blob hydration failed");
    }
    assert_eq!(b_store.blob_bytes(&hash), Some(bytes));
    assert_eq!(a_store.blob_reads.load(Ordering::Relaxed), 3);
    assert_eq!(b_store.blob_writes.load(Ordering::Relaxed), 1);
    a.clear();
    b.clear();
}

#[tokio::test]
async fn blob_hydration_falls_back_when_the_first_authorized_peer_is_missing_the_blob() {
    let missing_endpoint = ReplicationEndpointSecret::generate();
    let source_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let missing_id = DeviceId::new();
    let source_id = DeviceId::new();
    let target_id = DeviceId::new();
    let workspace = WorkspaceId::new();
    let missing_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let source_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let bytes = vec![0x5a; 4096];
    let hash = source_store.add_blob(bytes.clone());
    let missing =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), missing_store.clone());
    let source =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), source_store.clone());
    let target =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store.clone());
    missing.provision_endpoint(missing_endpoint.secret_bytes());
    source.provision_endpoint(source_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| {
        missing.endpoint_addr().is_some()
            && source.endpoint_addr().is_some()
            && target.endpoint_addr().is_some()
    })
    .await;
    let target_peer = |relationship| AuthorizedPeer {
        relationship_id: relationship,
        device_id: target_id,
        endpoint_id: target_endpoint.endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: Vec::new(),
        via: None,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    missing.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: missing_id,
        peers: vec![target_peer(RelationshipId::new())],
        local_build: None,
    });
    source.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: source_id,
        peers: vec![target_peer(RelationshipId::new())],
        local_build: None,
    });
    target.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: target_id,
        peers: vec![
            AuthorizedPeer {
                relationship_id: RelationshipId::new(),
                device_id: missing_id,
                endpoint_id: missing_endpoint.endpoint_id(),
                all_data: true,
                authorized_profiles: Vec::new(),
                workspaces: Vec::new(),
                via: None,
                display_name: None,
                build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
            },
            AuthorizedPeer {
                relationship_id: RelationshipId::new(),
                device_id: source_id,
                endpoint_id: source_endpoint.endpoint_id(),
                all_data: true,
                authorized_profiles: Vec::new(),
                workspaces: Vec::new(),
                via: None,
                display_name: None,
                build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
            },
        ],
        local_build: None,
    });
    target.connect_peer_at(missing_id, missing.endpoint_addr().unwrap());
    target.connect_peer_at(source_id, source.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            target.status().peers.get(&missing_id),
            Some(PeerConnectionState::Connected { .. })
        ) && matches!(
            target.status().peers.get(&source_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    tokio::time::timeout(
        Duration::from_secs(5),
        target.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("fallback hydration timed out")
    .expect("fallback hydration failed");
    // Whether the missing peer is asked at all depends on the race between the
    // hydration and the inventory exchange that teaches the directory who
    // holds what. Either way it never *serves*: the holder does.
    assert!(missing_store.blob_reads.load(Ordering::Relaxed) <= 1);
    assert_eq!(source_store.blob_reads.load(Ordering::Relaxed), 1);
    assert_eq!(target_store.blob_bytes(&hash), Some(bytes));
    missing.clear();
    source.clear();
    target.clear();
}

/// Once an inventory exchange has taught the directory who holds a blob, a
/// miss asks the holder — and only the holder — instead of sweeping every
/// connected peer in snapshot order.
#[tokio::test]
async fn blob_hydration_asks_a_known_holder_first() {
    let bystander_endpoint = ReplicationEndpointSecret::generate();
    let holder_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let bystander_id = DeviceId::new();
    let holder_id = DeviceId::new();
    let target_id = DeviceId::new();
    let workspace = WorkspaceId::new();
    let bystander_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let holder_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let bytes = vec![0x5a; 4096];
    let hash = holder_store.add_blob(bytes.clone());
    let bystander =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), bystander_store.clone());
    let holder =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), holder_store.clone());
    let target =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store.clone());
    bystander.provision_endpoint(bystander_endpoint.secret_bytes());
    holder.provision_endpoint(holder_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| {
        bystander.endpoint_addr().is_some()
            && holder.endpoint_addr().is_some()
            && target.endpoint_addr().is_some()
    })
    .await;
    let grants = |local_device: DeviceId, peers: Vec<(DeviceId, &ReplicationEndpointSecret)>| {
        AuthorizationSnapshot {
            session_id: ControlSessionId::new(),
            sequence: 1,
            local_device,
            peers: peers
                .into_iter()
                .map(|(device_id, endpoint)| AuthorizedPeer {
                    relationship_id: RelationshipId::new(),
                    device_id,
                    endpoint_id: endpoint.endpoint_id(),
                    all_data: true,
                    authorized_profiles: Vec::new(),
                    workspaces: Vec::new(),
                    via: None,
                    display_name: None,
                    build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
                })
                .collect(),
            local_build: None,
        }
    };
    bystander.replace_authorization(grants(bystander_id, vec![(target_id, &target_endpoint)]));
    holder.replace_authorization(grants(holder_id, vec![(target_id, &target_endpoint)]));
    target.replace_authorization(grants(
        target_id,
        vec![
            (bystander_id, &bystander_endpoint),
            (holder_id, &holder_endpoint),
        ],
    ));
    target.connect_peer_at(bystander_id, bystander.endpoint_addr().unwrap());
    target.connect_peer_at(holder_id, holder.endpoint_addr().unwrap());

    // Wait until the sync pass has taught the target's directory who holds
    // the blob; hydrating before that would legitimately sweep.
    wait_until(|| target.blob_holders(workspace, &hash) == vec![holder_id]).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        target.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("holder-directed hydration timed out")
    .expect("holder-directed hydration failed");
    assert_eq!(target_store.blob_bytes(&hash), Some(bytes));
    assert_eq!(
        bystander_store.blob_reads.load(Ordering::Relaxed),
        0,
        "a peer the directory knows does not hold the blob is never asked"
    );
    assert_eq!(holder_store.blob_reads.load(Ordering::Relaxed), 1);
    bystander.clear();
    holder.clear();
    target.clear();
}

/// A parked hydration names the offline holder it is waiting for, when one is
/// known — the difference between "waiting for Studio" and "the device
/// holding it".
#[tokio::test]
async fn a_parked_hydration_names_the_offline_holder() {
    let holder_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let holder_id = DeviceId::new();
    let target_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let holder_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let hash = holder_store.add_blob(vec![1, 2, 3]);
    let holder =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), holder_store.clone());
    let target =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store.clone());
    holder.provision_endpoint(holder_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| holder.endpoint_addr().is_some() && target.endpoint_addr().is_some()).await;
    holder.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: holder_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: target_id,
            endpoint_id: target_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("Target".into()),
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: target_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: holder_id,
            endpoint_id: holder_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("Studio".into()),
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.connect_peer_at(holder_id, holder.endpoint_addr().unwrap());
    // The holder advertises the blob, then leaves. Only then is the park
    // expected to know whom it waits for.
    wait_until(|| target.blob_holders(workspace, &hash) == vec![holder_id]).await;
    target.disconnect_peer(holder_id);
    wait_until(|| {
        matches!(
            target.status().peers.get(&holder_id),
            Some(PeerConnectionState::Disconnected)
        )
    })
    .await;

    let ticket = target.hydrate_blob(workspace, hash.clone());
    wait_for_hydration_state(ticket.subscribe(), |state| {
        matches!(
            state,
            BlobHydrationState::WaitingForPeer {
                reason: BlobWaitReason::Offline { holders }
            } if holders == &vec!["Studio".to_string()]
        )
    })
    .await;

    // And the named park still finishes when the holder returns.
    target.connect_peer_at(holder_id, holder.endpoint_addr().unwrap());
    tokio::time::timeout(Duration::from_secs(5), ticket.wait())
        .await
        .expect("named park did not finish when the holder returned")
        .expect("named park hydration failed");
    assert_eq!(target_store.blob_bytes(&hash), Some(vec![1, 2, 3]));
    holder.clear();
    target.clear();
}

/// The cold-boot case: the session directory is empty (a restart threw it
/// away, as designed) and every holder is offline — and the parked hydration
/// still names the origin, because the advertisement's provenance was written
/// to the store when it arrived.
#[tokio::test]
async fn a_parked_hydration_names_the_durable_origin_after_the_session_forgets() {
    let origin_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let origin_id = DeviceId::new();
    let target_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let origin_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let hash = origin_store.add_blob_with_origin(vec![1, 2, 3], &origin_id.to_storage());
    let origin =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), origin_store.clone());
    let target =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store.clone());
    origin.provision_endpoint(origin_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| origin.endpoint_addr().is_some() && target.endpoint_addr().is_some()).await;
    origin.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: origin_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: target_id,
            endpoint_id: target_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("Target".into()),
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: target_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: origin_id,
            endpoint_id: origin_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("Studio".into()),
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.connect_peer_at(origin_id, origin.endpoint_addr().unwrap());
    // The inventory lands and its provenance is durably recorded — that, not
    // the session directory, is what must survive the restart.
    wait_until(|| target_store.advertised_origin(&hash).is_some()).await;
    target.disconnect_peer(origin_id);
    wait_until(|| {
        matches!(
            target.status().peers.get(&origin_id),
            Some(PeerConnectionState::Disconnected)
        )
    })
    .await;
    target.clear_blob_directory();

    let ticket = target.hydrate_blob(workspace, hash.clone());
    wait_for_hydration_state(ticket.subscribe(), |state| {
        matches!(
            state,
            BlobHydrationState::WaitingForPeer {
                reason: BlobWaitReason::Offline { holders }
            } if holders == &vec!["Studio".to_string()]
        )
    })
    .await;
    origin.clear();
    target.clear();
}

#[tokio::test]
async fn blob_hydration_rejects_bytes_that_do_not_match_the_announced_hash() {
    let source_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let source_id = DeviceId::new();
    let target_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let source_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let expected = b"expected bytes";
    let hash = BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(expected).to_hex()).unwrap();
    source_store.blobs.lock().unwrap().insert(
        hash.clone(),
        (
            koru_store_core::model::Blob {
                hash: hash.clone(),
                size: 15,
                mime: "image/png".into(),
                filename: None,
                created_at: koru_store_core::Timestamp::from_millis(1),
                attributes: Default::default(),
            },
            b"different bytes".to_vec(),
        ),
    );
    let source =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), source_store.clone());
    let target =
        RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store.clone());
    source.provision_endpoint(source_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| source.endpoint_addr().is_some() && target.endpoint_addr().is_some()).await;
    source.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: source_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: target_id,
            endpoint_id: target_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: target_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: source_id,
            endpoint_id: source_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.connect_peer_at(source_id, source.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            target.status().peers.get(&source_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        target.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("integrity rejection timed out")
    .unwrap_err();
    assert_eq!(error.code, "blob_checksum_mismatch");
    assert!(!error.retryable);
    assert_eq!(target_store.blob_bytes(&hash), None);
    source.clear();
    target.clear();
}

/// Pair two runtimes over `all_data` and wait until the target is connected to
/// the source. Every blob test needs this and only this before it can ask for a
/// transfer.
async fn connected_pair(
    source_store: Arc<InventoryStore>,
    target_store: Arc<InventoryStore>,
) -> (RuntimeHandle, RuntimeHandle) {
    let source_endpoint = ReplicationEndpointSecret::generate();
    let target_endpoint = ReplicationEndpointSecret::generate();
    let source_id = DeviceId::new();
    let target_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let source = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), source_store);
    let target = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), target_store);
    source.provision_endpoint(source_endpoint.secret_bytes());
    target.provision_endpoint(target_endpoint.secret_bytes());
    wait_until(|| source.endpoint_addr().is_some() && target.endpoint_addr().is_some()).await;
    source.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: source_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: target_id,
            endpoint_id: target_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: target_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: source_id,
            endpoint_id: source_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    target.connect_peer_at(source_id, source.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            target.status().peers.get(&source_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    (source, target)
}

fn build_on_protocol(protocol: u32) -> koru_device::BuildPayload {
    koru_device::BuildPayload {
        app_version: "1.0.0".into(),
        channel: "test".into(),
        peer_protocol: Some(protocol),
        ..Default::default()
    }
}

/// Two devices on different peer protocol numbers keep their connection and
/// their trust gossip — that is how the older one learns it must update — but
/// no sync pass runs in either direction and a data-plane request is refused
/// by name, so nothing is half-applied between builds that disagree about
/// what the frames mean.
#[tokio::test]
async fn peers_on_different_protocol_numbers_stay_connected_but_do_not_sync() {
    let workspace = WorkspaceId::new();
    let a_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let b_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_key = DeviceSigningKey::generate();
    let b_key = DeviceSigningKey::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship_id = RelationshipId::new();
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    let snapshot =
        |local, local_protocol, peer, endpoint: &ReplicationEndpointSecret, peer_protocol| {
            AuthorizationSnapshot {
                session_id: ControlSessionId::new(),
                sequence: 1,
                local_device: local,
                peers: vec![AuthorizedPeer {
                    relationship_id,
                    device_id: peer,
                    endpoint_id: endpoint.endpoint_id(),
                    all_data: true,
                    authorized_profiles: Vec::new(),
                    workspaces: vec![workspace],
                    via: None,
                    display_name: None,
                    build: Some(build_on_protocol(peer_protocol)),
                }],
                local_build: Some(build_on_protocol(local_protocol)),
            }
        };
    a.replace_authorization(snapshot(a_id, PEER_PROTOCOL_VERSION, b_id, &b_endpoint, 4));
    b.replace_authorization(snapshot(b_id, 4, a_id, &a_endpoint, PEER_PROTOCOL_VERSION));
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        ) && matches!(
            b.status().peers.get(&a_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    // Each side says once, where a status surface reads it, why nothing moves.
    wait_until(|| {
        [a.status(), b.status()]
            .iter()
            .zip(["profile_units_required", "incompatible_protocol"])
            .all(|(status, code)| {
                status.last_sync_event.as_ref().is_some_and(|event| {
                    event.stage == "sync_refused" && event.error_code.as_deref() == Some(code)
                })
            })
    })
    .await;

    // Trust gossip still crosses: the connection is for exactly that.
    let a_descriptor =
        DeviceDescriptorV1::new(a_id, "A".into(), &a_key, a_endpoint.endpoint_id(), 1);
    let b_descriptor =
        DeviceDescriptorV1::new(b_id, "B".into(), &b_key, b_endpoint.endpoint_id(), 1);
    let relationship = koru_device::TrustRelationshipV1::sign(
        relationship_id,
        a_descriptor,
        b_descriptor,
        [7; 32],
        1,
        &a_key,
        &b_key,
    );
    let batch = vec![koru_device::TrustRecord::Relationship(relationship)];
    let delivered = a.send_trust_records(b_id, batch.clone());
    let queued =
        tokio::task::spawn_blocking(move || delivered.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap();
    assert_eq!(queued, Ok(true));
    let records = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PairingEvent::TrustRecordsReceived { records, .. }) =
                b.take_pairing_events().into_iter().next()
            {
                break records;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip must still reach an incompatible peer");
    assert_eq!(records, batch);

    // But no sync pass ran in either direction — a catalog is the first
    // thing a pass asks for — and a blob cannot be fetched from that peer.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a_store.catalog_reads.load(Ordering::SeqCst), 0);
    assert_eq!(b_store.catalog_reads.load(Ordering::SeqCst), 0);
    let hash = a_store.add_blob(vec![1, 2, 3]);
    let ticket = b.hydrate_blob(workspace, hash);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = ticket.state().borrow().clone();
    let parks_without_naming_anyone = matches!(
        &state,
        BlobHydrationState::WaitingForPeer {
            reason: BlobWaitReason::Offline { holders }
        } if holders.is_empty()
    );
    assert!(
        parks_without_naming_anyone,
        "an incompatible peer is not a blob source and not a holder to wait on: {state:?}"
    );
    a.clear();
    b.clear();
}

/// Hydration was built for image nodes and refused every other content type, so
/// a shared PDF or `.url` could not reach a second device however small it was.
/// Content type is not what makes a transfer safe — the digest is.
#[tokio::test]
async fn a_blob_that_is_not_an_image_hydrates_like_any_other() {
    let workspace = WorkspaceId::new();
    let source_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let bytes = b"https://koru.app/\n".to_vec();
    let hash = source_store.add_blob_with_mime(bytes.clone(), "text/uri-list");
    let (source, target) = connected_pair(source_store, target_store.clone()).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        target.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("hydrating a non-image blob timed out")
    .expect("hydrating a non-image blob failed");
    assert_eq!(target_store.blob_bytes(&hash), Some(bytes));
    source.clear();
    target.clear();
}

/// Lifting the content-type bound must not lift the size bound with it. Full
/// payloads are still buffered on both sides, so the cap is what keeps a
/// transfer's memory cost knowable until streaming lands.
#[tokio::test]
async fn a_blob_larger_than_the_cap_is_still_refused() {
    let workspace = WorkspaceId::new();
    let source_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    let target_store = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
    // Announced rather than allocated: the receiver refuses from the first
    // chunk's metadata, so proving that costs no memory.
    let hash = source_store.add_blob_announcing(
        b"a very large file".to_vec(),
        "application/pdf",
        MAX_BLOB_HYDRATION_BYTES + 1,
    );
    let (source, target) = connected_pair(source_store, target_store.clone()).await;

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        target.hydrate_blob(workspace, hash.clone()).wait(),
    )
    .await
    .expect("the size refusal timed out")
    .unwrap_err();
    assert_eq!(error.code, "blob_too_large");
    assert!(!error.retryable);
    assert_eq!(target_store.blob_bytes(&hash), None);
    source.clear();
    target.clear();
}

#[tokio::test]
async fn a_durable_change_hint_transfers_new_manifest_crdt_bytes() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let (a_store, b_store) = ManifestStore::pair(workspace, manifest);
    let a_store = Arc::new(a_store);
    let b_store = Arc::new(b_store);
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    a_store.rename("Renamed on mobile");
    a.notify_local_change();
    tokio::time::timeout(Duration::from_secs(5), async {
        while b_store.name().as_deref() != Some("Renamed on mobile") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the connected peer did not import the changed manifest");

    // The accepting side owns an inbound connection. Its local durable edits
    // must be able to wake the initiator through that same registered session;
    // mobile commonly occupies this side after desktop initiates pairing.
    b_store.rename("Renamed on accepting peer");
    b.notify_local_change();
    tokio::time::timeout(Duration::from_secs(5), async {
        while a_store.name().as_deref() != Some("Renamed on accepting peer") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the accepting peer could not announce its changed manifest");
    a.clear();
    b.clear();
}

#[tokio::test]
async fn an_unchanged_large_inventory_does_not_delay_a_later_manifest() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let (a_store, b_store) = ManifestStore::pair_with_documents(workspace, manifest, 1_333);
    let a_store = Arc::new(a_store);
    let b_store = Arc::new(b_store);
    let a = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), a_store.clone());
    let b = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), b_store.clone());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    // Equal versions are skipped locally. A later remote edit must therefore
    // remain a bounded single-document pull rather than waiting behind serial
    // no-op joins for every unchanged document in a large inventory.
    b_store.rename("Later remote rename");
    b.notify_local_change();
    tokio::time::timeout(Duration::from_secs(2), async {
        while a_store.name().as_deref() != Some("Later remote rename") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a later manifest was delayed by unchanged inventory entries");
    a.clear();
    b.clear();
}

#[tokio::test]
async fn exact_address_cannot_substitute_a_different_endpoint_identity() {
    let local_secret = ReplicationEndpointSecret::generate();
    let expected_secret = ReplicationEndpointSecret::generate();
    let actual_secret = ReplicationEndpointSecret::generate();
    let local = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let actual = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&actual_secret.secret_bytes()))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .unwrap();
    local.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| local.status().endpoint_id.is_some()).await;
    let peer = DeviceId::new();
    local.replace_authorization(snapshot(peer, expected_secret.endpoint_id()));
    local.connect_peer_at(peer, actual.addr());
    wait_until(|| local.status().peers.contains_key(&peer)).await;
    assert!(matches!(
        local.status().peers.get(&peer),
        Some(PeerConnectionState::Unauthorized | PeerConnectionState::Failed { .. })
    ));
    actual.close().await;
}

#[tokio::test]
async fn an_authorized_endpoint_connects_over_an_injected_direct_address() {
    let local_secret = ReplicationEndpointSecret::generate();
    let remote_secret = ReplicationEndpointSecret::generate();
    let remote = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&remote_secret.secret_bytes()))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .unwrap();
    let accepting = remote.clone();
    tokio::spawn(async move {
        let incoming = accepting.accept().await.unwrap();
        let connection = incoming.accept().unwrap().await.unwrap();
        connection.closed().await;
    });

    let local = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    local.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| local.status().endpoint_id.is_some()).await;
    let peer = DeviceId::new();
    local.replace_authorization(snapshot(peer, remote_secret.endpoint_id()));
    local.connect_peer_at(peer, remote.addr());
    wait_until(|| {
        matches!(
            local.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    assert_eq!(
        local.status().peers.get(&peer),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Direct
        })
    );
    local.clear();
    remote.close().await;
}

/// A dead route may keep QUIC discovery and handshaking pending for seconds.
/// That network wait must not occupy the command actor: durable changes still
/// start their debounce, and explicit disconnect still takes effect.
#[tokio::test]
async fn a_stalled_outbound_dial_does_not_block_the_command_actor() {
    let local_secret = ReplicationEndpointSecret::generate();
    let missing_secret = ReplicationEndpointSecret::generate();
    let local = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    local.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| local.status().endpoint_id.is_some()).await;

    let peer = DeviceId::new();
    local.replace_authorization(snapshot(peer, missing_secret.endpoint_id()));
    let black_hole =
        EndpointAddr::new(EndpointId::from_bytes(missing_secret.endpoint_id().as_bytes()).unwrap())
            .with_ip_addr("203.0.113.1:9".parse().unwrap());
    local.connect_peer_at(peer, black_hole);
    wait_until(|| {
        matches!(
            local.status().peers.get(&peer),
            Some(PeerConnectionState::Connecting | PeerConnectionState::BackingOff)
        )
    })
    .await;

    local.notify_local_change();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if local
                .status()
                .last_sync_event
                .is_some_and(|event| event.stage == "change_waiting_for_connection")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an outbound dial blocked the change debounce command");

    local.disconnect_peer(peer);
    tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            if local.status().peers.get(&peer) == Some(&PeerConnectionState::Disconnected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an outbound dial blocked explicit disconnect");
    local.clear();
}

#[tokio::test]
async fn a_failed_change_announcement_keeps_the_quic_connection_until_actual_close() {
    let local_secret = ReplicationEndpointSecret::generate();
    let remote_secret = ReplicationEndpointSecret::generate();
    let remote = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&remote_secret.secret_bytes()))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .unwrap();
    let accepting = remote.clone();
    tokio::spawn(async move {
        let incoming = accepting.accept().await.unwrap();
        let connection = incoming.accept().unwrap().await.unwrap();
        // Refuse the first request stream without closing the QUIC
        // connection. One failed stream is not proof that every later stream
        // on the connection is unusable.
        let (send, recv) = connection.accept_bi().await.unwrap();
        drop((send, recv));
        std::future::pending::<()>().await;
    });

    let local = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    local.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| local.status().endpoint_id.is_some()).await;
    let peer = DeviceId::new();
    local.replace_authorization(snapshot(peer, remote_secret.endpoint_id()));
    local.connect_peer_at(peer, remote.addr());
    wait_until(|| {
        matches!(
            local.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    local.notify_local_change();
    wait_until(|| {
        local
            .status()
            .last_sync_event
            .is_some_and(|event| event.stage == "change_announcement_failed")
    })
    .await;
    let current = local.status();
    let event = current.last_sync_event.unwrap();
    assert_eq!(event.peer, peer);
    assert_eq!(event.stage, "change_announcement_failed");
    assert_eq!(event.error_code.as_deref(), Some("sync_request"));
    assert!(matches!(
        current.peers.get(&peer),
        Some(PeerConnectionState::Connected { .. })
    ));
    local.disconnect_peer(peer);
    remote.close().await;
}

#[tokio::test]
async fn an_unexpected_disconnect_enters_bounded_backoff() {
    let local_secret = ReplicationEndpointSecret::generate();
    let remote_secret = ReplicationEndpointSecret::generate();
    let remote = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&remote_secret.secret_bytes()))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .unwrap();
    let accepting = remote.clone();
    tokio::spawn(async move {
        let incoming = accepting.accept().await.unwrap();
        let _connection = incoming.accept().unwrap().await.unwrap();
        std::future::pending::<()>().await;
    });

    let local = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    local.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| local.status().endpoint_id.is_some()).await;
    let peer = DeviceId::new();
    local.replace_authorization(snapshot(peer, remote_secret.endpoint_id()));
    local.connect_peer_at(peer, remote.addr());
    wait_until(|| {
        matches!(
            local.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    remote.close().await;
    wait_until(|| {
        matches!(
            local.status().peers.get(&peer),
            Some(PeerConnectionState::BackingOff)
        )
    })
    .await;
    local.disconnect_peer(peer);
    wait_until(|| local.status().peers.get(&peer) == Some(&PeerConnectionState::Disconnected))
        .await;
}

#[tokio::test]
async fn pairing_alpn_carries_the_signed_exchange_before_authorization_exists() {
    let inviter_endpoint = ReplicationEndpointSecret::generate();
    let joiner_endpoint = ReplicationEndpointSecret::generate();
    let inviter_key = DeviceSigningKey::generate();
    let joiner_key = DeviceSigningKey::generate();
    let inviter_descriptor = DeviceDescriptorV1::new(
        DeviceId::new(),
        "Windows".into(),
        &inviter_key,
        inviter_endpoint.endpoint_id(),
        1,
    );
    let joiner_descriptor = DeviceDescriptorV1::new(
        DeviceId::new(),
        "Android".into(),
        &joiner_key,
        joiner_endpoint.endpoint_id(),
        1,
    );
    let inviter = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let joiner = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    inviter.provision_endpoint(inviter_endpoint.secret_bytes());
    joiner.provision_endpoint(joiner_endpoint.secret_bytes());
    wait_until(|| inviter.endpoint_addr().is_some() && joiner.endpoint_addr().is_some()).await;

    let invitation = koru_pairing::PairingInvitationV1::new(inviter_descriptor, None);
    inviter.begin_pairing(invitation.clone());
    let half =
        koru_pairing::PairingJoinV1::sign(&invitation, joiner_descriptor, &joiner_key, 42).unwrap();
    joiner.join_pairing_at(half.clone(), inviter.endpoint_addr().unwrap());

    let (attempt_id, received) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PairingEvent::JoinRequested { attempt_id, join }) =
                inviter.take_pairing_events().into_iter().next()
            {
                break (attempt_id, join);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("inviter did not receive the signed join half");
    assert_eq!(*received, half);
    let prepared = received.complete(&invitation, &inviter_key).unwrap();
    inviter.complete_pairing(attempt_id, prepared.clone());

    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(event) = joiner.take_pairing_events().into_iter().next() {
                match event {
                    PairingEvent::Completed { prepared } => break prepared,
                    PairingEvent::Failed { code } => panic!("joiner failed: {code}"),
                    PairingEvent::JoinRequested { .. } => panic!("joiner received inviter event"),
                    PairingEvent::TrustRecordReceived { .. } => {
                        panic!("joiner received an unrelated trust event")
                    }
                    PairingEvent::TrustRecordsReceived { .. } => {
                        panic!("joiner received an unrelated trust gossip")
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("joiner did not receive the prepared pairing");
    assert_eq!(*completed, prepared);
    completed.verify().unwrap();
    inviter.clear();
    joiner.clear();
}

#[tokio::test]
async fn trust_gossip_batches_arrive_without_interrupting_the_connection() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_key = DeviceSigningKey::generate();
    let b_key = DeviceSigningKey::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship_id = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        ) && matches!(
            b.status().peers.get(&a_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    let a_descriptor =
        DeviceDescriptorV1::new(a_id, "A".into(), &a_key, a_endpoint.endpoint_id(), 1);
    let b_descriptor =
        DeviceDescriptorV1::new(b_id, "B".into(), &b_key, b_endpoint.endpoint_id(), 1);
    let relationship = koru_device::TrustRelationshipV1::sign(
        relationship_id,
        a_descriptor,
        b_descriptor.clone(),
        [7; 32],
        1,
        &a_key,
        &b_key,
    );
    let grant = koru_device::AllDataGrantV1::sign(
        relationship_id,
        b_id,
        a_id,
        koru_device::RecordId::new(),
        2,
        &a_key,
    );
    let batch = vec![
        koru_device::TrustRecord::Relationship(relationship),
        koru_device::TrustRecord::AllDataGrant(grant),
    ];
    let delivered = a.send_trust_records(b_id, batch.clone());
    // Receiving on the blocking pool: the actor runs on this test's
    // current-thread runtime, and `recv_timeout` on the test's own thread
    // would starve it.
    let queued =
        tokio::task::spawn_blocking(move || delivered.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap();
    assert_eq!(queued, Ok(true), "a connected peer queues the batch");

    let (delivery_id, from, records) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PairingEvent::TrustRecordsReceived {
                delivery_id,
                from,
                records,
            }) = b.take_pairing_events().into_iter().next()
            {
                break (delivery_id, from, records);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("receiver did not see the gossip batch");
    assert_eq!(from, a_id);
    assert_eq!(records, batch);

    // Gossip is not a teardown: the connection and the fence survive the
    // batch, before and after its ack.
    b.complete_trust_record(delivery_id, true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        a.status().peers.get(&b_id),
        Some(PeerConnectionState::Connected { .. })
    ));
    a.clear();
    b.clear();
}

#[tokio::test]
async fn trust_gossip_to_an_unconnected_peer_reports_not_delivered() {
    // The answer a revocation-propagation retry depends on: a batch that
    // never reached a live connection must read as not delivered, so the
    // sender retries instead of marking it sent.
    let endpoint = ReplicationEndpointSecret::generate();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some()).await;

    let key = DeviceSigningKey::generate();
    let b_key = DeviceSigningKey::generate();
    let descriptor =
        DeviceDescriptorV1::new(DeviceId::new(), "A".into(), &key, endpoint.endpoint_id(), 1);
    let other = DeviceDescriptorV1::new(
        DeviceId::new(),
        "B".into(),
        &b_key,
        ReplicationEndpointSecret::generate().endpoint_id(),
        1,
    );
    let record = koru_device::TrustRecord::Relationship(koru_device::TrustRelationshipV1::sign(
        RelationshipId::new(),
        descriptor,
        other,
        [3; 32],
        1,
        &key,
        &b_key,
    ));
    let delivered = a.send_trust_records(DeviceId::new(), vec![record]);
    let queued =
        tokio::task::spawn_blocking(move || delivered.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap();
    assert_eq!(queued, Ok(false));
    a.clear();
}

#[tokio::test]
async fn a_connection_that_dies_on_first_use_accumulates_backoff() {
    // A authorized for B, but B authorizes nobody: B closes A's connection
    // right after the handshake, every time. Each cycle must count — the old
    // rule reset on connect and pinned the retry at its fastest forever.
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    // B deliberately gets no snapshot: A is a stranger to it.
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| a.status().backoff_failures.get(&b_id).copied().unwrap_or(0) >= 3).await;
    a.clear();
    b.clear();
}

#[tokio::test]
async fn a_connection_that_proves_itself_resets_its_backoff() {
    // A long-lived connection whose close is a blip, not a pattern: the
    // failure count starts fresh instead of climbing.
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    let snapshot = |local_device, device_id, endpoint_id| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id,
            endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    a.replace_authorization(snapshot(a_id, b_id, b_endpoint.endpoint_id()));
    b.replace_authorization(snapshot(b_id, a_id, a_endpoint.endpoint_id()));
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    // Let the connection prove itself, then force a close by **changing** the
    // grant — adding a workspace closes every connection without killing the
    // peer. It used to reinstall the identical snapshot; that is a no-op now
    // (ADR-154), which would have made this test assert nothing.
    tokio::time::sleep(EARLY_DEATH + Duration::from_millis(500)).await;
    let mut widened = snapshot(b_id, a_id, a_endpoint.endpoint_id());
    widened.peers[0].workspaces.push(WorkspaceId::new());
    b.replace_authorization(widened);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let failures = a.status().backoff_failures.get(&b_id).copied().unwrap_or(0);
    assert!(
        failures <= 1,
        "a proved connection must not accumulate backoff, got {failures}"
    );
    a.clear();
    b.clear();
}

#[tokio::test]
async fn urgent_lifecycle_and_revocation_records_cross_before_authorization_changes() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let a_key = DeviceSigningKey::generate();
    let a_id = DeviceId::new();
    let b_id = DeviceId::new();
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    a.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: a_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: b_id,
            endpoint_id: b_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    b.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: b_id,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: a_id,
            endpoint_id: a_endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        ) && matches!(
            b.status().peers.get(&a_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    let lifecycle = koru_device::ProfileLifecycleV1::sign(
        koru_device::RecordId::new(),
        a_id,
        koru_store_core::ProfileId::new(),
        koru_device::ProfileLifecycleStateV1::Deleted,
        1,
        uuid::Uuid::new_v4(),
        41,
        &a_key,
    );
    a.send_trust_record(
        b_id,
        koru_device::TrustRecord::ProfileLifecycle(lifecycle.clone()),
    );
    let lifecycle_delivery = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PairingEvent::TrustRecordReceived {
                delivery_id,
                from,
                record,
            }) = b.take_pairing_events().into_iter().next()
            {
                break (delivery_id, from, record);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("receiver did not see the signed Profile lifecycle");
    assert_eq!(lifecycle_delivery.1, a_id);
    assert_eq!(
        *lifecycle_delivery.2,
        koru_device::TrustRecord::ProfileLifecycle(lifecycle)
    );
    b.complete_trust_record(lifecycle_delivery.0, true);
    assert!(matches!(
        a.status().peers.get(&b_id),
        Some(PeerConnectionState::Connected { .. })
    ));

    let revocation = koru_device::RevocationV1::sign(
        relationship,
        b_id,
        a_id,
        koru_device::RecordId::new(),
        42,
        &a_key,
    );
    a.send_trust_record(
        b_id,
        koru_device::TrustRecord::Revocation(revocation.clone()),
    );
    let delivery = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PairingEvent::TrustRecordReceived {
                delivery_id,
                from,
                record,
            }) = b.take_pairing_events().into_iter().next()
            {
                break (delivery_id, from, record);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("receiver did not see the signed revocation");
    assert_eq!(delivery.1, a_id);
    assert_eq!(
        *delivery.2,
        koru_device::TrustRecord::Revocation(revocation)
    );
    assert!(matches!(
        a.status().peers.get(&b_id),
        Some(PeerConnectionState::Connected { .. })
    ));

    b.complete_trust_record(delivery.0, true);
    wait_until(|| a.status().peers.get(&b_id) == Some(&PeerConnectionState::Disconnected)).await;
    a.clear();
    b.clear();
}

#[test]
fn rapid_connections_are_reported_once_per_rolling_minute_burst() {
    let started = Instant::now();
    let mut logs = LifecycleLogState::default();

    for offset in 0..RAPID_CONNECTION_THRESHOLD {
        assert_eq!(
            logs.connected(started + Duration::from_secs(offset as u64)),
            LifecycleLogDecision::Ordinary
        );
    }
    assert_eq!(
        logs.connected(started + Duration::from_secs(RAPID_CONNECTION_THRESHOLD as u64)),
        LifecycleLogDecision::Rapid { connections: 11 }
    );
    assert_eq!(
        logs.connected(started + Duration::from_secs(11)),
        LifecycleLogDecision::Ordinary,
        "one burst produces one info record"
    );

    // Once every member of the first burst has aged out, a later burst may
    // cross the threshold and report again.
    let later = started + RAPID_CONNECTION_WINDOW + Duration::from_secs(12);
    for offset in 0..RAPID_CONNECTION_THRESHOLD {
        assert_eq!(
            logs.connected(later + Duration::from_secs(offset as u64)),
            LifecycleLogDecision::Ordinary
        );
    }
    assert_eq!(
        logs.connected(later + Duration::from_secs(RAPID_CONNECTION_THRESHOLD as u64)),
        LifecycleLogDecision::Rapid { connections: 11 }
    );
}

#[test]
fn crossed_dials_choose_one_physical_connection_at_both_ends() {
    let a = DeviceId::new();
    let b = DeviceId::new();
    let (lower, higher) = if a < b { (a, b) } else { (b, a) };

    // The lower id keeps the connection it initiated.
    assert_eq!(
        choose_connection(
            lower,
            higher,
            ConnectionDirection::Inbound,
            ConnectionDirection::Outbound,
        ),
        ConnectionChoice::UseCandidate
    );
    assert_eq!(
        choose_connection(
            lower,
            higher,
            ConnectionDirection::Outbound,
            ConnectionDirection::Inbound,
        ),
        ConnectionChoice::KeepCurrent
    );

    // The higher id makes the matching choice from the other endpoint: it
    // keeps the inbound half of that same physical connection.
    assert_eq!(
        choose_connection(
            higher,
            lower,
            ConnectionDirection::Outbound,
            ConnectionDirection::Inbound,
        ),
        ConnectionChoice::UseCandidate
    );
    assert_eq!(
        choose_connection(
            higher,
            lower,
            ConnectionDirection::Inbound,
            ConnectionDirection::Outbound,
        ),
        ConnectionChoice::KeepCurrent
    );
}

/// A restarted peer dials again before QUIC notices its old session died —
/// minutes, on an idle relay path. Keeping the current connection here locked
/// every restarted desktop out of its own mesh until that timeout: dial,
/// `duplicate peer connection`, retry, for five minutes, seen live. The
/// newest same-direction session is the one whose far end is alive, so it
/// replaces — and the replace path closes the old connection, so nothing is
/// orphaned.
#[test]
fn a_same_direction_reconnect_replaces_the_stale_session() {
    let local = DeviceId::new();
    let remote = DeviceId::new();
    for direction in [ConnectionDirection::Inbound, ConnectionDirection::Outbound] {
        assert_eq!(
            choose_connection(local, remote, direction, direction),
            ConnectionChoice::UseCandidate
        );
    }
}

#[test]
fn peer_error_summary_keeps_the_exact_remote_error_code() {
    let result = Ok(protocol::PeerFrame::Error {
        code: "document_not_found".into(),
    });

    assert_eq!(
        frame_result_summary(&result),
        "peer_error:document_not_found"
    );
}

// --------------------------------------------- reconnects that change nothing
//
// The three tests below are the data-plane half of ADR-154. What they defend
// is not a feature but an *absence*: an agent reconnect must not be visible
// down here at all. They are written as "stayed connected for a while", which
// is the only honest shape — a closed connection reopens, so a snapshot taken
// once could miss the gap entirely.

/// A pair of runtimes that know each other, connected and settled.
///
/// Answers the two handles, their device ids and the snapshot each installed,
/// so a test can reinstall a grant or change one field of it. No store behind
/// either — these tests are about the connection, not what crosses it, which
/// is what distinguishes this from [`connected_pair`].
async fn storeless_pair() -> (
    RuntimeHandle,
    RuntimeHandle,
    DeviceId,
    DeviceId,
    AuthorizationSnapshot,
    AuthorizationSnapshot,
) {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let (a_id, b_id) = (DeviceId::new(), DeviceId::new());
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    let grant = |local_device, device_id, endpoint_id| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id,
            endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    let a_grant = grant(a_id, b_id, b_endpoint.endpoint_id());
    let b_grant = grant(b_id, a_id, a_endpoint.endpoint_id());
    a.replace_authorization(a_grant.clone());
    b.replace_authorization(b_grant.clone());
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    (a, b, a_id, b_id, a_grant, b_grant)
}

/// Whether `a` dropped its connection to `b` before `deadline` elapsed.
///
/// **The opposite of [`stayed_connected`], and deliberately not the same call
/// with a `!` in front of it.** "It must not disconnect" is a *window* — two
/// seconds of watching is the assertion, and a longer one only asserts it
/// harder. "It must disconnect" is a *liveness deadline*: the claim is that
/// the fencing happens, not that it happens inside two seconds, so the number
/// is only there to fail a runtime that never does it. Sharing one helper made
/// the second read as a two-second budget, and a budget on a machine running
/// eight test binaries against real sockets is a coin flip —
/// `a_changed_grant_still_replaces_wholesale` failed one run in four on an
/// otherwise green tree. See PRINCIPLES, "Measure ratios, not wall clocks".
async fn disconnected_within(a: &RuntimeHandle, b_id: DeviceId, deadline: Duration) -> bool {
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        if !matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        ) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Whether `a` stayed connected to `b` for the whole window.
async fn stayed_connected(a: &RuntimeHandle, b_id: DeviceId, window: Duration) -> bool {
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        if !matches!(
            a.status().peers.get(&b_id),
            Some(PeerConnectionState::Connected { .. })
        ) {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    true
}

/// **The churn, at its source.** Every agent reconnect re-sends the grant it
/// already sent; treating that as a replacement closed every peer. A new
/// session id is the *only* difference, and it is bookkeeping.
#[tokio::test]
async fn an_unchanged_authorization_keeps_live_connections() {
    let (a, b, _, b_id, _, b_grant) = storeless_pair().await;
    let mut resent = b_grant.clone();
    resent.session_id = ControlSessionId::new();
    resent.sequence = 2;
    b.replace_authorization(resent);
    assert!(
        stayed_connected(&a, b_id, Duration::from_secs(2)).await,
        "a re-sent grant closed a live connection"
    );
    // Nothing backed off, so there may be no entry at all — the absence and a
    // zero mean the same thing here.
    assert_eq!(
        a.status().backoff_failures.get(&b_id).copied().unwrap_or(0),
        0
    );
    a.clear();
    b.clear();
}

/// Trust gossip arrives constantly and carries names. A name is not authority
/// — the struct says so — so learning one must not cost the mesh.
#[tokio::test]
async fn a_cosmetic_change_keeps_live_connections() {
    let (a, b, _, b_id, _, b_grant) = storeless_pair().await;
    let mut renamed = b_grant.clone();
    renamed.session_id = ControlSessionId::new();
    renamed.peers[0].display_name = Some("Steven's laptop".to_owned());
    b.replace_authorization(renamed);
    assert!(
        stayed_connected(&a, b_id, Duration::from_secs(2)).await,
        "learning a display name closed a live connection"
    );
    a.clear();
    b.clear();
}

/// The other side of the guarantee: a grant that really moved still fences and
/// reconnects everything, exactly as before.
#[tokio::test]
async fn a_changed_grant_still_replaces_wholesale() {
    let (a, b, _, b_id, _, b_grant) = storeless_pair().await;
    let mut widened = b_grant.clone();
    widened.peers[0].workspaces.push(WorkspaceId::new());
    b.replace_authorization(widened);
    // Observed from A, whose connection B closes. Thirty seconds is a deadline
    // for a fencing that normally takes milliseconds — it costs nothing when
    // the runtime is right and is the difference between a green suite and a
    // coin flip when the machine is busy.
    assert!(
        disconnected_within(&a, b_id, Duration::from_secs(30)).await,
        "a changed grant left the old connections up"
    );
    a.clear();
    b.clear();
}

/// The agent also re-issues a `ConnectPeer` per peer per session. Dialling a
/// device that is already connected bumped the intent generation, which made
/// the live dial stale — one `stale connection attempt` per peer per launch.
#[tokio::test]
async fn connect_peer_on_a_live_connection_does_not_redial() {
    let (a, b, _, b_id, _, _) = storeless_pair().await;
    a.connect_peer(b_id);
    assert!(
        stayed_connected(&a, b_id, Duration::from_secs(2)).await,
        "connecting an already-connected peer dropped the connection"
    );
    a.clear();
    b.clear();
}

/// **The maintainers' phone: actively syncing with the desktop, shown as offline.**
///
/// The desktop dialed the phone, so the phone's only connection is inbound —
/// live, direct, and syncing. Meanwhile the phone's own outbound dial to the
/// desktop keeps failing (no route back: firewall, no discovery record), and
/// the retry loop repainted the peer `Resolving` → `Connecting` →
/// `BackingOff` right over the `Connected` the inbound establishment wrote —
/// while a dial that did complete lost the crossed-connection tiebreak, was
/// discarded, and left `Connecting` standing forever. The status map must
/// follow the connections map: a device with a live connection reads
/// `Connected`, whatever the dialer is doing.
///
/// Ids are ordered so the higher-id side (B, the phone of this story) keeps
/// the inbound connection under [`choose_connection`], the leg where the
/// repaint had nothing to correct it.
#[tokio::test]
async fn an_inbound_connection_survives_the_outbound_retry_loop() {
    let a_endpoint = ReplicationEndpointSecret::generate();
    let b_endpoint = ReplicationEndpointSecret::generate();
    let (first, second) = (DeviceId::new(), DeviceId::new());
    let (a_id, b_id) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    let relationship = RelationshipId::new();
    let workspace = WorkspaceId::new();
    let a = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    let b = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    a.provision_endpoint(a_endpoint.secret_bytes());
    b.provision_endpoint(b_endpoint.secret_bytes());
    wait_until(|| a.endpoint_addr().is_some() && b.endpoint_addr().is_some()).await;
    let grant = |local_device, device_id, endpoint_id| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id,
            endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    };
    a.replace_authorization(grant(a_id, b_id, b_endpoint.endpoint_id()));
    b.replace_authorization(grant(b_id, a_id, a_endpoint.endpoint_id()));

    // B dials a peer it has no route to — no exact address, no discovery in
    // an isolated config — and rests in the failure/retry loop.
    b.connect_peer(a_id);
    wait_until(|| {
        matches!(
            b.status().peers.get(&a_id),
            Some(PeerConnectionState::BackingOff | PeerConnectionState::Failed { .. })
        )
    })
    .await;

    // Only now does A dial B: the connection lands inbound on B, which must
    // read Connected...
    a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
    wait_until(|| {
        matches!(
            b.status().peers.get(&a_id),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;

    // ...and stay Connected across B's pending retries (the first delays are
    // well under this window), which before the fix repainted it.
    assert!(
        stayed_connected(&b, a_id, Duration::from_secs(4)).await,
        "the outbound retry loop repainted a live inbound connection"
    );
    a.clear();
    b.clear();
}

/// A release is a clock, not a teardown — and a retain inside the window is
/// what an agent reconnect does.
#[tokio::test]
async fn a_released_runtime_clears_after_grace_unless_retained() {
    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    let bound = runtime.status().endpoint_id;

    // Reclaimed in time: the endpoint is still the one that was bound, well
    // past the deadline the release set.
    runtime.release(Duration::from_millis(300));
    runtime.retain();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        runtime.status().endpoint_id,
        bound,
        "a reclaimed runtime was cleared anyway"
    );

    // Nothing reclaims it, so the grant does die — volatile, just not
    // instantly.
    runtime.release(Duration::from_millis(300));
    wait_until(|| runtime.status().endpoint_id.is_none()).await;
    runtime.clear();
}

fn snapshot(peer: DeviceId, endpoint_id: ReplicationEndpointId) -> AuthorizationSnapshot {
    AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: DeviceId::new(),
        peers: vec![AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: peer,
            endpoint_id,
            all_data: false,
            authorized_profiles: Vec::new(),
            workspaces: vec![WorkspaceId::new()],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was not reached");
}

async fn wait_for_hydration_state(
    mut state: watch::Receiver<BlobHydrationState>,
    predicate: impl Fn(&BlobHydrationState) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate(&state.borrow_and_update()) {
                return;
            }
            state
                .changed()
                .await
                .expect("hydration state sender closed");
        }
    })
    .await
    .expect("expected hydration state was not observed");
}

/// **The relay is a route, not a hint.** A peer reachable only through a
/// custom relay (`RelayProviderConfig::CustomUrls`, the shape the Koru
/// relays will use) connects, and the recorded path says so:
/// `PathKind::Relay`. The peer endpoint is built with its IP transports
/// cleared, so nothing — not even same-host holepunching — can quietly turn
/// this into a direct connection; the relay forwarding real traffic is the
/// only way the connection can exist.
#[tokio::test]
async fn a_relay_only_peer_connects_with_path_kind_relay() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();

    // The peer: a bare iroh endpoint speaking the replication ALPN, with the
    // relay as its only transport. It accepts and holds connections open.
    let peer_secret = iroh::SecretKey::generate();
    let peer_endpoint_id = ReplicationEndpointId::from_bytes(*peer_secret.public().as_bytes());
    let peer_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(peer_secret)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            relay_url.clone(),
        )))
        .clear_ip_transports()
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("relay-only peer endpoint binds");
    // Registered with the relay, or the dial has nowhere to be forwarded to.
    peer_endpoint.online().await;
    let peer_addr = peer_endpoint.addr();
    let accept_endpoint = peer_endpoint.clone();
    let accept_task = tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            if let Ok(connection) = incoming.await {
                tokio::spawn(async move {
                    connection.closed().await;
                });
            }
        }
    });

    let secret = ReplicationEndpointSecret::generate();
    let local_device = DeviceId::new();
    let peer_device = DeviceId::new();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.endpoint_addr().is_some()).await;
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: peer_device,
            endpoint_id: peer_endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![WorkspaceId::new()],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });

    runtime.connect_peer_at(peer_device, peer_addr);
    wait_until(|| {
        matches!(
            runtime.status().peers.get(&peer_device),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    assert_eq!(
        runtime.status().peers.get(&peer_device),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Relay
        })
    );

    runtime.clear();
    accept_task.abort();
    peer_endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

/// **A dead relay has a name, and it is not a guess at a timeout.** The
/// runtime is pointed at a relay that existed and died — a test-support
/// server started and shut down, so the URL is real and the port refuses.
/// A dial to an authorized peer fails, and the surfaced state carries the
/// stable `relay_unreachable` code. The evidence is the endpoint's own
/// relay-connection state (`Endpoint::home_relay_status` holding no live
/// relay connection while relays are configured), never the dial error:
/// the dial here fails with a missing-address error, not a timeout, and
/// still reads as relay-unreachable because the relay state says so.
#[tokio::test]
async fn a_dead_relay_surfaces_the_stable_relay_unreachable_code() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().to_string();
    relay.shutdown().await.expect("relay shuts down");

    let secret = ReplicationEndpointSecret::generate();
    let peer_secret = ReplicationEndpointSecret::generate();
    let peer_device = DeviceId::new();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    runtime.replace_authorization(snapshot(peer_device, peer_secret.endpoint_id()));

    // Identity only: with lookup disabled and the relay dead, no route to
    // the peer exists. The retry loop keeps running underneath; between
    // attempts the peer rests on the diagnostic code.
    runtime.connect_peer(peer_device);
    wait_until(|| {
        matches!(
            runtime.status().peers.get(&peer_device),
            Some(PeerConnectionState::Failed {
                code: "relay_unreachable"
            })
        )
    })
    .await;
    runtime.clear();
}

/// The pure mapping rows behind the relay status: connected wins, a
/// recorded error is disconnected, silence is an attempt in flight — and
/// the configuration baseline names the first configured URL's host for a
/// relay that was never reached, because iroh never selects an unreachable
/// relay as home and the watcher list simply stays empty
/// (`guide/contributing/deps/IROH_RELAY.md`).
#[test]
fn relay_status_readings_map_to_exactly_one_state() {
    assert_eq!(
        relay_state_reading("h".into(), true, false),
        RelayConnectionStatus::Connected { host: "h".into() }
    );
    assert_eq!(
        relay_state_reading("h".into(), true, true),
        RelayConnectionStatus::Connected { host: "h".into() }
    );
    assert_eq!(
        relay_state_reading("h".into(), false, true),
        RelayConnectionStatus::Disconnected { host: "h".into() }
    );
    assert_eq!(
        relay_state_reading("h".into(), false, false),
        RelayConnectionStatus::Connecting { host: "h".into() }
    );
    assert_eq!(
        relay_provider_baseline(&RelayProviderConfig::Disabled),
        RelayConnectionStatus::Off
    );
    assert_eq!(
        relay_provider_baseline(&RelayProviderConfig::CustomUrls(vec![
            "https://use1-1.relay.koru.do".into(),
            "https://euw1-1.relay.koru.do".into(),
        ])),
        RelayConnectionStatus::Disconnected {
            host: "use1-1.relay.koru.do".into()
        }
    );
}

/// The relay lifecycle log lines, pinned: one per state, naming the relay
/// host wherever there is one to name. [`set_relay_status`] emits exactly
/// these under the `koru_sync::relay` target, and only when the stored
/// reading actually changes — transitions are news; watcher wakes and
/// retries are not. (Desktop logs and mobile logcat both inherit the
/// target, so the same line serves both.)
#[test]
fn relay_transitions_log_the_state_and_the_host() {
    assert_eq!(
        relay_transition_news(&RelayConnectionStatus::Off),
        ("relay off", None)
    );
    assert_eq!(
        relay_transition_news(&RelayConnectionStatus::Connecting { host: "h".into() }),
        ("relay connecting", Some("h"))
    );
    assert_eq!(
        relay_transition_news(&RelayConnectionStatus::Connected { host: "h".into() }),
        ("relay connected", Some("h"))
    );
    assert_eq!(
        relay_transition_news(&RelayConnectionStatus::Disconnected { host: "h".into() }),
        ("relay disconnected", Some("h"))
    );
}

/// [`set_relay_status`] emits exactly one `info` line per actual state
/// change under the `koru_sync::relay` target — a repeated reading logs
/// nothing, because the watcher wakes more often than the state changes
/// and transitions are the news. Captured with a thread-local subscriber;
/// `set_relay_status` is synchronous, so the events land on this thread.
#[test]
fn set_relay_status_logs_once_per_transition_and_never_on_repeat() {
    struct Capture(Arc<Mutex<Vec<String>>>);
    struct Line(String);
    impl tracing::field::Visit for Line {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.target() == "koru_sync::relay"
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = Line(String::new());
            event.record(&mut line);
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(line.0.trim().to_owned());
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    let lines = Arc::new(Mutex::new(Vec::new()));
    let status = RwLock::new(NetworkStatus::default());
    tracing::subscriber::with_default(Capture(Arc::clone(&lines)), || {
        // The default reading is Off: repeating it is not a transition.
        set_relay_status(&status, RelayConnectionStatus::Off);
        set_relay_status(
            &status,
            RelayConnectionStatus::Connecting { host: "h".into() },
        );
        // The watcher re-reporting an unchanged state logs nothing.
        set_relay_status(
            &status,
            RelayConnectionStatus::Connecting { host: "h".into() },
        );
        set_relay_status(
            &status,
            RelayConnectionStatus::Connected { host: "h".into() },
        );
        set_relay_status(&status, RelayConnectionStatus::Off);
    });
    assert_eq!(
        *lines.lock().unwrap_or_else(|error| error.into_inner()),
        vec![
            "message=relay connecting host=h".to_owned(),
            "message=relay connected host=h".to_owned(),
            "message=relay off".to_owned(),
        ]
    );
}

/// **The status names the relay it reached.** A runtime pointed at a live
/// in-memory relay reports `Connected` with that relay's host, driven by
/// the endpoint's own home-relay watcher — no peer, no dial, no timer, and
/// never `Endpoint::online` (which hangs forever on a dead relay).
#[tokio::test]
async fn the_relay_status_reads_connected_with_the_relays_host() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let host = relay_url
        .host_str()
        .expect("the test relay URL has a host")
        .to_owned();

    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_within(Duration::from_secs(10), || {
        runtime.status().relay == RelayConnectionStatus::Connected { host: host.clone() }
    })
    .await;

    runtime.clear();
    relay.shutdown().await.expect("relay shuts down");
}

/// **A relay that dies mid-session reads `Disconnected`.** The home relay
/// was selected and connected, so its death is the one case iroh reports as
/// `Disconnected { last_error }` — and the status follows without any
/// caller polling anything.
#[tokio::test]
async fn a_relay_that_dies_mid_session_reads_disconnected() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let host = relay_url
        .host_str()
        .expect("the test relay URL has a host")
        .to_owned();

    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_within(Duration::from_secs(10), || {
        runtime.status().relay == RelayConnectionStatus::Connected { host: host.clone() }
    })
    .await;

    relay.shutdown().await.expect("relay shuts down");
    // Reconnect attempts pass through `Connecting`; the settled reading
    // between failed attempts is `Disconnected`, under the same host.
    wait_within(Duration::from_secs(30), || {
        runtime.status().relay == RelayConnectionStatus::Disconnected { host: host.clone() }
    })
    .await;

    runtime.clear();
}

/// **Relays disabled read `Off`** — before provisioning, and with a bound
/// endpoint alike. Off is configuration, not a connection failure.
#[tokio::test]
async fn disabled_relays_read_off() {
    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::Disabled,
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    assert_eq!(runtime.status().relay, RelayConnectionStatus::Off);
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    assert_eq!(runtime.status().relay, RelayConnectionStatus::Off);
    runtime.clear();
}

/// **The live switch moves the status — and the relay route works — from a
/// start with relays off.** An endpoint bound under `Disabled` has no relay
/// transport at all, so this is the one switch that still rebinds
/// (`guide/contributing/deps/IROH_RELAY.md`): the status follows the new endpoint's
/// watcher to `Connected` with the relay's host, the identity is unchanged,
/// and a peer reachable only through the relay connects.
#[tokio::test]
async fn switching_relays_on_live_moves_the_status_from_off_to_connected() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let host = relay_url
        .host_str()
        .expect("the test relay URL has a host")
        .to_owned();
    let (peer_endpoint, peer_addr, accept_task) = spawn_relay_only_echo_peer(&relay_url).await;
    let peer_endpoint_id = ReplicationEndpointId::from_bytes(*peer_endpoint.id().as_bytes());
    let peer_device = DeviceId::new();

    let secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::Disabled,
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    let identity = runtime
        .status()
        .endpoint_id
        .expect("a provisioned runtime reports its endpoint id");
    assert_eq!(runtime.status().relay, RelayConnectionStatus::Off);

    runtime.replace_relay_provider(RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]));
    wait_within(Duration::from_secs(15), || {
        runtime.status().relay == RelayConnectionStatus::Connected { host: host.clone() }
    })
    .await;
    assert_eq!(runtime.status().endpoint_id, Some(identity));

    // The rebound endpoint carries real relay traffic: a relay-only peer
    // connects, with the path saying so.
    runtime.replace_authorization(snapshot(peer_device, peer_endpoint_id));
    runtime.connect_peer_at(peer_device, peer_addr);
    wait_within(Duration::from_secs(10), || {
        runtime.status().peers.get(&peer_device)
            == Some(&PeerConnectionState::Connected {
                path: PathKind::Relay,
            })
    })
    .await;

    runtime.clear();
    accept_task.abort();
    peer_endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

/// **Authorization denial is not a relay failure.** Under the very same
/// dead-relay configuration as
/// [`a_dead_relay_surfaces_the_stable_relay_unreachable_code`], a peer that
/// is absent from the authorization snapshot surfaces `Unauthorized` — the
/// two causes are distinguished by what actually happened, not inferred
/// from the shared network conditions.
#[tokio::test]
async fn an_unauthorized_peer_surfaces_authorization_denial_not_relay_failure() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().to_string();
    relay.shutdown().await.expect("relay shuts down");

    let secret = ReplicationEndpointSecret::generate();
    let authorized_secret = ReplicationEndpointSecret::generate();
    let authorized_device = DeviceId::new();
    let stranger_device = DeviceId::new();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.status().endpoint_id.is_some()).await;
    runtime.replace_authorization(snapshot(authorized_device, authorized_secret.endpoint_id()));

    runtime.connect_peer(stranger_device);
    wait_until(|| {
        runtime.status().peers.get(&stranger_device) == Some(&PeerConnectionState::Unauthorized)
    })
    .await;
    // The surfaced states of the two armed causes differ.
    assert_ne!(
        runtime.status().peers.get(&stranger_device),
        Some(&PeerConnectionState::Failed {
            code: "relay_unreachable"
        })
    );
    runtime.clear();
}

/// A reconnect after a live relay switch crosses a real network handshake,
/// so these waits are longer than [`wait_until`]'s second.
async fn wait_within(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        if Instant::now() >= deadline {
            panic!("condition was not reached");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **The maintainers' defect, as a test: switching relays off must not touch a
/// direct connection** (decision №6 — relay-off disables relays only;
/// direct/LAN connectivity remains). Two runtimes — the desktop and the
/// phone — both configured with the relay, connected over a DIRECT
/// localhost path. Relays go off on one. The direct connection SURVIVES:
/// through a continuous bounded observation window both sides read
/// `Connected { path: Direct }` on every poll — no disconnect, no state
/// transition, no reconnect — and the switched runtime's endpoint keeps
/// the very socket it was bound to (a rebind would have torn every
/// connection down and changed the bound address). The relay status
/// honestly reads `Off`.
#[tokio::test]
async fn switching_relays_off_leaves_a_direct_connection_untouched() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().to_string();

    let desktop_secret = ReplicationEndpointSecret::generate();
    let phone_secret = ReplicationEndpointSecret::generate();
    let desktop_device = DeviceId::new();
    let phone_device = DeviceId::new();
    let config = || IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.clone()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    };
    let desktop = RuntimeHandle::spawn(config());
    let phone = RuntimeHandle::spawn(config());
    desktop.provision_endpoint(desktop_secret.secret_bytes());
    phone.provision_endpoint(phone_secret.secret_bytes());
    wait_until(|| desktop.endpoint_addr().is_some() && phone.endpoint_addr().is_some()).await;

    let authorize =
        |local: DeviceId, peer: DeviceId, endpoint: ReplicationEndpointId| AuthorizationSnapshot {
            session_id: ControlSessionId::new(),
            sequence: 1,
            local_device: local,
            peers: vec![AuthorizedPeer {
                relationship_id: RelationshipId::new(),
                device_id: peer,
                endpoint_id: endpoint,
                all_data: true,
                authorized_profiles: Vec::new(),
                workspaces: vec![WorkspaceId::new()],
                via: None,
                display_name: None,
                build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
            }],
            local_build: None,
        };
    desktop.replace_authorization(authorize(
        desktop_device,
        phone_device,
        phone_secret.endpoint_id(),
    ));
    phone.replace_authorization(authorize(
        phone_device,
        desktop_device,
        desktop_secret.endpoint_id(),
    ));

    // Dial the phone at its direct localhost addresses only, so the live
    // path is IP from the first instant.
    let phone_addr = phone.endpoint_addr().expect("phone endpoint bound");
    let direct_addr = EndpointAddr::from_parts(
        phone_addr.id,
        phone_addr.addrs.iter().filter(|a| a.is_ip()).cloned(),
    );
    assert!(
        !direct_addr.is_empty(),
        "the phone advertises direct addresses"
    );
    desktop.connect_peer_at(phone_device, direct_addr);
    wait_within(Duration::from_secs(10), || {
        desktop.status().peers.get(&phone_device)
            == Some(&PeerConnectionState::Connected {
                path: PathKind::Direct,
            })
            && matches!(
                phone.status().peers.get(&desktop_device),
                Some(PeerConnectionState::Connected { .. })
            )
    })
    .await;
    let bound = desktop.endpoint_addr().expect("desktop endpoint bound");

    // The toggle. Relays off on the desktop.
    desktop.replace_relay_provider(RelayProviderConfig::Disabled);

    // A continuous bounded observation window: the direct connection must
    // survive on both sides, on every poll — one non-Connected reading is
    // the regression.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert_eq!(
            desktop.status().peers.get(&phone_device),
            Some(&PeerConnectionState::Connected {
                path: PathKind::Direct
            }),
            "the desktop's direct connection must survive relays going off"
        );
        assert!(
            matches!(
                phone.status().peers.get(&desktop_device),
                Some(PeerConnectionState::Connected { .. })
            ),
            "the phone's side of the direct connection must survive too"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // No rebind happened: the endpoint kept its bound socket addresses.
    assert_eq!(
        desktop.endpoint_addr().as_ref(),
        Some(&bound),
        "relays-off must not rebind the endpoint"
    );
    // And the switch was not silently ignored: the status reads Off.
    assert_eq!(desktop.status().relay, RelayConnectionStatus::Off);

    desktop.clear();
    phone.clear();
    relay.shutdown().await.expect("relay shuts down");
}

/// A bare relay-only iroh endpoint speaking the replication ALPN whose
/// accept loop echoes one bidirectional stream per connection — enough to
/// prove, end to end, that a relay is actually forwarding real traffic.
async fn spawn_relay_only_echo_peer(
    relay_url: &iroh::RelayUrl,
) -> (Endpoint, EndpointAddr, tokio::task::JoinHandle<()>) {
    let peer_secret = iroh::SecretKey::generate();
    let peer_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(peer_secret)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            relay_url.clone(),
        )))
        .clear_ip_transports()
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("relay-only peer endpoint binds");
    peer_endpoint.online().await;
    let peer_addr = peer_endpoint.addr();
    let accept_endpoint = peer_endpoint.clone();
    let accept_task = tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            if let Ok(connection) = incoming.await {
                tokio::spawn(async move {
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        if let Ok(bytes) = recv.read_to_end(64).await {
                            let _ = send.write_all(&bytes).await;
                            let _ = send.finish();
                        }
                    }
                });
            }
        }
    });
    (peer_endpoint, peer_addr, accept_task)
}

/// One echo round trip over `connection`; panics with `context` if the
/// connection cannot carry it.
async fn echo_roundtrip(connection: &iroh::endpoint::Connection, context: &str) {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .unwrap_or_else(|error| panic!("{context}: open_bi failed: {error:?}"));
    send.write_all(b"ping")
        .await
        .unwrap_or_else(|error| panic!("{context}: write failed: {error:?}"));
    send.finish()
        .unwrap_or_else(|error| panic!("{context}: finish failed: {error:?}"));
    let echoed = tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(64))
        .await
        .unwrap_or_else(|_| panic!("{context}: echo timed out"))
        .unwrap_or_else(|error| panic!("{context}: read failed: {error:?}"));
    assert_eq!(echoed, b"ping", "{context}: echo did not round-trip");
}

/// **Verified by experiment: `Endpoint::remove_relay` does not touch an
/// established relay-routed connection.** iroh 1.1.0's map edit removes the
/// relay from the configuration map only; a connection already forwarding
/// through that relay keeps forwarding (the echo still round-trips seconds
/// later), until *we* close it — which is exactly why the runtime's
/// relays-off switch closes relay-only connections explicitly instead of
/// trusting the map edit (`guide/contributing/deps/IROH_RELAY.md`).
#[tokio::test]
async fn remove_relay_leaves_an_established_relay_connection_forwarding() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let (peer_endpoint, peer_addr, accept_task) = spawn_relay_only_echo_peer(&relay_url).await;

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            relay_url.clone(),
        )))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("dialer endpoint binds");
    let connection = endpoint
        .connect(peer_addr, REPLICATION_ALPN_V1)
        .await
        .expect("relay-routed connection establishes");
    echo_roundtrip(&connection, "before remove_relay").await;

    // The URL used at bind identifies the map entry: removal returns it.
    let removed = endpoint.remove_relay(&relay_url).await;
    assert!(
        removed.is_some(),
        "remove_relay identified the configured URL and removed its entry"
    );

    // A bounded observation window: the connection survives the map edit
    // and still forwards real traffic through the relay.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        connection.close_reason().is_none(),
        "the relay-routed connection survived remove_relay"
    );
    echo_roundtrip(&connection, "after remove_relay").await;

    // Off therefore requires an explicit close — and that close works.
    connection.close(0u32.into(), b"relays disabled");
    wait_within(Duration::from_secs(5), || {
        connection.close_reason().is_some()
    })
    .await;

    accept_task.abort();
    peer_endpoint.close().await;
    endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

/// **Verified by experiment: `Endpoint::remove_relay` does not stop NEW
/// relay dials either.** After the sole configured relay is removed from
/// the map, a fresh dial to an addr carrying that relay URL still connects
/// straight through the relay: iroh 1.1.0's relay transport routes
/// datagrams to whatever relay URL the remote's addressing names, spinning
/// up a relay connection on demand without consulting the map (the map
/// feeds net-report probing and auth tokens, not datagram routing —
/// `socket/transports/relay/actor.rs`, `active_relay_handle_for_endpoint`).
/// This is why the runtime's relays-off switch cannot rely on the map edit
/// alone: it also strips relay addresses from its own dials and refuses
/// relay-only connections at establishment while relays are off.
#[tokio::test]
async fn remove_relay_does_not_gate_new_dials_so_the_runtime_must() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let (peer_endpoint, peer_addr, accept_task) = spawn_relay_only_echo_peer(&relay_url).await;

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            relay_url.clone(),
        )))
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("dialer endpoint binds");
    assert!(
        endpoint.remove_relay(&relay_url).await.is_some(),
        "remove_relay identified the configured URL and removed its entry"
    );

    // The dial target names the relay URL (a peer's addressing always may).
    // If this ever starts failing, iroh has begun gating dials on the map —
    // revisit `guide/contributing/deps/IROH_RELAY.md` and the relays-off switch design.
    let connection = tokio::time::timeout(
        Duration::from_secs(10),
        endpoint.connect(peer_addr, REPLICATION_ALPN_V1),
    )
    .await
    .expect("dial resolved within its bound")
    .expect("a dial naming a removed relay URL still connects through it");
    echo_roundtrip(&connection, "dial after remove_relay").await;
    assert!(
        connection.paths().iter().any(|path| path.is_relay()),
        "the post-removal connection is relay-routed"
    );

    connection.close(0u32.into(), b"probe done");
    accept_task.abort();
    peer_endpoint.close().await;
    endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

/// Decision №8's fixed precedence, every row as a table. The compiled
/// channel list is a parameter — this resolver never knows a hostname.
#[test]
fn relay_precedence_covers_every_row_of_decision_8() {
    let over = vec!["https://operator.example".to_string()];
    let compiled = RelayProviderConfig::CustomUrls(vec!["https://channel.example".to_string()]);
    let none: &[String] = &[];
    #[rustfmt::skip]
    let table: &[(bool, bool, Option<&[String]>, &RelayProviderConfig, RelayProviderConfig, &str)] = &[
        // Row 1: Relay Off disables every relay, whatever else is set.
        (false, false, None,         &RelayProviderConfig::DefaultNetwork, RelayProviderConfig::Disabled, "off beats the default network"),
        (false, false, Some(&over), &compiled,                             RelayProviderConfig::Disabled, "off beats the override"),
        (false, true,  Some(&over), &compiled,                             RelayProviderConfig::Disabled, "off beats everything under isolation too"),
        // Row 2: Relay On uses the override when one is set — including in
        // an isolated configuration, where the deliberate injection is how a
        // test points at a local relay without seeing a public one.
        (true,  false, Some(&over), &RelayProviderConfig::DefaultNetwork,  RelayProviderConfig::CustomUrls(over.clone()), "on takes the override"),
        (true,  false, Some(&over), &compiled,                             RelayProviderConfig::CustomUrls(over.clone()), "the override beats the channel list"),
        (true,  true,  Some(&over), &compiled,                             RelayProviderConfig::CustomUrls(over.clone()), "an injected override stays applied under isolation"),
        // Row 3: otherwise the compiled channel list, whatever it is.
        (true,  false, None,        &compiled,                             compiled.clone(),                              "on falls through to the channel list"),
        (true,  false, None,        &RelayProviderConfig::DefaultNetwork,  RelayProviderConfig::DefaultNetwork,           "a DefaultNetwork channel argument passes through unchanged"),
        (true,  false, Some(none),  &compiled,                             compiled.clone(),                              "an empty override is no override"),
        // Row 4: isolated configuration never reaches the compiled/public list.
        (true,  true,  None,        &compiled,                             RelayProviderConfig::Disabled,                 "isolation never falls through to a public relay"),
        (true,  true,  Some(none),  &RelayProviderConfig::DefaultNetwork,  RelayProviderConfig::Disabled,                 "isolation with an empty override stays isolated"),
    ];
    for (enabled, isolated, override_urls, channel, expected, why) in table {
        assert_eq!(
            &resolve_relay_provider(*enabled, *isolated, *override_urls, channel),
            expected,
            "{why}"
        );
    }
}

/// Decision №5's compiled lists, pinned to the exact hostnames clients ship
/// with: internal builds dial `dev1`, production builds `use1-1` — aliases of
/// the same service, and never iroh's default network (decision №2).
#[test]
fn channel_relays_compile_the_koru_only_list_per_build_channel() {
    assert_eq!(
        channel_relays(RelayChannelKind::Internal),
        RelayProviderConfig::CustomUrls(vec!["https://dev1.relay.koru.do".to_string()])
    );
    assert_eq!(
        channel_relays(RelayChannelKind::Production),
        RelayProviderConfig::CustomUrls(vec!["https://use1-1.relay.koru.do".to_string()])
    );
    assert_eq!(
        RelayChannelKind::for_internal_build(true),
        RelayChannelKind::Internal
    );
    assert_eq!(
        RelayChannelKind::for_internal_build(false),
        RelayChannelKind::Production
    );
}

/// The whole-configuration helper every host consumes (Plan 5.2), decision
/// №8's rows × both build channels. Beyond the relay provider it pins the
/// two boundaries the flip must not move: address lookup stays
/// [`AddressLookupConfig::DefaultNetwork`] everywhere except isolated mode
/// (decision №2 — n0 keeps address lookup; only relay transport is
/// Koru-only), and Relay-Off is not isolation — lookup survives it.
#[test]
fn resolved_network_config_covers_every_precedence_row_per_channel() {
    let over = vec!["https://operator.example".to_string()];
    let none: &[String] = &[];
    for kind in [RelayChannelKind::Internal, RelayChannelKind::Production] {
        let compiled = channel_relays(kind);
        #[rustfmt::skip]
        let table: &[(bool, bool, Option<&[String]>, RelayProviderConfig, AddressLookupConfig, &str)] = &[
            // Row 1: Relay Off disables every relay — and only relays: the
            // n0 address lookup is untouched outside isolation.
            (false, false, None,        RelayProviderConfig::Disabled,                 AddressLookupConfig::DefaultNetwork, "off disables relays, not lookup"),
            (false, false, Some(&over), RelayProviderConfig::Disabled,                 AddressLookupConfig::DefaultNetwork, "off beats the override"),
            (false, true,  Some(&over), RelayProviderConfig::Disabled,                 AddressLookupConfig::Disabled,       "off beats everything under isolation too"),
            // Row 2: Relay On uses the override when one is set — including
            // in an isolated configuration, whose lookup stays disabled.
            (true,  false, Some(&over), RelayProviderConfig::CustomUrls(over.clone()), AddressLookupConfig::DefaultNetwork, "the override beats the channel list"),
            (true,  true,  Some(&over), RelayProviderConfig::CustomUrls(over.clone()), AddressLookupConfig::Disabled,       "an injected override stays applied under isolation"),
            // Row 3: otherwise the compiled Koru list for this channel.
            (true,  false, None,        compiled.clone(),                              AddressLookupConfig::DefaultNetwork, "the channel default is the compiled Koru list"),
            (true,  false, Some(none),  compiled.clone(),                              AddressLookupConfig::DefaultNetwork, "an empty override is no override"),
            // Row 4: isolated configuration never reaches a public relay.
            (true,  true,  None,        RelayProviderConfig::Disabled,                 AddressLookupConfig::Disabled,       "isolation never falls through to the compiled list"),
            (true,  true,  Some(none),  RelayProviderConfig::Disabled,                 AddressLookupConfig::Disabled,       "isolation with an empty override stays isolated"),
        ];
        for (enabled, isolated, override_urls, relay, lookup, why) in table {
            let config = resolve_network_config(*enabled, kind, *isolated, *override_urls);
            assert_eq!(&config.relay_provider, relay, "{kind:?}: {why}");
            assert_eq!(&config.address_lookup, lookup, "{kind:?}: lookup — {why}");
        }
    }
}

/// The env-reading entry point honors the Off row whatever the machine's
/// `KORU_IROH_RELAYS`/`KORU_IROH_ISOLATED` say — which is also what lets this
/// suite assert it without owning the process environment; the remaining
/// rows are the pure table above.
#[test]
fn host_network_config_disables_relays_when_off_whatever_the_environment_says() {
    for kind in [RelayChannelKind::Internal, RelayChannelKind::Production] {
        assert_eq!(
            host_network_config(false, kind).relay_provider,
            RelayProviderConfig::Disabled
        );
        assert_eq!(
            host_relay_provider(false, kind),
            RelayProviderConfig::Disabled
        );
    }
}

/// Decision №6's live switch, through the in-memory relay — with both
/// kinds of peer on the same runtime. Switching relays off closes exactly
/// the relay-only connection: that peer waits in the ordinary reconnect
/// states — never a failed or unauthorized one, and never
/// `Failed { relay_unreachable }`, because relays are deliberately off and
/// an owner who turned them off has been denied nothing. The direct peer's
/// connection is not touched, the endpoint keeps its bound socket (no
/// rebind), the identity never changes, and the relay status reads `Off`.
/// Switching back on reconnects the relay-only peer through the relay,
/// moves the status back to `Connected` — and the direct peer never
/// flapped: its endpoint accepted exactly one connection across the whole
/// on → off → on session.
#[tokio::test]
async fn switching_relays_off_and_on_keeps_identity_and_restores_the_peer() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let host = relay_url
        .host_str()
        .expect("the test relay URL has a host")
        .to_owned();

    // A relay-only peer: IP transports cleared, so the relay route is the
    // only way any connection to it can exist.
    let (peer_endpoint, peer_addr, accept_task) = spawn_relay_only_echo_peer(&relay_url).await;
    let peer_endpoint_id = ReplicationEndpointId::from_bytes(*peer_endpoint.id().as_bytes());

    // A directly reachable peer on localhost, counting every connection its
    // endpoint accepts: one at the start, and still one at the end, is the
    // proof it never flapped.
    let direct_secret = iroh::SecretKey::generate();
    let direct_endpoint_id = ReplicationEndpointId::from_bytes(*direct_secret.public().as_bytes());
    let direct_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(direct_secret)
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("direct peer endpoint binds");
    let direct_addr = direct_endpoint.addr();
    let accepted = Arc::new(AtomicUsize::new(0));
    let direct_accept_task = {
        let endpoint = direct_endpoint.clone();
        let accepted = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                if let Ok(connection) = incoming.await {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        connection.closed().await;
                    });
                }
            }
        })
    };

    let secret = ReplicationEndpointSecret::generate();
    let local_device = DeviceId::new();
    let peer_device = DeviceId::new();
    let direct_device = DeviceId::new();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.endpoint_addr().is_some()).await;
    let authorized = |device_id, endpoint_id| AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id,
        endpoint_id,
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![WorkspaceId::new()],
        via: None,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device,
        peers: vec![
            authorized(peer_device, peer_endpoint_id),
            authorized(direct_device, direct_endpoint_id),
        ],
        local_build: None,
    });
    runtime.connect_peer_at(peer_device, peer_addr);
    runtime.connect_peer_at(direct_device, direct_addr);
    wait_within(Duration::from_secs(10), || {
        runtime.status().peers.get(&peer_device)
            == Some(&PeerConnectionState::Connected {
                path: PathKind::Relay,
            })
            && runtime.status().peers.get(&direct_device)
                == Some(&PeerConnectionState::Connected {
                    path: PathKind::Direct,
                })
    })
    .await;
    let identity = runtime
        .status()
        .endpoint_id
        .expect("a provisioned runtime reports its endpoint id");
    let bound = runtime.endpoint_addr().expect("bound");

    // Off: the relay-only connection closes — the map edit alone would
    // leave it forwarding (verified in
    // `remove_relay_leaves_an_established_relay_connection_forwarding`),
    // so the runtime closes it explicitly. The peer has no other route and
    // waits in ordinary reconnect states — an appropriate non-connected
    // state, never one that implies denial.
    runtime.replace_relay_provider(RelayProviderConfig::Disabled);
    wait_within(Duration::from_secs(10), || {
        !matches!(
            runtime.status().peers.get(&peer_device),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    assert!(
        matches!(
            runtime.status().peers.get(&peer_device),
            Some(
                PeerConnectionState::Resolving
                    | PeerConnectionState::Connecting
                    | PeerConnectionState::BackingOff
                    | PeerConnectionState::Disconnected
            )
        ),
        "a relay-only peer with relays off waits; it is not failed or unauthorized: {:?}",
        runtime.status().peers.get(&peer_device)
    );
    assert_ne!(
        runtime.status().peers.get(&peer_device),
        Some(&PeerConnectionState::Failed {
            code: "relay_unreachable"
        }),
        "relays deliberately off must never read as a relay failure"
    );
    // The direct peer was not touched, and nothing rebound: same identity,
    // same bound socket, relay status honestly `Off`.
    assert_eq!(
        runtime.status().peers.get(&direct_device),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Direct
        })
    );
    assert_eq!(runtime.status().endpoint_id, Some(identity));
    assert_eq!(runtime.endpoint_addr().as_ref(), Some(&bound));
    assert_eq!(runtime.status().relay, RelayConnectionStatus::Off);

    // On again: the relays return into the same endpoint, the relay-only
    // peer reconnects through the relay, the status follows — and the
    // identity still has not changed.
    runtime.replace_relay_provider(RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]));
    wait_within(Duration::from_secs(15), || {
        runtime.status().peers.get(&peer_device)
            == Some(&PeerConnectionState::Connected {
                path: PathKind::Relay,
            })
    })
    .await;
    wait_within(Duration::from_secs(15), || {
        runtime.status().relay == RelayConnectionStatus::Connected { host: host.clone() }
    })
    .await;
    assert_eq!(runtime.status().endpoint_id, Some(identity));
    // The direct peer never flapped: still connected, and its endpoint
    // accepted exactly the one connection from the start of the session.
    assert_eq!(
        runtime.status().peers.get(&direct_device),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Direct
        })
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the direct peer's endpoint saw exactly one connection across on → off → on"
    );

    runtime.clear();
    accept_task.abort();
    direct_accept_task.abort();
    peer_endpoint.close().await;
    direct_endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

/// The switch never touches direct/LAN semantics: replacing the provider
/// with its current value is a no-op — nothing rebinds, the connection
/// stays — and switching relays off edits the relay map in place, leaving
/// a direct-pathed peer connected throughout with the identity unchanged.
/// (The strict no-transition observation of the off switch is
/// [`switching_relays_off_leaves_a_direct_connection_untouched`], the
/// regression test for the defect where the switch tore down every
/// connection.)
#[tokio::test]
async fn switching_relays_off_keeps_a_direct_peer_connected() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();

    // A directly reachable peer on localhost, no relay of its own.
    let peer_secret = iroh::SecretKey::generate();
    let peer_endpoint_id = ReplicationEndpointId::from_bytes(*peer_secret.public().as_bytes());
    let peer_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(peer_secret)
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("direct peer endpoint binds");
    let peer_addr = peer_endpoint.addr();
    let accept_endpoint = peer_endpoint.clone();
    let accept_task = tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            if let Ok(connection) = incoming.await {
                tokio::spawn(async move {
                    connection.closed().await;
                });
            }
        }
    });

    let secret = ReplicationEndpointSecret::generate();
    let peer_device = DeviceId::new();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig {
        relay_provider: RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]),
        address_lookup: AddressLookupConfig::Disabled,
        direct_paths: DirectPathPolicy::Prefer,
    });
    runtime.provision_endpoint(secret.secret_bytes());
    wait_until(|| runtime.endpoint_addr().is_some()).await;
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: DeviceId::new(),
        peers: vec![AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: peer_device,
            endpoint_id: peer_endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![WorkspaceId::new()],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        }],
        local_build: None,
    });
    runtime.connect_peer_at(peer_device, peer_addr);
    wait_within(Duration::from_secs(10), || {
        runtime.status().peers.get(&peer_device)
            == Some(&PeerConnectionState::Connected {
                path: PathKind::Direct,
            })
    })
    .await;
    let identity = runtime
        .status()
        .endpoint_id
        .expect("a provisioned runtime reports its endpoint id");
    let bound = runtime.endpoint_addr().expect("bound");

    // Same value: nothing happens. The endpoint keeps its socket and the
    // connection is not disturbed.
    runtime.replace_relay_provider(RelayProviderConfig::CustomUrls(vec![relay_url.to_string()]));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runtime.endpoint_addr().as_ref(), Some(&bound));
    assert_eq!(
        runtime.status().peers.get(&peer_device),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Direct
        })
    );

    // Off: the relay map empties in place — the endpoint keeps its socket,
    // the direct peer keeps its connection, the identity never changes.
    runtime.replace_relay_provider(RelayProviderConfig::Disabled);
    wait_within(Duration::from_secs(10), || {
        runtime.status().relay == RelayConnectionStatus::Off
    })
    .await;
    assert_eq!(runtime.endpoint_addr().as_ref(), Some(&bound));
    assert_eq!(
        runtime.status().peers.get(&peer_device),
        Some(&PeerConnectionState::Connected {
            path: PathKind::Direct
        })
    );
    assert_eq!(runtime.status().endpoint_id, Some(identity));

    runtime.clear();
    accept_task.abort();
    peer_endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

// ------------------------------------------------ the Cloud arm at the edge

fn cloud_id(byte: u8) -> koru_device::cloud::CloudId {
    let mut bytes = [byte; 16];
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    koru_device::cloud::CloudId::from_bytes(bytes)
}

fn cloud_dataset() -> koru_device::cloud::CloudReplicaPrincipalV1 {
    koru_device::cloud::CloudReplicaPrincipalV1::Workspace {
        workspace_id: cloud_id(0x44),
        incarnation_id: cloud_id(0x55),
        logical_replica_id: cloud_id(0x66),
    }
}

fn cloud_principal() -> koru_network_edge::NetworkPrincipal {
    koru_device::cloud::CloudPrincipalV1 {
        version: 1,
        permit_id: cloud_id(0x11).as_uuid(),
        account_id: cloud_id(0x22),
        profile_id: cloud_id(0x33),
        dataset: cloud_dataset(),
        client_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        worker_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        credential_id: cloud_id(0x77),
        authorization_version: 3,
        membership_epoch: 4,
        cloud_access_epoch: 5,
        authority_workspace_id: cloud_id(0x88),
        authority_workspace_incarnation_id: cloud_id(0x99),
        control_session_id: cloud_id(0xaa).as_uuid(),
        account_generation: 2,
        session_generation: 1,
        expires_at_seconds: 1_800_000_000,
    }
    .into()
}

#[test]
fn the_cloud_content_token_is_exact_and_distinct_from_the_peer_alpn() {
    assert_eq!(cloud_content_protocol().as_str(), "koru-cloud-content-v1");
    assert_ne!(cloud_content_protocol(), replication_protocol());
    assert_eq!(replication_protocol().as_str(), "koru/replication/1");
}

/// **A Cloud session never enters the peer path**, whichever token it names.
/// With no acceptor installed it is reset rather than dropped, so the other
/// side learns at once instead of waiting on a stream nobody owns.
#[tokio::test]
async fn a_cloud_session_is_reset_when_no_cloud_acceptor_is_installed() {
    let store = Arc::new(InventoryStore::new(WorkspaceId::new(), DocumentId::new()));
    let runtime = SessionRuntimeHandle::new(store, Arc::new(PendingEgress::default()));
    let principal = cloud_principal();

    for protocol in [cloud_content_protocol(), replication_protocol()] {
        let (mut peer, accepted) = koru_network_edge::TransportSession::pair(
            principal.clone(),
            principal.clone(),
            protocol,
            4,
        );
        runtime.accept(accepted);
        assert_eq!(
            peer.receive.recv().await,
            Err(koru_network_edge::EdgeError::Reset(1))
        );
    }
}

/// With an acceptor installed, a Cloud session on the Cloud token reaches it
/// whole — and one on the peer token still does not: a permit cannot
/// authorize a peer protocol token.
#[tokio::test]
async fn a_cloud_content_session_reaches_the_installed_acceptor_and_a_peer_token_does_not() {
    let store = Arc::new(InventoryStore::new(WorkspaceId::new(), DocumentId::new()));
    let runtime = SessionRuntimeHandle::new(store, Arc::new(PendingEgress::default()));
    let taken = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&taken);
    runtime.set_cloud_acceptor(Arc::new(
        move |session: koru_network_edge::TransportSession| {
            sink.lock().unwrap().push(session);
        },
    ));
    let principal = cloud_principal();

    let (_peer, accepted) = koru_network_edge::TransportSession::pair(
        principal.clone(),
        principal.clone(),
        cloud_content_protocol(),
        4,
    );
    runtime.accept(accepted);
    {
        let held = taken.lock().unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].principal, principal);
    }

    let (mut peer, accepted) = koru_network_edge::TransportSession::pair(
        principal.clone(),
        principal,
        replication_protocol(),
        4,
    );
    runtime.accept(accepted);
    assert_eq!(
        peer.receive.recv().await,
        Err(koru_network_edge::EdgeError::Reset(1))
    );
    assert_eq!(taken.lock().unwrap().len(), 1);
}

/// A device-only handler rejects Cloud explicitly. There is no projection
/// from an authority context to a bilateral trust principal, so a Cloud
/// connection event is ignored rather than registered as a peer.
#[test]
fn a_cloud_connection_event_never_becomes_a_peer() {
    let store = Arc::new(InventoryStore::new(WorkspaceId::new(), DocumentId::new()));
    let runtime = SessionRuntimeHandle::new(store, Arc::new(PendingEgress::default()));
    runtime.replace_authorization(AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: DeviceId::new(),
        peers: Vec::new(),
        local_build: None,
    });
    runtime.connection_event(koru_network_edge::ConnectionEvent::Connected(
        koru_network_edge::ConnectionMetadata {
            connection_id: 77,
            principal: cloud_principal(),
            path: koru_network_edge::PathMetadata {
                kind: PathKind::Direct,
                outbound: false,
                selected_is_ip: false,
            },
        },
    ));
    assert!(runtime.status().peers.is_empty());
    assert!(
        runtime
            .authenticated_principal(&cloud_principal())
            .is_none()
    );
}

/// **The Cloud dial is not this runtime's to make.** It belongs to the
/// Agent's Cloud coordinator, a later increment, which resolves the live
/// permit, rechecks generations and current authority, and confirms the
/// authenticated remote endpoint equals the issued route's. Until that
/// exists a Cloud target is closed rather than dialed on partial authority.
#[tokio::test]
async fn a_cloud_target_is_closed_until_the_agents_cloud_coordinator_lands() {
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    assert_eq!(
        runtime
            .open_product_session(
                koru_network_edge::NetworkTarget::Cloud {
                    permit_id: cloud_id(0x11).as_uuid(),
                    dataset: cloud_dataset(),
                },
                cloud_content_protocol(),
            )
            .await
            .err(),
        Some(koru_network_edge::EdgeError::Closed)
    );
    // And a device target still refuses a token this runtime does not serve.
    assert_eq!(
        runtime
            .open_product_session(
                koru_network_edge::NetworkTarget::Device {
                    device_id: DeviceId::new(),
                },
                cloud_content_protocol(),
            )
            .await
            .err(),
        Some(koru_network_edge::EdgeError::InvalidProtocol)
    );
    runtime.clear();
}

// --------------------------- D246: the selected-path fence, armed not asserted

/// A peer that serves a blob a few bytes at a time, with D246's selected-path
/// answer **armed**.
///
/// The real thing is `iroh::endpoint::Connection::paths_stream()` reporting a
/// selection change. Nothing in iroh 1.1 lets a test make a live connection
/// migrate its selected path deterministically — `clear_ip_transports` fixes
/// the shape at bind time — so the state is armed at the seam the gate reads,
/// rather than a connection being asserted to fail.
#[derive(Clone)]
struct PathSwitchingPeer {
    workspace: WorkspaceId,
    blob: koru_store_core::model::Blob,
    bytes: Arc<Vec<u8>>,
    served: Arc<AtomicUsize>,
    /// Chunks served before the selected path stops being an IP path.
    switch_after: usize,
    admitted: Arc<AtomicBool>,
    closed: Arc<tokio::sync::Notify>,
}

impl PathSwitchingPeer {
    fn holding(bytes: Vec<u8>, switch_after: usize) -> Self {
        let hash = BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(&bytes).to_hex()).unwrap();
        Self {
            workspace: WorkspaceId::new(),
            blob: koru_store_core::model::Blob {
                hash,
                size: bytes.len() as u64,
                mime: "image/png".into(),
                filename: None,
                created_at: koru_store_core::Timestamp::from_millis(1),
                attributes: Default::default(),
            },
            bytes: Arc::new(bytes),
            served: Arc::new(AtomicUsize::new(0)),
            switch_after,
            admitted: Arc::new(AtomicBool::new(true)),
            closed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn key(&self) -> hydration::BlobHydrationKey {
        hydration::BlobHydrationKey {
            workspace: self.workspace,
            hash: self.blob.hash.clone(),
        }
    }
}

impl RequestTransport for PathSwitchingPeer {
    fn stable_id(&self) -> usize {
        246
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let protocol::PeerFrame::BlobRequest { offset, .. } = frame else {
                return Err(FrameError::message("frame", "not a blob request"));
            };
            let start = *offset as usize;
            let end = (start + 8).min(self.bytes.len());
            let served = self.served.fetch_add(1, Ordering::SeqCst) + 1;
            if served >= self.switch_after {
                // The selection moves to the relay. Both halves of the gate
                // learn it: the per-chunk answer and the watcher.
                self.admitted.store(false, Ordering::SeqCst);
                self.closed.notify_waiters();
            }
            Ok(protocol::PeerFrame::BlobChunk(protocol::BlobChunk {
                workspace_id: self.workspace,
                blob: self.blob.clone(),
                offset: *offset,
                bytes: self.bytes[start..end].to_vec(),
                complete: end == self.bytes.len(),
            }))
        })
    }

    fn blob_path_admitted(&self) -> bool {
        self.admitted.load(Ordering::SeqCst)
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let closed = Arc::clone(&self.closed);
        let admitted = Arc::clone(&self.admitted);
        Box::pin(async move {
            loop {
                if !admitted.load(Ordering::SeqCst) {
                    return;
                }
                closed.notified().await;
            }
        })
    }
}

fn fenced_peer(workspace: WorkspaceId) -> protocol::AuthenticatedPeer {
    authenticated_test_peer(&AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: vec![workspace],
        via: None,
        display_name: None,
        build: None,
    })
}

fn hydration_state() -> tokio::sync::watch::Sender<hydration::BlobHydrationState> {
    tokio::sync::watch::channel(hydration::BlobHydrationState::CheckingLocal).0
}

/// **The forced path switch.** Part way in, the selected path stops being an IP
/// path: the transfer stops, nothing is admitted into the Store, and the
/// refusal carries the contract's own `relay_path_refused`.
#[tokio::test]
async fn a_selected_path_leaving_ip_stops_a_peer_blob_transfer() {
    let connection = PathSwitchingPeer::holding(vec![7u8; 80], 4);
    let store = Arc::new(InventoryStore::new(connection.workspace, DocumentId::new()));
    let peer = fenced_peer(connection.workspace);
    let state = hydration_state();

    let error = hydration::hydrate_blob_from_peer(
        &connection.key(),
        &peer,
        &connection,
        store.as_ref(),
        &state,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "relay_path_refused");
    assert!(
        error.retryable,
        "selection comes back, and so does the peer"
    );
    assert_eq!(
        store.blob_writes.load(Ordering::Relaxed),
        0,
        "a transfer that stopped must not have admitted anything"
    );
    // Ten chunks would have finished the file; it stopped at the switch.
    assert!(
        connection.served.load(Ordering::SeqCst) <= 4,
        "chunks kept being requested after the selected path left IP"
    );
}

/// A connection whose selected path is already a relay is never asked for a
/// chunk at all.
#[tokio::test]
async fn a_relayed_selection_requests_no_chunk() {
    let connection = PathSwitchingPeer::holding(vec![3u8; 40], usize::MAX);
    connection.admitted.store(false, Ordering::SeqCst);
    let store = Arc::new(InventoryStore::new(connection.workspace, DocumentId::new()));
    let peer = fenced_peer(connection.workspace);
    let state = hydration_state();

    let error = hydration::hydrate_blob_from_peer(
        &connection.key(),
        &peer,
        &connection,
        store.as_ref(),
        &state,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "relay_path_refused");
    assert_eq!(connection.served.load(Ordering::SeqCst), 0);
}

/// A direct selection carries the file, which is the row of the table that has
/// to keep working.
#[tokio::test]
async fn a_direct_selection_carries_the_whole_blob() {
    let connection = PathSwitchingPeer::holding(vec![9u8; 40], usize::MAX);
    let store = Arc::new(InventoryStore::new(connection.workspace, DocumentId::new()));
    let peer = fenced_peer(connection.workspace);
    let state = hydration_state();

    hydration::hydrate_blob_from_peer(
        &connection.key(),
        &peer,
        &connection,
        store.as_ref(),
        &state,
    )
    .await
    .unwrap();

    assert_eq!(store.blob_writes.load(Ordering::Relaxed), 1);
}

/// **§6's second point.** A Local-only Workspace asks no peer for a blob: the
/// refusal is in front of the first frame, not after the bytes have crossed.
#[tokio::test]
async fn a_local_only_workspace_sends_no_blob_request() {
    let connection = PathSwitchingPeer::holding(vec![1u8; 40], usize::MAX);
    let store = Arc::new(InventoryStore::new(connection.workspace, DocumentId::new()));
    store.peer_blobs_closed.store(true, Ordering::Relaxed);
    let peer = fenced_peer(connection.workspace);
    let state = hydration_state();

    let outcome = hydration::hydrate_blob_from_peers(
        &connection.key(),
        vec![(peer, connection.clone())],
        hydration::ParkDecision {
            worth_waiting: false,
            known_holders: Vec::new(),
        },
        store.clone(),
        &state,
        Arc::new(RwLock::new(NetworkStatus::default())),
        Arc::new(Mutex::new(hydration::BlobDirectory::new())),
    )
    .await;

    match outcome {
        hydration::BlobHydrationOutcome::Failed(error) => {
            assert_eq!(error.code, "mode_refuses_transport");
        }
        other => panic!("a closed Workspace hydrated: {other:?}"),
    }
    assert_eq!(connection.served.load(Ordering::SeqCst), 0);
    assert_eq!(store.blob_writes.load(Ordering::Relaxed), 0);
}

/// **The predicate against the real API**, in both directions, so the doubles
/// above are standing in for something that behaves this way.
///
/// Two bare endpoints on localhost with no relay at all: the only path that
/// can exist is an IP one, and it must read as selected once real bytes have
/// crossed it.
#[tokio::test]
async fn the_selected_path_predicate_admits_a_direct_connection() {
    let server_secret = iroh::SecretKey::generate();
    let server = Endpoint::builder(presets::Minimal)
        .secret_key(server_secret)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
        .bind()
        .await
        .expect("a direct server endpoint binds");
    let server_addr = server.addr();
    let accept = server.clone();
    let accepting = tokio::spawn(async move {
        while let Some(incoming) = accept.accept().await {
            if let Ok(connection) = incoming.await {
                tokio::spawn(async move {
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        if let Ok(bytes) = recv.read_to_end(64).await {
                            let _ = send.write_all(&bytes).await;
                            let _ = send.finish();
                        }
                    }
                });
            }
        }
    });

    let client = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .expect("a direct client endpoint binds");
    let direct_addr = EndpointAddr::from_parts(
        server_addr.id,
        server_addr.addrs.iter().filter(|a| a.is_ip()).cloned(),
    );
    assert!(!direct_addr.is_empty(), "the server advertises an IP path");
    let connection = client
        .connect(direct_addr, REPLICATION_ALPN_V1)
        .await
        .expect("the direct dial connects");
    // A path is *selected* once it is carrying application data, so the gate
    // is asked after a round trip rather than at the instant of connecting.
    echo_roundtrip(&connection, "direct").await;

    assert!(
        blob_path::selected_path_is_ip(&connection),
        "a connection with nothing but IP paths must admit blob bytes"
    );
    // And the helper that must never be used for this agrees for its own
    // reason, which is why the two are different functions.
    assert_eq!(path_kind(&connection), PathKind::Direct);

    connection.close(0u32.into(), b"done");
    accepting.abort();
    client.close().await;
    server.close().await;
}

/// The other direction, and the one the gate exists for: a connection whose
/// only transport is the relay is refused, even though it carries traffic
/// perfectly well.
#[tokio::test]
async fn the_selected_path_predicate_refuses_a_relayed_connection() {
    let relay = koru_relay_test_support::TestRelayServer::start()
        .await
        .expect("in-memory relay starts");
    let relay_url = relay.url().clone();
    let (peer_endpoint, peer_addr, accepting) = spawn_relay_only_echo_peer(&relay_url).await;

    let client = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            relay_url.clone(),
        )))
        .clear_ip_transports()
        .bind()
        .await
        .expect("a relay-only client endpoint binds");
    client.online().await;
    let connection = client
        .connect(peer_addr, REPLICATION_ALPN_V1)
        .await
        .expect("the relayed dial connects");
    // It works — bytes really do cross. That is the point: the refusal is
    // about *where* they cross, not about whether they can.
    echo_roundtrip(&connection, "relayed").await;

    assert!(
        !blob_path::selected_path_is_ip(&connection),
        "blob bytes must not be admitted over a relayed selection"
    );
    assert_eq!(path_kind(&connection), PathKind::Relay);

    connection.close(0u32.into(), b"done");
    accepting.abort();
    client.close().await;
    peer_endpoint.close().await;
    relay.shutdown().await.expect("relay shuts down");
}

// ------------------------------------------- the organization status (Plan 2.2)

/// **A pass's readings are installed whole**: a full pass replaces what the
/// peer said before, a retry scoped to failed Workspaces only updates what
/// it touched, within one pass the worse reading of a subject wins, and a
/// peer that disconnects takes its readings with it.
#[test]
fn root_sync_readings_are_installed_whole_and_leave_with_the_peer() {
    let peer = DeviceId::new();
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let status = RwLock::new(NetworkStatus::default());

    let mut pass = RootSyncPass::default();
    pass.note(
        protocol::CatalogUnitRef::Profile { profile },
        RootSyncState::UpToDate,
    );
    pass.note_workspace(workspace, RootSyncState::UpToDate);
    pass.note_workspace(workspace, RootSyncState::Pending);
    pass.note_workspace(workspace, RootSyncState::UpToDate);
    pass.install(&status, peer, true);
    let rows = status.read().unwrap().root_sync[&peer].clone();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.profile == Some(profile) && row.state == RootSyncState::UpToDate)
    );
    assert!(
        rows.iter()
            .any(|row| row.workspace == Some(workspace) && row.state == RootSyncState::Pending),
        "the worse reading of one subject wins within a pass"
    );

    // A retry pass touching only the Workspace keeps the Profile's reading.
    let mut retry = RootSyncPass::default();
    retry.note_workspace(workspace, RootSyncState::UpToDate);
    retry.install(&status, peer, false);
    let rows = status.read().unwrap().root_sync[&peer].clone();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.profile == Some(profile)));
    assert!(
        rows.iter()
            .any(|row| row.workspace == Some(workspace) && row.state == RootSyncState::UpToDate)
    );

    // A full pass that says nothing about the Profile forgets it.
    let mut next = RootSyncPass::default();
    next.note_workspace(workspace, RootSyncState::Held);
    next.install(&status, peer, true);
    let rows = status.read().unwrap().root_sync[&peer].clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, RootSyncState::Held);

    // Seeding from this device's own admissions: a closed mode row, a
    // ceiling mismatch and a pending overlay each read as their state; an
    // ordinary subject gets no reading from the seed.
    let mut seeded = RootSyncPass::default();
    let other = WorkspaceId::new();
    seeded.seed(&[
        protocol::RootSubjectAdmission {
            subject: protocol::CatalogUnitRef::Profile { profile },
            allows_peer: false,
            ceiling_mismatch: false,
            overlay_pending: false,
        },
        protocol::RootSubjectAdmission {
            subject: protocol::CatalogUnitRef::Workspace { workspace },
            allows_peer: true,
            ceiling_mismatch: true,
            overlay_pending: false,
        },
        protocol::RootSubjectAdmission {
            subject: protocol::CatalogUnitRef::Workspace { workspace: other },
            allows_peer: true,
            ceiling_mismatch: false,
            overlay_pending: true,
        },
        protocol::RootSubjectAdmission {
            subject: protocol::CatalogUnitRef::Workspace {
                workspace: WorkspaceId::new(),
            },
            allows_peer: true,
            ceiling_mismatch: false,
            overlay_pending: false,
        },
    ]);
    assert_eq!(seeded.subjects.len(), 3);
    assert_eq!(
        seeded.subjects[&protocol::CatalogUnitRef::Profile { profile }],
        RootSyncState::PausedBySettings
    );
    assert_eq!(
        seeded.subjects[&protocol::CatalogUnitRef::Workspace { workspace }],
        RootSyncState::BlockedByMismatch
    );
    assert_eq!(
        seeded.subjects[&protocol::CatalogUnitRef::Workspace { workspace: other }],
        RootSyncState::Pending
    );

    // Disconnecting the peer drops its readings; connecting keeps them.
    set_peer(
        &status,
        peer,
        PeerConnectionState::Connected {
            path: PathKind::Direct,
        },
    );
    assert!(status.read().unwrap().root_sync.contains_key(&peer));
    set_peer(&status, peer, PeerConnectionState::Disconnected);
    assert!(!status.read().unwrap().root_sync.contains_key(&peer));
}
