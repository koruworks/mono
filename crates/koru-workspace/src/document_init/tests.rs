// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{Workspace, document_root, seed_document};
use koru_core::{Attr, AttrValue, Content, DocumentId, Mark, Node, WorkspaceId};

fn empty() -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(
        &loro::LoroDoc::new()
            .export(loro::ExportMode::snapshot())
            .unwrap(),
    )
    .unwrap()
}

fn source() -> (WorkspaceDoc, NodeId) {
    let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "private").unwrap();
    let root = doc
        .insert(Parent::Root, Node::document("Draft title").unwrap())
        .unwrap();
    doc.set_mark(root, 0..5, Mark::Bold, true).unwrap();
    doc.set_attr(root, &Attr::new("tag", AttrValue::Text("authored".into())))
        .unwrap();
    doc.insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    let list = doc
        .insert(Parent::Node(root), Node::list_block().unwrap())
        .unwrap();
    doc.insert(Parent::Node(list), Node::paragraph("Nested body").unwrap())
        .unwrap();
    (doc, root)
}

fn sync(a: &mut WorkspaceDoc, b: &mut WorkspaceDoc) {
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();
}

#[test]
fn initialization_preserves_complete_content_and_only_destination_identity() {
    let (source, source_root) = source();
    let before = source.version();
    let cutting = source.cutting(source_root).unwrap();
    let mut destination = empty();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    destination.stamp_workspace_id(workspace).unwrap();
    destination.stamp_document_id(document).unwrap();
    let root = initialize_document_from_cutting(&mut destination, &cutting, "").unwrap();
    assert_eq!(destination.id(), Some(workspace));
    assert_eq!(destination.stamped_document_id(), Some(document));
    assert_eq!(destination.own_document_id(), Some(document));
    assert_ne!(
        destination.document_id(root),
        source.document_id(source_root)
    );
    let source_nodes = source.walk().unwrap();
    let destination_nodes = destination.walk().unwrap();
    assert_eq!(source_nodes.len(), destination_nodes.len());
    for ((from, from_depth), (to, to_depth)) in source_nodes.iter().zip(&destination_nodes) {
        let original = source.read(*from).unwrap();
        let copied = destination.read(*to).unwrap();
        assert_ne!(from, to);
        assert_eq!(from_depth, to_depth);
        assert_eq!(original.kind, copied.kind);
        assert_eq!(original.meta, copied.meta);
        match (&original.content, &copied.content) {
            (
                Content::Text {
                    text: original_text,
                    attrs: original_attrs,
                },
                Content::Text {
                    text: copied_text,
                    attrs: copied_attrs,
                },
            ) => {
                assert_eq!(original_text, copied_text);
                assert_eq!(original_attrs.len(), copied_attrs.len());
                for (from_attr, to_attr) in original_attrs.iter().zip(copied_attrs) {
                    assert_ne!(from_attr.id, to_attr.id);
                    assert_eq!(from_attr.role, to_attr.role);
                    assert_eq!(from_attr.value, to_attr.value);
                    assert_eq!(from_attr.label, to_attr.label);
                }
            }
            _ => assert_eq!(original.content, copied.content),
        }
        assert_eq!(source.runs(*from).unwrap(), destination.runs(*to).unwrap());
    }
    assert_eq!(source.version(), before);
    // The initialized content is the opening baseline. Canonical edits use
    // the ordinary undo manager without importing private editor history.
    destination.enable_undo();
    assert!(!destination.can_undo());
    destination.insert_text(root, 0, "Later ").unwrap();
    destination.checkpoint();
    assert!(destination.undo().unwrap());
    assert_eq!(
        destination.read(root).unwrap().content.as_text(),
        Some("Draft title")
    );
}

#[test]
fn initialization_refuses_even_an_empty_published_seed_without_writing() {
    let (source, root) = source();
    let mut destination = empty();
    let seed = seed_document(&mut destination, "", koru_core::explore::KIND_DOCUMENT).unwrap();
    let before = destination.version();
    assert!(
        initialize_document_from_cutting(&mut destination, &source.cutting(root).unwrap(), "")
            .is_err()
    );
    assert_eq!(destination.version(), before);
    assert_eq!(document_root(&destination), Some(seed));
}

