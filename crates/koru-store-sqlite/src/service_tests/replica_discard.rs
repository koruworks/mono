// SPDX-License-Identifier: MPL-2.0

//! `discard_empty_replica_document`: a Cloud lookup's empty container goes,
//! and nothing that holds content or that the manifest names ever does.
use super::*;
use koru_store_core::DocumentId;

fn memory_store() -> Store<SqliteCatalog, SqliteBackend> {
    let catalog =
        SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db")).unwrap();
    Store::open(catalog, SqliteBackend::in_memory()).unwrap()
}

#[test]
fn an_empty_unlisted_container_is_discarded_and_its_id_is_free_again() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let candidate = DocumentId::new();
    store
        .provision_replica_document(workspace, candidate, at())
        .unwrap();
    // A version read opens it, as a Cloud pass does.
    assert!(store.version(workspace, candidate).unwrap().is_empty());
    assert!(
        store
            .discard_empty_replica_document(workspace, candidate)
            .unwrap()
    );
    assert_eq!(store.locate(candidate).unwrap(), None);
    // Gone: provisioning again starts from nothing, and the id is free.
    store
        .provision_replica_document(workspace, candidate, at())
        .unwrap();
}

#[test]
fn a_container_with_content_or_a_listed_document_is_kept() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let (listed, _) = store
        .create_document(workspace, "Kept", "page", None, None, at())
        .unwrap();
    assert!(
        !store
            .discard_empty_replica_document(workspace, listed)
            .unwrap(),
        "the manifest names it"
    );
    let filled = DocumentId::new();
    store
        .provision_replica_document(workspace, filled, at())
        .unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_map("m").insert("k", "v").unwrap();
    doc.commit();
    store
        .apply_update(
            workspace,
            filled,
            doc.export(loro::ExportMode::all_updates()).unwrap(),
            at(),
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace, filled, at()).unwrap();
    assert!(
        !store
            .discard_empty_replica_document(workspace, filled)
            .unwrap(),
        "it holds content"
    );
    assert!(!store.version(workspace, filled).unwrap().is_empty());
    let manifest = store.manifest_id(workspace).unwrap();
    assert!(
        !store
            .discard_empty_replica_document(workspace, manifest)
            .unwrap()
    );
}
