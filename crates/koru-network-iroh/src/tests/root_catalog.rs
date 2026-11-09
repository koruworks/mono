// SPDX-License-Identifier: MPL-2.0

//! The sync pass's root-catalog half (protocol 6), against the fake store:
//! the publication job first, root units before any inventory, personal v2
//! after content bootstrap, a scoped peer asked nothing, and the three
//! answers that are states rather than failures — pending placement, a held
//! unit, a blocked subject.

use super::profile_units::profile_identity;
use super::*;

fn vector(peer: u64) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc.get_map("m").insert("key", peer as i64).unwrap();
    doc.commit();
    doc.oplog_vv().encode()
}

fn all_data_peer() -> protocol::AuthenticatedPeer {
    authenticated_test_peer(&AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: Vec::new(),
        via: None,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    })
}

fn scoped_peer(workspace: WorkspaceId) -> protocol::AuthenticatedPeer {
    authenticated_test_peer(&AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: DeviceId::new(),
        endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
        all_data: false,
        authorized_profiles: vec![ProfileId::from_uuid(workspace.as_uuid())],
        workspaces: vec![workspace],
        via: None,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    })
}

/// A source whose root inventory is one Profile unit, one Workspace unit
/// and one personal unit for the Workspace, plus the Profile units the
/// existing bootstrap pulls.
fn source_with_root(workspace: WorkspaceId, manifest: DocumentId) -> Arc<InventoryStore> {
    let source = Arc::new(InventoryStore::new(workspace, manifest));
    let owner = ProfileId::from_uuid(workspace.as_uuid());
    source.root_units.lock().unwrap().insert(
        protocol::CatalogUnitRef::Profile { profile: owner },
        vector(11),
    );
    source.root_units.lock().unwrap().insert(
        protocol::CatalogUnitRef::Workspace { workspace },
        vector(12),
    );
    source
        .personal_units
        .lock()
        .unwrap()
        .insert(workspace, vector(13));
    source.unit_versions.lock().unwrap().insert(
        protocol::ProfileUnitRef::Identity { profile: owner },
        vector(1),
    );
    source
}

#[derive(Clone)]
struct RootTransport {
    source: Arc<InventoryStore>,
    peer: protocol::AuthenticatedPeer,
    /// Which request kinds were sent, in order.
    sent: Arc<Mutex<Vec<&'static str>>>,
}

impl RequestTransport for RootTransport {
    fn stable_id(&self) -> usize {
        901
    }
    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.sent.lock().unwrap().push(frame_kind(frame));
            Ok(
                protocol::handle_frame(self.source.as_ref(), &self.peer, frame.clone())
                    .into_iter()
                    .next()
                    .expect("one response"),
            )
        })
    }
    fn blob_path_admitted(&self) -> bool {
        true
    }
    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

async fn run_pass(
    source: Arc<InventoryStore>,
    target: Arc<InventoryStore>,
    peer: protocol::AuthenticatedPeer,
) -> (
    SyncPassResult,
    Vec<&'static str>,
    Arc<RwLock<NetworkStatus>>,
) {
    let transport = RootTransport {
        source,
        peer: peer.clone(),
        sent: Arc::new(Mutex::new(Vec::new())),
    };
    let status = Arc::new(RwLock::new(NetworkStatus::default()));
    let result = pull_missing(
        transport.clone(),
        peer,
        target,
        Arc::clone(&status),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        1,
    )
    .await;
    let sent = transport.sent.lock().unwrap().clone();
    (result, sent, status)
}

/// The order of a pass for the same person's device: publication job, root
/// catalog and units, Profile units, inventory and provisioning, then the
/// personal v2 unit — and root units land before any inventory is asked.
#[tokio::test]
async fn root_units_are_pulled_before_inventories_and_personal_after_content() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let source = source_with_root(workspace, manifest);
    let target = Arc::new(InventoryStore::new(workspace, manifest));
    target.workspace_ready.store(false, Ordering::SeqCst);
    target.placement_known.store(false, Ordering::SeqCst);
    let (result, sent, status) = run_pass(source.clone(), target.clone(), all_data_peer()).await;
    assert!(!result.retry_all, "{:?}", result.retry_error_code);
    assert!(result.failed_workspaces.is_empty());
    assert_eq!(
        *target.unit_events.lock().unwrap(),
        [
            "publication_job",
            "root_profile",
            "root_workspace",
            "identity",
            "workspace",
            "personal_v2"
        ]
    );
    let first_root = sent
        .iter()
        .position(|kind| *kind == "root_catalog_request")
        .expect("root catalog requested");
    let first_inventory = sent
        .iter()
        .position(|kind| *kind == "inventory_request")
        .expect("inventory requested");
    assert!(first_root < first_inventory, "{sent:?}");
    assert!(
        sent.contains(&"root_unit_request") && sent.contains(&"personal_request"),
        "{sent:?}"
    );
    assert_eq!(
        *source.root_units.lock().unwrap(),
        *target.root_units.lock().unwrap()
    );
    assert_eq!(
        *source.personal_units.lock().unwrap(),
        *target.personal_units.lock().unwrap()
    );
    assert_eq!(
        status
            .read()
            .unwrap()
            .last_sync_event
            .as_ref()
            .map(|event| event.stage),
        Some("sync_complete")
    );
    // Equal roots the second time: the digest short-circuits the units.
    let (result, sent, _) = run_pass(source, target, all_data_peer()).await;
    assert!(!result.retry_all);
    assert!(
        !sent.contains(&"root_unit_request"),
        "equal digests pull no unit: {sent:?}"
    );
}

