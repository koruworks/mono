// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::explore;
use koru_core::{Node, Parent, WorkspaceId};

fn fixture() -> (
    Workspace,
    DocumentId,
    mpsc::Receiver<()>,
    Arc<crate::store::link::fake::FakeLink>,
) {
    let workspace = WorkspaceId::new();
    let mut owner = Workspace::new(WorkspaceDoc::create(workspace, "active").unwrap());
    let id = DocumentId::new();
    let mut doc = WorkspaceDoc::create(workspace, "closed").unwrap();
    doc.stamp_document_id(id).unwrap();
    let row = doc
        .insert(Parent::Root, Node::paragraph("").unwrap())
        .unwrap();
    doc.enable_undo();
    doc.insert_text(row, 0, "accepted").unwrap();
    doc.checkpoint();
    let (session, link) = StoreSession::for_system_test(
        explore::store_workspace(workspace),
        explore::store_document(id),
    );
    owner.others.insert(id, doc);
    owner.doc_sessions.insert(id, session);
    let (wake, woke) = mpsc::channel();
    owner.waker = Some(crate::store::link::Waker::new(move || {
        let _ = wake.send(());
    }));
    (owner, id, woke, link)
}

#[test]
fn close_fences_reopen_and_returns_unbuffered_edits_with_undo() {
    let _guard = crate::document_load::tests::WORKERS.lock().unwrap();
    let (mut owner, id, woke, _) = fixture();
    assert!(owner.close_document(id));
    assert!(owner.document_retiring(id));
    assert!(owner.document(id).is_none());
    assert!(owner.resident_document_mut(id).is_none());
    assert!(
        owner.ensure_document(id).is_err(),
        "must not locate or join"
    );
    assert!(owner.unconfirmed().would_lose_work());
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    assert!(!owner.document_retiring(id));
    let restored = owner.others.get_mut(&id).unwrap();
    let row = restored.children(Parent::Root).unwrap()[0];
    assert_eq!(
        restored.read(row).unwrap().content.as_text(),
        Some("accepted")
    );
    assert!(restored.undo().unwrap());
    assert_eq!(restored.read(row).unwrap().content.as_text(), Some(""));
    // The worker still owns its permit until it observes release. Wait only
    // in this test, never on the UI path, before the shared-capacity next test.
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
}

#[test]
fn switching_workspace_does_not_discard_a_failed_retirement() {
    let _guard = crate::document_load::tests::WORKERS.lock().unwrap();
    let (mut owner, id, woke, _) = fixture();
    assert!(owner.close_document(id));
    owner.current = Some(WorkspaceId::new());
    owner.others.clear();
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    assert!(owner.document(id).is_some());
    assert_eq!(owner.unconfirmed().documents, 1);
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
}

#[test]
fn failed_close_retires_after_the_original_session_acknowledges() {
    let _guard = crate::document_load::tests::WORKERS.lock().unwrap();
    let (mut owner, id, woke, link) = fixture();
    assert!(owner.close_document(id));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(owner.retirement_retries.contains(&id));
    let doc = owner.others.get_mut(&id).unwrap();
    let session = owner.doc_sessions.get_mut(&id).unwrap();
    let batch = session.publish(doc).unwrap().unwrap();
    link.acks(batch, loro_protocol::UpdateStatusCode::Ok);
    session.drain(doc).unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    assert!(owner.document_retiring(id));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(!owner.document_retiring(id));
    assert!(owner.document(id).is_none());
    assert!(owner.unconfirmed().is_empty());
}

#[test]
fn ordinary_capture_refuses_a_retiring_task_list_before_joining() {
    let _guard = crate::document_load::tests::WORKERS.lock().unwrap();
    let (mut owner, id, woke, _) = fixture();
    let workspace = owner.others[&id].id().unwrap();
    let mut manifest = koru_store_core::manifest::Manifest::create(
        explore::store_workspace(workspace),
        "W",
        koru_store_core::model::Timestamp::from_millis(1),
    )
    .unwrap();
    manifest
        .add_document(
            explore::store_document(id),
            "Tasks",
            crate::tasks::KIND_TASK_LIST,
            None,
        )
        .unwrap();
    owner.manifest = Some(manifest);
    assert!(owner.close_document(id));
    let result = owner.create_native_task("new", None, None, None);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("still being saved")
    );
    assert!(!owner.doc_sessions.contains_key(&id));
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    owner.waker.as_ref().unwrap().rearm();
    owner.poll_document_retirements();
    woke.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
}

#[test]
fn final_replacement_guard_rechecks_edits_accepted_during_profile_preparation() {
    let (mut owner, id, _woke, link) = fixture();
    let doc = owner.others.get_mut(&id).unwrap();
    let session = owner.doc_sessions.get_mut(&id).unwrap();
    let batch = session.publish(doc).unwrap().unwrap();
    link.acks(batch, loro_protocol::UpdateStatusCode::Ok);
    session.drain(doc).unwrap();
    owner.prepare_document_replacement().unwrap();
    // The asynchronous incoming profile is still being prepared. An accepted
    // edit after its start has no acknowledgement and this fake has no disk.
    let doc = owner.others.get_mut(&id).unwrap();
    let row = doc.children(Parent::Root).unwrap()[0];
    doc.insert_text(row, 8, " later").unwrap();
    assert!(owner.prepare_document_replacement().is_err());
    assert_eq!(
        owner
            .document(id)
            .unwrap()
            .read(row)
            .unwrap()
            .content
            .as_text(),
        Some("accepted later")
    );
    assert!(owner.doc_sessions.contains_key(&id));
}
