// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{TransportMode, TransportSubject};

fn memory() -> Store<SqliteCatalog, SqliteBackend> {
    Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap()
}

#[test]
fn sync_modes_first_inventory_allows_absent_workspace_but_preserves_existing_off() {
    let mut store = memory();
    let missing = WorkspaceId::new();
    store.admit_peer_inventory_request(missing).unwrap();
    // Missing policy is still an error at the content boundary.
    assert!(
        store
            .transport_admission(TransportSubject::Workspace(missing))
            .is_err()
    );
    let workspace = workspace(&mut store);
    assert!(store.admit_peer_inventory_request(workspace).is_err());
    store.bind_peer_scope(uuid::Uuid::new_v4()).unwrap();
    store.admit_peer_inventory_request(workspace).unwrap();
    let state = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap();
    store
        .set_transport_mode(koru_store_core::SetTransportMode {
            subject: state.subject,
            expected_revision: state.revision,
            mode: TransportMode::LocalOnly,
        })
        .unwrap();
    assert_eq!(
        store
            .admit_peer_inventory_request(workspace)
            .unwrap_err()
            .code(),
        koru_store_core::errors::Code::Forbidden
    );
}

#[test]
fn sync_modes_first_inventory_respects_known_placement_profile() {
    use koru_store_core::root_catalog::{CatalogUnitRef, seed_workspace_unit};
    let mut store = memory();
    let profile = store
        .create_profile("Owner", None, None, Locale::default(), at())
        .unwrap();
    let relationship = uuid::Uuid::new_v4();
    store.bind_peer_scope(relationship).unwrap();
    let workspace = WorkspaceId::new();
    let bytes = seed_workspace_unit(workspace, profile.id, None, 42).unwrap();
    let staged = store
        .stage_root_unit(
            relationship,
            CatalogUnitRef::Workspace { workspace },
            &[bytes],
        )
        .unwrap();
    store
        .commit_staged_root_unit(relationship, staged, None)
        .unwrap();
    assert!(store.workspace_including_deleted(workspace).is_err());
    store.admit_peer_inventory_request(workspace).unwrap();
    store
        .choose_transport_mode(profile.id, TransportMode::LocalOnly, true)
        .unwrap();
    assert_eq!(
        store
            .admit_peer_inventory_request(workspace)
            .unwrap_err()
            .code(),
        koru_store_core::errors::Code::Forbidden
    );
}

#[test]
fn sync_modes_link_promotes_existing_defaults_and_is_idempotent() {
    let mut store = memory();
    let workspace = workspace(&mut store);
    let owner = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap()
        .owner;
    let relationship = uuid::Uuid::new_v4();
    store.bind_peer_scope(relationship).unwrap();
    let before = store.transport_mode_group(owner).unwrap();
    assert_eq!(before.profile.mode, TransportMode::P2p);
    assert_eq!(before.children[0].mode, TransportMode::P2p);
    assert!(!before.profile.explicit_choice && !before.children[0].explicit_choice);
    assert!(
        store
            .transport_admission(TransportSubject::Workspace(workspace))
            .unwrap()
            .allows_peer()
    );
    store.bind_peer_scope(relationship).unwrap();
    let after = store.transport_mode_group(owner).unwrap();
    assert_eq!(before.profile, after.profile);
    assert_eq!(before.children, after.children);
}

#[test]
fn sync_modes_link_preserves_explicit_off_and_cloud_default_preserves_p2p() {
    let mut store = memory();
    let workspace = workspace(&mut store);
    let subject = TransportSubject::Workspace(workspace);
    let owner = store.transport_mode(subject).unwrap().owner;
    store
        .set_transport_mode(koru_store_core::SetTransportMode {
            subject,
            expected_revision: 1,
            mode: TransportMode::LocalOnly,
        })
        .unwrap();
    store.bind_peer_scope(uuid::Uuid::new_v4()).unwrap();
    assert_eq!(
        store.transport_mode(subject).unwrap().mode,
        TransportMode::LocalOnly
    );
    store
        .choose_transport_mode(owner, TransportMode::P2p, true)
        .unwrap();
    assert!(
        store
            .initialize_profile_transport_mode(owner, TransportMode::Cloud)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.transport_mode_group(owner).unwrap().profile.mode,
        TransportMode::P2p
    );
    assert_eq!(
        store.transport_mode(subject).unwrap().mode,
        TransportMode::LocalOnly
    );
}

#[test]
fn sync_modes_explicit_profile_off_caps_children_after_linking() {
    let mut store = memory();
    let workspace = workspace(&mut store);
    let owner = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap()
        .owner;
    store
        .choose_transport_mode(owner, TransportMode::LocalOnly, false)
        .unwrap();
    store.bind_peer_scope(uuid::Uuid::new_v4()).unwrap();
    let group = store.transport_mode_group(owner).unwrap();
    assert_eq!(group.profile.mode, TransportMode::LocalOnly);
    assert_eq!(group.children[0].mode, TransportMode::LocalOnly);
    assert_eq!(group.children[0].revision, 1);
}

#[test]
fn sync_modes_cloud_default_includes_peers_and_preserves_workspace_exception() {
    let mut store = memory();
    let workspace = workspace(&mut store);
    let owner = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap()
        .owner;
    store
        .initialize_profile_transport_mode(owner, TransportMode::Cloud)
        .unwrap();
    let admission = store
        .transport_admission(TransportSubject::Workspace(workspace))
        .unwrap();
    assert!(admission.allows_cloud() && admission.allows_peer());
    assert!(!admission.explicit_choice);
    store
        .set_transport_mode(koru_store_core::SetTransportMode {
            subject: TransportSubject::Workspace(workspace),
            expected_revision: admission.revision,
            mode: TransportMode::P2p,
        })
        .unwrap();
    store
        .initialize_profile_transport_mode(owner, TransportMode::Cloud)
        .unwrap();
    assert_eq!(
        store
            .transport_mode(TransportSubject::Workspace(workspace))
            .unwrap()
            .mode,
        TransportMode::P2p
    );
}

#[test]
fn sync_modes_profile_off_on_keeps_inherited_workspaces_following() {
    let mut store = memory();
    let workspace = workspace(&mut store);
    let owner = store
        .transport_mode(TransportSubject::Workspace(workspace))
        .unwrap()
        .owner;
    for mode in [
        TransportMode::P2p,
        TransportMode::LocalOnly,
        TransportMode::Cloud,
    ] {
        store.choose_transport_mode(owner, mode, true).unwrap();
        let row = store
            .transport_mode(TransportSubject::Workspace(workspace))
            .unwrap();
        assert_eq!(row.mode, mode);
        assert!(!row.explicit_choice);
    }
}
