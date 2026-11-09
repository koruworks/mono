// SPDX-License-Identifier: MPL-2.0

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use koru_device::{DeviceSigningKey, ReplicationEndpointSecret};
use loro_protocol::protocol::{BatchId, CrdtType, ProtocolMessage, UpdateStatusCode};

use super::*;

struct FakeStore {
    workspace: WorkspaceId,
    document: DocumentId,
    events: Mutex<Vec<&'static str>>,
    applied: Mutex<Vec<Vec<u8>>>,
    fail_commit: bool,
}

impl ReplicationStore for FakeStore {
    fn profile_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        unit: ProfileUnitRef,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_authorized_profile_unit(unit, || {
            self.events.lock().unwrap().push("profile_export");
            Ok(vec![7])
        })
    }
    fn profiles(&self, peer: &AuthenticatedPeer) -> Result<Vec<ProfileReplicaState>, StoreFailure> {
        let profile_id = ProfileId::from_uuid(self.workspace.as_uuid());
        peer.with_catalog_scope(|| {
            if !peer.all_data && !peer.authorized_profiles.contains(&profile_id) {
                return Ok(Vec::new());
            }
            Ok(vec![ProfileReplicaState {
                unit: ProfileUnitRef::Identity {
                    profile: profile_id,
                },
                version: loro::VersionVector::new().encode(),
                identity: Some(ProfileIdentityState {
                    name: "PRIVATE PROFILE NAME".into(),
                    purpose: Some("PRIVATE PURPOSE".into()),
                    avatar: Some("PRIVATE AVATAR".into()),
                    icon: Some("PRIVATE ICON".into()),
                    color: Some("PRIVATE COLOR".into()),
                    account: Some("PRIVATE ACCOUNT".into()),
                    locale: "PRIVATE LOCALE".into(),
                    created_at_ms: 42,
                    kind: "normal".into(),
                }),
            }])
        })
    }

    fn catalog(
        &self,
        peer: &AuthenticatedPeer,
    ) -> Result<Vec<WorkspaceCatalogState>, StoreFailure> {
        let profile = ProfileId::from_uuid(self.workspace.as_uuid());
        peer.with_catalog_scope(|| {
            Ok((peer.all_data
                || peer.authorized_profiles.contains(&profile)
                || peer.authorized_workspaces.contains(&self.workspace))
            .then_some(WorkspaceCatalogState {
                workspace_id: self.workspace,
                profile_id: profile,
                digest: vec![1],
            })
            .into_iter()
            .collect())
        })
    }
    fn bind_root_scope(&self, peer: &AuthenticatedPeer) -> Result<bool, StoreFailure> {
        Ok(peer.binds_root_scope())
    }
    /// The eligible root inventory as the rule reads it: the Workspace unit
    /// for every grant that covers the Workspace, the Profile unit only for
    /// a grant that may know the Profile, the personal unit only for the
    /// same person's device. Sentinel-free: ids and versions, nothing else.
    fn root_catalog(&self, peer: &AuthenticatedPeer) -> Result<RootCatalogInventory, StoreFailure> {
        let profile = ProfileId::from_uuid(self.workspace.as_uuid());
        peer.with_catalog_scope(|| {
            let mut inventory = RootCatalogInventory::default();
            if peer.all_data || peer.authorized_profiles.contains(&profile) {
                inventory.units.push(RootUnitState {
                    unit: CatalogUnitRef::Profile { profile },
                    version: loro::VersionVector::new().encode(),
                });
            }
            if peer.all_data
                || peer.authorized_profiles.contains(&profile)
                || peer.authorized_workspaces.contains(&self.workspace)
            {
                inventory.units.push(RootUnitState {
                    unit: CatalogUnitRef::Workspace {
                        workspace: self.workspace,
                    },
                    version: loro::VersionVector::new().encode(),
                });
            }
            if peer.all_data {
                inventory.personal.push(PersonalUnitState {
                    workspace: self.workspace,
                    version: loro::VersionVector::new().encode(),
                });
            }
            Ok(inventory)
        })
    }
    fn root_unit_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        _: CatalogUnitRef,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_catalog_scope(|| {
            self.events.lock().unwrap().push("root_export");
            Ok(vec![9])
        })
    }
    fn personal_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        _: WorkspaceId,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_root_scope(|| {
            self.events.lock().unwrap().push("personal_export");
            Ok(vec![10])
        })
    }
    fn workspace_profile(&self, workspace: WorkspaceId) -> Result<ProfileId, StoreFailure> {
        (workspace == self.workspace)
            .then(|| ProfileId::from_uuid(workspace.as_uuid()))
            .ok_or_else(|| StoreFailure::new("workspace_not_found"))
    }
    fn provision_replica(
        &self,
        _: &AuthenticatedPeer,
        _: &ReplicaDescriptor,
    ) -> Result<(), StoreFailure> {
        Ok(())
    }
    fn provision_document(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<(), StoreFailure> {
        Ok(())
    }
    fn inventory(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceInventory, StoreFailure> {
        let profile = self.workspace_profile(workspace)?;
        peer.with_authorized_workspace_profile(workspace, profile, || {
            self.events.lock().unwrap().push("inventory");
            Ok(WorkspaceInventory {
                workspace_id: workspace,
                replica: {
                    let mut replica = ReplicaDescriptor {
                        profile_id: profile,
                        profile_name: "PRIVATE PROFILE NAME".into(),
                        profile_locale: "PRIVATE LOCALE".into(),
                        profile_created_at_ms: 42,
                        profile_icon: Some("PRIVATE ICON".into()),
                        profile_color: Some("PRIVATE COLOR".into()),
                        profile_kind: "normal".into(),
                        workspace_id: workspace,
                        workspace_name: "Workspace".into(),
                        workspace_created_at_ms: 1,
                        workspace_icon: None,
                        workspace_color: None,
                        workspace_pinned: false,
                        workspace_archived: false,
                        workspace_deleted_at_ms: None,
                        manifest_document: DocumentId::new(),
                    };
                    if !peer.all_data {
                        replica.redact_profile_metadata();
                    }
                    replica
                },
                documents: Vec::new(),
                blobs: Vec::new(),
            })
        })
    }
    fn locate(
        &self,
        peer: &AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, StoreFailure> {
        peer.with_authorized_workspace_profile(
            self.workspace,
            ProfileId::from_uuid(self.workspace.as_uuid()),
            || Ok((document == self.document).then_some(self.workspace)),
        )
    }
    fn manifest_id(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<DocumentId, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || Ok(DocumentId::new()),
        )
    }
    fn version(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        _: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || {
                // Non-empty, so a joining peer that sends less is genuinely behind.
                let doc = loro::LoroDoc::new();
                doc.set_peer_id(1).unwrap();
                doc.get_text("t").insert(0, "x").unwrap();
                doc.commit();
                Ok(doc.oplog_vv().encode())
            },
        )
    }
    fn updates_since(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        _: DocumentId,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || {
                self.events.lock().unwrap().push("updates_since");
                Ok(vec![0xAA; 8])
            },
        )
    }
    fn snapshot(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        _: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || {
                self.events.lock().unwrap().push("snapshot");
                Ok(vec![0xBB; 8])
            },
        )
    }
    fn read_blob_chunk(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: &BlobHash,
        _: u64,
        _: u32,
    ) -> Result<BlobChunk, StoreFailure> {
        Err(StoreFailure::new("blob_not_found"))
    }
    fn store_remote_blob(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: &Blob,
        _: Vec<u8>,
    ) -> Result<(), StoreFailure> {
        Ok(())
    }
    fn apply_remote_updates(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        _: DocumentId,
        updates: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || {
                self.events.lock().unwrap().push("apply");
                if self.fail_commit {
                    return Err(StoreFailure::new("storage_failed"));
                }
                self.events.lock().unwrap().push("flush");
                self.applied.lock().unwrap().extend(updates);
                Ok(DurableAck {
                    durable_sequence: Some(7),
                })
            },
        )
    }
}

