// SPDX-License-Identifier: MPL-2.0

//! Adapter between the opaque Store and the iroh replication data plane.
//!
//! This is deliberately in the daemon: `koru-store-core` continues to know
//! neither devices nor networking, while the in-process replicator receives a
//! narrow durable port over opaque Loro bytes.

use std::sync::Arc;

use koru_device::DeviceId;
use koru_replication::protocol::{
    AuthenticatedPeer, BlobChunk, CatalogUnitRef, DocumentReplicaState, DurableAck,
    PLACEMENT_PENDING, ProfileReplicaState, ReplicaDescriptor, ReplicationStore,
    RootCatalogInventory, RootJoin, StoreFailure, WorkspaceCatalogState, WorkspaceInventory,
};
use koru_replication::root_exchange;
use koru_store_api::{AppState, KoruStore};
use koru_store_core::root_catalog::ExportSubject;
use koru_store_core::{DocumentId, ProfileId, ProvisionedReplica, WorkspaceId};

pub struct LiveReplicationStore {
    state: Arc<AppState>,
}

impl LiveReplicationStore {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Every content operation on one Workspace, in this order: the
    /// root-aware grant check (§7.2: UUID grants by id, Profile grants by
    /// current placement, all-data by scope), then §4.4's transport gate (a
    /// Local-only Workspace is neither listed, served nor admitted over the
    /// peer path), then the relationship fence around the Store call.
    ///
    /// The first two are `root_exchange::admit_peer_description`, the one
    /// predicate the phone's adapter asks for the same answers, so the two
    /// cannot drift apart over which half of §4.4 they keep.
    fn with_workspace<T>(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        operation: impl FnOnce(&mut KoruStore) -> Result<T, StoreFailure>,
    ) -> Result<T, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::admit_peer_description(&store, peer, workspace)?;
        peer.with_fence(|| operation(&mut store))
    }

    /// The export boundary's three steps for one root or personal unit:
    /// prepare under the lock, derive off it, recheck under it right before
    /// the bytes are handed to the frame.
    fn export_root_unit(
        &self,
        peer: &AuthenticatedPeer,
        subject: ExportSubject,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        let source = {
            let store = self.state.store.lock().map_err(|_| failed("internal"))?;
            root_exchange::prepare_root_export(&store, peer, subject)?
        };
        let recipient = (!version.is_empty()).then_some(version);
        let publication = root_exchange::derive_root_export(&source, peer, recipient)?;
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::recheck_root_export(&store, peer, &publication)?;
        Ok(publication.bytes)
    }
}

pub struct LiveBlobHydration {
    runtime: HydrationRuntime,
}

enum HydrationRuntime {
    Iroh(koru_network_iroh::RuntimeHandle),
    Session(koru_network_iroh::SessionRuntimeHandle),
}

impl LiveBlobHydration {
    pub fn new(runtime: koru_network_iroh::RuntimeHandle) -> Self {
        Self {
            runtime: HydrationRuntime::Iroh(runtime),
        }
    }

    pub fn from_sessions(runtime: koru_network_iroh::SessionRuntimeHandle) -> Self {
        Self {
            runtime: HydrationRuntime::Session(runtime),
        }
    }
}

impl koru_store_api::state::BlobHydrationService for LiveBlobHydration {
    fn hydrate(
        &self,
        workspace: WorkspaceId,
        hash: koru_store_core::model::BlobHash,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), koru_store_api::state::BlobHydrationFailure>,
                > + Send
                + 'static,
        >,
    > {
        let ticket = match &self.runtime {
            HydrationRuntime::Iroh(runtime) => runtime.hydrate_blob(workspace, hash),
            HydrationRuntime::Session(runtime) => runtime.hydrate_blob(workspace, hash),
        };
        Box::pin(async move {
            // `wait_unless_parked`, not `wait`: a blob whose only holder is
            // offline must answer the HTTP request with the truth, not hold
            // it open until that device comes back. The hydration entry
            // itself keeps waiting and re-runs when a peer connects.
            ticket.wait_unless_parked().await.map_err(|error| {
                koru_store_api::state::BlobHydrationFailure {
                    code: error.code,
                    retryable: error.retryable,
                    waiting_on: error.waiting_on,
                    // The peer path knows nothing about the blob engine's
                    // holds; `CloudFallbackHydration` attaches one when the
                    // Cloud pass it falls through to names one.
                    hold: None,
                }
            })
        })
    }
}

/// **The peer runtime's holder directory, read for a waiting list.** Whichever
/// runtime this daemon hosts, the answer is the devices it heard advertise
/// each blob this session, as storage ids; the route names them.
impl koru_store_api::state::BlobHolderDirectory for crate::sync_control::ReplicationRuntime {
    fn advertised_holders(
        &self,
        workspace: WorkspaceId,
        hashes: &[koru_store_core::model::BlobHash],
    ) -> std::collections::HashMap<koru_store_core::model::BlobHash, Vec<String>> {
        let advertised = match self {
            Self::Iroh(runtime) => runtime.advertised_blob_holders(workspace, hashes),
            Self::Session(runtime) => runtime.advertised_blob_holders(workspace, hashes),
        };
        advertised
            .into_iter()
            .map(|(hash, devices)| {
                let devices = devices.into_iter().map(DeviceId::to_storage).collect();
                (hash, devices)
            })
            .collect()
    }
}

/// **The one question, asked once** (§6). Every peer blob point in this file
/// goes through here, and here goes straight to `Store::admit_transport`: the
/// stored mode, then the Profile ceiling, then the in-memory fence, at the
/// generation the answer is taken at. Nothing in this file re-derives any of
/// it, and the Cloud path asks the identical question through the identical
/// call.
///
/// Asked again at each point rather than remembered, because a restrictive
/// choice closes admission in memory *before* its durable write: an answer
/// cached across one would be a transfer continuing under a mode nobody holds.
fn peer_blob_admitted(store: &KoruStore, workspace: WorkspaceId) -> Result<(), StoreFailure> {
    store
        .admit_transport(
            koru_store_core::TransportSubject::Workspace(workspace),
            koru_store_core::TransportPath::Peer,
        )
        .map(|_| ())
        .map_err(|error| match error.code() {
            // The contract's own code for "the Workspace transport mode forbids
            // this", so a peer reads the same name the Cloud plane returns.
            koru_store_core::errors::Code::Forbidden => {
                failed(koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str())
            }
            // A stored child above its Profile, or a row this build cannot
            // decode. Reported as what it is rather than as a refusal.
            _ => map_error(error),
        })
}

