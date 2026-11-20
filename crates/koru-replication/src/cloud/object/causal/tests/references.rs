// SPDX-License-Identifier: MPL-2.0

use super::*;
use loro::LoroValue;
use loro::{LoroMap, TreeID, TreeParentId};

fn node(doc: &LoroDoc, parent: TreeParentId, kind: &str, blob: Option<&str>) -> (TreeID, LoroMap) {
    let tree = doc.get_tree("nodes");
    let id = tree.create(parent).unwrap();
    let meta = tree.get_meta(id).unwrap();
    meta.insert("id", uuid::Uuid::new_v4().to_string()).unwrap();
    meta.insert("kind", kind).unwrap();
    if let Some(blob) = blob {
        meta.insert("blob", blob).unwrap();
    }
    (id, meta)
}
fn restored(doc: &LoroDoc) -> RestoredContent {
    let raw = snapshot_object(1, doc);
    let bound = bind_fixture(&raw);
    restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap()
}

#[test]
fn strict_references_are_canonical_distinct_include_trash_and_bind_exact_content() {
    let doc = LoroDoc::new();
    doc.get_map("meta").insert("schema_version", 5).unwrap();
    let hash = format!("b3:{}", "ab".repeat(32));
    let (_, dual) = node(&doc, TreeParentId::Root, "image", Some(&hash));
    dual.insert("embed", uuid::Uuid::new_v4().to_string())
        .unwrap();
    let (trash, _) = node(&doc, TreeParentId::Root, "trash", None);
    let (folder, _) = node(&doc, TreeParentId::Node(trash), "document", None);
    node(
        &doc,
        TreeParentId::Node(folder),
        "file",
        Some(&format!("b3:{}", "AB".repeat(32))),
    );
    let second = format!("b3:{}", "cd".repeat(32));
    node(&doc, TreeParentId::Node(folder), "file", Some(&second));
    let restored = restored(&doc);
    let result = restored.strict_blob_references().unwrap();
    assert_eq!(
        result.hashes(),
        &std::collections::BTreeSet::from([hash, second])
    );
    assert_eq!(result.proof(), restored.proof());
    assert_eq!(result.proof().unit(), template(1).unit);
    assert_eq!(result.schema_version(), 5);
    assert_eq!(result.algorithm(), "files-plain-v0");
    let debug = format!("{result:?}");
    assert!(debug.contains("reference_count: 2"));
    for hash in result.hashes() {
        assert!(!debug.contains(hash));
    }
    assert!(!debug.contains(&format!("{:?}", result.proof())));
    assert!(!debug.contains(&identity::uuid_text(result.proof().cloud_unit_id())));
    assert_eq!(
        restored
            .strict_blob_references_with_budget(ReferenceBudget {
                nodes: 1,
                ..Default::default()
            })
            .unwrap_err(),
        StrictReferenceError::Budget
    );
}

#[test]
fn bad_hashes_are_unknown_even_when_the_canonical_ui_walk_would_skip_or_parse_them() {
    for hash in ["", " ", "b3:aa", "b3:not-hex", "sha256:abcd", " b3:abcd "] {
        let doc = LoroDoc::new();
        node(&doc, TreeParentId::Root, "image", Some(hash));
        assert_eq!(
            restored(&doc).strict_blob_references().unwrap_err(),
            StrictReferenceError::Hash,
            "{hash:?}"
        );
    }
}

#[test]
fn malformed_reference_unknown_root_and_unit_identity_fail_closed_without_changing_normal_restore()
{
    let doc = LoroDoc::new();
    let (_, meta) = node(&doc, TreeParentId::Root, "paragraph", None);
    meta.insert("blob", 99).unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Shape
    );
    meta.delete("blob").unwrap();
    doc.get_map("future-files")
        .insert("hidden", "blob")
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );
    let doc = LoroDoc::new();
    doc.get_map("meta")
        .insert("document", uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Scope
    );
}

/// Found in production 2026-11-17: 261 documents moved between Workspaces
/// kept the Workspace they were created in as `meta.id`, and every one was
/// refused, so their owner's storage could never be counted.
#[test]
fn a_document_moved_from_another_workspace_still_counts_its_files() {
    let doc = LoroDoc::new();
    let hash = format!("b3:{}", "ab".repeat(32));
    node(&doc, TreeParentId::Root, "image", Some(&hash));
    doc.get_map("meta")
        .insert("id", uuid::Uuid::new_v4().to_string())
        .unwrap();
    let references = restored(&doc).strict_blob_references().unwrap();
    assert_eq!(references.hashes().iter().collect::<Vec<_>>(), vec![&hash]);
}