#[derive(Default)]
struct OperationGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl OperationGate {
    fn enter_and_wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn wait_until_entered(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.0 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
}

struct BlockingStore {
    workspace: WorkspaceId,
    document: DocumentId,
    gate: Arc<OperationGate>,
}

impl ReplicationStore for BlockingStore {
    fn workspace_profile(&self, workspace: WorkspaceId) -> Result<ProfileId, StoreFailure> {
        (workspace == self.workspace)
            .then(|| ProfileId::from_uuid(workspace.as_uuid()))
            .ok_or_else(|| StoreFailure::new("workspace_not_found"))
    }

    fn provision_replica(
        &self,
        _: &AuthenticatedPeer,
        _: &ReplicaDescriptor,
    ) -> Result<(), StoreFailure> {
        Ok(())
    }

    fn provision_document(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<(), StoreFailure> {
        Ok(())
    }

    fn inventory(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
    ) -> Result<WorkspaceInventory, StoreFailure> {
        Err(StoreFailure::new("not_used"))
    }

    fn locate(
        &self,
        peer: &AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, StoreFailure> {
        peer.with_authorized_workspace_profile(
            self.workspace,
            ProfileId::from_uuid(self.workspace.as_uuid()),
            || Ok((document == self.document).then_some(self.workspace)),
        )
    }

    fn manifest_id(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
    ) -> Result<DocumentId, StoreFailure> {
        Ok(DocumentId::new())
    }

    fn version(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        Ok(Vec::new())
    }

    fn updates_since(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: DocumentId,
        _: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        Ok(Vec::new())
    }

    fn read_blob_chunk(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: &BlobHash,
        _: u64,
        _: u32,
    ) -> Result<BlobChunk, StoreFailure> {
        Err(StoreFailure::new("not_used"))
    }

    fn store_remote_blob(
        &self,
        _: &AuthenticatedPeer,
        _: WorkspaceId,
        _: &Blob,
        _: Vec<u8>,
    ) -> Result<(), StoreFailure> {
        Err(StoreFailure::new("not_used"))
    }

    fn apply_remote_updates(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        _: DocumentId,
        _: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        peer.with_authorized_workspace_profile(
            workspace,
            self.workspace_profile(workspace)?,
            || {
                self.gate.enter_and_wait();
                Ok(DurableAck {
                    durable_sequence: Some(1),
                })
            },
        )
    }
}

#[test]
fn all_data_peer_can_discover_workspace_ids_before_having_a_local_catalog() {
    let workspace = WorkspaceId::new();
    let store = FakeStore {
        workspace,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let mut allowed = peer(workspace);
    allowed.all_data = true;
    allowed.authorized_workspaces.clear();
    let profiles = store.profiles(&allowed).unwrap();
    let workspaces = store.catalog(&allowed).unwrap();
    assert_eq!(
        handle_frame(&store, &allowed, PeerFrame::CatalogRequest),
        vec![PeerFrame::Catalog {
            digest: catalog_digest(&allowed, &profiles, &workspaces).unwrap(),
            profiles,
            workspaces,
        }]
    );

    let mut denied = peer(workspace);
    denied.authorized_workspaces.clear();
    assert_eq!(
        handle_frame(&store, &denied, PeerFrame::CatalogRequest),
        vec![PeerFrame::Error {
            code: "unauthorized_catalog".into()
        }]
    );
}

#[test]
fn success_ack_exists_only_after_the_durable_store_call() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let peer = peer(workspace);
    let message = ProtocolMessage::DocUpdate {
        crdt: CrdtType::Loro,
        room_id: format!("document:{}", document.to_storage()),
        updates: vec![vec![1, 2, 3]],
        batch_id: BatchId([4; 8]),
    };
    let frame = PeerFrame::Loro {
        workspace_id: workspace,
        message: loro_protocol::encoding::encode(&message).unwrap(),
    };
    assert_eq!(
        doc_update_targets(&store, &peer, &frame),
        vec![(workspace, document, 1)]
    );
    let replies = handle_frame(&store, &peer, frame);
    assert_eq!(*store.events.lock().unwrap(), ["apply", "flush"]);
    let PeerFrame::LoroBatch { messages, .. } = &replies[0] else {
        panic!("expected Loro response")
    };
    assert!(matches!(
        loro_protocol::encoding::try_decode(&messages[0]),
        Some(ProtocolMessage::Ack {
            status: UpdateStatusCode::Ok,
            ..
        })
    ));
}

#[test]
fn failed_commit_never_produces_a_success_ack() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: true,
    };
    let message = ProtocolMessage::DocUpdate {
        crdt: CrdtType::Loro,
        room_id: format!("document:{}", document.to_storage()),
        updates: vec![vec![1]],
        batch_id: BatchId([9; 8]),
    };
    let replies = handle_frame(
        &store,
        &peer(workspace),
        PeerFrame::Loro {
            workspace_id: workspace,
            message: loro_protocol::encoding::encode(&message).unwrap(),
        },
    );
    let PeerFrame::LoroBatch { messages, .. } = &replies[0] else {
        panic!("expected Loro response")
    };
    assert!(!matches!(
        loro_protocol::encoding::try_decode(&messages[0]),
        Some(ProtocolMessage::Ack {
            status: UpdateStatusCode::Ok,
            ..
        })
    ));
}

#[test]
fn cross_profile_inventory_blob_and_loro_frames_reveal_nothing() {
    let allowed = WorkspaceId::new();
    let requested = WorkspaceId::new();
    let store = FakeStore {
        workspace: requested,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let hash = BlobHash::from_digest(BlobHash::BLAKE3, &blake3::hash(b"private").to_hex()).unwrap();
    let frames = [
        PeerFrame::InventoryRequest {
            workspace_id: requested,
        },
        PeerFrame::BlobRequest {
            workspace_id: requested,
            hash,
            offset: 0,
            max_bytes: 1,
        },
        PeerFrame::Loro {
            workspace_id: requested,
            message: join_request(store.document, Vec::new()),
        },
    ];
    for frame in frames {
        assert_eq!(
            handle_frame(&store, &peer(allowed), frame),
            vec![PeerFrame::Error {
                code: "unauthorized_workspace".into()
            }]
        );
    }
    assert!(store.events.lock().unwrap().is_empty());
}

#[test]
fn scoped_unknown_and_unrelated_workspace_probes_are_indistinguishable() {
    let existing = WorkspaceId::new();
    let allowed = WorkspaceId::new();
    let unknown = WorkspaceId::new();
    let store = FakeStore {
        workspace: existing,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let peer = peer(allowed);

    let probe =
        |workspace_id| handle_frame(&store, &peer, PeerFrame::InventoryRequest { workspace_id });
    assert_eq!(probe(existing), probe(unknown));
    assert_eq!(
        probe(existing),
        vec![PeerFrame::Error {
            code: "unauthorized_workspace".into(),
        }]
    );
    assert!(store.events.lock().unwrap().is_empty());
}

#[test]
fn profile_scope_exposes_fresh_identity_and_workspace_data_but_refuses_legacy() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::from_uuid(workspace.as_uuid());
    let store = FakeStore {
        workspace,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let mut peer = peer(workspace);
    peer.authorized_workspaces.clear();
    peer.authorized_profiles.insert(profile);

    let catalog = handle_frame(&store, &peer, PeerFrame::CatalogRequest);
    let PeerFrame::Catalog {
        profiles,
        workspaces,
        ..
    } = &catalog[0]
    else {
        panic!("expected scoped catalog")
    };
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].unit, ProfileUnitRef::Identity { profile });
    assert_eq!(
        profiles[0].identity.as_ref().unwrap().name,
        "PRIVATE PROFILE NAME"
    );
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].workspace_id, workspace);

    let inventory = handle_frame(
        &store,
        &peer,
        PeerFrame::InventoryRequest {
            workspace_id: workspace,
        },
    );
    let PeerFrame::Inventory(inventory) = &inventory[0] else {
        panic!("expected authorized inventory")
    };
    assert!(!inventory.replica.has_profile_metadata());
    assert_eq!(inventory.replica.workspace_name, "Workspace");

    assert_eq!(
        handle_frame(
            &store,
            &peer,
            PeerFrame::ProfileRequest {
                unit: ProfileUnitRef::LegacyPrivate { profile },
                version: Vec::new(),
            },
        ),
        vec![PeerFrame::Error {
            code: "profile_metadata_not_authorized".into(),
        }]
    );
}

