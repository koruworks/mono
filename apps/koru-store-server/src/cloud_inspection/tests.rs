// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_core::{BlobHash, Node};

fn fixture() -> (WorkspaceDoc, CloudDocumentSelection, NodeId, NodeId) {
    let workspace = koru_core::WorkspaceId::new();
    let document = koru_core::DocumentId::new();
    let mut model = WorkspaceDoc::create(workspace, "Cloud copy").unwrap();
    model.stamp_document_id(document).unwrap();
    let root = model
        .insert(Parent::Root, Node::document("Keep this note").unwrap())
        .unwrap();
    let file = model
        .insert(
            Parent::Node(root),
            Node::file(
                "attachment",
                BlobHash::new("b3", &"a".repeat(64)),
                "application/octet-stream",
            )
            .unwrap(),
        )
        .unwrap();
    let selection = CloudDocumentSelection {
        account_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: workspace.to_storage(),
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        cloud_unit_id: uuid::Uuid::new_v4().to_string(),
        document_id: document.to_storage(),
        source_head_digest: "ab".repeat(32),
    };
    (model, selection, root, file)
}
fn detached(model: &WorkspaceDoc) -> loro::LoroDoc {
    let doc = loro::LoroDoc::new();
    doc.import(&model.export_snapshot().unwrap()).unwrap();
    doc
}
#[test]
fn explicit_last_reference_removal_preserves_note_and_ordinary_concurrent_edit() {
    let (mut cloud, selection, root, file) = fixture();
    let delta = remove_file(detached(&cloud), &selection, &file.to_storage(), 42).unwrap();
    cloud.insert_text(root, 0, "Concurrent ").unwrap();
    cloud.import(&delta).unwrap();
    assert!(hashes(&cloud).unwrap().is_empty());
    assert_eq!(
        cloud.read(root).unwrap().content.as_text(),
        Some("Concurrent Keep this note")
    );
    assert!(cloud.read(file).is_err());
}
#[test]
fn trash_retains_reference_and_duplicate_hash_remains_counted_after_one_removal() {
    let (mut cloud, selection, root, file) = fixture();
    let other = cloud
        .insert(
            Parent::Node(root),
            Node::image(
                BlobHash::new("b3", &"a".repeat(64)),
                "image/png",
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
    cloud.trash(file, 1, None).unwrap();
    assert_eq!(hashes(&cloud).unwrap().len(), 1);
    let delta = remove_file(detached(&cloud), &selection, &file.to_storage(), 42).unwrap();
    cloud.import(&delta).unwrap();
    assert_eq!(hashes(&cloud).unwrap().len(), 1);
    assert!(cloud.read(other).is_ok());
}
#[test]
fn inherited_trash_removes_only_leaf_and_keeps_note_recovery_and_other_references() {
    let (mut cloud, selection, root, file) = fixture();
    let sibling = cloud
        .insert(
            Parent::Node(root),
            Node::file("keep", BlobHash::new("b3", &"b".repeat(64)), "text/plain").unwrap(),
        )
        .unwrap();
    cloud.trash(root, 17, None).unwrap();
    let parent = cloud.parent(root).unwrap();
    let view = project(detached(&cloud), selection.clone()).unwrap();
    assert!(
        view.document
            .blocks
            .iter()
            .find(|b| b.id == file.to_storage())
            .unwrap()
            .in_trash
    );
    let delta = remove_file(detached(&cloud), &selection, &file.to_storage(), 42).unwrap();
    cloud.import(&delta).unwrap();
    assert_eq!(cloud.parent(root).unwrap(), parent);
    assert!(cloud.is_trashed(root));
    assert_eq!(
        cloud.read(root).unwrap().content.as_text(),
        Some("Keep this note")
    );
    assert_eq!(cloud.parent(sibling).unwrap(), Parent::Node(root));
    assert_eq!(
        hashes(&cloud).unwrap(),
        BTreeSet::from([format!("b3:{}", "b".repeat(64))])
    );
    cloud.restore(root, None).unwrap();
    assert!(!cloud.in_trash(sibling));
    assert_eq!(
        cloud.read(root).unwrap().content.as_text(),
        Some("Keep this note")
    );
}
#[test]
fn wrong_exact_identity_schema_and_nonfile_targets_are_unavailable_without_mutating_source() {
    let (cloud, selection, root, file) = fixture();
    let original = cloud.version();
    let mut wrong = selection.clone();
    wrong.document_id = uuid::Uuid::new_v4().to_string();
    assert!(project(detached(&cloud), wrong).is_err());
    let mut wrong = selection.clone();
    wrong.workspace_id = uuid::Uuid::new_v4().to_string();
    assert!(remove_file(detached(&cloud), &wrong, &file.to_storage(), 42).is_err());
    assert!(remove_file(detached(&cloud), &selection, &root.to_storage(), 42).is_err());
    let future = detached(&cloud);
    future.get_map("meta").insert("schema", 999).unwrap();
    assert!(project(future, selection).is_err());
    assert_eq!(cloud.version(), original);
}
#[test]
fn unknown_raw_child_cannot_be_skipped_to_authorize_leaf_removal() {
    let (cloud, selection, _, file) = fixture();
    let doc = detached(&cloud);
    let tree = doc.get_tree("nodes");
    let target = tree
        .nodes()
        .into_iter()
        .find(|id| {
            tree.get_meta(*id)
                .unwrap()
                .get("id")
                .and_then(|v| v.as_value().cloned())
                == Some(loro::LoroValue::String(file.to_storage().into()))
        })
        .unwrap();
    let child = tree.create(loro::TreeParentId::Node(target)).unwrap();
    tree.get_meta(child)
        .unwrap()
        .insert("kind", "paragraph")
        .unwrap();
    assert!(remove_file(doc, &selection, &file.to_storage(), 42).is_err());
}

#[test]
fn canvas_attachment_leaf_uses_same_strict_targeted_removal() {
    let (mut cloud, selection, root, _) = fixture();
    let canvas = cloud
        .insert(
            Parent::Node(root),
            Node::new(
                Kind::Canvas,
                koru_core::Content::blob(
                    BlobHash::new("b3", &"c".repeat(64)),
                    "application/json",
                    None,
                    None,
                ),
            )
            .unwrap(),
        )
        .unwrap();
    let view = project(detached(&cloud), selection.clone()).unwrap();
    assert!(
        view.removable_files
            .iter()
            .any(|f| f.node_id == canvas.to_storage())
    );
    let delta = remove_file(detached(&cloud), &selection, &canvas.to_storage(), 42).unwrap();
    cloud.import(&delta).unwrap();
    assert!(
        !hashes(&cloud)
            .unwrap()
            .contains(&format!("b3:{}", "c".repeat(64)))
    );
    assert!(cloud.read(root).is_ok());
}

fn raw_text(doc: &loro::LoroDoc, name: &str, kind: &str, text: &str) -> NodeId {
    let tree = doc.get_tree(name);
    let tid = tree.create(loro::TreeParentId::Root).unwrap();
    let meta = tree.get_meta(tid).unwrap();
    let id = NodeId::new();
    meta.insert("id", id.to_storage()).unwrap();
    meta.insert("kind", kind).unwrap();
    meta.ensure_mergeable_text("text")
        .unwrap()
        .insert(0, text)
        .unwrap();
    id
}
#[test]
fn mixed_legacy_trees_project_completely_and_edit_original_lineage_only() {
    use koru_core::schema::DocumentTree;
    let (mut current, selection, current_root, current_file) = fixture();
    let source = detached(&current);
    let task = raw_text(&source, "outline", "task", "Original task");
    let legacy_root = raw_text(&source, "library", "document", "Recover legacy note");
    let mut legacy =
        WorkspaceDoc::adopt_detached_tree(source.clone(), DocumentTree::Library, budget()).unwrap();
    let legacy_file = legacy
        .insert(
            Parent::Node(legacy_root),
            Node::file(
                "legacy attachment",
                BlobHash::new("b3", &"b".repeat(64)),
                "text/plain",
            )
            .unwrap(),
        )
        .unwrap();
    legacy.trash(legacy_root, 10, None).unwrap();
    current
        .import(&legacy.export_all_updates().unwrap())
        .unwrap();
    let before = current.version();
    let expected = inspect_file_references(current.doc(), budget())
        .unwrap()
        .node_ids()
        .to_vec();
    let view = project(detached(&current), selection.clone()).unwrap();
    let mut actual = view
        .document
        .blocks
        .iter()
        .map(|b| NodeId::parse(&b.id).unwrap())
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, expected);
    assert!(
        view.document
            .blocks
            .iter()
            .any(|b| b.id == task.to_storage())
    );
    assert!(
        view.document
            .blocks
            .iter()
            .find(|b| b.id == legacy_file.to_storage())
            .unwrap()
            .in_trash
    );
    let delta = remove_file(
        detached(&current),
        &selection,
        &legacy_file.to_storage(),
        42,
    )
    .unwrap();
    current.insert_text(current_root, 0, "Concurrent ").unwrap();
    current.import(&delta).unwrap();
    assert!(current.version().includes_vv(&before));
    assert!(current.read(current_file).is_ok());
    assert_eq!(
        current.read(current_root).unwrap().content.as_text(),
        Some("Concurrent Keep this note")
    );
    let mut legacy =
        WorkspaceDoc::adopt_detached_tree(detached(&current), DocumentTree::Library, budget())
            .unwrap();
    assert!(legacy.read(legacy_file).is_err());
    legacy.restore(legacy_root, None).unwrap();
    assert_eq!(
        legacy.read(legacy_root).unwrap().content.as_text(),
        Some("Recover legacy note")
    );
    assert_eq!(
        hashes(&current).unwrap(),
        BTreeSet::from([format!("b3:{}", "a".repeat(64))])
    );
}
#[test]
fn pure_legacy_edit_never_authors_a_current_tree_and_cross_tree_duplicate_is_refused() {
    use koru_core::schema::DocumentTree;
    let (_, selection, _, _) = fixture();
    let source = loro::LoroDoc::new();
    source
        .get_map("meta")
        .insert("id", selection.workspace_id.clone())
        .unwrap();
    source
        .get_map("meta")
        .insert("document", selection.document_id.clone())
        .unwrap();
    let note = raw_text(&source, "outline", "document", "Legacy body");
    let mut legacy =
        WorkspaceDoc::adopt_detached_tree(source, DocumentTree::Outline, budget()).unwrap();
    let file = legacy
        .insert(
            Parent::Node(note),
            Node::image(
                BlobHash::new("b3", &"b".repeat(64)),
                "image/png",
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let old = detached(&legacy);
    let delta = remove_file(detached(&legacy), &selection, &file.to_storage(), 42).unwrap();
    old.import(&delta).unwrap();
    assert!(!DocumentTree::Nodes.exists_in(&old));
    assert!(
        inspect_file_references(&old, budget())
            .unwrap()
            .blobs()
            .is_empty()
    );
    assert!(project(old.clone(), selection.clone()).is_ok());
    let tree = old.get_tree("library");
    let other = tree.create(loro::TreeParentId::Root).unwrap();
    let meta = tree.get_meta(other).unwrap();
    meta.insert("id", note.to_storage()).unwrap();
    meta.insert("kind", "paragraph").unwrap();
    assert!(project(old.clone(), selection).is_err());
    assert!(WorkspaceDoc::adopt_detached_tree(old, DocumentTree::Outline, budget()).is_err());
}

#[test]
fn embedded_attachment_discloses_both_slots_and_removes_only_attachment_leaf() {
    let (mut cloud, selection, root, file) = fixture();
    let target = cloud
        .insert(
            Parent::Root,
            Node::document("Embedded target survives").unwrap(),
        )
        .unwrap();
    let tree = cloud.doc().get_tree("nodes");
    let tid = tree
        .nodes()
        .into_iter()
        .find(|tid| {
            tree.get_meta(*tid)
                .unwrap()
                .get("id")
                .and_then(|v| v.as_value().cloned())
                == Some(loro::LoroValue::String(file.to_storage().into()))
        })
        .unwrap();
    tree.get_meta(tid)
        .unwrap()
        .insert("embed", target.to_storage())
        .unwrap();
    let view = project(detached(&cloud), selection.clone()).unwrap();
    let block = view
        .document
        .blocks
        .iter()
        .find(|b| b.id == file.to_storage())
        .unwrap();
    assert_eq!(block.kind, "file");
    assert!(block.blob.is_some() && block.reference.is_some());
    assert!(
        view.removable_files
            .iter()
            .any(|f| f.node_id == file.to_storage())
    );
    let delta = remove_file(detached(&cloud), &selection, &file.to_storage(), 42).unwrap();
    cloud.import(&delta).unwrap();
    assert!(hashes(&cloud).unwrap().is_empty());
    assert_eq!(
        cloud.read(target).unwrap().content.as_text(),
        Some("Embedded target survives")
    );
    assert_eq!(
        cloud.read(root).unwrap().content.as_text(),
        Some("Keep this note")
    );
}
