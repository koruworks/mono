// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::{Node, Parent};

pub(crate) static WORKERS: Mutex<()> = Mutex::new(());

#[test]
fn empty_document_is_seeded_before_its_first_undo() {
    let mut doc = WorkspaceDoc::adopt(loro::LoroDoc::new()).unwrap();
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    assert!(initialize(&mut doc, workspace, document, Some(("Title", "page"))).unwrap());
    let root = crate::document_root(&doc).unwrap();
    let body = doc.children(Parent::Node(root)).unwrap()[0];
    assert!(!doc.undo().unwrap(), "initial seed is not undoable");
    doc.insert_text(body, 0, "accepted edit").unwrap();
    doc.checkpoint();
    assert!(doc.undo().unwrap());
    assert_eq!(doc.read(body).unwrap().content.as_text(), Some(""));
    assert_eq!(doc.read(root).unwrap().content.as_text(), Some("Title"));
}

#[test]
fn legacy_root_identity_does_not_refuse_its_store_identity() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let mut doc = WorkspaceDoc::create(workspace, "legacy").unwrap();
    doc.insert(Parent::Root, Node::document("legacy root").unwrap())
        .unwrap();
    assert_ne!(doc.own_document_id(), Some(document));
    assert_eq!(doc.stamped_document_id(), None);
    assert!(initialize(&mut doc, workspace, document, None).unwrap());
    assert_eq!(doc.stamped_document_id(), Some(document));
    assert_eq!(doc.own_document_id(), Some(document));
}

#[test]
fn incompatible_and_conflicting_documents_are_not_repaired() {
    let workspace = WorkspaceId::new();
    for newer in [false, true] {
        let mut doc = WorkspaceDoc::create(workspace, "unchanged").unwrap();
        if newer {
            doc.doc()
                .get_map("meta")
                .insert("schema_version", i64::MAX)
                .unwrap();
            doc.doc().commit();
        } else {
            doc.stamp_document_id(DocumentId::new()).unwrap();
        }
        let before = doc.doc().oplog_vv();
        assert!(
            initialize(
                &mut doc,
                workspace,
                DocumentId::new(),
                Some(("seed", "page"))
            )
            .is_err()
        );
        assert_eq!(doc.doc().oplog_vv(), before);
    }
}

#[test]
fn warm_authority_and_undo_win_a_prepared_race() {
    let workspace = WorkspaceId::new();
    let id = DocumentId::new();
    let mut warm = WorkspaceDoc::create(workspace, "warm").unwrap();
    warm.stamp_document_id(id).unwrap();
    let row = warm
        .insert(Parent::Root, Node::paragraph("").unwrap())
        .unwrap();
    warm.enable_undo();
    warm.insert_text(row, 0, "live").unwrap();
    warm.checkpoint();
    let mut owner = Workspace::holding_for_tests(warm, id);
    let mut stale = WorkspaceDoc::create(workspace, "stale").unwrap();
    stale.stamp_document_id(id).unwrap();
    let (session, _) = StoreSession::for_system_test(
        explore::store_workspace(workspace),
        explore::store_document(id),
    );
    let (_tx, ready) = mpsc::channel();
    let (release, _rx) = mpsc::channel();
    let load = DocumentLoad {
        value: Arc::new(Mutex::new(Some(Ok(PreparedDocument {
            workspace,
            document: id,
            doc: stale,
            session,
            replay_trouble: Vec::new(),
            seeded: false,
        })))),
        ready,
        release: Some(release),
        epoch: 0,
    };
    assert_eq!(owner.install_prepared_document(load).unwrap(), id);
    assert_eq!(owner.ws.read(row).unwrap().content.as_text(), Some("live"));
    assert!(owner.ws.undo().unwrap());
    assert_eq!(owner.ws.read(row).unwrap().content.as_text(), Some(""));
}

#[test]
fn permits_count_canceled_workers_until_disposal_finishes() {
    let _guard = WORKERS.lock().unwrap();
    let mut releases = Vec::new();
    let mut workers = Vec::new();
    let (finished, disposed) = mpsc::channel();
    for _ in 0..8 {
        let (release, released) = mpsc::channel();
        releases.push(release);
        let finished = finished.clone();
        let waker = crate::store::link::Waker::new(move || {
            let _ = finished.send(());
        });
        workers.push(
            DocumentLoad::launch(0, Some(waker), move |_| {
                released.recv().unwrap();
                Err("canceled fake read".into())
            })
            .unwrap(),
        );
    }
    // Canceled windows disappear from the owner's map, but their actual
    // workers are still blocked in the injected read and count against eight.
    drop(workers);
    assert!(DocumentLoad::launch(0, None, |_| Err("must not start".into())).is_err());
    assert!(!capacity_available());
    for release in releases {
        release.send(()).unwrap();
    }
    for _ in 0..8 {
        disposed
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
    }
    // Ready wakes precede final disposal; the production bound is released
    // only after the canceled result has been dropped.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while RUNNING.load(Ordering::Acquire) != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(capacity_available());
}