#[test]
fn catalog_digest_requires_the_same_established_scope() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::new();
    let profiles = [ProfileReplicaState {
        unit: ProfileUnitRef::Private { profile },
        version: loro::VersionVector::new().encode(),
        identity: None,
    }];
    let workspaces = [WorkspaceCatalogState {
        workspace_id: workspace,
        profile_id: profile,
        digest: vec![1, 2, 3],
    }];
    let mut by_profile = peer(workspace);
    by_profile.authorized_workspaces.clear();
    by_profile.authorized_profiles.insert(profile);
    let same_scope = AuthenticatedPeer {
        device_id: DeviceId::new(),
        relationship_id: RelationshipId::new(),
        ..by_profile.clone()
    };
    let by_workspace = peer(workspace);
    let mut empty = peer(workspace);
    empty.authorized_workspaces.clear();

    assert_eq!(
        catalog_digest(&by_profile, &profiles, &workspaces),
        catalog_digest(&same_scope, &profiles, &workspaces)
    );
    assert_ne!(
        catalog_digest(&by_profile, &profiles, &workspaces),
        catalog_digest(&by_workspace, &[], &workspaces)
    );
    assert_eq!(catalog_digest(&empty, &[], &[]), None);
}

#[test]
fn profile_units_require_owner_authority_even_with_exact_workspace_grant() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let units = [
        ProfileUnitRef::Identity { profile: owner },
        ProfileUnitRef::Private { profile: owner },
        ProfileUnitRef::Personal { owner, workspace },
    ];
    let mut peer = peer(workspace);
    for unit in units {
        assert_eq!(
            peer.check_authorized_profile_unit(unit).unwrap_err().code,
            "unauthorized_profile"
        );
    }
    peer.authorized_profiles.insert(ProfileId::new());
    for unit in units {
        assert!(peer.check_authorized_profile_unit(unit).is_err());
    }
    peer.authorized_profiles.insert(owner);
    for unit in units {
        peer.check_authorized_profile_unit(unit).unwrap();
    }
    assert!(
        peer.check_authorized_profile_unit(ProfileUnitRef::LegacyPrivate { profile: owner })
            .is_err()
    );
    peer.all_data = true;
    peer.check_authorized_profile_unit(ProfileUnitRef::LegacyPrivate { profile: owner })
        .unwrap();
    peer.authorization_fence.revoke();
    for unit in units {
        assert_eq!(
            peer.with_authorized_profile_unit(unit, || Ok(()))
                .unwrap_err()
                .code,
            "relationship_revoked"
        );
    }
}