#[test]
fn structural_units_require_supported_shapes_before_returning_empty() {
    for index in [0, 2, 3, 4] {
        let raw = cp(index, 1, 0, "base", "base_state");
        let bound = bind_fixture(&raw);
        let content = restore_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            head(bound),
            limits(),
        )
        .unwrap();
        assert!(
            content
                .strict_blob_references()
                .unwrap()
                .hashes()
                .is_empty(),
            "unit {index}"
        );
    }
    let raw = cp(0, 1, 0, "base", "base_state");
    let bound = bind_fixture(&raw);
    let content = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    // The normal manifest reader is forgiving. Strict classification must not
    // certify arbitrary content merely because its envelope says manifest.
    content
        .doc
        .get_map("meta")
        .insert("blob", "b3:hidden")
        .unwrap();
    assert_eq!(
        content.strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );
}

#[test]
fn exact_head_mismatch_and_missing_ancestor_never_yield_inspectable_content() {
    let checkpoint = cp(1, 1, 0, "base", "base_state");
    let first = segment(1, 2, 1);
    let second = segment(1, 3, 2);
    let base = bind_fixture(&checkpoint);
    assert!(
        restore_chain(
            base,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            HeadCommitment {
                result_version_digest: [0; 32]
            },
            limits()
        )
        .is_err()
    );
    let tail = bind_fixture(&second);
    assert!(
        restore_chain(
            base,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[tail],
            head(tail),
            limits()
        )
        .is_err()
    );
    let first = bind_fixture(&first);
    assert!(
        restore_chain(
            base,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[first, tail],
            head(tail),
            limits()
        )
        .is_ok()
    );
}

#[test]
fn manifest_scope_is_bound_by_restore_and_listing_kind_remains_an_open_string() {
    let raw = cp(0, 1, 0, "base", "base_state");
    let bound = bind_fixture(&raw);
    let content = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    let doc = LoroDoc::new();
    doc.import(&content.snapshot().unwrap()).unwrap();
    let tree = doc.get_tree("documents");
    let id = tree.create(TreeParentId::Root).unwrap();
    let entry = tree.get_meta(id).unwrap();
    entry
        .insert("doc_id", uuid::Uuid::new_v4().to_string())
        .unwrap();
    entry.insert("title", "Listing metadata").unwrap();
    entry.insert("kind", "future-listing-label").unwrap();
    let check = |doc: &LoroDoc| {
        let raw = snapshot_object(0, doc);
        let bound = bind_fixture(&raw);
        restore_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            head(bound),
            limits(),
        )
    };
    assert!(
        check(&doc)
            .unwrap()
            .strict_blob_references()
            .unwrap()
            .hashes()
            .is_empty()
    );
    entry.insert("kind", 42).unwrap();
    assert_eq!(
        check(&doc).unwrap().strict_blob_references().unwrap_err(),
        StrictReferenceError::Shape
    );
    entry.insert("kind", "page").unwrap();
    entry
        .insert("blob", format!("b3:{}", "ab".repeat(32)))
        .unwrap();
    assert_eq!(
        check(&doc).unwrap().strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );
    entry.delete("blob").unwrap();
    doc.get_map("meta")
        .insert("workspace_id", uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(matches!(check(&doc), Err(Error::Scope)));
}

#[test]
fn strict_helper_reference_proof_refuses_blob_parent_with_children() {
    let doc = LoroDoc::new();
    let (file, _) = node(
        &doc,
        TreeParentId::Root,
        "file",
        Some(&format!("b3:{}", "ab".repeat(32))),
    );
    node(&doc, TreeParentId::Node(file), "paragraph", None);
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Shape
    );
}

/// Found in production 2026-11-17: 34 helper documents (the Workspace system
/// document and OCR readings) were refused as unsupported, so their owner's
/// accounting never completed. They use no file of their own.
#[test]
fn helper_documents_use_no_files_and_near_misses_stay_refused() {
    let system = LoroDoc::new();
    system
        .get_map("meta")
        .insert("system_schema_version", 1)
        .unwrap();
    system.get_map("tags").insert("t1", "work").unwrap();
    system.get_map("lists").insert("l1", "inbox").unwrap();
    let references = restored(&system).strict_blob_references().unwrap();
    assert!(references.hashes().is_empty());
    assert_eq!(references.schema_version(), 0);

    let reading = LoroDoc::new();
    let hash = format!("b3:{}", "cd".repeat(32));
    reading.get_map("readings").insert(&hash, "text").unwrap();
    let references = restored(&reading).strict_blob_references().unwrap();
    assert!(
        references.hashes().is_empty(),
        "a reading names a file; it does not use it"
    );

    // A system-shaped document with an extra meta key is not recognized.
    let extra = LoroDoc::new();
    extra
        .get_map("meta")
        .insert("system_schema_version", 1)
        .unwrap();
    extra.get_map("meta").insert("other", 1).unwrap();
    extra.get_map("tags").insert("t1", "work").unwrap();
    assert_eq!(
        restored(&extra).strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );
    // A reading root beside a page tree is a page with an unknown root.
    let mixed = LoroDoc::new();
    mixed.get_map("readings").insert(&hash, "text").unwrap();
    node(&mixed, TreeParentId::Root, "paragraph", None);
    assert_eq!(
        restored(&mixed).strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );
}

