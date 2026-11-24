// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::ids::{WorkspaceId, page_document_id};

fn event(document: DocumentId, version: Option<&loro::VersionVector>) -> Event {
    Event::new(document.as_uuid(), document.as_uuid(), version, "device")
}

fn vector(peer: u64, count: i32) -> loro::VersionVector {
    let mut vv = loro::VersionVector::new();
    vv.insert(peer, count);
    vv
}

#[test]
fn exact_event_round_trips_and_retry_does_not_write() {
    let mut system = SystemDoc::create().unwrap();
    let id = DocumentId::new();
    let event = event(id, Some(&vector(1, 7)));
    assert_eq!(system.write_purge_event(&event).unwrap(), Insert::Inserted);
    let version = system.version();
    assert_eq!(system.write_purge_event(&event).unwrap(), Insert::Present);
    assert_eq!(system.version(), version);
    let restored = SystemDoc::from_snapshot(&system.export_snapshot().unwrap()).unwrap();
    assert_eq!(restored.purge_events().unwrap(), vec![event]);
    assert_eq!(restored.purged_documents(), vec![id]);
    assert!(restored.purge_coverage(id).unwrap().covers(&vector(1, 7)));
}

#[test]
fn changed_payload_or_document_under_same_event_id_is_refused() {
    let mut system = SystemDoc::create().unwrap();
    let original = event(DocumentId::new(), Some(&vector(1, 7)));
    system.write_purge_event(&original).unwrap();
    let mut changed = original.clone();
    changed.version = None;
    assert_eq!(
        system.write_purge_event(&changed).unwrap(),
        Insert::Conflict
    );
    changed = original.clone();
    changed.document_id = Uuid::new_v4();
    assert_eq!(
        system.write_purge_event(&changed).unwrap(),
        Insert::Conflict
    );
    assert_eq!(system.purge_events().unwrap(), vec![original]);
}

#[test]
fn offline_deletes_merge_in_both_orders_and_all_events_survive() {
    let base = SystemDoc::create().unwrap().export_snapshot().unwrap();
    let id = DocumentId::new();
    let mut a = SystemDoc::from_snapshot(&base).unwrap();
    let mut b = SystemDoc::from_snapshot(&base).unwrap();
    let ea = event(id, Some(&vector(1, 3)));
    let eb = event(id, Some(&vector(2, 8)));
    a.write_purge_event(&ea).unwrap();
    b.write_purge_event(&eb).unwrap();
    let sa = a.export_snapshot().unwrap();
    let sb = b.export_snapshot().unwrap();
    a.absorb(&sb).unwrap();
    b.absorb(&sa).unwrap();
    assert_eq!(a.purge_events().unwrap(), b.purge_events().unwrap());
    assert_eq!(a.purge_events().unwrap().len(), 2);
    assert_eq!(a.purge_coverage(id), b.purge_coverage(id));
    let coverage = a.purge_coverage(id).unwrap();
    assert!(coverage.covers(&vector(1, 3)));
    assert!(coverage.covers(&vector(2, 8)));
    assert!(!coverage.covers(&vector(1, 4)));
}

#[test]
fn unknown_event_dominates_known_evidence_after_merge_and_restart() {
    let mut a = SystemDoc::create().unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    let id = DocumentId::new();
    a.write_purge_event(&event(id, Some(&vector(1, 3))))
        .unwrap();
    b.write_purge_event(&event(id, None)).unwrap();
    a.absorb(&b.export_snapshot().unwrap()).unwrap();
    let restored = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    assert_eq!(restored.purge_coverage(id), Some(Coverage::Unknown));
    assert!(restored.is_document_purged(id));
}

#[test]
fn late_recover_and_keep_cannot_end_permanent_deletion() {
    let workspace = WorkspaceId::new();
    let page = PageKey::Bucket(Bucket::Today);
    let id = page_document_id(workspace, page, 0);
    let mut a = SystemDoc::create().unwrap();
    a.add_page_generation(page, 0).unwrap();
    let deletion = EventId::new();
    a.write_deletion(page, 0, deletion, &vector(1, 1), 100)
        .unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    a.write_purge_event(&event(id, Some(&vector(1, 1))))
        .unwrap();
    b.write_recover(page, 0, 2, [deletion]).unwrap();
    b.write_keep(
        page,
        0,
        deletion,
        DecisionId::new(),
        &Keep {
            replica: 2,
            covers: vector(2, 9),
        },
    )
    .unwrap();
    let sa = a.export_snapshot().unwrap();
    let sb = b.export_snapshot().unwrap();
    a.absorb(&sb).unwrap();
    b.absorb(&sa).unwrap();
    // The recoverable protocol may say Live; final-purge precedence is an
    // independent, permanent fact queried before any recovery or reseeding.
    assert!(a.is_document_purged(id));
    assert!(b.is_document_purged(id));
    assert_eq!(a.purge_events().unwrap(), b.purge_events().unwrap());
}

#[test]
fn corrupt_import_cannot_replace_existing_immutable_event() {
    let mut system = SystemDoc::create().unwrap();
    let id = DocumentId::new();
    let original = event(id, Some(&vector(1, 1)));
    system.write_purge_event(&original).unwrap();
    let corrupt = SystemDoc::from_snapshot(&system.export_snapshot().unwrap()).unwrap();
    let mut different = original.clone();
    different.version = None;
    corrupt
        .doc
        .get_map(C_PAGES)
        .ensure_mergeable_map(PURGED)
        .unwrap()
        .ensure_mergeable_map(&id.to_storage())
        .unwrap()
        .insert(&original.event_id.to_string(), write_event(&different))
        .unwrap();
    corrupt.doc.commit();
    let version = system.version();
    assert!(matches!(
        system.import(&corrupt.export_snapshot().unwrap()),
        Err(StoreError::Malformed("purge_event_conflict"))
    ));
    assert_eq!(system.version(), version);
    assert_eq!(system.purge_events().unwrap(), vec![original]);
}

#[test]
fn corrupt_evidence_preserves_tombstone_and_forces_unknown_coverage() {
    let system = SystemDoc::create().unwrap();
    let id = DocumentId::new();
    system
        .doc
        .get_map(C_PAGES)
        .ensure_mergeable_map(PURGED)
        .unwrap()
        .ensure_mergeable_map(&id.to_storage())
        .unwrap()
        .insert(&Uuid::new_v4().to_string(), "bad payload")
        .unwrap();
    system.doc.commit();
    assert!(system.is_document_purged(id));
    assert!(system.purge_events().is_err());
    assert_eq!(system.purge_coverage(id), Some(Coverage::Unknown));
}
