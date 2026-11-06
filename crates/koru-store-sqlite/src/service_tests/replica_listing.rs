// SPDX-License-Identifier: MPL-2.0

//! `list_replica_document`: a hosted creation's content goes first as an
//! unlisted replica document, and only then is it listed, once, in one
//! attributed manifest commit (Cloud MCP TECHNICAL §5.5, §5.6).
use super::*;
use koru_store_core::DocumentId;

fn memory_store() -> Store<SqliteCatalog, SqliteBackend> {
    let catalog =
        SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db")).unwrap();
    Store::open(catalog, SqliteBackend::in_memory()).unwrap()
}

#[test]
fn an_unlisted_replica_document_is_listed_once_with_its_attribution() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    let seeded = DocumentId::new();
    store
        .provision_replica_document(workspace, seeded, at())
        .unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_map("m").insert("k", "v").unwrap();
    doc.commit();
    store
        .apply_update(
            workspace,
            seeded,
            doc.export(loro::ExportMode::all_updates()).unwrap(),
            at(),
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(workspace, seeded, at()).unwrap();
    assert!(
        store
            .documents(workspace)
            .unwrap()
            .iter()
            .all(|entry| entry.id != seeded),
        "content alone is not listed"
    );

    let message = r#"{"koru":"actor/1","kind":"assistant"}"#;
    let manifest_id = store.manifest_id(workspace).unwrap();
    let before = store.version(workspace, manifest_id).unwrap();
    store
        .list_replica_document(
            workspace,
            seeded,
            "Seeded",
            "page",
            None,
            None,
            Some(message),
            at(),
        )
        .unwrap()
        .expect("listed");
    let listed = store.documents(workspace).unwrap();
    let entry = listed.iter().find(|entry| entry.id == seeded).unwrap();
    assert_eq!(
        (entry.title.as_str(), entry.kind.as_str()),
        ("Seeded", "page")
    );
    // The manifest commit carries the message: every change since `before`.
    let manifest = loro::LoroDoc::new();
    manifest
        .import(&store.snapshot(workspace, manifest_id).unwrap())
        .unwrap();
    let mut messages = Vec::new();
    for (&peer, &end) in manifest.oplog_vv().iter() {
        let mut counter = before.get(&peer).copied().unwrap_or(0);
        while counter < end {
            let change = manifest.get_change(loro::ID::new(peer, counter)).unwrap();
            messages.push(change.message.as_deref().map(str::to_owned));
            counter = change.id.counter + change.len as i32;
        }
    }
    assert!(!messages.is_empty());
    assert!(messages.iter().all(|m| m.as_deref() == Some(message)));

    // Twice is harmless: the entry is keyed by document id.
    assert_eq!(
        store
            .list_replica_document(workspace, seeded, "Again", "page", None, None, None, at())
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .documents(workspace)
            .unwrap()
            .iter()
            .filter(|entry| entry.id == seeded)
            .count(),
        1
    );
}

#[test]
fn only_a_document_the_store_holds_is_listed_and_never_the_manifest() {
    let mut store = memory_store();
    let workspace = workspace(&mut store);
    assert!(
        store
            .list_replica_document(
                workspace,
                DocumentId::new(),
                "Nothing",
                "page",
                None,
                None,
                None,
                at()
            )
            .is_err()
    );
    let manifest = store.manifest_id(workspace).unwrap();
    assert!(
        store
            .list_replica_document(workspace, manifest, "M", "page", None, None, None, at())
            .is_err()
    );
}