impl ReplicationStore for LiveReplicationStore {
    fn admit_peer_blob_transfer(&self, workspace: WorkspaceId) -> Result<(), StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer_blob_admitted(&store, workspace)
    }

    fn record_state_change(
        &self,
        event: &koru_replication::StateChangeEvent,
        retain_after_millis: i64,
    ) -> Result<koru_replication::StateChangeEvent, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        let stored = store
            .record_sync_state_event(
                &to_store_event(event),
                koru_store_core::model::Timestamp::from_millis(retain_after_millis),
            )
            .map_err(map_error)?;
        from_store_event(stored)
    }

    fn state_changes(
        &self,
        peer: DeviceId,
        retain_after_millis: i64,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<koru_replication::StateChangeEvent>, StoreFailure> {
        self.state
            .store
            .lock()
            .map_err(|_| failed("internal"))?
            .sync_state_events(
                &peer.to_storage(),
                koru_store_core::model::Timestamp::from_millis(retain_after_millis),
                before_id,
                limit,
            )
            .map_err(map_error)?
            .into_iter()
            .map(from_store_event)
            .collect()
    }

    fn latest_state_change_times(
        &self,
        retain_after_millis: i64,
    ) -> Result<Vec<(DeviceId, i64)>, StoreFailure> {
        self.state
            .store
            .lock()
            .map_err(|_| failed("internal"))?
            .latest_sync_state_event_times(koru_store_core::model::Timestamp::from_millis(
                retain_after_millis,
            ))
            .map_err(map_error)?
            .into_iter()
            .map(|(peer, at)| {
                DeviceId::parse(&peer)
                    .map(|peer| (peer, at.millis()))
                    .ok_or_else(|| failed("corrupt_state"))
            })
            .collect()
    }

    fn prune_state_changes(&self, retain_after_millis: i64) -> Result<usize, StoreFailure> {
        self.state
            .store
            .lock()
            .map_err(|_| failed("internal"))?
            .prune_sync_state_events(koru_store_core::model::Timestamp::from_millis(
                retain_after_millis,
            ))
            .map_err(map_error)
    }

    fn profiles(&self, peer: &AuthenticatedPeer) -> Result<Vec<ProfileReplicaState>, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer.with_catalog_scope(|| {
            let mut result = Vec::new();
            for profile in store.profiles().map_err(map_error)? {
                if !peer.all_data && !peer.authorized_profiles.contains(&profile.id) {
                    continue;
                }
                // §4.4: a Profile whose device-local mode closes the peer
                // path lists none of its units — its name is in the identity
                // unit, and nothing about it leaves.
                if root_exchange::admit_peer_profile(&store, profile.id).is_err() {
                    continue;
                }
                for state in store.profile_unit_states(profile.id).map_err(map_error)? {
                    if peer.check_authorized_profile_unit(state.unit).is_err() {
                        continue;
                    }
                    if let Some(workspace) = state.unit.workspace() {
                        // A v1 personal address is not exported since
                        // catalog 13 (personal data travels as v2 on the
                        // root exchange); it is skipped, never an error.
                        if matches!(
                            state.unit,
                            koru_store_core::profile_units::ProfileUnitRef::Personal { .. }
                        ) || root_exchange::authorize_workspace(&store, peer, workspace).is_err()
                        {
                            continue;
                        }
                    }
                    let identity = matches!(
                        state.unit,
                        koru_store_core::profile_units::ProfileUnitRef::Identity { .. }
                    )
                    .then(|| {
                        koru_replication::protocol::ProfileIdentityState {
                            name: profile.name.clone(),
                            purpose: profile.purpose.clone(),
                            avatar: profile.avatar.clone(),
                            icon: profile.icon.clone(),
                            color: profile.color.clone(),
                            account: profile.account.clone(),
                            locale: profile.locale.to_string(),
                            created_at_ms: profile.created_at.millis(),
                            kind: profile.kind.as_str().to_owned(),
                        }
                    });
                    result.push(ProfileReplicaState {
                        unit: state.unit,
                        version: store
                            .profile_unit_version(state.unit)
                            .map_err(map_error)?
                            .encode(),
                        identity,
                    });
                }
            }
            Ok(result)
        })
    }

    fn provision_profile(
        &self,
        peer: &AuthenticatedPeer,
        state: &ProfileReplicaState,
    ) -> Result<(), StoreFailure> {
        state.validate()?;
        peer.check_authorized_profile_unit(state.unit)?;
        let identity = state
            .identity
            .as_ref()
            .ok_or_else(|| failed("invalid_profile_identity"))?;
        let locale = koru_store_core::text::Locale::parse(&identity.locale)
            .ok_or_else(|| failed("invalid_locale"))?;
        let profile = koru_store_core::model::Profile {
            id: state.unit.profile(),
            name: identity.name.clone(),
            purpose: identity.purpose.clone(),
            avatar: identity.avatar.clone(),
            icon: identity.icon.clone(),
            color: identity.color.clone(),
            account: identity.account.clone(),
            locale,
            created_at: koru_store_core::model::Timestamp::from_millis(identity.created_at_ms),
            kind: koru_store_core::model::ProfileKind::parse(&identity.kind),
        };
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer.with_authorized_profile(profile.id, || {
            store.provision_replica_profile(&profile).map_err(map_error)
        })
    }

    fn profile_version(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
    ) -> Result<Vec<u8>, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer.with_authorized_profile_unit(unit, || {
            if let Some(workspace) = unit.workspace() {
                root_exchange::authorize_workspace(&store, peer, workspace)?;
            }
            root_exchange::admit_peer_profile(&store, unit.profile())?;
            store
                .profile_unit_version(unit)
                .map(|version| version.encode())
                .map_err(map_error)
        })
    }

    fn profile_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer.with_authorized_profile_unit(unit, || {
            if let Some(workspace) = unit.workspace() {
                root_exchange::authorize_workspace(&store, peer, workspace)?;
            }
            root_exchange::admit_peer_profile(&store, unit.profile())?;
            store
                .profile_unit_updates_since(unit, &version)
                .map_err(map_error)
        })
    }

    fn apply_remote_profile_updates(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
        updates: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        peer.with_authorized_profile_unit(unit, || {
            if let Some(workspace) = unit.workspace() {
                root_exchange::authorize_workspace(&store, peer, workspace)?;
            }
            root_exchange::admit_peer_profile(&store, unit.profile())?;
            store
                .apply_profile_unit_updates(unit, &updates)
                .map_err(map_error)?;
            Ok(DurableAck {
                durable_sequence: None,
            })
        })
    }

    fn catalog(
        &self,
        peer: &AuthenticatedPeer,
    ) -> Result<Vec<WorkspaceCatalogState>, StoreFailure> {
        peer.with_catalog_scope(|| {
            let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
            let workspaces = store.workspaces_including_deleted().map_err(map_error)?;
            let mut result = Vec::new();
            for workspace in workspaces {
                // Authorized by the root-aware rule, and only while this
                // device's mode admits the peer path for it (§4.4): a
                // Local-only Workspace is not a row in this list.
                if root_exchange::admit_peer_description(&store, peer, workspace.id).is_err() {
                    continue;
                }
                let placement = store
                    .current_placement_profile(workspace.id)
                    .map_err(map_error)?
                    .unwrap_or(workspace.owner);
                let digest = store
                    .workspace_sync_digest(workspace.id)
                    .map_err(map_error)?;
                result.push(WorkspaceCatalogState {
                    workspace_id: workspace.id,
                    // A bootstrap hint since protocol 6; see the type's doc.
                    profile_id: placement,
                    digest,
                });
            }
            Ok(result)
        })
    }

    fn authorize_workspace(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<(), StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::authorize_workspace(&store, peer, workspace)
    }

    fn admit_peer_subject(&self, workspace: WorkspaceId) -> Result<(), StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::admit_peer_subject(&store, workspace)
    }

    /// The Profile a Workspace is currently placed under here — the root
    /// placement, with the row's owner only for a row without a unit or an
    /// unassigned register (the only evidence a projection has). Grant
    /// evaluation does not read this; [`Self::authorize_workspace`] does
    /// its own root read.
    fn workspace_profile(&self, workspace: WorkspaceId) -> Result<ProfileId, StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        let row = store
            .workspace_including_deleted(workspace)
            .map_err(map_error)?;
        Ok(store
            .current_placement_profile(workspace)
            .map_err(map_error)?
            .unwrap_or(row.owner))
    }

    fn provision_replica(
        &self,
        peer: &AuthenticatedPeer,
        replica: &ReplicaDescriptor,
    ) -> Result<(), StoreFailure> {
        let discoverability_changed;
        let result = {
            let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
            let previous = store.workspace_including_deleted(replica.workspace_id).ok();
            let was_discoverable = previous
                .as_ref()
                .is_some_and(|workspace| workspace.deleted_at.is_none());
            let result = match root_exchange::provision_replica(&mut store, peer, replica) {
                Ok(ProvisionedReplica::Ready) => Ok(()),
                Ok(ProvisionedReplica::PlacementPending) => {
                    tracing::info!(
                        peer = %peer.device_id.to_storage(),
                        workspace = %replica.workspace_id.to_storage(),
                        "root_sync direction=peer_to_local action=provisioning_pending reason=no_root_placement"
                    );
                    Err(failed(PLACEMENT_PENDING))
                }
                Err(error) => Err(error),
            };
            let is_discoverable = store
                .workspace_including_deleted(replica.workspace_id)
                .ok()
                .is_some_and(|workspace| workspace.deleted_at.is_none());
            discoverability_changed = result.is_ok() && was_discoverable != is_discoverable;
            if result.is_ok() && peer_object_logs_enabled() {
                let action = if previous.is_none() {
                    "workspace_received"
                } else {
                    "workspace_metadata_received"
                };
                tracing::info!(
                    "peer_object direction=peer_to_local action={} peer={} workspace={} archived={} deleted={}",
                    action,
                    peer.device_id.to_storage(),
                    replica.workspace_id.to_storage(),
                    replica.workspace_archived,
                    replica.workspace_deleted_at_ms.is_some()
                );
            }
            result
        };
        // The Store commit and its mutex guard are both behind us. A reader
        // woken here can immediately reconcile, including when replication
        // created the first workspace or changed a tombstone in either
        // direction.
        if result.is_ok() && discoverability_changed {
            self.state.notify_catalog_change();
        }
        result
    }

    // ---- protocol 6: the root-catalog exchange

    fn bind_root_scope(&self, peer: &AuthenticatedPeer) -> Result<bool, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::bind_root_scope(&mut store, peer)
    }

    fn publish_pending_root_overlays(&self) -> Result<usize, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::publish_pending_root_overlays(&mut store)
    }

    fn root_sync_admissions(
        &self,
    ) -> Result<Vec<koru_replication::protocol::RootSubjectAdmission>, StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::root_sync_admissions(&store)
    }

    fn root_catalog(&self, peer: &AuthenticatedPeer) -> Result<RootCatalogInventory, StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::root_catalog(&store, peer)
    }

    fn root_unit_version(
        &self,
        peer: &AuthenticatedPeer,
        unit: CatalogUnitRef,
    ) -> Result<Vec<u8>, StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::root_unit_version(&store, peer, unit)
    }

    fn root_unit_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        unit: CatalogUnitRef,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        let subject = match unit {
            CatalogUnitRef::Profile { profile } => ExportSubject::ProfileCatalog(profile),
            CatalogUnitRef::Workspace { workspace } => ExportSubject::WorkspaceCatalog(workspace),
        };
        self.export_root_unit(peer, subject, version)
    }

    fn apply_remote_root_updates(
        &self,
        peer: &AuthenticatedPeer,
        unit: CatalogUnitRef,
        updates: Vec<Vec<u8>>,
        advertised: Option<&loro::VersionVector>,
    ) -> Result<RootJoin, StoreFailure> {
        // Staged under one lock, committed under another: a policy change
        // in between is found by the commit's recheck, not papered over.
        let staged = {
            let store = self.state.store.lock().map_err(|_| failed("internal"))?;
            root_exchange::stage_root_unit(&store, peer, unit, &updates)?
        };
        let (join, lifecycle_event) = {
            let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
            let (join, outcome) =
                root_exchange::commit_root_unit_outcome(&mut store, peer, staged, advertised)?;
            // A Profile deletion or restore that arrived from another device
            // is the same fact a local batch publishes (Plan 1.3's
            // `profile.deleted` / `profile.restored`): the windows refresh
            // their Profile catalog on it, and the one whose open Profile
            // just went settles into `NeedsProfile` (Plan 2.2, §6.3). The
            // row's visibility after the commit says which it was.
            let lifecycle_event = match (unit, &outcome) {
                (
                    CatalogUnitRef::Profile { profile },
                    koru_store_core::service::RootJoinOutcome::Applied {
                        lifecycle_changed: true,
                        ..
                    },
                ) => {
                    let active = store
                        .profiles()
                        .map(|profiles| profiles.iter().any(|row| row.id == profile))
                        .unwrap_or(true);
                    Some((profile, active))
                }
                _ => None,
            };
            (join, lifecycle_event)
        };
        if matches!(join, RootJoin::Applied { .. }) {
            // Placement or lifecycle may have moved a listing; readers
            // reconcile from the catalog, which is the only truth they read.
            self.state.notify_catalog_change();
        }
        if let Some((profile, active)) = lifecycle_event {
            if active {
                self.state
                    .publish_catalog_event(koru_events::ProfileRestored {
                        profile_id: profile.as_uuid(),
                    });
            } else {
                self.state
                    .publish_catalog_event(koru_events::ProfileDeleted {
                        profile_id: profile.as_uuid(),
                    });
            }
        }
        Ok(join)
    }

    fn personal_version(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<Vec<u8>, StoreFailure> {
        let store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::personal_version(&store, peer, workspace)
    }

    fn personal_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.export_root_unit(peer, ExportSubject::PersonalV2(workspace), version)
    }

    fn apply_remote_personal_updates(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        updates: Vec<Vec<u8>>,
        advertised: Option<&loro::VersionVector>,
    ) -> Result<RootJoin, StoreFailure> {
        let staged = {
            let store = self.state.store.lock().map_err(|_| failed("internal"))?;
            root_exchange::stage_personal_unit(&store, peer, workspace, &updates)?
        };
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        root_exchange::commit_personal_unit(&mut store, peer, staged, advertised)
    }

    fn provision_document(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<(), StoreFailure> {
        let (result, is_new) = self.with_workspace(peer, workspace, |store| {
            let is_new = store.locate(document).map_err(map_error)?.is_none();
            let result = store
                .provision_replica_document(
                    workspace,
                    document,
                    koru_store_core::model::Timestamp::now(),
                )
                .map_err(map_error);
            Ok((result, is_new))
        })?;
        if result.is_ok() && is_new && peer_object_logs_enabled() {
            tracing::info!(
                "peer_object direction=peer_to_local action=document_received peer={} workspace={} document={}",
                peer.device_id.to_storage(),
                workspace.to_storage(),
                document.to_storage()
            );
        }
        result
    }

    fn inventory(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceInventory, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            let record = store
                .workspace_including_deleted(workspace)
                .map_err(map_error)?;
            // The bootstrap placement hint is the current root placement;
            // the row's owner only when there is no unit or no register.
            let placement = store
                .current_placement_profile(workspace)
                .map_err(map_error)?
                .unwrap_or(record.owner);
            let profile = store.profile(placement).map_err(map_error)?;
            let manifest = if record.deleted_at.is_some() {
                store
                    .replica_manifest_id_including_deleted(workspace)
                    .map_err(map_error)?
            } else {
                store.manifest_id(workspace).map_err(map_error)?
            };
            let mut documents = vec![DocumentReplicaState {
                document_id: manifest,
                room_id: format!("workspace:{}:manifest", workspace.to_storage()),
                version: store
                    .version(workspace, manifest)
                    .map_err(|error| map_document_error(error, "version", workspace, manifest))?
                    .encode(),
                is_manifest: true,
            }];
            if record.deleted_at.is_none() {
                for entry in store.documents(workspace).map_err(map_error)? {
                    let version = match store.version(workspace, entry.id) {
                        Ok(version) => version,
                        // A manifest entry with no physical document cannot be
                        // served — and it must not take the whole workspace's
                        // inventory down with it. The peer that holds the content
                        // advertises it instead, and this store pulls it there.
                        Err(error)
                            if error.code() == koru_store_core::errors::Code::DocumentNotFound =>
                        {
                            tracing::warn!(
                                workspace = %workspace.to_storage(),
                                document = %entry.id.to_storage(),
                                "sync inventory skipping a listed document with no content"
                            );
                            continue;
                        }
                        Err(error) => {
                            return Err(map_document_error(error, "version", workspace, entry.id));
                        }
                    };
                    documents.push(DocumentReplicaState {
                        document_id: entry.id,
                        room_id: format!("document:{}", entry.id.to_storage()),
                        version: version.encode(),
                        is_manifest: false,
                    });
                }
            }
            let mut inventory = WorkspaceInventory {
                workspace_id: workspace,
                replica: ReplicaDescriptor {
                    profile_id: profile.id,
                    profile_name: profile.name,
                    profile_locale: profile.locale.to_string(),
                    profile_created_at_ms: profile.created_at.millis(),
                    profile_icon: profile.icon,
                    profile_color: profile.color,
                    profile_kind: profile.kind.as_str().to_owned(),
                    workspace_id: record.id,
                    workspace_name: record.name,
                    workspace_created_at_ms: record.created_at.millis(),
                    workspace_icon: record.icon,
                    workspace_color: record.color,
                    workspace_pinned: record.pinned,
                    workspace_archived: record.archived,
                    workspace_deleted_at_ms: record.deleted_at.map(|at| at.millis()),
                    manifest_document: manifest,
                },
                documents,
                // **A Local-only Workspace advertises no blobs** (§8). Not a
                // refusal — the documents still go, and a deleted Workspace has
                // advertised nothing since long before this. Advertising bytes
                // this device will not serve is how a scheduler on the other
                // side learns to retry forever, and it would put a hash list
                // into a frame for a Workspace that is closed to transfer.
                blobs: if record.deleted_at.is_some()
                    || peer_blob_admitted(&store, workspace).is_err()
                {
                    Vec::new()
                } else {
                    store.blobs(workspace).map_err(map_error)?
                },
            };
            if !peer.all_data {
                inventory.replica.redact_profile_metadata();
            }
            tracing::trace!(
                "sync direction=local_to_peer action=inventory workspace={} documents={}",
                workspace.to_storage(),
                inventory.documents.len()
            );
            Ok(inventory)
        })
    }

    fn locate(
        &self,
        peer: &AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, StoreFailure> {
        let mut store = self.state.store.lock().map_err(|_| failed("internal"))?;
        let Some(workspace) = store.locate(document).map_err(map_error)? else {
            return Ok(None);
        };
        root_exchange::authorize_workspace(&store, peer, workspace)?;
        root_exchange::admit_peer_subject(&store, workspace)?;
        peer.with_fence(|| Ok(Some(workspace)))
    }

    fn document_name(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Option<String> {
        self.with_workspace(peer, workspace, |store| {
            Ok(store
                .documents(workspace)
                .ok()
                .and_then(|entries| entries.into_iter().find(|entry| entry.id == document))
                .map(|entry| entry.title))
        })
        .ok()
        .flatten()
    }

    fn manifest_id(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<DocumentId, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            store
                .replica_manifest_id_including_deleted(workspace)
                .map_err(map_error)
        })
    }

    fn version(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            store
                .version(workspace, document)
                .map(|version| version.encode())
                .map_err(map_error)
        })
    }

    fn updates_since(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            store
                .updates_since(workspace, document, version)
                .map_err(map_error)
        })
    }

    fn snapshot(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            store.snapshot(workspace, document).map_err(map_error)
        })
    }

    fn blob_origin(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Option<String> {
        self.with_workspace(peer, workspace, |store| {
            Ok(store.blob_origin(workspace, hash).ok().flatten())
        })
        .ok()
        .flatten()
    }

    fn record_blob_origins(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        blobs: &[koru_store_core::model::Blob],
    ) -> Result<usize, StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            store
                .record_blob_origins(workspace, blobs, koru_store_core::model::Timestamp::now())
                .map_err(map_error)
        })
    }

    fn read_blob_chunk(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        offset: u64,
        max_bytes: u32,
    ) -> Result<BlobChunk, StoreFailure> {
        const CHUNK_MAX: usize = 1024 * 1024;
        self.with_workspace(peer, workspace, |store| {
            // **Before the read, not after it.** §6's third point: a Local-only
            // Workspace serves no blob to a peer, and a mode that changed since
            // the first chunk stops the rest of the transfer rather than
            // finishing it under a mode nobody holds.
            peer_blob_admitted(store, workspace)?;
            let blob = store.blob(workspace, hash).map_err(map_error)?;
            if offset > blob.size {
                return Err(failed("invalid_offset"));
            }
            // Integrity is checked once, at the start of the transfer, and the rest
            // of it is ranged reads. This used to read *and re-hash the whole blob*
            // for every 1 MiB chunk, which is fine for a thumbnail and quadratic for
            // a file: sending 32 MiB meant a gigabyte of reading and hashing.
            if offset == 0 {
                store.verify_blob(workspace, hash).map_err(map_error)?;
            }
            let requested = usize::try_from(max_bytes)
                .unwrap_or(CHUNK_MAX)
                .clamp(1, CHUNK_MAX);
            let bytes = store
                .read_blob_range(workspace, hash, offset, requested)
                .map_err(map_error)?;
            let end = offset.saturating_add(bytes.len() as u64);
            Ok(BlobChunk {
                workspace_id: workspace,
                blob: blob.clone(),
                offset,
                bytes,
                complete: end >= blob.size,
            })
        })
    }

    fn store_remote_blob(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        blob: &koru_store_core::model::Blob,
        bytes: Vec<u8>,
    ) -> Result<(), StoreFailure> {
        self.with_workspace(peer, workspace, |store| {
            // **§6's fifth point.** A1 §5's "incoming Store admission": bytes
            // that arrived for a Workspace this device has closed are not
            // written, whatever asked for them and however far they got.
            peer_blob_admitted(store, workspace)?;
            // The record is stored as announced: its attributes name the
            // *origin* device, and this one is a holder, not the origin.
            store
                .put_remote_blob(workspace, blob, &bytes)
                .map_err(map_error)?;
            Ok(())
        })
    }

    fn apply_remote_updates(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
        updates: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        let now = koru_store_core::model::Timestamp::now();
        let (durable_sequence, is_manifest, before_catalog, after_catalog) =
            self.with_workspace(peer, workspace, |store| {
                let manifest = store.manifest_id(workspace).ok();
                let before_catalog = store
                    .workspace_including_deleted(workspace)
                    .ok()
                    .map(|row| (row.name, row.deleted_at.is_none()));
                for update in &updates {
                    store
                        .apply_update(
                            workspace,
                            document,
                            update.clone(),
                            now,
                            peer.update_origin(),
                        )
                        .map_err(map_error)?;
                }
                // This fsync is the boundary the peer acknowledgment represents.
                let sequence = store.flush(workspace, document, now).map_err(map_error)?;
                let after_catalog = store
                    .workspace_including_deleted(workspace)
                    .ok()
                    .map(|row| (row.name, row.deleted_at.is_none()));
                Ok((
                    sequence,
                    manifest == Some(document),
                    before_catalog,
                    after_catalog,
                ))
            })?;
        tracing::info!(
            "sync direction=peer_to_local action=durable_commit peer={} workspace={} document={} updates={} sequence={:?}",
            peer.device_id.to_storage(),
            workspace.to_storage(),
            document.to_storage(),
            updates.len(),
            durable_sequence
        );
        if is_manifest {
            let name_changed = before_catalog.as_ref().map(|row| &row.0)
                != after_catalog.as_ref().map(|row| &row.0);
            let discoverability_changed =
                before_catalog.as_ref().map(|row| row.1) != after_catalog.as_ref().map(|row| row.1);
            tracing::info!(
                peer = %peer.device_id.to_storage(),
                workspace = %workspace.to_storage(),
                document = %document.to_storage(),
                name_changed,
                sequence = ?durable_sequence,
                "workspace_change direction=peer_to_local action=manifest_received"
            );
            if name_changed {
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    workspace = %workspace.to_storage(),
                    document = %document.to_storage(),
                    "workspace_change direction=peer_to_local action=rename"
                );
            }
            if discoverability_changed {
                // `flush` durably projected the manifest tombstone before the
                // Store lock was released. This is the replicated delete or
                // restore path; wake exact-room readers only after that point.
                self.state.notify_catalog_change();
            }
        }

        // Local clients learn about the exact committed bytes only after the
        // Store lock has released and durability has succeeded.
        let room = match is_manifest {
            true => koru_store_api::rooms::Room::Manifest(workspace),
            false => koru_store_api::rooms::Room::Document(document),
        };
        let message = loro_protocol::protocol::ProtocolMessage::DocUpdate {
            crdt: loro_protocol::protocol::CrdtType::Loro,
            room_id: room.to_id(),
            updates,
            batch_id: loro_protocol::protocol::BatchId([0; 8]),
        };
        if let Ok(frame) = loro_protocol::encoding::encode(&message) {
            self.state
                .rooms
                .broadcast(&room, koru_store_api::rooms::ConnectionId(u64::MAX), frame);
        }
        Ok(DurableAck { durable_sequence })
    }
}

