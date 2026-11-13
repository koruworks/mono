// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn history_revision_metadata_is_exact_bounded_and_does_not_read_snapshot() {
    let (mut w, document) = with_document();
    let vv = loro::VersionVector::default().encode();
    let mut capture = history_capture(document, b"opaque snapshot", &[1]);
    let sum = checksum(capture.snapshot);
    capture.checksum = &sum;
    capture.version_vector = &vv;
    let history = w.capture_history(&capture).unwrap();
    // A corrupt snapshot cannot be read by document_version, but this metadata
    // projection neither loads nor makes an integrity claim about those bytes.
    w.connection
        .execute(
            "UPDATE document_versions SET snapshot=zeroblob(1000000) WHERE id=?1",
            [history.version.to_storage()],
        )
        .unwrap();
    assert!(w.document_version(history.version).is_err());
    let exact = w
        .document_version_revision(history.version, 65_536)
        .unwrap()
        .unwrap();
    assert_eq!(exact.id, history.version);
    assert_eq!(exact.document, document);
    assert_eq!(exact.version_vector, vv);
    assert!(
        w.document_version_revision(DocumentVersionId::new(), 65_536)
            .unwrap()
            .is_none()
    );
    for budget in [0, 65_537] {
        assert_eq!(
            w.document_version_revision(history.version, budget)
                .unwrap_err()
                .code(),
            Code::InvalidRequest
        );
    }
    w.connection
        .execute(
            "UPDATE document_versions SET version_vector=zeroblob(65537) WHERE id=?1",
            [history.version.to_storage()],
        )
        .unwrap();
    assert_eq!(
        w.document_version_revision(history.version, 65_536)
            .unwrap_err()
            .code(),
        Code::PayloadTooLarge
    );
    for value in ["x'ff'", "'not a blob'"] {
        w.connection
            .execute(
                &format!("UPDATE document_versions SET version_vector={value} WHERE id=?1"),
                [history.version.to_storage()],
            )
            .unwrap();
        assert_eq!(
            w.document_version_revision(history.version, 65_536)
                .unwrap_err()
                .code(),
            Code::CorruptState
        );
    }
    // The schema forbids NULL before a read can see it.
    assert!(
        w.connection
            .execute(
                "UPDATE document_versions SET version_vector=NULL WHERE id=?1",
                [history.version.to_storage()]
            )
            .is_err()
    );
    w.connection
        .execute(
            "DELETE FROM document_history WHERE version_id=?1",
            [history.version.to_storage()],
        )
        .unwrap();
    w.connection
        .execute(
            "DELETE FROM document_versions WHERE id=?1",
            [history.version.to_storage()],
        )
        .unwrap();
    assert!(
        w.document_version_revision(history.version, 65_536)
            .unwrap()
            .is_none()
    );
}
