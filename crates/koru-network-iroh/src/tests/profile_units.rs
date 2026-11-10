// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn profile_identity() -> protocol::ProfileIdentityState {
    protocol::ProfileIdentityState {
        name: "Profile".into(),
        purpose: None,
        avatar: None,
        icon: None,
        color: None,
        account: None,
        locale: "en".into(),
        created_at_ms: 1,
        kind: "normal".into(),
    }
}

fn peer_for(workspace: WorkspaceId) -> protocol::AuthenticatedPeer {
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

fn vector(peer: u64) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc.get_map("m").insert("key", peer as i64).unwrap();
    doc.commit();
    doc.oplog_vv().encode()
}

#[derive(Clone)]
struct UnitTransport {
    source: Arc<InventoryStore>,
    peer: protocol::AuthenticatedPeer,
    wrong_unit: bool,
}

impl RequestTransport for UnitTransport {
    fn stable_id(&self) -> usize {
        900
    }
    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut reply = protocol::handle_frame(self.source.as_ref(), &self.peer, frame.clone())
                .into_iter()
                .next()
                .expect("one response");
            if self.wrong_unit {
                if let protocol::PeerFrame::ProfileUpdates { unit, .. } = &mut reply {
                    *unit = protocol::ProfileUnitRef::LegacyPrivate {
                        profile: unit.profile(),
                    };
                }
            }
            Ok(reply)
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

#[test]
fn profile_unit_wire_requires_exact_tag_and_identity_only_metadata() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    for (i, unit) in [
        protocol::ProfileUnitRef::LegacyPrivate { profile },
        protocol::ProfileUnitRef::Identity { profile },
        protocol::ProfileUnitRef::Private { profile },
        protocol::ProfileUnitRef::Personal {
            owner: profile,
            workspace,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let version = vector(i as u64 + 1);
        let row = protocol::ProfileReplicaState {
            unit,
            version: version.clone(),
            identity: matches!(unit, protocol::ProfileUnitRef::Identity { .. })
                .then(profile_identity),
        };
        row.validate().unwrap();
        for frame in [
            protocol::PeerFrame::ProfileRequest { unit, version },
            protocol::PeerFrame::ProfileUpdates {
                unit,
                updates: vec![vec![1, 2]],
            },
            protocol::PeerFrame::Catalog {
                digest: vec![],
                profiles: vec![row],
                workspaces: vec![],
            },
        ] {
            let encoded = serde_json::to_value(&frame).unwrap();
            assert_eq!(
                serde_json::from_value::<protocol::PeerFrame>(encoded.clone()).unwrap(),
                frame
            );
            if let Some(object) = encoded
                .as_object()
                .filter(|object| object.contains_key("unit"))
            {
                let mut missing = object.clone();
                missing.remove("unit");
                missing.insert("profile_id".into(), serde_json::to_value(profile).unwrap());
                assert!(serde_json::from_value::<protocol::PeerFrame>(missing.into()).is_err());
                let mut unknown = encoded.clone();
                unknown["unit"]["type"] = "future_unit".into();
                assert!(serde_json::from_value::<protocol::PeerFrame>(unknown).is_err());
            }
        }
    }
    let mut row = protocol::ProfileReplicaState {
        unit: protocol::ProfileUnitRef::Private { profile },
        version: vector(8),
        identity: Some(profile_identity()),
    };
    assert_eq!(row.validate().unwrap_err().code, "invalid_profile_identity");
    row.unit = protocol::ProfileUnitRef::Identity { profile };
    row.identity.as_mut().unwrap().kind = "holder".into();
    row.validate().unwrap();
    row.identity.as_mut().unwrap().kind.clear();
    assert!(row.validate().is_err());
    let mut encoded = serde_json::to_value(row).unwrap();
    encoded["identity"].as_object_mut().unwrap().remove("kind");
    assert!(serde_json::from_value::<protocol::ProfileReplicaState>(encoded).is_err());
}

#[tokio::test]
async fn profile_units_bootstrap_identity_private_then_workspace_then_personal() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::from_uuid(workspace.as_uuid());
    let manifest = DocumentId::new();
    let source = Arc::new(InventoryStore::new(workspace, manifest));
    let target = Arc::new(InventoryStore::new(workspace, manifest));
    target.workspace_ready.store(false, Ordering::SeqCst);
    for (index, unit) in [
        protocol::ProfileUnitRef::Identity { profile: owner },
        protocol::ProfileUnitRef::Private { profile: owner },
        protocol::ProfileUnitRef::Personal { owner, workspace },
    ]
    .into_iter()
    .enumerate()
    {
        source
            .unit_versions
            .lock()
            .unwrap()
            .insert(unit, vector(index as u64 + 1));
    }
    let peer = peer_for(workspace);
    let result = pull_missing(
        UnitTransport {
            source: source.clone(),
            peer: peer.clone(),
            wrong_unit: false,
        },
        peer,
        target.clone(),
        Arc::new(RwLock::new(NetworkStatus::default())),
        Arc::new(Mutex::new(crate::hydration::BlobDirectory::new())),
        None,
        1,
    )
    .await;
    assert!(!result.retry_all, "{:?}", result.retry_error_code);
    assert!(result.failed_workspaces.is_empty());
    // The order this test is about is the units'. Every pass also folds
    // pending root overlays before it frames anything (§4.3), so that step
    // leads the log; asserting the unit order through it keeps this test
    // about bootstrap rather than about everything a pass does.
    let events = target.unit_events.lock().unwrap().clone();
    assert_eq!(
        events.first().copied(),
        Some("publication_job"),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .copied()
            .filter(|event| *event != "publication_job")
            .collect::<Vec<_>>(),
        ["identity", "private", "workspace", "personal"],
        "{events:?}"
    );
    assert_eq!(
        *source.unit_versions.lock().unwrap(),
        *target.unit_versions.lock().unwrap()
    );
}

#[tokio::test]
async fn profile_units_reject_wrong_response_scope_and_success_without_causal_coverage() {
    let workspace = WorkspaceId::new();
    let owner = ProfileId::from_uuid(workspace.as_uuid());
    let unit = protocol::ProfileUnitRef::Private { profile: owner };
    for wrong_unit in [true, false] {
        let source = Arc::new(InventoryStore::new(workspace, DocumentId::new()));
        source.unit_versions.lock().unwrap().insert(unit, vector(1));
        let target = InventoryStore::new(workspace, DocumentId::new());
        target
            .unit_versions
            .lock()
            .unwrap()
            .insert(unit, loro::VersionVector::new().encode());
        target.stall_units.store(true, Ordering::SeqCst);
        let peer = peer_for(workspace);
        let transport = UnitTransport {
            source,
            peer: peer.clone(),
            wrong_unit,
        };
        let mut result = SyncPassResult::default();
        pull_profile_units(
            &transport,
            &peer,
            &target,
            &Arc::new(RwLock::new(NetworkStatus::default())),
            vec![protocol::ProfileReplicaState {
                unit,
                version: vector(1),
                identity: None,
            }],
            &mut result,
            &mut CatalogChanges::default(),
        )
        .await;
        assert!(result.retry_all);
        assert_eq!(
            result.retry_error_code.as_deref(),
            Some(if wrong_unit {
                "unexpected_profile_frame"
            } else {
                "profile_unit_stalled"
            })
        );
        assert_eq!(
            target.unit_events.lock().unwrap().len(),
            usize::from(!wrong_unit)
        );
    }
}