#[test]
fn direct_unit_requests_and_updates_share_admission_before_store_calls() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let store = FakeStore {
        workspace,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let mut peer = peer(workspace);
    for unit in [
        ProfileUnitRef::Private { profile: owner },
        ProfileUnitRef::Personal { owner, workspace },
    ] {
        for frame in [
            PeerFrame::ProfileRequest {
                unit,
                version: loro::VersionVector::new().encode(),
            },
            PeerFrame::ProfileUpdates {
                unit,
                updates: vec![vec![1]],
            },
        ] {
            assert_eq!(
                handle_frame(&store, &peer, frame),
                vec![PeerFrame::Error {
                    code: "unauthorized_profile".into()
                }]
            );
        }
    }
    assert!(store.events.lock().unwrap().is_empty());
    peer.authorized_profiles.insert(owner);
    let unit = ProfileUnitRef::Private { profile: owner };
    assert_eq!(
        handle_frame(
            &store,
            &peer,
            PeerFrame::ProfileRequest {
                unit,
                version: loro::VersionVector::new().encode()
            }
        ),
        vec![PeerFrame::ProfileUpdates {
            unit,
            updates: vec![vec![7]]
        }]
    );
    assert_eq!(*store.events.lock().unwrap(), ["profile_export"]);
    assert_eq!(
        handle_frame(
            &store,
            &peer,
            PeerFrame::ProfileRequest {
                unit,
                version: vec![255]
            }
        ),
        vec![PeerFrame::Error {
            code: "invalid_profile_version".into()
        }]
    );
    assert_eq!(*store.events.lock().unwrap(), ["profile_export"]);
}

#[test]
fn catalog_digest_uses_exact_unit_kind_owner_workspace_and_vector() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::new();
    let mut peer = peer(workspace);
    peer.all_data = true;
    let make = |unit| ProfileReplicaState {
        unit,
        version: loro::VersionVector::new().encode(),
        identity: None,
    };
    let baseline = make(ProfileUnitRef::Private { profile: owner });
    let digest = catalog_digest(&peer, &[baseline.clone()], &[]).unwrap();
    assert_eq!(
        catalog_digest(&peer, &[baseline.clone(), baseline.clone()], &[]),
        None
    );
    for unit in [
        ProfileUnitRef::LegacyPrivate { profile: owner },
        ProfileUnitRef::Private {
            profile: ProfileId::new(),
        },
        ProfileUnitRef::Personal { owner, workspace },
    ] {
        assert_ne!(
            Some(digest.clone()),
            catalog_digest(&peer, &[make(unit)], &[])
        );
    }
    let first = make(ProfileUnitRef::Personal { owner, workspace });
    let second = make(ProfileUnitRef::Personal {
        owner,
        workspace: WorkspaceId::new(),
    });
    assert_ne!(
        catalog_digest(&peer, &[first.clone()], &[]),
        catalog_digest(&peer, &[second.clone()], &[])
    );
    assert_eq!(
        catalog_digest(&peer, &[first.clone(), second.clone()], &[]),
        catalog_digest(&peer, &[second, first], &[])
    );
    let mut changed = baseline;
    let doc = loro::LoroDoc::new();
    doc.get_map("m").insert("key", 1).unwrap();
    doc.commit();
    changed.version = doc.oplog_vv().encode();
    assert_ne!(Some(digest), catalog_digest(&peer, &[changed], &[]));
}