fn to_store_event(
    event: &koru_replication::StateChangeEvent,
) -> koru_store_core::ports::SyncStateEvent {
    koru_store_core::ports::SyncStateEvent {
        id: event.event_id,
        received_at: koru_store_core::model::Timestamp::from_millis(event.at_millis),
        peer_device_id: event.peer.to_storage(),
        kind: event.kind.clone(),
        profile: event.profile,
        workspace: event.workspace,
        document: event.document,
        document_name: event.document_name.clone(),
        blob_hash: event.blob_hash.clone(),
        detail: event.detail.clone(),
        items: event.items,
    }
}

fn from_store_event(
    event: koru_store_core::ports::SyncStateEvent,
) -> Result<koru_replication::StateChangeEvent, StoreFailure> {
    let peer = DeviceId::parse(&event.peer_device_id).ok_or_else(|| failed("corrupt_state"))?;
    Ok(koru_replication::StateChangeEvent {
        event_id: event.id,
        sequence: event.id.and_then(|id| u64::try_from(id).ok()).unwrap_or(0),
        at_millis: event.received_at.millis(),
        peer,
        kind: event.kind,
        profile: event.profile,
        workspace: event.workspace,
        document: event.document,
        document_name: event.document_name,
        blob_hash: event.blob_hash,
        detail: event.detail,
        items: event.items,
    })
}

