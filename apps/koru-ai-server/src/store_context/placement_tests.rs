// SPDX-License-Identifier: MPL-2.0

//! Real worker dispatch over memory only: no root, listener or file bytes.

use super::*;
use koru_ai_core::Timestamp;
use koru_store_core::Locale;
use koru_store_core::root_catalog::{CatalogUnitRef, WorkspaceCatalogUnit};
use koru_store_core::transport::{
    SetTransportMode, TransportMode, TransportPath, TransportSubject,
};
use koru_store_server::memory::MemoryStore;

struct Fixture {
    memory: MemoryStore,
    adapter: StoreContextAdapter,
    destination: DocumentDestination,
    other: ProfileId,
    relationship: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("ai-placement-test").unwrap();
        let relationship = Uuid::new_v4();
        let (profile, other, workspace) = memory
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile = store
                    .create_profile("First", None, None, Locale::default(), at)
                    .unwrap()
                    .id;
                let other = store
                    .create_profile("Second", None, None, Locale::default(), at)
                    .unwrap()
                    .id;
                let workspace = store.create_workspace(profile, "Work", at).unwrap().id;
                for subject in [
                    TransportSubject::Profile(profile),
                    TransportSubject::Profile(other),
                    TransportSubject::Workspace(workspace),
                ] {
                    store
                        .set_transport_mode(SetTransportMode {
                            subject,
                            expected_revision: 1,
                            mode: TransportMode::Cloud,
                        })
                        .unwrap();
                }
                store
                    .bind_account_scope(relationship, Uuid::new_v4(), at.millis())
                    .unwrap();
                (profile, other, workspace)
            })
            .unwrap();
        Self {
            adapter: StoreContextAdapter::new(memory.client()),
            memory,
            other,
            relationship,
            destination: DocumentDestination {
                profile_id: profile,
                workspace_id: workspace,
                parent_document_id: None,
                index: None,
                title: "Result".into(),
            },
        }
    }

    fn remote_placement(&self, profile: Option<ProfileId>) {
        let workspace = self.destination.workspace_id;
        let relationship = self.relationship;
        self.memory
            .with_store(|store| {
                let unit_ref = CatalogUnitRef::Workspace { workspace };
                let snapshot = store
                    .eligible_export_source(unit_ref, TransportPath::Cloud)
                    .unwrap()
                    .unwrap();
                let unit =
                    WorkspaceCatalogUnit::from_snapshot(&snapshot, workspace, 9_001).unwrap();
                unit.set_placement(profile, Uuid::new_v4()).unwrap();
                let staged = store
                    .stage_root_unit(relationship, unit_ref, &[unit.snapshot().unwrap()])
                    .unwrap();
                assert!(!staged.is_pending());
                store
                    .commit_staged_root_unit(relationship, staged, None)
                    .unwrap();
            })
            .unwrap();
    }

    fn export(&self, key: Uuid) -> Result<DocumentId, AiError> {
        self.adapter
            .create_document(key, &self.destination, "Retained result")
    }
}

#[test]
fn current_profile_can_read_export_and_replay_but_previous_profile_cannot() {
    let mut f = Fixture::new();
    let key = Uuid::new_v4();
    let document = f.export(key).unwrap();
    assert_eq!(f.export(key).unwrap(), document);
    assert!(
        f.adapter
            .resolve_document(document, Some(f.destination.profile_id))
            .unwrap()
            .markdown
            .contains("Retained result")
    );
    f.memory
        .with_store(|store| {
            store
                .move_workspace(f.destination.workspace_id, f.other)
                .unwrap()
        })
        .unwrap();
    assert!(
        f.adapter
            .resolve_document(document, Some(f.destination.profile_id))
            .is_err()
    );
    assert!(
        f.export(key).is_err(),
        "old authorization cannot replay a creation receipt"
    );
    f.destination.profile_id = f.other;
    assert!(f.adapter.resolve_document(document, Some(f.other)).is_ok());
    assert!(f.export(Uuid::new_v4()).is_ok());
    assert_eq!(
        f.adapter.profile_catalog().unwrap().workspace_owners,
        [(f.destination.workspace_id, f.other)]
            .into_iter()
            .collect()
    );
}

#[test]
fn unassigned_or_unknown_placement_refuses_stale_owner_reads_and_exports() {
    for target in [None, Some(ProfileId::new())] {
        let f = Fixture::new();
        let key = Uuid::new_v4();
        let document = f.export(key).unwrap();
        f.remote_placement(target);
        f.memory
            .with_store(|store| {
                assert_eq!(
                    store.workspace(f.destination.workspace_id).unwrap().owner,
                    f.destination.profile_id,
                    "the old projection really remains"
                );
                assert_eq!(
                    store
                        .current_placement_profile(f.destination.workspace_id)
                        .unwrap(),
                    target
                );
            })
            .unwrap();
        assert!(
            f.adapter
                .resolve_document(document, Some(f.destination.profile_id))
                .is_err()
        );
        assert!(
            f.adapter
                .read_workspace_file(
                    f.destination.workspace_id,
                    document,
                    f.destination.profile_id
                )
                .is_err()
        );
        assert!(f.export(key).is_err());
        assert!(f.export(Uuid::new_v4()).is_err());
        assert!(
            f.adapter
                .profile_catalog()
                .unwrap()
                .workspace_owners
                .is_empty()
        );
        if let Some(unknown) = target {
            assert!(f.adapter.resolve_document(document, Some(unknown)).is_err());
        }
        assert_eq!(
            f.memory
                .with_store(|store| store.documents(f.destination.workspace_id).unwrap().len())
                .unwrap(),
            1,
            "refused writes create nothing"
        );
    }
}