#[test]
fn old_and_unknown_protocols_are_refused_before_catalog_or_payload() {
    let workspace = WorkspaceId::new();
    let store = FakeStore {
        workspace,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let mut peer = peer(workspace);
    peer.all_data = true;
    for version in [None, Some(3), Some(4), Some(5), Some(7)] {
        peer.peer_protocol = version;
        let code = match version {
            Some(7) => "incompatible_protocol",
            // 5 has the unit addresses but writes placement from
            // descriptors and exports personal units on the v1 address.
            Some(5) => ROOT_CATALOG_REQUIRED,
            _ => "profile_units_required",
        };
        assert_eq!(peer.protocol_refusal(), Some(code));
        for request in [PeerFrame::CatalogRequest, PeerFrame::RootCatalogRequest] {
            assert_eq!(
                handle_frame(&store, &peer, request),
                vec![PeerFrame::Error { code: code.into() }]
            );
        }
        assert_eq!(catalog_digest(&peer, &[], &[]), None);
        assert_eq!(root_catalog_digest(&peer, &[], &[]), None);
    }
    assert!(store.events.lock().unwrap().is_empty());
    peer.peer_protocol = Some(PEER_PROTOCOL_VERSION);
    assert_eq!(PEER_PROTOCOL_VERSION, 6);
    assert_eq!(peer.protocol_refusal(), None);
}

#[test]
fn a_revoked_relationship_cannot_admit_an_already_encoded_update() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let peer = peer(workspace);
    let message = ProtocolMessage::DocUpdate {
        crdt: CrdtType::Loro,
        room_id: format!("document:{}", document.to_storage()),
        updates: vec![vec![1, 2, 3]],
        batch_id: BatchId([5; 8]),
    };
    peer.authorization_fence.revoke();

    let replies = handle_frame(
        &store,
        &peer,
        PeerFrame::Loro {
            workspace_id: workspace,
            message: loro_protocol::encoding::encode(&message).unwrap(),
        },
    );

    assert_eq!(
        replies,
        vec![PeerFrame::Error {
            code: "relationship_revoked".into()
        }]
    );
    assert!(store.events.lock().unwrap().is_empty());
}

#[test]
fn revocation_waits_for_an_admitted_durable_operation_then_fences_later_frames() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let gate = Arc::new(OperationGate::default());
    let store = Arc::new(BlockingStore {
        workspace,
        document,
        gate: Arc::clone(&gate),
    });
    let peer = peer(workspace);
    let message = ProtocolMessage::DocUpdate {
        crdt: CrdtType::Loro,
        room_id: format!("document:{}", document.to_storage()),
        updates: vec![vec![1]],
        batch_id: BatchId([7; 8]),
    };
    let frame = PeerFrame::Loro {
        workspace_id: workspace,
        message: loro_protocol::encoding::encode(&message).unwrap(),
    };

    let operation_store = Arc::clone(&store);
    let operation_peer = peer.clone();
    let operation =
        std::thread::spawn(move || handle_frame(&*operation_store, &operation_peer, frame));
    gate.wait_until_entered();

    let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
    let revoking_peer = peer.clone();
    let revocation = std::thread::spawn(move || {
        revoking_peer.authorization_fence.revoke();
        revoked_tx.send(()).unwrap();
    });
    assert!(revoked_rx.recv_timeout(Duration::from_millis(50)).is_err());

    gate.release();
    assert!(matches!(
        &operation.join().unwrap()[0],
        PeerFrame::LoroBatch { .. }
    ));
    revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    revocation.join().unwrap();

    assert_eq!(
        handle_frame(
            &*store,
            &peer,
            PeerFrame::InventoryRequest {
                workspace_id: workspace,
            },
        ),
        vec![PeerFrame::Error {
            code: "relationship_revoked".into(),
        }]
    );
}

/// The protocol refuses to encode any message over 256KB, and the old reply
/// builder silently dropped what would not encode — a manifest whose catch-up
/// outgrew the limit could never bootstrap another device again. An oversized
/// update must travel as fragments and apply as the exact original bytes.
#[test]
fn an_oversized_update_travels_as_fragments_and_applies_as_one() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let room_id = format!("document:{}", document.to_storage());
    let bytes: Vec<u8> = (0..600 * 1024).map(|i| (i % 251) as u8).collect();

    let messages = fragments::doc_update_messages(CrdtType::Loro, &room_id, bytes.clone());
    assert!(messages.len() > 2, "600KB must fragment");
    let encoded: Vec<Vec<u8>> = messages
        .iter()
        .map(|message| {
            loro_protocol::encoding::encode(message).expect("every fragment fits the wire cap")
        })
        .collect();

    let frame = PeerFrame::LoroBatch {
        workspace_id: workspace,
        messages: encoded,
    };
    // The count treats the fragment header as the one update it carries.
    assert_eq!(doc_update_count(&frame), 1);

    let replies = handle_frame(&store, &peer(workspace), frame);
    assert_eq!(
        *store.applied.lock().unwrap(),
        vec![bytes],
        "the reassembled update must be the exact original bytes, applied once"
    );
    let PeerFrame::LoroBatch { messages, .. } = &replies[0] else {
        panic!("expected Loro response")
    };
    assert!(matches!(
        loro_protocol::encoding::try_decode(&messages[0]),
        Some(ProtocolMessage::Ack {
            status: UpdateStatusCode::Ok,
            ..
        })
    ));
}

fn join_request(document: DocumentId, version: Vec<u8>) -> Vec<u8> {
    loro_protocol::encoding::encode(&ProtocolMessage::JoinRequest {
        crdt: CrdtType::Loro,
        room_id: format!("document:{}", document.to_storage()),
        version,
        auth: Vec::new(),
    })
    .unwrap()
}

fn doc_update_payloads(replies: &[PeerFrame]) -> Vec<Vec<Vec<u8>>> {
    let PeerFrame::LoroBatch { messages, .. } = &replies[0] else {
        panic!("expected Loro response")
    };
    messages
        .iter()
        .filter_map(
            |encoded| match loro_protocol::encoding::try_decode(encoded) {
                Some(ProtocolMessage::DocUpdate { updates, .. }) => Some(updates),
                _ => None,
            },
        )
        .collect()
}

/// A brand-new replica has no history to diff against, so it is answered with
/// the compact snapshot encoding, never the whole op journal — the journal of
/// a long-lived manifest is megabytes of replay for state that fits in a
/// fraction of that.
#[test]
fn a_peer_with_no_history_is_answered_with_a_snapshot() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let replies = handle_frame(
        &store,
        &peer(workspace),
        PeerFrame::Loro {
            workspace_id: workspace,
            message: join_request(document, loro::VersionVector::new().encode()),
        },
    );
    assert_eq!(*store.events.lock().unwrap(), ["snapshot"]);
    assert_eq!(doc_update_payloads(&replies), vec![vec![vec![0xBB; 8]]]);
}

