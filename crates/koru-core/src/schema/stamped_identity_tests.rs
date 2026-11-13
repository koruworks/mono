// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn explicit_identity_is_distinct_from_legacy_root_fallback() {
    let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "legacy").unwrap();
    let root = doc
        .insert(Parent::Root, Node::document("title").unwrap())
        .unwrap();
    let before = doc.doc().oplog_vv();
    assert_eq!(doc.stamped_document_id(), None);
    assert_eq!(doc.own_document_id(), Some(doc.document_id(root)));
    assert_eq!(doc.doc().oplog_vv(), before, "identity reads never repair");
    let stored = DocumentId::new();
    assert!(doc.stamp_document_id(stored).unwrap());
    assert_eq!(doc.stamped_document_id(), Some(stored));
    assert_eq!(doc.own_document_id(), Some(stored));
    assert!(!doc.stamp_document_id(DocumentId::new()).unwrap());
    assert_eq!(doc.stamped_document_id(), Some(stored));
}
