// SPDX-License-Identifier: MPL-2.0

use super::*;

fn context(root: &Path) -> Context {
    Context {
        segment: "test".into(),
        root: Some(root.to_owned()),
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        title: "My note".into(),
        parent: None,
        kind: koru_core::explore::KIND_DOCUMENT.into(),
    }
}

fn source_doc(context: &Context) -> WorkspaceDoc {
    let mut doc = WorkspaceDoc::create(context.workspace, "source").unwrap();
    doc.stamp_document_id(context.document).unwrap();
    doc.insert(
        koru_core::schema::Parent::Root,
        koru_core::node::Node::paragraph("local writing").unwrap(),
    )
    .unwrap();
    doc
}

#[test]
fn kept_copy_has_a_fresh_identity_and_covers_source_history() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let doc = source_doc(&context);
    let original = doc.version();
    let a = DocumentId::new();
    let b = DocumentId::new();
    let first = doc.fork_as_document(a).unwrap();
    let second = doc.fork_as_document(b).unwrap();
    assert_eq!(doc.own_document_id(), Some(context.document));
    assert_eq!(first.own_document_id(), Some(a));
    assert_eq!(second.own_document_id(), Some(b));
    assert_ne!(first.own_document_id(), second.own_document_id());
    assert!(first.version().includes_vv(&original));
    assert!(second.version().includes_vv(&original));
    assert!(doc.fork_as_document(context.document).is_err());
}

#[test]
fn restart_reuses_exact_seed_but_later_edits_need_another_decision() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let mut doc = source_doc(&context);
    let candidate = DocumentId::new();
    let copy = doc.fork_as_document(candidate).unwrap();
    let decision = Decision {
        format: 1,
        workspace: context.workspace.to_storage(),
        document: context.document.to_storage(),
        replica: doc.peer_id(),
        version: doc.version().encode(),
        candidate: Some(candidate.to_storage()),
        title: context.title.clone(),
        parent: None,
        kind: context.kind.clone(),
        initial_update: copy.export_all_updates().unwrap(),
    };
    let bytes = serde_json::to_vec(&decision).unwrap();
    crate::store::purge_intent::write_once(
        &decision_dir(&context).unwrap().join("decision.json"),
        &bytes,
    )
    .unwrap();
    let replay = previous_decision(&context, &doc.version())
        .unwrap()
        .unwrap();
    assert_eq!(replay.candidate, decision.candidate);
    assert_eq!(replay.replica, decision.replica);
    assert_eq!(replay.initial_update, decision.initial_update);
    doc.rename("later local writing").unwrap();
    assert!(
        previous_decision(&context, &doc.version())
            .unwrap()
            .is_none()
    );
}

fn receipt(
    context: &Context,
    version: &loro::VersionVector,
    files: Vec<(PathBuf, String)>,
) -> Release {
    Release {
        format: 1,
        workspace: context.workspace.to_storage(),
        document: context.document.to_storage(),
        version: version.encode(),
        store_version: None,
        replacement: None,
        files,
    }
}

#[test]
fn release_retries_partial_cleanup_and_does_not_delete_changed_files() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let copy_dir = root
        .path()
        .join("purge-copies")
        .join(context.workspace.to_storage())
        .join(context.document.to_storage());
    fs::create_dir_all(&copy_dir).unwrap();
    let one = copy_dir.join("one.loro");
    let two = copy_dir.join("two.loro");
    fs::write(&one, b"one").unwrap();
    fs::write(&two, b"two").unwrap();
    let receipt = receipt(
        &context,
        &source_doc(&context).version(),
        vec![
            (one.clone(), koru_store_core::checksum(b"one")),
            (two.clone(), koru_store_core::checksum(b"two")),
        ],
    );
    let path = release_dir(&context).unwrap().join("receipt.json");
    crate::store::purge_intent::write_once(&path, &serde_json::to_vec(&receipt).unwrap()).unwrap();
    fs::write(&two, b"new writing").unwrap();
    assert!(finish_release(&context, &receipt, &path).is_err());
    assert!(!one.exists());
    assert_eq!(fs::read(&two).unwrap(), b"new writing");
    fs::write(&two, b"two").unwrap();
    finish_release(&context, &receipt, &path).unwrap();
    finish_release(&context, &receipt, &path).unwrap();
    assert!(path.with_extension("done").exists());
    assert!(!two.exists());
}

#[test]
fn completed_release_never_hides_a_new_outbox_revision() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let doc = source_doc(&context);
    let receipt = receipt(&context, &doc.version(), Vec::new());
    let path = release_dir(&context).unwrap().join("receipt.json");
    crate::store::purge_intent::write_once(&path, &serde_json::to_vec(&receipt).unwrap()).unwrap();
    finish_release(&context, &receipt, &path).unwrap();
    let outbox = outbox::write(
        Some(root.path()),
        explore::store_workspace(context.workspace),
        explore::store_document(context.document),
        1,
        &loro::VersionVector::new(),
        &doc.export_all_updates().unwrap(),
    )
    .unwrap();
    assert!(resume_release(&context, None).unwrap().is_none());
    assert!(outbox.exists());
}

#[test]
fn release_refuses_paths_outside_the_old_documents_recovery_directories() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let unrelated = root.path().join("unrelated.loro");
    fs::write(&unrelated, b"retain").unwrap();
    let receipt = receipt(
        &context,
        &source_doc(&context).version(),
        vec![(unrelated.clone(), koru_store_core::checksum(b"retain"))],
    );
    let path = release_dir(&context).unwrap().join("receipt.json");
    assert!(finish_release(&context, &receipt, &path).is_err());
    assert_eq!(fs::read(unrelated).unwrap(), b"retain");
}