/// A replica that has *some* history gets a delta: sending it a snapshot
/// would re-ship everything it already holds.
#[test]
fn a_peer_with_some_history_is_answered_with_a_delta() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    // Non-empty, and from another peer entirely, so it does not include ours.
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(7).unwrap();
    doc.get_text("t").insert(0, "y").unwrap();
    doc.commit();
    let replies = handle_frame(
        &store,
        &peer(workspace),
        PeerFrame::Loro {
            workspace_id: workspace,
            message: join_request(document, doc.oplog_vv().encode()),
        },
    );
    assert_eq!(*store.events.lock().unwrap(), ["updates_since"]);
    assert_eq!(doc_update_payloads(&replies), vec![vec![vec![0xAA; 8]]]);
}

/// A group with a missing fragment must not apply a truncated update.
#[test]
fn an_incomplete_fragment_group_is_dropped_not_applied() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let store = FakeStore {
        workspace,
        document,
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    };
    let room_id = format!("document:{}", document.to_storage());
    let bytes: Vec<u8> = vec![7; 600 * 1024];
    let mut messages = fragments::doc_update_messages(CrdtType::Loro, &room_id, bytes);
    messages.pop();
    let encoded: Vec<Vec<u8>> = messages
        .iter()
        .map(|message| loro_protocol::encoding::encode(message).unwrap())
        .collect();
    let _ = handle_frame(
        &store,
        &peer(workspace),
        PeerFrame::LoroBatch {
            workspace_id: workspace,
            messages: encoded,
        },
    );
    assert!(store.applied.lock().unwrap().is_empty());
    assert!(!store.events.lock().unwrap().contains(&"apply"));
}

fn peer(workspace: WorkspaceId) -> AuthenticatedPeer {
    let signing = DeviceSigningKey::generate();
    let endpoint = ReplicationEndpointSecret::generate();
    let descriptor = koru_device::DeviceDescriptorV1::new(
        DeviceId::new(),
        "peer".into(),
        &signing,
        endpoint.endpoint_id(),
        1,
    );
    AuthenticatedPeer {
        relationship_id: RelationshipId::new(),
        device_id: descriptor.device_id,
        endpoint_id: descriptor.endpoint_id,
        all_data: false,
        authorized_profiles: BTreeSet::new(),
        authorized_workspaces: [workspace].into_iter().collect(),
        authorization_fence: AuthorizationFence::active(),
        display_name: None,
        incompatible: false,
        peer_protocol: Some(PEER_PROTOCOL_VERSION),
    }
}

/// The wedge that cost an Android tablet a working device: an adapter runs
/// `profiles` (or `apply_remote_profile_updates`) under the fence and, inside
/// it, calls `check_authorized_*` for each row. That second check used to take
/// a second *read* of the fence's `RwLock`. When a `replace_authorization`
/// revocation queued as a writer in between, the inner read waited behind the
/// writer and the writer waited on the outer guard the same thread still held
/// — with the Store lock in hand, so every later Store call timed out until
/// the process was restarted.
///
/// The watchdog is the point of the test: a regression *hangs* rather than
/// failing, so the operation reports through a channel and the assertion is on
/// the channel, not on the thread.
#[test]
fn a_check_nested_inside_a_fenced_operation_survives_a_concurrent_revocation() {
    let workspace = WorkspaceId::new();
    let peer = peer(workspace);
    let revoking = peer.clone();
    // The operation signals that it is inside the fence, then waits until the
    // revocation has definitely started before taking the nested check.
    // Without that ordering the race is not reproduced at all.
    let inside = Arc::new((Mutex::new(false), Condvar::new()));
    let revoking_started = Arc::new((Mutex::new(false), Condvar::new()));
    let (report, outcome) = std::sync::mpsc::channel();

    let operation_inside = Arc::clone(&inside);
    let operation_revoking = Arc::clone(&revoking_started);
    let operation = std::thread::spawn(move || {
        let result = peer.with_catalog_scope(|| {
            {
                let (lock, signal) = &*operation_inside;
                *lock.lock().unwrap() = true;
                signal.notify_all();
            }
            {
                // Wait for the revoking thread to have entered `revoke`. It
                // cannot return while this operation holds the barrier, so
                // this waits on the flag it sets first, bounded so a
                // regression fails the test rather than hanging it.
                let (lock, signal) = &*operation_revoking;
                let mut started = lock.lock().unwrap();
                while !*started {
                    let (next, timeout) = signal
                        .wait_timeout(started, Duration::from_secs(5))
                        .unwrap();
                    started = next;
                    if timeout.timed_out() {
                        break;
                    }
                }
            }
            // The nested check. This is the call that used to deadlock.
            let _ = peer.check_authorized_workspace_profile(workspace, ProfileId::new());
            peer.check_active()
        });
        let _ = report.send(result);
    });

    {
        let (lock, signal) = &*inside;
        let mut entered = lock.lock().unwrap();
        while !*entered {
            entered = signal.wait(entered).unwrap();
        }
    }
    let revoker_signal = Arc::clone(&revoking_started);
    let revoker = std::thread::spawn(move || {
        {
            let (lock, signal) = &*revoker_signal;
            *lock.lock().unwrap() = true;
            signal.notify_all();
        }
        revoking.authorization_fence.revoke();
    });

    let result = outcome
        .recv_timeout(Duration::from_secs(20))
        .expect("the fenced operation finished rather than deadlocking on its own nested check");
    // It was admitted before the revocation, so the fence lets it finish; a
    // refusal would also be a correct outcome of the race. What it must never
    // do is wedge.
    assert!(result.is_ok() || result.unwrap_err().code == "relationship_revoked");
    operation.join().unwrap();
    revoker.join().unwrap();
}

