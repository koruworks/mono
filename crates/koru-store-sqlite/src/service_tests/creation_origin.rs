// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{CreationOriginLookup, DocumentId, errors::Code, ports::UpdateOrigin};

fn memory_store() -> Store<SqliteCatalog, SqliteBackend> {
    Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap()
}
fn seed_bytes(value: &str) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.get_map("body").insert("text", value).unwrap();
    doc.commit();
    doc.export(loro::ExportMode::all_updates()).unwrap()
}

#[test]
fn strict_store_replay_preserves_human_edits_trash_and_original_evidence() {
    let mut store = memory_store();
    let ws = workspace(&mut store);
    let id = DocumentId::new();
    let original = seed_bytes("initial");
    let result = store
        .create_seeded_document_with_origin(
            ws,
            "Result",
            "page",
            None,
            None,
            "native:owner",
            "key",
            "hash",
            id,
            original.clone(),
            None,
            at(),
        )
        .unwrap();
    assert!(!result.creation.replayed);
    assert_eq!(result.origin.initial_update, original);
    let edited = loro::LoroDoc::new();
    edited.import(&store.snapshot(ws, id).unwrap()).unwrap();
    edited
        .get_map("body")
        .insert("text", "human revision")
        .unwrap();
    edited.commit();
    store
        .apply_update(
            ws,
            id,
            edited.export(loro::ExportMode::all_updates()).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(ws, id, at()).unwrap();
    let version = store.document_report(ws, id).unwrap().version_vector;
    let replay = store
        .create_seeded_document_with_origin(
            ws,
            "Changed title",
            "page",
            None,
            None,
            "native:owner",
            "key",
            "hash",
            id,
            seed_bytes("retry seed"),
            None,
            at(),
        )
        .unwrap();
    assert!(replay.creation.replayed);
    assert_eq!(replay.origin.initial_update, original);
    assert_eq!(
        store.document_report(ws, id).unwrap().version_vector,
        version
    );
    let read = loro::LoroDoc::new();
    read.import(&store.snapshot(ws, id).unwrap()).unwrap();
    assert_eq!(
        read.get_map("body")
            .get("text")
            .unwrap()
            .into_value()
            .unwrap()
            .as_string()
            .unwrap()
            .as_str(),
        "human revision"
    );
    store.trash_document_entry(ws, id, at(), None).unwrap();
    assert!(matches!(
        store.creation_origin(ws, id).unwrap(),
        CreationOriginLookup::Known(_)
    ));
    assert_eq!(
        store.document_report(ws, id).unwrap().trashed_at,
        Some(at())
    );
}

#[test]
fn strict_store_refuses_wrong_workspace_kind_and_unproven_candidate() {
    let mut store = memory_store();
    let ws = workspace(&mut store);
    let other = workspace(&mut store);
    let id = DocumentId::new();
    store
        .create_seeded_document_with_origin(
            ws,
            "Result",
            "page",
            None,
            None,
            "native:owner",
            "key",
            "hash",
            id,
            seed_bytes("initial"),
            None,
            at(),
        )
        .unwrap();
    for (workspace, kind) in [(other, "page"), (ws, "task")] {
        assert_eq!(
            store
                .create_seeded_document_with_origin(
                    workspace,
                    "Result",
                    kind,
                    None,
                    None,
                    "native:owner",
                    "key",
                    "hash",
                    id,
                    seed_bytes("retry"),
                    None,
                    at()
                )
                .err()
                .unwrap()
                .code(),
            Code::IdempotencyConflict
        );
    }
    let (ordinary, _) = store
        .create_document(ws, "Human", "page", None, None, at())
        .unwrap();
    assert_eq!(
        store.creation_origin(ws, ordinary).unwrap(),
        CreationOriginLookup::Unknown
    );
    assert_eq!(
        store
            .create_seeded_document_with_origin(
                ws,
                "Result",
                "page",
                None,
                None,
                "native:owner",
                "new",
                "hash",
                ordinary,
                seed_bytes("retry"),
                None,
                at()
            )
            .err()
            .unwrap()
            .code(),
        Code::IdempotencyConflict
    );
    assert_eq!(
        store
            .documents(ws)
            .unwrap()
            .iter()
            .find(|e| e.id == ordinary)
            .unwrap()
            .title,
        "Human"
    );
}

#[test]
fn conditional_write_rechecks_live_human_revision_and_pins_exact_result() {
    let mut store = memory_store();
    let ws = workspace(&mut store);
    let (id, _) = store
        .create_document(ws, "Page", "page", None, None, at())
        .unwrap();
    let before = store.version(ws, id).unwrap();
    let client = loro::LoroDoc::new();
    client.import(&store.snapshot(ws, id).unwrap()).unwrap();
    client.get_map("body").insert("text", "proposal").unwrap();
    client.commit();
    let update = client.export(loro::ExportMode::updates(&before)).unwrap();
    // Equivalent state reconstructed in a separate VV is accepted, not compared
    // as a caller's opaque token. The owner decodes before equality.
    let equivalent = loro::VersionVector::decode(&before.encode()).unwrap();
    let retained = store
        .apply_update_at_version(
            ws,
            id,
            &equivalent.encode(),
            update.clone(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    assert_eq!(
        loro::VersionVector::decode(&retained.version_vector).unwrap(),
        client.oplog_vv()
    );
    assert!(store.document_version(ws, retained.id).unwrap().is_some());
    assert_eq!(
        store
            .apply_update_at_version(ws, id, &before.encode(), update, at(), UpdateOrigin::Store)
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
    assert_eq!(store.version(ws, id).unwrap(), client.oplog_vv());
}

#[test]
fn conditional_write_refuses_missing_ancestors_without_live_pending_contamination() {
    let mut store = memory_store();
    let ws = workspace(&mut store);
    let (id, _) = store
        .create_document(ws, "Page", "page", None, None, at())
        .unwrap();
    let before = store.version(ws, id).unwrap();
    let remote = loro::LoroDoc::new();
    remote.get_map("missing").insert("first", 1).unwrap();
    remote.commit();
    let ancestor = remote.oplog_vv();
    remote.get_map("missing").insert("second", 2).unwrap();
    remote.commit();
    let delta = remote.export(loro::ExportMode::updates(&ancestor)).unwrap();
    let probe = loro::LoroDoc::new();
    assert!(probe.import(&delta).unwrap().pending.is_some());
    assert_eq!(
        store
            .apply_update_at_version(ws, id, &before.encode(), delta, at(), UpdateOrigin::Store)
            .unwrap_err()
            .code(),
        Code::CrdtRejected
    );
    assert_eq!(store.version(ws, id).unwrap(), before);
    // Supplying only the old ancestor later must not awaken refused operations.
    let ancestor_doc = remote.fork_at(&remote.vv_to_frontiers(&ancestor)).unwrap();
    store
        .apply_update(
            ws,
            id,
            ancestor_doc
                .export(loro::ExportMode::all_updates())
                .unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    assert!(
        !store
            .version(ws, id)
            .unwrap()
            .includes_vv(&remote.oplog_vv())
    );
}

#[test]
fn creation_history_recovery_reuses_first_marker_after_lost_reply_and_human_edit() {
    let mut store = memory_store();
    let ws = workspace(&mut store);
    let id = DocumentId::new();
    let key = koru_store_core::ids::HistoryEntryId::new().to_storage();
    store
        .create_seeded_document_with_origin(
            ws,
            "Page",
            "page",
            None,
            None,
            "native:owner",
            &key,
            "hash",
            id,
            seed_bytes("first"),
            None,
            at(),
        )
        .unwrap();
    let first = store
        .retain_creation_version(ws, id, "native:owner", &key, "hash", at())
        .unwrap();
    let edit = loro::LoroDoc::new();
    edit.import(&store.snapshot(ws, id).unwrap()).unwrap();
    edit.get_map("body")
        .insert("text", "later human edit")
        .unwrap();
    edit.commit();
    store
        .apply_update(
            ws,
            id,
            edit.export(loro::ExportMode::all_updates()).unwrap(),
            at(),
            UpdateOrigin::Store,
        )
        .unwrap();
    store.flush(ws, id, at()).unwrap();
    let live = store.version(ws, id).unwrap();
    let replay = store
        .retain_creation_version(
            ws,
            id,
            "native:owner",
            &key,
            "hash",
            Timestamp::from_millis(at().millis() + 99),
        )
        .unwrap();
    assert_eq!(replay, first);
    let entries = store.history_entries(ws, id, None, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id.to_storage(), key);
    assert_eq!(entries[0].recorded_at, at());
    assert!(entries[0].pinned);
    assert_eq!(store.version(ws, id).unwrap(), live);
    assert_eq!(
        store
            .retain_creation_version(ws, id, "native:other", &key, "hash", at())
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
}