#[test]
fn initialization_refuses_trash_and_newer_schema_without_writing() {
    let (source, root) = source();
    let cutting = source.cutting(root).unwrap();
    let mut trash = empty();
    let deleted = trash
        .insert(Parent::Root, Node::paragraph("kept in bin").unwrap())
        .unwrap();
    trash.trash(deleted, 1, None).unwrap();
    assert!(trash.walk().unwrap().is_empty());
    let before = trash.version();
    assert!(initialize_document_from_cutting(&mut trash, &cutting, "").is_err());
    assert_eq!(trash.version(), before);

    let mut newer = empty();
    newer
        .doc()
        .get_map("meta")
        .insert("schema_version", koru_core::schema::SCHEMA_VERSION + 1)
        .unwrap();
    newer.doc().commit();
    let before = newer.version();
    assert!(initialize_document_from_cutting(&mut newer, &cutting, "").is_err());
    assert_eq!(newer.version(), before);
}

#[test]
fn registration_visible_peer_seed_and_authored_title_survive_merge_and_repair() {
    let (source, source_root) = source();
    let mut local = empty();
    let mut peer = WorkspaceDoc::from_snapshot(&local.export_snapshot().unwrap()).unwrap();
    // The manifest registration is visible before either client publishes its
    // body. A peer can seed and edit while our private cutting is initialized.
    let peer_root =
        seed_document(&mut peer, "Peer title", koru_core::explore::KIND_DOCUMENT).unwrap();
    peer.set_mark(peer_root, 0..4, Mark::Italic, true).unwrap();
    let attr = Attr::new("tag", AttrValue::Text("peer".into()));
    peer.set_attr(peer_root, &attr).unwrap();
    let peer_body = peer.children(Parent::Node(peer_root)).unwrap()[0];
    peer.set_text(peer_body, "Peer body").unwrap();
    let local_root =
        initialize_document_from_cutting(&mut local, &source.cutting(source_root).unwrap(), "")
            .unwrap();
    sync(&mut local, &mut peer);
    assert!(local.unify_document_roots().unwrap());
    assert!(peer.unify_document_roots().unwrap());
    sync(&mut local, &mut peer);
    for doc in [&local, &peer] {
        assert_eq!(doc.children(Parent::Root).unwrap().len(), 1);
        assert_eq!(
            doc.read(local_root).unwrap().content.as_text(),
            Some("Draft title")
        );
        assert_eq!(
            doc.read(peer_root).unwrap().content.as_text(),
            Some("Peer title")
        );
        assert_eq!(
            doc.read(peer_body).unwrap().content.as_text(),
            Some("Peer body")
        );
        assert!(doc.runs(local_root).unwrap()[0].has(Mark::Bold));
        assert!(doc.runs(peer_root).unwrap()[0].has(Mark::Italic));
        assert!(doc.read(peer_root).unwrap().content.attrs().contains(&attr));
    }
    assert_eq!(local.walk().unwrap(), peer.walk().unwrap());
}

#[test]
fn already_received_peer_content_is_refused_without_mutation() {
    let (source, source_root) = source();
    let mut destination = empty();
    let mut peer = WorkspaceDoc::from_snapshot(&destination.export_snapshot().unwrap()).unwrap();
    let peer_root =
        seed_document(&mut peer, "Peer content", koru_core::explore::KIND_DOCUMENT).unwrap();
    destination
        .import(&peer.export_all_updates().unwrap())
        .unwrap();
    let before = destination.version();
    assert!(
        initialize_document_from_cutting(
            &mut destination,
            &source.cutting(source_root).unwrap(),
            ""
        )
        .is_err()
    );
    assert_eq!(destination.version(), before);
    assert_eq!(
        destination.read(peer_root).unwrap().content.as_text(),
        Some("Peer content")
    );
}

#[test]
fn initialization_cannot_reopen_an_existing_active_or_parked_owner() {
    let (source, source_root) = source();
    let cutting = source.cutting(source_root).unwrap();
    let id = DocumentId::new();
    let scope = source.id().unwrap();
    let mut workspace = Workspace::holding_for_tests(source, id);
    let before = workspace.ws.version();
    let contents = InitialContents::Cutting {
        cutting: &cutting,
        title: "",
    };
    assert!(
        workspace
            .open_document_in_with_contents(scope, id, contents)
            .is_err()
    );
    assert_eq!(workspace.active(), Some(id));
    assert_eq!(workspace.ws.version(), before);
    let parked = DocumentId::new();
    workspace.others.insert(parked, empty());
    assert!(
        workspace
            .open_document_in_with_contents(scope, parked, contents)
            .is_err()
    );
    assert_eq!(workspace.active(), Some(id));
    assert!(workspace.others.contains_key(&parked));
    assert_eq!(workspace.ws.version(), before);
}