/// The barrier still means what its callers rely on: `revoke` does not return
/// while an operation it admitted is still running, and refuses the next one.
#[test]
fn revocation_waits_for_an_operation_it_admitted_and_refuses_the_next() {
    let workspace = WorkspaceId::new();
    let peer = peer(workspace);
    let revoking = peer.clone();
    let inside = Arc::new((Mutex::new(false), Condvar::new()));
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let operation_inside = Arc::clone(&inside);
    let operation_released = Arc::clone(&released);
    let operation = std::thread::spawn(move || {
        peer.with_catalog_scope(|| {
            {
                let (lock, signal) = &*operation_inside;
                *lock.lock().unwrap() = true;
                signal.notify_all();
            }
            std::thread::sleep(Duration::from_millis(200));
            operation_released.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    });

    {
        let (lock, signal) = &*inside;
        let mut entered = lock.lock().unwrap();
        while !*entered {
            entered = signal.wait(entered).unwrap();
        }
    }
    revoking.authorization_fence.revoke();
    assert!(
        released.load(std::sync::atomic::Ordering::SeqCst),
        "revoke returned while an operation it admitted was still running"
    );
    assert_eq!(
        revoking.with_catalog_scope(|| Ok(())).unwrap_err().code,
        "relationship_revoked"
    );
    operation.join().unwrap();
}

// ------------------------------------------------ protocol 6: the root frames

fn store_for(workspace: WorkspaceId) -> FakeStore {
    FakeStore {
        workspace,
        document: DocumentId::new(),
        events: Mutex::new(Vec::new()),
        applied: Mutex::new(Vec::new()),
        fail_commit: false,
    }
}

fn version_from(peer_id: u64) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(peer_id).unwrap();
    doc.get_map("m").insert("k", 1).unwrap();
    doc.commit();
    doc.oplog_vv().encode()
}

/// Every root frame round-trips through serde, an unknown unit tag is
/// refused, and a frame carries nothing but addresses, versions and bytes.
#[test]
fn root_frames_round_trip_and_refuse_unknown_unit_tags() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let units = vec![
        RootUnitState {
            unit: CatalogUnitRef::Profile { profile },
            version: version_from(1),
        },
        RootUnitState {
            unit: CatalogUnitRef::Workspace { workspace },
            version: version_from(2),
        },
    ];
    for unit in &units {
        unit.validate().unwrap();
    }
    let personal = vec![PersonalUnitState {
        workspace,
        version: version_from(3),
    }];
    for frame in [
        PeerFrame::RootCatalogRequest,
        PeerFrame::RootCatalog {
            digest: vec![1, 2, 3],
            units: units.clone(),
            personal: personal.clone(),
        },
        PeerFrame::RootUnitRequest {
            unit: CatalogUnitRef::Workspace { workspace },
            version: version_from(4),
        },
        PeerFrame::RootUnitUpdates {
            unit: CatalogUnitRef::Profile { profile },
            updates: vec![vec![5, 6]],
        },
        PeerFrame::PersonalRequest {
            workspace,
            version: version_from(7),
        },
        PeerFrame::PersonalUpdates {
            workspace,
            updates: vec![vec![8]],
        },
    ] {
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            serde_json::from_value::<PeerFrame>(encoded.clone()).unwrap(),
            frame
        );
        assert_eq!(
            frame_workspace(&frame),
            None,
            "root frames are account-scoped"
        );
        if encoded
            .as_object()
            .is_some_and(|object| object.contains_key("unit"))
        {
            let mut unknown = encoded.clone();
            unknown["unit"]["type"] = "future_unit".into();
            assert!(serde_json::from_value::<PeerFrame>(unknown).is_err());
            let mut extra = encoded.clone();
            extra["unit"]["name"] = "SENTINEL".into();
            assert!(serde_json::from_value::<PeerFrame>(extra).is_err());
        }
    }
    assert!(
        RootUnitState {
            unit: CatalogUnitRef::Profile { profile },
            version: vec![0xff, 0xfe],
        }
        .validate()
        .is_err()
    );
}

/// The root digest is scope-bound, order-independent, moves with a version,
/// refuses duplicates, and carries personal units only for the same
/// person's device.
#[test]
fn root_catalog_digest_is_scope_bound_and_order_independent() {
    let workspace = WorkspaceId::new();
    let profile = ProfileId::from_uuid(workspace.as_uuid());
    let a = RootUnitState {
        unit: CatalogUnitRef::Profile { profile },
        version: version_from(1),
    };
    let b = RootUnitState {
        unit: CatalogUnitRef::Workspace { workspace },
        version: version_from(2),
    };
    let mut all = peer(workspace);
    all.all_data = true;
    let scoped = peer(workspace);
    let baseline = root_catalog_digest(&all, &[a.clone(), b.clone()], &[]).unwrap();
    assert_eq!(
        Some(&baseline),
        root_catalog_digest(&all, &[b.clone(), a.clone()], &[]).as_ref()
    );
    assert_ne!(
        Some(&baseline),
        root_catalog_digest(&scoped, &[a.clone(), b.clone()], &[]).as_ref(),
        "a value computed for one grant never equals one computed for another"
    );
    let mut moved = b.clone();
    moved.version = version_from(3);
    assert_ne!(
        Some(&baseline),
        root_catalog_digest(&all, &[a.clone(), moved], &[]).as_ref()
    );
    assert_eq!(
        root_catalog_digest(&all, &[a.clone(), a.clone()], &[]),
        None,
        "duplicates are refused"
    );
    let personal = PersonalUnitState {
        workspace,
        version: version_from(4),
    };
    assert!(
        root_catalog_digest(
            &all,
            std::slice::from_ref(&a),
            std::slice::from_ref(&personal)
        )
        .is_some()
    );
    assert_eq!(
        root_catalog_digest(&scoped, std::slice::from_ref(&b), &[personal]),
        None,
        "a scoped peer is never framed a personal unit"
    );
    let mut revoked = all.clone();
    revoked.authorization_fence = AuthorizationFence::active();
    revoked.authorization_fence.revoke();
    assert_eq!(root_catalog_digest(&revoked, &[a, b], &[]), None);
}

