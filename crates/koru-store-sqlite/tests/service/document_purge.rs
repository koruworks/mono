// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn purge_freeze_survives_restart_and_keep_requires_durable_coverage() {
    let mut f = Fixture::new();
    let ws = f.a_workspace();
    let doc = loro::LoroDoc::new();
    doc.get_text("body")
        .insert(0, "locally edited source")
        .unwrap();
    doc.commit();
    let (source, _) = f
        .store
        .import_document(
            ws,
            "Source",
            "page",
            None,
            doc.export(loro::ExportMode::snapshot()).unwrap(),
            at(1),
        )
        .unwrap();
    assert!(
        f.store.freeze_document_purge(ws, source, at(2)).is_err(),
        "no final evidence cannot freeze content"
    );
    let system = loro::LoroDoc::new();
    system.get_map("meta").insert("schema", 1).unwrap();
    system
        .get_map("pages")
        .ensure_mergeable_map("purged")
        .unwrap()
        .ensure_mergeable_map(&source.to_storage())
        .unwrap()
        .insert("event", "retained evidence")
        .unwrap();
    system.commit();
    f.store
        .import_document(
            ws,
            "",
            "koru-system",
            None,
            system.export(loro::ExportMode::snapshot()).unwrap(),
            at(3),
        )
        .unwrap();
    let child = f
        .store
        .import_document(
            ws,
            "Independent child",
            "page",
            Some(source),
            doc.export(loro::ExportMode::snapshot()).unwrap(),
            at(3),
        )
        .unwrap()
        .0;
    let frozen = f.store.freeze_document_purge(ws, source, at(4)).unwrap();
    let expected = f.store.version(ws, source).unwrap();
    f.restart();
    doc.get_text("body").insert(0, "late ").unwrap();
    doc.commit();
    assert!(
        f.store
            .apply_update(
                ws,
                source,
                doc.export(loro::ExportMode::all_updates()).unwrap(),
                at(5),
                koru_store_core::ports::UpdateOrigin::Store
            )
            .is_err()
    );
    assert!(
        f.store
            .release_document_purge(ws, source, &expected, Some(DocumentId::new()), at(6))
            .is_err()
    );
    assert!(f.store.locate(source).unwrap().is_some());
    let kept = f
        .store
        .create_seeded_document(
            ws,
            "Source",
            "page",
            None,
            None,
            "test",
            "keep-source",
            "same-source",
            None,
            frozen,
            at(7),
        )
        .unwrap();
    f.restart();
    assert!(
        f.store
            .release_document_purge(ws, source, &expected, Some(kept.document), at(8))
            .unwrap()
    );
    // Reproduce the boundary after workspace deletion, before catalog cleanup.
    SqliteCatalog::open(&f.dir.path().join("catalog.db"))
        .unwrap()
        .record_location(source, ws)
        .unwrap();
    f.restart();
    assert!(
        !f.store
            .release_document_purge(ws, source, &expected, Some(kept.document), at(9))
            .unwrap()
    );
    assert!(f.store.locate(source).unwrap().is_none());
    let children = f.store.documents(ws).unwrap();
    assert!(
        children
            .iter()
            .any(|entry| entry.id == child && entry.parent.is_none()),
        "a child with no deletion evidence is detached, never discarded"
    );
    assert!(f.store.snapshot(ws, child).is_ok());
    assert!(
        f.store
            .documents(ws)
            .unwrap()
            .iter()
            .any(|e| e.id == kept.document)
    );
    assert!(
        !f.store
            .documents(ws)
            .unwrap()
            .iter()
            .any(|e| e.id == source)
    );
    assert!(
        f.store
            .provision_replica_document(ws, source, at(9))
            .is_err(),
        "old identity remains final after rows go"
    );
    assert!(
        !f.store
            .release_document_purge(ws, source, &expected, Some(kept.document), at(10))
            .unwrap()
    );
}
