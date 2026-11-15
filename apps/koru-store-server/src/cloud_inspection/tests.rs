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