/// `RootCatalogRequest` needs a catalog scope, answers the eligible units
/// with the digest the receiver recomputes, and lists a personal unit only
/// for the same person's device; a scoped peer's `PersonalRequest` is refused
/// before the store is asked.
#[test]
fn root_catalog_and_personal_requests_are_admitted_by_scope() {
    let workspace = WorkspaceId::new();
    let store = store_for(workspace);
    let mut denied = peer(workspace);
    denied.authorized_workspaces.clear();
    assert_eq!(
        handle_frame(&store, &denied, PeerFrame::RootCatalogRequest),
        vec![PeerFrame::Error {
            code: "unauthorized_catalog".into()
        }]
    );

    let mut all = peer(workspace);
    all.all_data = true;
    let inventory = store.root_catalog(&all).unwrap();
    assert_eq!(inventory.units.len(), 2);
    assert_eq!(inventory.personal.len(), 1);
    let answer = handle_frame(&store, &all, PeerFrame::RootCatalogRequest);
    let [
        PeerFrame::RootCatalog {
            digest,
            units,
            personal,
        },
    ] = answer.as_slice()
    else {
        panic!("expected one root catalog, got {}", answer.len())
    };
    assert_eq!(
        Some(digest.clone()),
        root_catalog_digest(&all, units, personal),
        "the digest is over exactly the listed units"
    );
    assert_eq!(units.len(), 2);
    assert_eq!(personal.len(), 1);

    // A Workspace grant sees the Workspace unit, no Profile unit and no
    // personal unit, and may not ask for one.
    let scoped = peer(workspace);
    let answer = handle_frame(&store, &scoped, PeerFrame::RootCatalogRequest);
    let [
        PeerFrame::RootCatalog {
            units, personal, ..
        },
    ] = answer.as_slice()
    else {
        panic!("expected one root catalog, got {}", answer.len())
    };
    assert_eq!(
        units.iter().map(|unit| unit.unit).collect::<Vec<_>>(),
        vec![CatalogUnitRef::Workspace { workspace }]
    );
    assert!(personal.is_empty());
    assert_eq!(
        handle_frame(
            &store,
            &scoped,
            PeerFrame::PersonalRequest {
                workspace,
                version: Vec::new(),
            }
        ),
        vec![PeerFrame::Error {
            code: ROOT_SCOPE_NOT_BOUND.into()
        }]
    );
    assert!(
        !store.events.lock().unwrap().contains(&"personal_export"),
        "the store is not asked for a personal unit a scoped peer may not have"
    );
    assert!(!scoped.binds_root_scope());
    assert!(all.binds_root_scope());
    assert_eq!(
        handle_frame(
            &store,
            &all,
            PeerFrame::PersonalRequest {
                workspace,
                version: Vec::new(),
            }
        ),
        vec![PeerFrame::PersonalUpdates {
            workspace,
            updates: vec![vec![10]],
        }]
    );
}

/// A root unit request decodes its version before the store is asked,
/// answers through the export boundary, and is fenced by revocation.
#[test]
fn root_unit_requests_decode_the_frontier_and_respect_the_fence() {
    let workspace = WorkspaceId::new();
    let store = store_for(workspace);
    let mut all = peer(workspace);
    all.all_data = true;
    let unit = CatalogUnitRef::Workspace { workspace };
    assert_eq!(
        handle_frame(
            &store,
            &all,
            PeerFrame::RootUnitRequest {
                unit,
                version: vec![0xff, 0xfe, 0xfd],
            }
        ),
        vec![PeerFrame::Error {
            code: "invalid_root_version".into()
        }]
    );
    assert!(store.events.lock().unwrap().is_empty());
    assert_eq!(
        handle_frame(
            &store,
            &all,
            PeerFrame::RootUnitRequest {
                unit,
                version: loro::VersionVector::new().encode(),
            }
        ),
        vec![PeerFrame::RootUnitUpdates {
            unit,
            updates: vec![vec![9]],
        }]
    );
    assert_eq!(*store.events.lock().unwrap(), ["root_export"]);
    all.authorization_fence.revoke();
    assert_eq!(
        handle_frame(
            &store,
            &all,
            PeerFrame::RootUnitRequest {
                unit,
                version: loro::VersionVector::new().encode(),
            }
        ),
        vec![PeerFrame::Error {
            code: "relationship_revoked".into()
        }]
    );
    assert_eq!(*store.events.lock().unwrap(), ["root_export"]);
}

/// The default `authorize_workspace` keeps every adapter that predates the
/// root rule exactly where it was: UUID or owner Profile, existence hidden
/// from a scoped peer.
#[test]
fn default_workspace_authorization_matches_the_legacy_owner_rule() {
    let workspace = WorkspaceId::new();
    let store = store_for(workspace);
    let by_uuid = peer(workspace);
    store.authorize_workspace(&by_uuid, workspace).unwrap();
    let mut by_profile = peer(workspace);
    by_profile.authorized_workspaces.clear();
    by_profile
        .authorized_profiles
        .insert(ProfileId::from_uuid(workspace.as_uuid()));
    store.authorize_workspace(&by_profile, workspace).unwrap();
    let mut other = peer(workspace);
    other.authorized_workspaces.clear();
    assert_eq!(
        store
            .authorize_workspace(&other, workspace)
            .unwrap_err()
            .code,
        "unauthorized_workspace"
    );
    assert_eq!(
        store
            .authorize_workspace(&other, WorkspaceId::new())
            .unwrap_err()
            .code,
        "unauthorized_workspace",
        "unknown and unrelated are indistinguishable to a scoped peer"
    );
    assert_eq!(
        store
            .authorize_workspace(&by_uuid, WorkspaceId::new())
            .unwrap_err()
            .code,
        "workspace_not_found"
    );
}