fn map_error(error: koru_store_core::Error) -> StoreFailure {
    StoreFailure::new(error.code().as_str()).with_detail(error.message())
}

fn map_document_error(
    error: koru_store_core::Error,
    operation: &'static str,
    workspace: WorkspaceId,
    document: DocumentId,
) -> StoreFailure {
    let detail = format!(
        "operation={operation} workspace={} document={}: {}",
        workspace.to_storage(),
        document.to_storage(),
        error.message()
    );
    StoreFailure::new(error.code().as_str()).with_detail(detail)
}

const fn failed(code: &'static str) -> StoreFailure {
    StoreFailure::new(code)
}

fn peer_object_logs_enabled() -> bool {
    koru_env::switch(koru_env::name::KORU_PEER_OBJECT_LOGS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_device::{DeviceId, RelationshipId, ReplicationEndpointSecret};
    use koru_replication::protocol::PeerFrame;
    use koru_store_core::manifest::Manifest;
    use koru_store_core::model::Timestamp;
    use koru_store_core::ports::UpdateOrigin;
    use koru_store_core::service::Store;
    use koru_store_core::text::Locale;
    use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

    /// Personal data is v2 and travels only to the same person's device
    /// (root-catalog TECHNICAL.final §5.1, §7.2): a Profile- or
    /// Workspace-granted peer is framed no personal unit, may not ask for one,
    /// and a Profile unit still needs its exact Profile grant.
    #[test]
    fn personal_units_need_the_scope_bound_relationship_and_profile_units_their_grant() {
        use koru_replication::protocol::ROOT_SCOPE_NOT_BOUND;
        use koru_store_core::profile_units::ProfileUnitRef;
        use koru_store_core::transport::{SetTransportMode, TransportMode, TransportSubject};
        let at = Timestamp::from_millis(31);
        let mut store = Store::open(
            SqliteCatalog::in_memory().unwrap(),
            SqliteBackend::in_memory(),
        )
        .unwrap();
        let a = store
            .create_profile("A", None, None, Locale::default(), at)
            .unwrap();
        let b = store
            .create_profile("B", None, None, Locale::default(), at)
            .unwrap();
        let workspace = store.create_workspace(b.id, "Held by B", at).unwrap().id;
        for (subject, revision) in [
            (TransportSubject::Profile(a.id), 1),
            (TransportSubject::Profile(b.id), 1),
            (TransportSubject::Workspace(workspace), 1),
        ] {
            store
                .set_transport_mode(SetTransportMode {
                    subject,
                    expected_revision: revision,
                    mode: TransportMode::P2p,
                })
                .unwrap();
        }
        let entity = koru_store_core::EntityRef::new(
            "task",
            workspace,
            DocumentId::new(),
            uuid::Uuid::new_v4(),
        )
        .unwrap();
        store
            .set_personal_attribute(
                b.id,
                &entity,
                "k",
                Some(&koru_store_core::PersonalAttributeValue::Text { text: "v".into() }),
            )
            .unwrap();
        let state = Arc::new(AppState::new(
            store,
            "<memory>".into(),
            "test".into(),
            at.millis(),
        ));
        let adapter = LiveReplicationStore::new(state);
        let endpoint = ReplicationEndpointSecret::generate();
        let mut peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: [a.id].into_iter().collect(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };
        assert!(adapter.root_catalog(&peer).unwrap().personal.is_empty());
        assert_eq!(
            adapter.personal_version(&peer, workspace).unwrap_err().code,
            ROOT_SCOPE_NOT_BOUND
        );
        peer.authorized_workspaces.insert(workspace);
        assert!(adapter.root_catalog(&peer).unwrap().personal.is_empty());
        assert_eq!(
            adapter
                .personal_updates_since(&peer, workspace, &loro::VersionVector::new())
                .unwrap_err()
                .code,
            ROOT_SCOPE_NOT_BOUND
        );
        // No v1 personal address is listed any more, for any grant.
        assert!(
            !adapter
                .profiles(&peer)
                .unwrap()
                .iter()
                .any(|row| matches!(row.unit, ProfileUnitRef::Personal { .. }))
        );
        peer.authorized_profiles.clear();
        assert!(adapter.profiles(&peer).unwrap().is_empty());
        assert_eq!(
            adapter
                .profile_version(&peer, ProfileUnitRef::Private { profile: a.id })
                .unwrap_err()
                .code,
            "unauthorized_profile"
        );
        peer.all_data = true;
        assert_eq!(adapter.root_catalog(&peer).unwrap().personal.len(), 1);
        assert!(adapter.personal_version(&peer, workspace).is_ok());
    }

    #[test]
    fn a_remote_flush_emits_the_normal_durable_commit_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let at = koru_store_core::model::Timestamp::from_millis(1_770_000_000_000);
        let profile = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let workspace = store.create_workspace(profile.id, "W", at).unwrap().id;
        let manifest_id = store.manifest_id(workspace).unwrap();
        // Ignore setup commits; the assertion is about the remote path below.
        store.take_committed();
        let snapshot = store.snapshot(workspace, manifest_id).unwrap();
        let mut replica = Manifest::from_snapshot(&snapshot).unwrap();
        let before = replica.version();
        replica
            .add_document(DocumentId::new(), "Remote", "page", None)
            .unwrap();
        let update = replica.export_updates_since(&before).unwrap();
        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        // §4.4: a Workspace at the seeded Local-only default admits no peer
        // bytes; the person has opened this one.
        open_transfers(
            &state,
            workspace,
            koru_store_core::transport::TransportMode::P2p,
        );
        let adapter = LiveReplicationStore::new(Arc::clone(&state));
        let endpoint = ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: Default::default(),
            authorized_workspaces: [workspace].into_iter().collect(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };

        adapter
            .apply_remote_updates(&peer, workspace, manifest_id, vec![update])
            .unwrap();
        let committed = state.store.lock().unwrap().take_committed();

        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].workspace, workspace);
        assert_eq!(committed[0].document, manifest_id);
        assert!(committed[0].is_manifest);
    }

    // ------------------- §6's peer points, one assertion per row of the table

    /// A real Store, one Workspace holding one blob, and a peer authorized for
    /// it. The mode starts at Cloud so the blob can be seeded through the very
    /// admission path the later rows close.
    fn peer_blob_fixture() -> (
        tempfile::TempDir,
        Arc<AppState>,
        LiveReplicationStore,
        AuthenticatedPeer,
        koru_store_core::ProfileId,
        WorkspaceId,
        koru_store_core::model::Blob,
    ) {
        use koru_store_core::transport::{SetTransportMode, TransportMode, TransportSubject};

        let dir = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let at = Timestamp::from_millis(1_770_000_000_000);
        let profile = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let workspace = store.create_workspace(profile.id, "W", at).unwrap().id;
        // Cloud, so this fixture can put a blob in through `put_remote_blob`
        // and then lower the mode to test what closing it does.
        store
            .set_transport_modes(&[
                SetTransportMode {
                    subject: TransportSubject::Profile(profile.id),
                    expected_revision: 1,
                    mode: TransportMode::Cloud,
                },
                SetTransportMode {
                    subject: TransportSubject::Workspace(workspace),
                    expected_revision: 1,
                    mode: TransportMode::Cloud,
                },
            ])
            .unwrap();
        let bytes = b"a small picture".to_vec();
        let blob = koru_store_core::model::Blob {
            hash: koru_store_core::model::BlobHash::parse(&format!(
                "b3:{}",
                blake3::hash(&bytes).to_hex()
            ))
            .unwrap(),
            size: bytes.len() as u64,
            mime: "image/png".into(),
            filename: None,
            created_at: at,
            attributes: Default::default(),
        };
        store.put_remote_blob(workspace, &blob, &bytes).unwrap();

        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        let adapter = LiveReplicationStore::new(Arc::clone(&state));
        let endpoint = ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: Default::default(),
            authorized_workspaces: [workspace].into_iter().collect(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };
        (dir, state, adapter, peer, profile.id, workspace, blob)
    }

    /// Arm this device's whole ladder for one Workspace — the Profile and every
    /// Workspace under it, each at its own revision.
    ///
    /// On a real device a person does this by answering the Profile prompt,
    /// which reaches `Store::choose_transport_mode` (D258). A fixture that is
    /// about the *exchange* arms it directly rather than driving a dialog to
    /// get there.
    fn open_transfers(
        state: &AppState,
        workspace: WorkspaceId,
        mode: koru_store_core::transport::TransportMode,
    ) {
        use koru_store_core::transport::{SetTransportMode, TransportSubject};
        let mut store = state.store.lock().unwrap();
        let owner = store
            .transport_mode(TransportSubject::Workspace(workspace))
            .unwrap()
            .owner;
        let group = store.transport_mode_group(owner).unwrap();
        let mut requests = vec![SetTransportMode {
            subject: group.profile.subject,
            expected_revision: group.profile.revision,
            mode,
        }];
        for child in &group.children {
            requests.push(SetTransportMode {
                subject: child.subject,
                expected_revision: child.revision,
                mode,
            });
        }
        store.set_transport_modes(&requests).unwrap();
    }

    fn choose(
        state: &AppState,
        subject: koru_store_core::transport::TransportSubject,
        revision: i64,
        mode: koru_store_core::transport::TransportMode,
    ) {
        state
            .store
            .lock()
            .unwrap()
            .set_transport_mode(koru_store_core::transport::SetTransportMode {
                subject,
                expected_revision: revision,
                mode,
            })
            .unwrap();
    }

    /// **Cloud and P2P both admit the peer path**, which is §6's table read
    /// across: the peer column is the same for the two upper rungs, and the
    /// rung below it is what changes.
    #[test]
    fn a_p2p_or_cloud_workspace_advertises_serves_and_admits_over_a_peer() {
        use koru_store_core::transport::{TransportMode, TransportSubject};
        let (_dir, state, adapter, peer, _profile, workspace, blob) = peer_blob_fixture();

        for mode in [TransportMode::Cloud, TransportMode::P2p] {
            let revision = state
                .store
                .lock()
                .unwrap()
                .transport_mode(TransportSubject::Workspace(workspace))
                .unwrap()
                .revision;
            choose(
                &state,
                TransportSubject::Workspace(workspace),
                revision,
                mode,
            );

            let inventory = adapter.inventory(&peer, workspace).unwrap();
            assert_eq!(inventory.blobs.len(), 1, "{mode:?}");
            let chunk = adapter
                .read_blob_chunk(&peer, workspace, &blob.hash, 0, 64 * 1024)
                .unwrap();
            assert!(chunk.complete, "{mode:?}");
            adapter
                .store_remote_blob(&peer, workspace, &blob, b"a small picture".to_vec())
                .unwrap();
            adapter.admit_peer_blob_transfer(workspace).unwrap();
        }
    }

    /// **A Local-only Workspace advertises nothing, serves nothing, admits
    /// nothing** — its documents included since protocol 6 (root-catalog
    /// TECHNICAL.final §3.5, §4.4: nothing about the subject enters or leaves
    /// this device over the peer path). Still not a broken adapter: the
    /// refusal has the blob contract's own name.
    #[test]
    fn a_local_only_workspace_advertises_serves_and_admits_no_blob() {
        use koru_store_core::transport::{TransportMode, TransportSubject};
        let (_dir, state, adapter, peer, profile, workspace, blob) = peer_blob_fixture();
        // The Profile first: a child cannot sit above its ceiling, and a
        // downgrade with a higher child is refused as one group.
        state
            .store
            .lock()
            .unwrap()
            .set_transport_modes(&[
                koru_store_core::transport::SetTransportMode {
                    subject: TransportSubject::Profile(profile),
                    expected_revision: 2,
                    mode: TransportMode::LocalOnly,
                },
                koru_store_core::transport::SetTransportMode {
                    subject: TransportSubject::Workspace(workspace),
                    expected_revision: 2,
                    mode: TransportMode::LocalOnly,
                },
            ])
            .unwrap();

        // 1. Advertising: not listed, not served as an inventory at all.
        assert_eq!(
            adapter.inventory(&peer, workspace).unwrap_err().code,
            koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str()
        );
        assert!(adapter.catalog(&peer).unwrap().is_empty());
        assert_eq!(
            adapter.admit_peer_subject(workspace).unwrap_err().code,
            koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str()
        );

        // 3. Serving.
        assert_eq!(
            adapter
                .read_blob_chunk(&peer, workspace, &blob.hash, 0, 64 * 1024)
                .unwrap_err()
                .code,
            koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str()
        );

        // 5. Admitting.
        assert_eq!(
            adapter
                .store_remote_blob(&peer, workspace, &blob, b"a small picture".to_vec())
                .unwrap_err()
                .code,
            koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str()
        );

        // 2. Requesting — the point the transport asks about before a frame
        // leaves this device.
        assert_eq!(
            adapter
                .admit_peer_blob_transfer(workspace)
                .unwrap_err()
                .code,
            koru_cloud_blob::BlobCode::ModeRefusesTransport.as_str()
        );
    }

    /// **The same rule stated as the privacy rule it is** (root-catalog
    /// TECHNICAL.final §3.5 decision 5, §4.4): a Local-only Workspace is not
    /// *described* to a peer — not its name, not its placement Profile's
    /// identity, not its manifest id, not its document list — and the
    /// strongest grant there is does not change that, because `all_data` is
    /// authorization and not a transport-policy override.
    ///
    /// The paired assertion on the phone's adapter is
    /// `koru_mobile_host`'s `a_local_only_workspace_is_described_to_no_peer`;
    /// both go through `root_exchange::admit_peer_description` so the two
    /// answering adapters cannot keep different halves of the rule.
    #[test]
    fn a_local_only_workspace_is_described_to_no_peer() {
        use koru_store_core::transport::TransportMode;
        let (_dir, state, adapter, _uuid_peer, profile, workspace, _blob) = peer_blob_fixture();
        let document = state
            .store
            .lock()
            .unwrap()
            .create_document(
                workspace,
                "Plan",
                "page",
                None,
                None,
                Timestamp::from_millis(1_770_000_000_001),
            )
            .unwrap()
            .0;
        let endpoint = ReplicationEndpointSecret::generate();
        let all_data = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: [profile].into_iter().collect(),
            authorized_workspaces: [workspace].into_iter().collect(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };

        // Open first, so the assertions below are about the mode and not
        // about a fixture that never worked: the full description crosses.
        open_transfers(&state, workspace, TransportMode::P2p);
        let described = adapter.inventory(&all_data, workspace).unwrap();
        assert_eq!(described.replica.workspace_name, "W");
        assert!(
            described
                .documents
                .iter()
                .any(|entry| entry.document_id == document)
        );
        assert_eq!(adapter.catalog(&all_data).unwrap().len(), 1);
        assert!(!adapter.profiles(&all_data).unwrap().is_empty());

        // Now the person closes it. Nothing about the subject leaves.
        open_transfers(&state, workspace, TransportMode::LocalOnly);
        assert!(
            adapter.catalog(&all_data).unwrap().is_empty(),
            "withheld by absence: a peer must not learn there is something it is not told"
        );
        assert!(adapter.profiles(&all_data).unwrap().is_empty());
        assert_eq!(
            adapter.inventory(&all_data, workspace).unwrap_err().code,
            koru_replication::protocol::MODE_REFUSES_TRANSPORT
        );
    }

    /// The Profile is a ceiling over its Workspaces at every one of the peer
    /// points, not only at the one a caller remembered to check.
    #[test]
    fn a_local_only_profile_closes_a_cloud_workspace_beneath_it() {
        use koru_store_core::transport::{TransportMode, TransportSubject};
        let (_dir, state, adapter, peer, profile, workspace, blob) = peer_blob_fixture();
        state
            .store
            .lock()
            .unwrap()
            .set_transport_modes(&[
                koru_store_core::transport::SetTransportMode {
                    subject: TransportSubject::Profile(profile),
                    expected_revision: 2,
                    mode: TransportMode::LocalOnly,
                },
                koru_store_core::transport::SetTransportMode {
                    subject: TransportSubject::Workspace(workspace),
                    expected_revision: 2,
                    mode: TransportMode::LocalOnly,
                },
            ])
            .unwrap();
        // The Workspace is raised back on its own; the Profile is not. An
        // explicit child above its Profile is refused, so this is the only way
        // the mismatch can arise — which is why the check is the effective
        // mode rather than the stored one.
        assert!(
            state
                .store
                .lock()
                .unwrap()
                .set_transport_mode(koru_store_core::transport::SetTransportMode {
                    subject: TransportSubject::Workspace(workspace),
                    expected_revision: 3,
                    mode: TransportMode::Cloud,
                })
                .is_err()
        );

        assert!(
            adapter.inventory(&peer, workspace).is_err(),
            "the Profile ceiling closes the Workspace's whole peer path"
        );
        assert!(
            adapter
                .read_blob_chunk(&peer, workspace, &blob.hash, 0, 64 * 1024)
                .is_err()
        );
    }

    #[test]
    fn scoped_desktop_adapter_hides_unknown_and_unrelated_workspaces_alike() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let at = Timestamp::from_millis(1_770_000_000_000);
        let allowed_profile = store
            .create_profile("Allowed", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let unrelated_profile = store
            .create_profile("Private", None, None, Locale::parse("fr").unwrap(), at)
            .unwrap();
        let unrelated = store
            .create_workspace(unrelated_profile.id, "Private workspace", at)
            .unwrap()
            .id;
        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        let adapter = LiveReplicationStore::new(state);
        let endpoint = ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: [allowed_profile.id].into_iter().collect(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };

        let unrelated_error = adapter.inventory(&peer, unrelated).unwrap_err();
        let unknown_error = adapter.inventory(&peer, WorkspaceId::new()).unwrap_err();
        assert_eq!(unrelated_error.code, "unauthorized_workspace");
        assert_eq!(unknown_error.code, unrelated_error.code);
    }

    #[test]
    fn two_real_stores_exchange_independent_offline_edits_and_converge() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_catalog = SqliteCatalog::open(&source_dir.path().join("catalog.db")).unwrap();
        let mut source_store =
            Store::open(source_catalog, SqliteBackend::new(source_dir.path())).unwrap();
        let at = koru_store_core::model::Timestamp::from_millis(1_770_000_000_000);
        let profile = source_store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        source_store
            .set_profile_appearance(profile.id, Some("🔬"), Some("teal"))
            .unwrap();
        let workspace = source_store
            .create_workspace(profile.id, "Shared", at)
            .unwrap()
            .id;
        let (document, _) = source_store
            .create_document(workspace, "Plan", "page", None, None, at)
            .unwrap();
        let source_state = Arc::new(AppState::new(
            source_store,
            source_dir.path().display().to_string(),
            "source".into(),
            at.millis(),
        ));
        let source = LiveReplicationStore::new(Arc::clone(&source_state));
        // **Both devices are paired, so both admit a peer blob transfer.**
        // Without this they stand where migration 12 left them - Local-only -
        // and this exchange is exactly what §6 closes there.
        open_transfers(
            &source_state,
            workspace,
            koru_store_core::transport::TransportMode::P2p,
        );
        let endpoint = ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Default::default(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };
        assert_eq!(
            source.document_name(&peer, workspace, document).as_deref(),
            Some("Plan")
        );
        let blob_bytes = vec![0x5a; 1024 * 1024 + 17];
        let blob = source_state
            .store
            .lock()
            .unwrap()
            .put_blob(
                workspace,
                &blob_bytes,
                "application/octet-stream",
                Some("offline.bin"),
                at,
                None,
            )
            .unwrap();

        let target_dir = tempfile::tempdir().unwrap();
        let target_catalog = SqliteCatalog::open(&target_dir.path().join("catalog.db")).unwrap();
        let target_store =
            Store::open(target_catalog, SqliteBackend::new(target_dir.path())).unwrap();
        let target_state = Arc::new(AppState::new(
            target_store,
            target_dir.path().display().to_string(),
            "target".into(),
            at.millis(),
        ));
        let target = LiveReplicationStore::new(Arc::clone(&target_state));
        let mut catalog_changes = target_state.subscribe_catalog_changes();
        assert_eq!(*catalog_changes.borrow_and_update(), 0);

        let inventory = source.inventory(&peer, workspace).unwrap();
        assert_eq!(inventory.blobs, [blob.clone()]);
        assert_eq!(inventory.replica.profile_icon.as_deref(), Some("🔬"));
        assert_eq!(inventory.replica.profile_color.as_deref(), Some("teal"));
        // Protocol 6: the same person's device learns placement from the
        // root units and the Profile from its identity unit before any
        // content is provisioned; a descriptor alone is pending.
        assert_eq!(
            target
                .provision_replica(&peer, &inventory.replica)
                .unwrap_err()
                .code,
            koru_replication::protocol::PLACEMENT_PENDING
        );
        assert!(target.bind_root_scope(&peer).unwrap());
        for remote in source.root_catalog(&peer).unwrap().units {
            let bytes = source
                .root_unit_updates_since(&peer, remote.unit, &loro::VersionVector::new())
                .unwrap();
            target
                .apply_remote_root_updates(&peer, remote.unit, vec![bytes], None)
                .unwrap();
        }
        for state in source.profiles(&peer).unwrap() {
            if matches!(
                state.unit,
                koru_store_core::profile_units::ProfileUnitRef::Identity { .. }
            ) {
                target.provision_profile(&peer, &state).unwrap();
            }
        }
        let _ = catalog_changes.borrow_and_update();
        target.provision_replica(&peer, &inventory.replica).unwrap();
        assert!(catalog_changes.has_changed().unwrap());
        let _ = catalog_changes.borrow_and_update();
        target.provision_replica(&peer, &inventory.replica).unwrap();
        assert!(
            !catalog_changes.has_changed().unwrap(),
            "an idempotent inventory must not wake an idle catalog reader"
        );
        let replicated_profile = target_state
            .store
            .lock()
            .unwrap()
            .profile(profile.id)
            .unwrap();
        assert_eq!(replicated_profile.icon.as_deref(), Some("🔬"));
        assert_eq!(replicated_profile.color.as_deref(), Some("teal"));
        // The receiving device has its own rows, seeded Local-only by the same
        // migration. A materialization on a second device initializes that
        // device's own choices, which is A1 §2 - so it is armed here too.
        open_transfers(
            &target_state,
            workspace,
            koru_store_core::transport::TransportMode::P2p,
        );
        for remote in &inventory.documents {
            if !remote.is_manifest {
                target
                    .provision_document(&peer, workspace, remote.document_id)
                    .unwrap();
            }
            exchange_document(&source, &target, &peer, workspace, remote.document_id);
        }
        let mut hydrated = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = source
                .read_blob_chunk(&peer, workspace, &blob.hash, offset, 64 * 1024)
                .unwrap();
            offset += chunk.bytes.len() as u64;
            hydrated.extend_from_slice(&chunk.bytes);
            if chunk.complete {
                break;
            }
        }
        assert_eq!(hydrated, blob_bytes);
        target
            .store_remote_blob(&peer, workspace, &blob, hydrated)
            .unwrap();
        assert_eq!(
            target_state
                .store
                .lock()
                .unwrap()
                .get_blob(workspace, &blob.hash)
                .unwrap(),
            blob_bytes
        );

        edit_text(&source_state, workspace, document, 101, "windows");
        edit_text(&target_state, workspace, document, 202, "mobile");

        exchange_document(&source, &target, &peer, workspace, document);
        exchange_document(&target, &source, &peer, workspace, document);

        let source_version = source.version(&peer, workspace, document).unwrap();
        let target_version = target.version(&peer, workspace, document).unwrap();
        assert_eq!(source_version, target_version);
        let source_text = text(&source_state, workspace, document);
        let target_text = text(&target_state, workspace, document);
        assert_eq!(source_text, target_text);
        assert!(source_text.contains("windows"));
        assert!(source_text.contains("mobile"));

        // Lifecycle is a root fact since catalog 13: the Trash travels in
        // W's unit, and the manifest's deletion hint is ignored by a device
        // that holds the unit.
        let manifest = inventory.replica.manifest_document;
        let unit = koru_store_core::root_catalog::CatalogUnitRef::Workspace { workspace };
        let exchange_root = || {
            let local =
                loro::VersionVector::decode(&target.root_unit_version(&peer, unit).unwrap())
                    .unwrap();
            let bytes = source.root_unit_updates_since(&peer, unit, &local).unwrap();
            target
                .apply_remote_root_updates(&peer, unit, vec![bytes], None)
                .unwrap();
        };
        source_state
            .store
            .lock()
            .unwrap()
            .delete_workspace(workspace, Timestamp::from_millis(at.millis() + 1))
            .unwrap();
        exchange_document(&source, &target, &peer, workspace, manifest);
        assert!(
            target_state
                .store
                .lock()
                .unwrap()
                .workspace_including_deleted(workspace)
                .unwrap()
                .deleted_at
                .is_none(),
            "a manifest hint does not trash a Workspace that has a unit"
        );
        let _ = catalog_changes.borrow_and_update();
        exchange_root();
        assert!(catalog_changes.has_changed().unwrap());
        assert!(
            target_state
                .store
                .lock()
                .unwrap()
                .workspace_including_deleted(workspace)
                .unwrap()
                .deleted_at
                .is_some()
        );

        source_state
            .store
            .lock()
            .unwrap()
            .restore_workspace(workspace)
            .unwrap();
        let _ = catalog_changes.borrow_and_update();
        exchange_root();
        assert!(catalog_changes.has_changed().unwrap());
        assert!(
            target_state
                .store
                .lock()
                .unwrap()
                .workspace_including_deleted(workspace)
                .unwrap()
                .deleted_at
                .is_none()
        );
    }

    #[test]
    fn device_history_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let at = koru_store_core::model::Timestamp::from_millis(1_770_000_000_000);
        let peer = DeviceId::new();
        {
            let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
            let store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
            let state = Arc::new(AppState::new(
                store,
                dir.path().display().to_string(),
                "test".into(),
                at.millis(),
            ));
            let adapter = LiveReplicationStore::new(state);
            let stored = adapter
                .record_state_change(
                    &koru_replication::StateChangeEvent {
                        event_id: None,
                        sequence: 1,
                        at_millis: at.millis(),
                        peer,
                        kind: "document_updated".into(),
                        profile: None,
                        workspace: None,
                        document: None,
                        document_name: Some("Roadmap".into()),
                        blob_hash: None,
                        detail: None,
                        items: Some(3),
                    },
                    at.millis() - 1,
                )
                .unwrap();
            assert!(stored.event_id.is_some());
        }

        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        let adapter = LiveReplicationStore::new(state);
        let history = adapter
            .state_changes(peer, at.millis() - 1, None, 50)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].document_name.as_deref(), Some("Roadmap"));
        assert_eq!(history[0].items, Some(3));
        assert_eq!(
            adapter.latest_state_change_times(at.millis() - 1).unwrap(),
            vec![(peer, at.millis())]
        );
    }

    fn exchange_document(
        source: &LiveReplicationStore,
        target: &LiveReplicationStore,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) {
        let version = target.version(peer, workspace, document).unwrap();
        let room_id = if target.manifest_id(peer, workspace).unwrap() == document {
            format!("workspace:{workspace}:manifest")
        } else {
            format!("document:{document}")
        };
        let request = loro_protocol::protocol::ProtocolMessage::JoinRequest {
            crdt: loro_protocol::protocol::CrdtType::Loro,
            room_id,
            version,
            auth: Vec::new(),
        };
        let request = loro_protocol::encoding::encode(&request).unwrap();
        let responses = koru_replication::protocol::handle_frame(
            source,
            peer,
            PeerFrame::Loro {
                workspace_id: workspace,
                message: request,
            },
        );
        for response in responses {
            let _ = koru_replication::protocol::handle_frame(target, peer, response);
        }
    }

    fn edit_text(
        state: &Arc<AppState>,
        workspace: WorkspaceId,
        document: DocumentId,
        peer_id: u64,
        value: &str,
    ) {
        let mut store = state.store.lock().unwrap();
        let base = store.version(workspace, document).unwrap();
        let live = loro::LoroDoc::new();
        live.import(&store.snapshot(workspace, document).unwrap())
            .unwrap();
        live.set_peer_id(peer_id).unwrap();
        live.get_text("t").insert(0, value).unwrap();
        live.commit();
        let update = live.export(loro::ExportMode::updates(&base)).unwrap();
        store
            .apply_update(
                workspace,
                document,
                update,
                Timestamp::now(),
                UpdateOrigin::Store,
            )
            .unwrap();
        store.flush(workspace, document, Timestamp::now()).unwrap();
    }

    fn text(state: &Arc<AppState>, workspace: WorkspaceId, document: DocumentId) -> String {
        state
            .store
            .lock()
            .unwrap()
            .fork_document(workspace, document)
            .unwrap()
            .get_text("t")
            .to_string()
    }

    #[test]
    fn inventory_skips_a_manifest_entry_that_has_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let at = koru_store_core::model::Timestamp::from_millis(1_770_000_000_000);
        let profile = store
            .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let workspace = store.create_workspace(profile.id, "W", at).unwrap().id;
        let (document, _) = store
            .create_document(workspace, "Real", "page", None, None, at)
            .unwrap();

        // A manifest entry whose content never arrived — the phantom that
        // used to take the whole workspace's inventory down with
        // document_not_found on every peer.
        let phantom = DocumentId::new();
        let manifest_id = store.manifest_id(workspace).unwrap();
        let snapshot = store.snapshot(workspace, manifest_id).unwrap();
        let mut manifest = Manifest::from_snapshot(&snapshot).unwrap();
        let before = manifest.version();
        manifest
            .add_document_at(phantom, "Ghost", "page", None, None)
            .unwrap();
        let update = manifest.export_updates_since(&before).unwrap();
        store
            .apply_update(workspace, manifest_id, update, at, None)
            .unwrap();
        store.flush(workspace, manifest_id, at).unwrap();

        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        open_transfers(
            &state,
            workspace,
            koru_store_core::transport::TransportMode::P2p,
        );
        let adapter = LiveReplicationStore::new(Arc::clone(&state));
        let endpoint = ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: true,
            authorized_profiles: Default::default(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };
        let inventory = adapter
            .inventory(&peer, workspace)
            .expect("a phantom entry must not fail the inventory");
        let ids: Vec<_> = inventory
            .documents
            .iter()
            .map(|document| document.document_id)
            .collect();
        assert!(ids.contains(&document));
        assert!(!ids.contains(&phantom));
    }
    #[test]
    fn moving_a_workspace_recomputes_profile_grants_without_identity_heuristics() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
        let mut store = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
        let at = Timestamp::from_millis(1_770_000_000_000);
        let from = store
            .create_profile("Same person", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let to = store
            .create_profile("Same person", None, None, Locale::parse("en").unwrap(), at)
            .unwrap();
        let workspace = store.create_workspace(from.id, "Move me", at).unwrap().id;
        // Both Profiles open on this device: the grant, not the mode, is
        // what this test turns on.
        for profile in [from.id, to.id] {
            let group = store.transport_mode_group(profile).unwrap();
            let mut requests = vec![koru_store_core::transport::SetTransportMode {
                subject: group.profile.subject,
                expected_revision: group.profile.revision,
                mode: koru_store_core::transport::TransportMode::P2p,
            }];
            for child in &group.children {
                requests.push(koru_store_core::transport::SetTransportMode {
                    subject: child.subject,
                    expected_revision: child.revision,
                    mode: koru_store_core::transport::TransportMode::P2p,
                });
            }
            store.set_transport_modes(&requests).unwrap();
        }
        let state = Arc::new(AppState::new(
            store,
            dir.path().display().to_string(),
            "test".into(),
            at.millis(),
        ));
        let adapter = LiveReplicationStore::new(Arc::clone(&state));
        let endpoint = ReplicationEndpointSecret::generate();
        let scoped = |profile| AuthenticatedPeer {
            relationship_id: RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: [profile].into_iter().collect(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: Some("same@example.com".into()),
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };
        let from_peer = scoped(from.id);
        let to_peer = scoped(to.id);

        assert!(adapter.inventory(&from_peer, workspace).is_ok());
        assert_eq!(
            adapter.inventory(&to_peer, workspace).unwrap_err().code,
            "unauthorized_workspace"
        );

        state
            .store
            .lock()
            .unwrap()
            .move_workspace(workspace, to.id)
            .unwrap();

        assert_eq!(
            adapter.inventory(&from_peer, workspace).unwrap_err().code,
            "unauthorized_workspace",
            "the old owner grant must be revoked on the next data-plane call"
        );
        assert!(
            adapter.inventory(&to_peer, workspace).is_ok(),
            "only an explicit grant for the new owner may authorize the workspace"
        );
    }
}

#[cfg(test)]
mod root_tests;
