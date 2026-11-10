// SPDX-License-Identifier: MPL-2.0
use super::*;

fn later() -> Timestamp {
    Timestamp::from_millis(at().millis() + 8 * 24 * 60 * 60 * 1_000)
}
fn known(w: &SqliteWorkspace, d: DocumentId) -> CreationOrigin {
    let CreationOriginLookup::Known(origin) = w.creation_origin(d).unwrap() else {
        panic!("missing origin")
    };
    origin
}

#[test]
fn strict_origin_survives_receipt_expiry_two_checkpoints_and_later_edits() {
    let mut w = repo();
    let d = DocumentId::new();
    let first = w
        .seed_document_with_origin(&candidate_seed(d, "key", "semantic", at()))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(1, vec![11])).unwrap();
    w.commit_updates(&batch(d, vec![vec![22]])).unwrap();
    w.commit_checkpoint(d, &checkpoint(2, vec![33])).unwrap();
    w.commit_updates(&batch(d, vec![vec![44]])).unwrap();
    assert!(!w.updates_after(d, None).unwrap().iter().any(|u| u.seq == 1));
    let before = w.updates_after(d, None).unwrap();
    let mut retry = candidate_seed(d, "key", "semantic", later());
    retry.initial_update = vec![99];
    let result = w.seed_document_with_origin(&retry).unwrap();
    assert!(result.seeded.replayed);
    assert_eq!(result.origin, first.origin);
    assert_eq!(result.seeded.initial_update, vec![1, 2, 3]);
    assert_eq!(w.updates_after(d, None).unwrap(), before);
    assert_eq!(
        w.connection
            .query_row("SELECT count(*) FROM document_creations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn strict_candidate_requires_original_scope_key_hash_and_known_local_origin() {
    let mut w = repo();
    let d = DocumentId::new();
    w.seed_document_with_origin(&candidate_seed(d, "key", "semantic", at()))
        .unwrap();
    for field in 0..3 {
        let mut retry = candidate_seed(d, "key", "semantic", later());
        match field {
            0 => retry.scope = "other".into(),
            1 => retry.key = "other".into(),
            _ => retry.request_hash = "other".into(),
        }
        assert_eq!(
            w.seed_document_with_origin(&retry).unwrap_err().code(),
            Code::IdempotencyConflict
        );
    }
    let legacy = DocumentId::new();
    w.create_document(legacy, DocumentKind::Document, at())
        .unwrap();
    assert_eq!(
        w.creation_origin(legacy).unwrap(),
        CreationOriginLookup::Unknown
    );
    assert_eq!(
        w.seed_document_with_origin(&candidate_seed(legacy, "key2", "hash", at()))
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
    assert_eq!(
        w.creation_origin(DocumentId::new()).unwrap(),
        CreationOriginLookup::Missing
    );
    assert_eq!(w.update_count(legacy).unwrap(), 0);
}

#[test]
fn origin_failure_rolls_back_seed_receipt_and_first_update_together() {
    let mut w = repo();
    let d = DocumentId::new();
    w.connection.execute_batch("CREATE TRIGGER fail_origin BEFORE INSERT ON document_creation_origins BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        w.seed_document_with_origin(&candidate_seed(d, "key", "hash", at()))
            .is_err()
    );
    assert!(w.document(d).unwrap().is_none());
    assert!(w.updates_after(d, None).unwrap().is_empty());
    assert_eq!(
        w.connection
            .query_row("SELECT count(*) FROM document_creations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(w.creation_origin(d).unwrap(), CreationOriginLookup::Missing);
    w.connection
        .execute_batch("DROP TRIGGER fail_origin")
        .unwrap();
    assert!(
        !w.seed_document_with_origin(&candidate_seed(d, "key", "hash", at()))
            .unwrap()
            .seeded
            .replayed
    );
}

#[test]
fn ordinary_expired_key_reuse_keeps_two_distinct_origins_and_purge_cascades() {
    let mut w = repo();
    let a = DocumentId::new();
    let b = DocumentId::new();
    w.seed_document(&seed(a, "key", "first", at())).unwrap();
    w.seed_document(&seed(b, "key", "second", later())).unwrap();
    assert_eq!(known(&w, a).request_hash, "first");
    assert_eq!(known(&w, b).request_hash, "second");
    // Ordinary canonical recovery retains its established hash-insensitive behavior.
    assert!(
        w.seed_document(&candidate_seed(a, "changed", "changed", later()))
            .unwrap()
            .replayed
    );
    w.drop_document(a).unwrap();
    assert_eq!(w.creation_origin(a).unwrap(), CreationOriginLookup::Missing);
    assert_eq!(known(&w, b).request_hash, "second");
}

#[test]
fn strict_input_bounds_are_bytes_and_live_receipt_cannot_name_another_candidate() {
    let mut w = repo();
    let a = DocumentId::new();
    w.seed_document_with_origin(&candidate_seed(a, "key", "hash", at()))
        .unwrap();
    assert_eq!(
        w.seed_document_with_origin(&candidate_seed(DocumentId::new(), "key", "hash", at()))
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
    for field in 0..4 {
        let mut request = candidate_seed(DocumentId::new(), "new", "hash", at());
        match field {
            0 => request.scope = "\u{e9}".repeat(129),
            1 => request.key = "\u{e9}".repeat(65),
            2 => request.request_hash.clear(),
            _ => request.initial_update.clear(),
        }
        assert_eq!(
            w.seed_document_with_origin(&request).unwrap_err().code(),
            Code::InvalidRequest
        );
    }
    assert_eq!(w.documents().unwrap().len(), 1);
}

fn schema_twelve() -> SqliteWorkspace {
    let mut connection = db::open_in_memory().unwrap();
    db::migrate(
        &mut connection,
        "workspace",
        &MIGRATIONS[..MIGRATIONS.len() - 1],
        12,
    )
    .unwrap();
    SqliteWorkspace { connection }
}
fn old_receipt(w: &mut SqliteWorkspace, d: DocumentId, key: &str, hash: &str) {
    w.connection.execute("INSERT INTO document_creations(scope,request_key,request_hash,document_id,initial_update,created_at) VALUES ('local',?1,?2,?3,x'010203',?4)",params![key,hash,d.to_storage(),at().millis()]).unwrap();
}

#[test]
fn populated_twelve_migration_backfills_only_unambiguous_retained_evidence() {
    let mut old = schema_twelve();
    let unique = DocumentId::new();
    let ambiguous = DocumentId::new();
    let malformed = DocumentId::new();
    let imported = DocumentId::new();
    for d in [unique, ambiguous, malformed, imported] {
        old.create_document(d, DocumentKind::Document, at())
            .unwrap();
    }
    old_receipt(&mut old, unique, "unique", "hash");
    old_receipt(&mut old, ambiguous, "first", "hash");
    old_receipt(&mut old, ambiguous, "second", "hash");
    old_receipt(&mut old, malformed, "bad", "");
    let w = SqliteWorkspace::from_connection(old.connection).unwrap();
    assert_eq!(w.schema_version().unwrap(), 13);
    assert_eq!(known(&w, unique).initial_update, vec![1, 2, 3]);
    for d in [ambiguous, malformed, imported] {
        assert_eq!(w.creation_origin(d).unwrap(), CreationOriginLookup::Unknown);
    }
    let reopened = SqliteWorkspace::from_connection(w.connection).unwrap();
    assert_eq!(known(&reopened, unique).request_key, "unique");
}

#[test]
fn newer_schema_is_refused_and_failed_migration_keeps_twelve() {
    let mut old = schema_twelve();
    old.connection
        .execute_batch("CREATE TABLE document_creation_origins (conflict TEXT)")
        .unwrap();
    assert!(db::migrate(&mut old.connection, "workspace", MIGRATIONS, 13).is_err());
    assert_eq!(db::user_version(&old.connection).unwrap(), 12);
    let connection = db::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA user_version=14").unwrap();
    assert_eq!(
        SqliteWorkspace::from_connection(connection)
            .err()
            .unwrap()
            .code(),
        Code::SchemaTooNew
    );
}

#[test]
fn history_once_rejects_key_collision_and_corrupt_referenced_version() {
    let mut w = repo();
    let d = DocumentId::new();
    w.create_document(d, DocumentKind::Document, at()).unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_map("body").insert("text", "initial").unwrap();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::snapshot()).unwrap();
    let version = doc.oplog_vv().encode();
    let frontiers = doc.oplog_frontiers().encode();
    let digest = checksum(&snapshot);
    let mut capture = HistoryCapture {
        document: d,
        snapshot: &snapshot,
        version_vector: &version,
        frontiers: &frontiers,
        checksum: &digest,
        reason: HistoryReason::McpAfter,
        recorded_at: at(),
        label: None,
        message: None,
        boundary: Some("native_creation"),
    };
    let non_rfc = HistoryEntryId::parse("11111111-1111-4111-0111-111111111111").unwrap();
    assert_eq!(
        w.capture_history_once(&capture, non_rfc)
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
    let identity = HistoryEntryId::new();
    let first = w.capture_history_once(&capture, identity).unwrap();
    capture.recorded_at = Timestamp::from_millis(at().millis() + 100);
    assert_eq!(w.capture_history_once(&capture, identity).unwrap(), first);
    capture.boundary = Some("other");
    assert_eq!(
        w.capture_history_once(&capture, identity)
            .unwrap_err()
            .code(),
        Code::IdempotencyConflict
    );
    capture.boundary = Some("native_creation");
    w.connection
        .execute(
            "UPDATE document_versions SET snapshot=x'00' WHERE id=?1",
            params![first.version.to_storage()],
        )
        .unwrap();
    assert_eq!(
        w.capture_history_once(&capture, identity)
            .unwrap_err()
            .code(),
        Code::CorruptState
    );
    assert_eq!(w.history_entries(d, None, 10).unwrap().len(), 1);
}

#[test]
fn history_once_late_failure_rolls_back_version_and_marker_then_replays_once() {
    let mut w = repo();
    let d = DocumentId::new();
    w.create_document(d, DocumentKind::Document, at()).unwrap();
    let doc = loro::LoroDoc::new();
    doc.get_map("body").insert("text", "first").unwrap();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::snapshot()).unwrap();
    let version = doc.oplog_vv().encode();
    let frontiers = doc.oplog_frontiers().encode();
    let digest = checksum(&snapshot);
    let capture = HistoryCapture {
        document: d,
        snapshot: &snapshot,
        version_vector: &version,
        frontiers: &frontiers,
        checksum: &digest,
        reason: HistoryReason::McpAfter,
        recorded_at: at(),
        label: None,
        message: None,
        boundary: Some("native_creation"),
    };
    let identity = HistoryEntryId::new();
    w.connection.execute_batch("CREATE TRIGGER fail_history BEFORE INSERT ON document_history BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(w.capture_history_once(&capture, identity).is_err());
    for table in ["document_history", "document_versions"] {
        assert_eq!(
            w.connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    w.connection
        .execute_batch("DROP TRIGGER fail_history")
        .unwrap();
    let first = w.capture_history_once(&capture, identity).unwrap();
    assert_eq!(w.capture_history_once(&capture, identity).unwrap(), first);
    assert_eq!(w.history_entries(d, None, 10).unwrap().len(), 1);
}
