// SPDX-License-Identifier: MPL-2.0

use super::*;

fn node(doc: &LoroDoc, parent: TreeParentId, kind: &str, hash: Option<&str>) -> (TreeID, LoroMap) {
    let tree = doc.get_tree(C_NODES);
    let tid = tree.create(parent).unwrap();
    let meta = tree.get_meta(tid).unwrap();
    meta.insert(K_ID, NodeId::new().to_storage()).unwrap();
    meta.insert(K_KIND, kind).unwrap();
    if let Some(hash) = hash {
        meta.insert(K_BLOB, hash).unwrap();
    }
    (tid, meta)
}
fn read(doc: &LoroDoc) -> Inspection<InspectedFileReferences> {
    inspect_file_references(doc, ReferenceBudget::default())
}
fn refusal(doc: &LoroDoc, expected: StrictReferenceError) {
    assert_eq!(read(doc).err(), Some(expected));
}

#[test]
fn live_trash_and_nested_descendants_are_all_retained_without_a_manifest() {
    let doc = LoroDoc::new();
    node(&doc, TreeParentId::Root, "image", Some("b3:live"));
    let (trash, _) = node(&doc, TreeParentId::Root, "trash", None);
    let (folder, _) = node(&doc, TreeParentId::Node(trash), "document", None);
    node(
        &doc,
        TreeParentId::Node(folder),
        "image",
        Some("b3:trashed"),
    );
    node(&doc, TreeParentId::Node(folder), "file", Some("b3:live"));
    let before = doc.state_vv();
    let references = read(&doc).unwrap();
    assert_eq!(references.blobs().len(), 3);
    assert!(references.blobs().contains(&"b3:trashed".to_owned()));
    assert_eq!(before, doc.state_vv(), "inspection never authors repairs");
}

#[test]
fn legacy_trees_are_inspected_and_physical_deletion_is_not_trash() {
    let doc = LoroDoc::new();
    let tree = doc.get_tree(C_LIBRARY);
    let tid = tree.create(TreeParentId::Root).unwrap();
    let meta = tree.get_meta(tid).unwrap();
    meta.insert(K_ID, NodeId::new().to_storage()).unwrap();
    meta.insert(K_KIND, "image").unwrap();
    meta.insert(K_BLOB, "b3:legacy").unwrap();
    assert_eq!(read(&doc).unwrap().blobs(), &["b3:legacy"]);
    tree.delete(tid).unwrap();
    assert!(read(&doc).unwrap().blobs().is_empty());
}

#[test]
fn malformed_and_duplicate_node_id_never_disappear_in_an_index() {
    let doc = LoroDoc::new();
    let (_, first) = node(&doc, TreeParentId::Root, "paragraph", None);
    let (_, second) = node(&doc, TreeParentId::Root, "paragraph", None);
    second
        .insert(K_ID, as_string(first.get(K_ID).unwrap()).unwrap())
        .unwrap();
    refusal(&doc, StrictReferenceError::DuplicateIdentity);
    second.insert(K_ID, "not-an-id").unwrap();
    refusal(&doc, StrictReferenceError::Identity);
    second.delete(K_ID).unwrap();
    refusal(&doc, StrictReferenceError::Shape);
}

#[test]
fn malformed_blob_and_unknown_kind_or_field_are_unknown() {
    let doc = LoroDoc::new();
    let (_, meta) = node(&doc, TreeParentId::Root, "image", Some("b3:ok"));
    meta.insert(K_BLOB, 42).unwrap();
    refusal(&doc, StrictReferenceError::Shape);
    meta.delete(K_BLOB).unwrap();
    refusal(&doc, StrictReferenceError::Shape);
    meta.insert(K_BLOB, "").unwrap();
    refusal(&doc, StrictReferenceError::Hash);
    meta.insert(K_BLOB, "b3:ok").unwrap();
    meta.insert(K_KIND, "future-file").unwrap();
    refusal(&doc, StrictReferenceError::Unsupported);
    meta.insert(K_KIND, "image").unwrap();
    meta.insert("future_blob", "b3:hidden").unwrap();
    refusal(&doc, StrictReferenceError::Unsupported);
}

#[test]
fn embed_precedence_cannot_hide_a_blob_or_attributes() {
    let doc = LoroDoc::new();
    let (_, meta) = node(&doc, TreeParentId::Root, "image", Some("b3:kept"));
    meta.insert(K_EMBED, NodeId::new().to_storage()).unwrap();
    assert_eq!(read(&doc).unwrap().blobs(), &["b3:kept"]);
    let attrs = meta.ensure_mergeable_map(K_ATTRS).unwrap();
    let record = attrs
        .ensure_mergeable_map(&AttrId::new().to_storage())
        .unwrap();
    record.insert(K_ATTR_ROLE, "attachment").unwrap();
    record.insert(K_ATTR_TYPE, "future_file_reference").unwrap();
    record.insert(K_ATTR_VALUE, "b3:hidden").unwrap();
    refusal(&doc, StrictReferenceError::Unsupported);
}

#[test]
fn malformed_attrs_and_future_schema_are_not_known_empty() {
    let doc = LoroDoc::new();
    let (_, meta) = node(&doc, TreeParentId::Root, "paragraph", None);
    meta.insert(K_ATTRS, "broken").unwrap();
    refusal(&doc, StrictReferenceError::Shape);
    meta.delete(K_ATTRS).unwrap();
    for version in [0, -1, SCHEMA_VERSION + 1] {
        doc.get_map(C_META).insert(K_SCHEMA, version).unwrap();
        refusal(&doc, StrictReferenceError::Schema);
    }
    doc.get_map(C_META).insert(K_SCHEMA, "5").unwrap();
    refusal(&doc, StrictReferenceError::Schema);
}

#[test]
fn every_traversal_limit_fails_without_partial_results() {
    let doc = LoroDoc::new();
    let (root, _) = node(&doc, TreeParentId::Root, "document", None);
    node(&doc, TreeParentId::Node(root), "image", Some("b3:child"));
    for budget in [
        ReferenceBudget {
            nodes: 1,
            ..Default::default()
        },
        ReferenceBudget {
            fields: 1,
            ..Default::default()
        },
        ReferenceBudget {
            depth: 0,
            ..Default::default()
        },
        ReferenceBudget {
            value_bytes: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            inspect_file_references(&doc, budget).err(),
            Some(StrictReferenceError::Budget)
        );
    }
}

#[test]
fn blob_kind_with_children_is_unknown_not_a_safe_leaf_or_subtree_purge() {
    for kind in ["file", "image", "canvas"] {
        let doc = LoroDoc::new();
        let (parent, _) = node(&doc, TreeParentId::Root, kind, Some("b3:retained"));
        node(&doc, TreeParentId::Node(parent), "paragraph", None);
        refusal(&doc, StrictReferenceError::Shape);
    }
}
