// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{Pending, Workspace, explore};
use koru_store_core::{manifest::Manifest, model::Timestamp};

#[test]
fn creation_echo_and_local_title_edit_reconcile_in_the_same_tick() {
    rename_before_creation_echo(true);
}

#[test]
fn local_title_changes_before_the_creation_echo_arrives() {
    rename_before_creation_echo(false);
}

fn rename_before_creation_echo(deliver_on_first_tick: bool) {
    let (document_session, _, document) = session();
    let (manifest_session, manifest_link, _) = session();
    let id = koru_core::DocumentId::new();
    let mut workspace = Workspace::new(document);
    let title = workspace
        .ws
        .insert(Parent::Root, Node::document("Made next door").unwrap())
        .unwrap();
    workspace.active = Some(id);
    workspace.doc_sessions.insert(id, document_session);
    workspace.manifest_session = Some(manifest_session);
    let manifest =
        Manifest::create(StoreWorkspaceId::new(), "Test", Timestamp::from_millis(1)).unwrap();
    let mut remote = Manifest::from_snapshot(&manifest.export_snapshot().unwrap()).unwrap();
    workspace.manifest = Some(manifest);
    workspace.pending_documents.push(Pending {
        id,
        title: "Made next door".into(),
        after: None,
        parent: None,
        kind: koru_core::explore::KIND_DOCUMENT.into(),
    });
    remote
        .add_document(
            explore::store_document(id),
            "Made next door",
            "document",
            None,
        )
        .unwrap();
    if deliver_on_first_tick {
        manifest_link.delivers(vec![remote.export_all_updates().unwrap()]);
    }

    workspace.ws.set_text(title, "Renamed next door").unwrap();
    workspace.tick();

    assert!(workspace.take_entries_changed());
    assert_eq!(workspace.entries()[0].title, "Renamed next door");
    if !deliver_on_first_tick {
        assert_eq!(workspace.pending_documents.len(), 1);
        assert!(workspace.manifest.as_ref().unwrap().entries().is_empty());
        manifest_link.delivers(vec![remote.export_all_updates().unwrap()]);
        workspace.tick();
        assert!(workspace.take_entries_changed());
        assert_eq!(workspace.entries()[0].title, "Renamed next door");
    }
    assert!(
        workspace.pending_documents.is_empty(),
        "the authoritative row replaced the provisional one"
    );
    for update in manifest_link.sent() {
        remote.import_frames(&[update]).unwrap();
    }
    assert_eq!(
        remote.entries()[0].title,
        "Renamed next door",
        "the rename was published in the same turn"
    );
}