fn system_with_dictionary_and_timezone() -> LoroDoc {
    let mut system = koru_core::system::SystemDoc::create().unwrap();
    system.add_dictionary_word("en-US", "Koru").unwrap();
    system.add_dictionary_word("ja", "仕事場").unwrap();
    system
        .set_default_timezone(Some(
            &koru_core::time::TimeZoneName::parse("America/New_York").unwrap(),
        ))
        .unwrap();
    let doc = LoroDoc::new();
    doc.import(&system.export_snapshot().unwrap()).unwrap();
    doc
}

#[test]
fn emitted_system_dictionary_timezone_and_purge_events_use_no_files() {
    let doc = system_with_dictionary_and_timezone();
    let deleted = uuid::Uuid::new_v4().to_string();
    let event_id = uuid::Uuid::new_v4().to_string();
    let events = doc
        .get_map("pages")
        .ensure_mergeable_map("purged")
        .unwrap()
        .ensure_mergeable_map(&deleted)
        .unwrap();
    let event = events.ensure_mergeable_map(&event_id).unwrap();
    event.insert("event_id", event_id.as_str()).unwrap();
    event.insert("document_id", deleted.as_str()).unwrap();
    event.insert("root_id", deleted.as_str()).unwrap();
    event.insert("by", "Desktop").unwrap();
    let content = restored(&doc);
    let references = content.strict_blob_references().unwrap();
    assert!(references.hashes().is_empty());
    assert_eq!(references.schema_version(), 0);
    assert_eq!(references.proof(), content.proof());

    // Clearing the timezone is a deleted key, not a null sentinel. Dictionary
    // removals also leave their language map empty; both are ordinary shapes.
    let mut system =
        koru_core::system::SystemDoc::from_snapshot(&doc.export(ExportMode::snapshot()).unwrap())
            .unwrap();
    system.set_default_timezone(None).unwrap();
    system.remove_dictionary_word("en-US", "Koru").unwrap();
    system.remove_dictionary_word("ja", "仕事場").unwrap();
    let cleared = LoroDoc::new();
    cleared.import(&system.export_snapshot().unwrap()).unwrap();
    assert!(
        restored(&cleared)
            .strict_blob_references()
            .unwrap()
            .hashes()
            .is_empty()
    );
}

#[test]
fn system_helper_rejects_new_schemas_malformed_timezone_and_dictionary() {
    for version in [0, 2] {
        let doc = system_with_dictionary_and_timezone();
        doc.get_map("meta")
            .insert("system_schema_version", version)
            .unwrap();
        assert_eq!(
            restored(&doc).strict_blob_references().unwrap_err(),
            StrictReferenceError::Schema
        );
    }
    let doc = system_with_dictionary_and_timezone();
    doc.get_map("meta")
        .insert("system_schema_version", "1")
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Schema
    );

    for zone in [LoroValue::I64(1), LoroValue::Null, "invalid zone".into()] {
        let doc = system_with_dictionary_and_timezone();
        doc.get_map("meta")
            .insert("default_timezone", zone)
            .unwrap();
        assert_eq!(
            restored(&doc).strict_blob_references().unwrap_err(),
            StrictReferenceError::Shape
        );
    }
    let doc = system_with_dictionary_and_timezone();
    doc.get_map("dictionary")
        .insert("en-US", "not a word set")
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Shape
    );
    for value in [LoroValue::Bool(false), "b3:unrecognized-word-value".into()] {
        let doc = system_with_dictionary_and_timezone();
        doc.get_map("dictionary")
            .ensure_mergeable_map("en-US")
            .unwrap()
            .insert("Koru", value)
            .unwrap();
        assert_eq!(
            restored(&doc).strict_blob_references().unwrap_err(),
            StrictReferenceError::Shape
        );
    }
}

#[test]
fn extended_system_helper_keeps_unknown_roots_fields_and_budget_failures_closed() {
    let doc = system_with_dictionary_and_timezone();
    doc.get_map("future-files")
        .insert("blob", "b3:unknown")
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );

    let doc = system_with_dictionary_and_timezone();
    doc.get_map("meta")
        .insert("future_blob", "b3:unknown")
        .unwrap();
    assert_eq!(
        restored(&doc).strict_blob_references().unwrap_err(),
        StrictReferenceError::Unsupported
    );

    let doc = system_with_dictionary_and_timezone();
    assert_eq!(
        restored(&doc)
            .strict_blob_references_with_budget(ReferenceBudget {
                fields: 1,
                ..Default::default()
            })
            .unwrap_err(),
        StrictReferenceError::Budget
    );
}
