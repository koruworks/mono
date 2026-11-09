// SPDX-License-Identifier: MPL-2.0

//! Memory-only: the receipt keys and phases, and the grant rule the
//! inventory applies. The Store-level exchange is covered where a catalog
//! exists (`apps/koru-store-server/src/replication.rs`); nothing here opens
//! one.

use super::*;

#[test]
fn receipt_keys_name_the_relationship_kind_and_subject_and_nothing_else() {
    let relationship = Uuid::new_v4();
    let workspace = WorkspaceId::new();
    let profile = ProfileId::new();
    assert_eq!(
        peer_scope_key(relationship),
        format!("root_catalog.peer_scope.{relationship}")
    );
    assert_eq!(
        join_receipt_key(relationship, JoinUnit::WorkspaceCatalog { workspace }),
        format!("root_catalog.join.{relationship}.workspace_catalog.{workspace}")
    );
    assert_eq!(
        join_receipt_key(relationship, JoinUnit::ProfileCatalog { profile }),
        format!("root_catalog.join.{relationship}.profile_catalog.{profile}")
    );
    assert_eq!(
        join_receipt_key(relationship, JoinUnit::PersonalV2 { workspace }),
        format!("root_catalog.join.{relationship}.personal_v2.{workspace}")
    );
}

#[test]
fn join_phases_are_ordered_and_a_receipt_round_trips_as_json() {
    assert!(JoinPhase::Prepared < JoinPhase::Staged);
    assert!(JoinPhase::Staged < JoinPhase::LocallyCommitted);
    assert!(JoinPhase::LocallyCommitted < JoinPhase::Reconciled);
    let frontier = {
        let doc = loro::LoroDoc::new();
        doc.set_peer_id(7).unwrap();
        doc.get_map("m").insert("k", 1).unwrap();
        doc.commit();
        doc.oplog_vv()
    };
    let receipt = RootJoinReceipt {
        relationship: Uuid::new_v4(),
        scope: UserScopeId::new(),
        unit: JoinUnit::WorkspaceCatalog {
            workspace: WorkspaceId::new(),
        },
        phase: JoinPhase::Reconciled,
        frontier: frontier.encode(),
        operation_id: Uuid::new_v4(),
        at_ms: 5,
    };
    let json = serde_json::to_string(&receipt).unwrap();
    let back: RootJoinReceipt = serde_json::from_str(&json).unwrap();
    assert_eq!(back, receipt);
    assert_eq!(back.frontier_vv(), frontier);
    // The receipt carries ids and a version vector only: no name can be in
    // it because no name is in the type.
    assert!(!json.contains("name"));
}

#[test]
fn grants_cover_workspaces_by_uuid_by_current_placement_or_by_all_data() {
    let workspace = WorkspaceId::new();
    let (granted, other) = (ProfileId::new(), ProfileId::new());
    let no_profiles = BTreeSet::new();
    let no_workspaces = BTreeSet::new();
    let profiles: BTreeSet<ProfileId> = [granted].into_iter().collect();
    let workspaces: BTreeSet<WorkspaceId> = [workspace].into_iter().collect();
    let all = RootGrant {
        all_data: true,
        profiles: &no_profiles,
        workspaces: &no_workspaces,
        path: TransportPath::Peer,
    };
    let by_profile = RootGrant {
        all_data: false,
        profiles: &profiles,
        workspaces: &no_workspaces,
        path: TransportPath::Peer,
    };
    let by_uuid = RootGrant {
        all_data: false,
        profiles: &no_profiles,
        workspaces: &workspaces,
        path: TransportPath::Peer,
    };
    assert!(grant_covers_workspace(&all, workspace, None));
    assert!(grant_covers_workspace(&by_uuid, workspace, Some(other)));
    assert!(!grant_covers_workspace(
        &by_uuid,
        WorkspaceId::new(),
        Some(granted)
    ));
    assert!(grant_covers_workspace(
        &by_profile,
        workspace,
        Some(granted)
    ));
    assert!(!grant_covers_workspace(&by_profile, workspace, Some(other)));
    // Unassigned or pending: a Profile grant matches nothing.
    assert!(!grant_covers_workspace(&by_profile, workspace, None));
}
