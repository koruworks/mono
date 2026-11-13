// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::lifecycle::validate_history_evidence;

fn history() -> (SqliteWorkspace, DocumentId, HistoryEntry, loro::LoroDoc) {
    let (mut repo, document) = with_document();
    let doc = loro::LoroDoc::new();
    doc.get_text("body")
        .insert(0, "heading and original detail")
        .unwrap();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::snapshot()).unwrap();
    let vv = doc.oplog_vv().encode();
    let frontier = doc.oplog_frontiers().encode();
    let sum = checksum(&snapshot);
    let mut capture = history_capture(document, &snapshot, &frontier);
    capture.version_vector = &vv;
    capture.checksum = &sum;
    let entry = repo.capture_history(&capture).unwrap();
    (repo, document, entry, doc)
}

#[test]
fn bounded_history_oversize_is_not_missing() {
    for (column, size) in [
        ("snapshot", 16 * 1024 * 1024 + 1),
        ("version_vector", 65537),
        ("frontiers", 65537),
    ] {
        let (repo, _, entry, _) = history();
        repo.connection
            .execute(
                &format!("UPDATE document_versions SET {column}=zeroblob(?1) WHERE id=?2"),
                params![size, entry.version.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.bounded_document_version(entry.version)
                .unwrap_err()
                .code(),
            Code::PayloadTooLarge
        );
        assert!(
            repo.bounded_document_version(DocumentVersionId::new())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn bounded_history_wrong_types_refuse() {
    for (column, value) in [
        ("snapshot", "'text'"),
        ("version_vector", "'text'"),
        ("frontiers", "3"),
        ("checksum", "x'00'"),
        ("captured_at", "'wrong'"),
        ("checksum", "'bad'"),
        ("snapshot", "x''"),
    ] {
        let (repo, _, entry, _) = history();
        repo.connection
            .execute(
                &format!("UPDATE document_versions SET {column}={value} WHERE id=?1"),
                [entry.version.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.bounded_document_version(entry.version)
                .unwrap_err()
                .code(),
            Code::CorruptState,
            "{column}"
        );
    }
}

#[test]
fn bounded_history_metadata_capped_before_transfer() {
    for column in ["checksum", "document_id"] {
        let (repo, _, entry, _) = history();
        repo.connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .unwrap();
        let value = if column == "checksum" {
            "a".repeat(65)
        } else {
            "d".repeat(37)
        };
        repo.connection
            .execute(
                &format!("UPDATE document_versions SET {column}=?1 WHERE id=?2"),
                params![value, entry.version.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.bounded_document_version(entry.version)
                .unwrap_err()
                .code(),
            Code::PayloadTooLarge
        );
    }
    let (repo, document, entry, _) = history();
    let bounded = repo
        .bounded_document_version(entry.version)
        .unwrap()
        .unwrap();
    assert_eq!(bounded.document, document);
    assert_eq!(bounded.id, entry.version);
    validate_history_evidence(
        &bounded.snapshot,
        &bounded.version_vector,
        &bounded.frontiers,
        &bounded.checksum,
    )
    .unwrap();
}

#[test]
fn retained_same_frontiers_corruption_refused() {
    let (mut repo, document, entry, doc) = history();
    repo.connection
        .execute(
            "UPDATE document_versions SET snapshot=x'00' WHERE id=?1",
            [entry.version.to_storage()],
        )
        .unwrap();
    let snapshot = doc.export(loro::ExportMode::snapshot()).unwrap();
    let vv = doc.oplog_vv().encode();
    let frontiers = doc.oplog_frontiers().encode();
    let sum = checksum(&snapshot);
    let mut capture = history_capture(document, &snapshot, &frontiers);
    capture.version_vector = &vv;
    capture.checksum = &sum;
    let retry = repo.capture_history(&capture).unwrap();
    assert_eq!(
        retry.version, entry.version,
        "ordinary dedup does not validate retained bytes"
    );
    let retained = repo
        .bounded_document_version(retry.version)
        .unwrap()
        .unwrap();
    assert_eq!(
        validate_history_evidence(
            &retained.snapshot,
            &retained.version_vector,
            &retained.frontiers,
            &retained.checksum
        )
        .err()
        .unwrap()
        .code(),
        Code::CorruptState
    );
    assert_eq!(retained.snapshot, [0]);
}

#[test]
fn bounded_origin_preserves_unknown() {
    let (mut repo, legacy) = with_document();
    assert_eq!(
        repo.bounded_creation_origin(legacy).unwrap(),
        CreationOriginLookup::Unknown
    );
    let candidate = DocumentId::new();
    assert_eq!(
        repo.bounded_creation_origin(candidate).unwrap(),
        CreationOriginLookup::Missing
    );
    let original = repo
        .seed_document_with_origin(&candidate_seed(candidate, "key", "semantic", at()))
        .unwrap();
    assert_eq!(
        repo.bounded_creation_origin(candidate).unwrap(),
        CreationOriginLookup::Known(original.origin)
    );
}

#[test]
fn bounded_origin_oversize_cannot_authorize_create() {
    for (column, expression, expected) in [
        (
            "initial_update",
            "zeroblob(16777217)",
            Code::PayloadTooLarge,
        ),
        ("scope", "zeroblob(257)", Code::CorruptState),
        ("initial_update", "'not a blob'", Code::CorruptState),
        ("request_key", "''", Code::CorruptState),
        ("created_at", "'bad'", Code::CorruptState),
    ] {
        let mut repo = repo();
        let candidate = DocumentId::new();
        repo.seed_document_with_origin(&candidate_seed(candidate, "key", "semantic", at()))
            .unwrap();
        repo.connection.execute(&format!("UPDATE document_creation_origins SET {column}={expression} WHERE document_id=?1"), [candidate.to_storage()]).unwrap();
        assert_eq!(
            repo.bounded_creation_origin(candidate).unwrap_err().code(),
            expected
        );
    }
}

#[test]
fn bounded_origin_non_ascii_limits_are_bytes() {
    for (column, max) in [("scope", 256), ("request_key", 128), ("request_hash", 128)] {
        let mut repo = repo();
        let candidate = DocumentId::new();
        repo.seed_document_with_origin(&candidate_seed(candidate, "key", "semantic", at()))
            .unwrap();
        repo.connection
            .execute(
                &format!("UPDATE document_creation_origins SET {column}=?1 WHERE document_id=?2"),
                params!["é".repeat(max / 2 + 1), candidate.to_storage()],
            )
            .unwrap();
        assert_eq!(
            repo.bounded_creation_origin(candidate).unwrap_err().code(),
            Code::PayloadTooLarge
        );
    }
}