/// A Profile- or Workspace-granted peer is asked no root unit and no
/// personal unit, binds no scope, and still syncs its content.
#[tokio::test]
async fn a_scoped_peer_is_asked_no_root_unit_and_its_content_still_syncs() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let source = source_with_root(workspace, manifest);
    let target = Arc::new(InventoryStore::new(workspace, manifest));
    target.workspace_ready.store(false, Ordering::SeqCst);
    target.placement_known.store(false, Ordering::SeqCst);
    let (result, sent, _) = run_pass(source, target.clone(), scoped_peer(workspace)).await;
    assert!(!result.retry_all, "{:?}", result.retry_error_code);
    assert!(
        !sent
            .iter()
            .any(|kind| kind.starts_with("root_") || kind.starts_with("personal_")),
        "a scoped peer is asked nothing of the root: {sent:?}"
    );
    assert!(target.unit_events.lock().unwrap().contains(&"workspace"));
    assert!(target.root_units.lock().unwrap().is_empty());
    assert!(target.personal_units.lock().unwrap().is_empty());
}

/// No root placement after the root exchange (the source framed no
/// Workspace unit): provisioning answers `placement_pending`, which retries
/// the whole pass rather than failing the Workspace.
#[tokio::test]
async fn a_pending_placement_retries_the_whole_pass_and_fails_nothing() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let source = source_with_root(workspace, manifest);
    source
        .root_units
        .lock()
        .unwrap()
        .remove(&protocol::CatalogUnitRef::Workspace { workspace });
    let target = Arc::new(InventoryStore::new(workspace, manifest));
    target.workspace_ready.store(false, Ordering::SeqCst);
    target.placement_known.store(false, Ordering::SeqCst);
    let (result, _, status) = run_pass(source, target.clone(), all_data_peer()).await;
    assert!(result.retry_all);
    assert_eq!(
        result.retry_error_code.as_deref(),
        Some(protocol::PLACEMENT_PENDING)
    );
    assert!(
        result.failed_workspaces.is_empty(),
        "pending is not a failed Workspace"
    );
    assert!(!target.unit_events.lock().unwrap().contains(&"workspace"));
    let status = status.read().unwrap();
    assert_eq!(
        status.last_sync_event.as_ref().map(|event| event.stage),
        Some("sync_incomplete")
    );
    // The status a person reads (Plan 2.2): the Workspace is pending, not
    // failed and not up to date, until its placement arrives.
    let readings = status.root_sync.values().flatten().collect::<Vec<_>>();
    assert!(
        readings
            .iter()
            .any(|row| row.workspace == Some(workspace) && row.state == RootSyncState::Pending),
        "{readings:?}"
    );
    assert!(
        !readings
            .iter()
            .any(|row| row.workspace == Some(workspace) && row.state == RootSyncState::UpToDate)
    );
}

/// A unit the source holds back and a Workspace this device's mode closes
/// are states: the pass completes, nothing is retried, nothing is asked for
/// the blocked Workspace.
#[tokio::test]
async fn a_held_unit_and_a_blocked_subject_are_states_not_failures() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let source = source_with_root(workspace, manifest);
    source.root_held.store(true, Ordering::SeqCst);
    let target = Arc::new(InventoryStore::new(workspace, manifest));
    target.peer_subject_blocked.store(true, Ordering::SeqCst);
    let (result, sent, status) = run_pass(source, target.clone(), all_data_peer()).await;
    assert!(!result.retry_all, "{:?}", result.retry_error_code);
    assert!(result.failed_workspaces.is_empty());
    assert!(
        sent.contains(&"root_unit_request"),
        "the unit was asked for"
    );
    assert!(
        !sent.contains(&"inventory_request"),
        "a blocked Workspace is never asked of a peer: {sent:?}"
    );
    assert!(target.root_units.lock().unwrap().is_empty());
    assert!(!target.unit_events.lock().unwrap().contains(&"workspace"));
    let status = status.read().unwrap();
    assert_eq!(
        status.last_sync_event.as_ref().map(|event| event.stage),
        Some("sync_complete")
    );
    // The readings (Plan 2.2): this device's own mode closes the Workspace's
    // peer path, which outranks the peer holding its unit back; neither is
    // "up to date", and no reading names a hidden Profile — the only
    // Profile named is the one both stores hold.
    let readings = status.root_sync.values().flatten().collect::<Vec<_>>();
    let about_workspace = readings
        .iter()
        .find(|row| row.workspace == Some(workspace))
        .expect("the Workspace has a reading");
    assert_eq!(about_workspace.state, RootSyncState::PausedBySettings);
    let owner = ProfileId::from_uuid(workspace.as_uuid());
    assert!(
        readings
            .iter()
            .filter_map(|row| row.profile)
            .all(|profile| profile == owner),
        "{readings:?}"
    );
}

/// The fake's identity fixture is what the Profile exchange in these passes
/// provisions; keep it in one place so a sentinel there is a sentinel here.
#[test]
fn the_profile_fixture_is_the_shared_one() {
    assert_eq!(profile_identity().kind, "normal");
}
