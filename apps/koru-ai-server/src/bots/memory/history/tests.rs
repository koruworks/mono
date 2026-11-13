// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{add_fact, fixture};
use super::*;

pub(in crate::bots::memory) fn evidence(
    doc: &WorkspaceDoc,
    page: &BotPage,
    version: DocumentVersionId,
) -> BoundedDocumentVersion {
    let snapshot = doc.export_snapshot().unwrap();
    let imported = loro::LoroDoc::new();
    imported.import(&snapshot).unwrap();
    BoundedDocumentVersion {
        id: koru_store_core::ids::DocumentVersionId::from_uuid(version.as_uuid()),
        document: koru_store_core::ids::DocumentId::from_uuid(page.document.as_uuid()),
        checksum: koru_store_core::lifecycle::checksum(&snapshot),
        snapshot,
        version_vector: imported.oplog_vv().encode(),
        frontiers: imported.oplog_frontiers().encode(),
        captured_at: koru_store_core::model::Timestamp::from_millis(10),
    }
}

#[test]
fn same_heading_changed_detail_cannot_pass_history_proof() {
    let (mut doc, page, version, root, _) = fixture();
    let fact = add_fact(&mut doc, root, "Stable summary");
    let detail = doc
        .insert(
            koru_core::Parent::Node(fact),
            koru_core::Node::paragraph("original detail").unwrap(),
        )
        .unwrap();
    let retained = evidence(&doc, &page, version);
    let old = index::encode_index(
        &validate(page.clone(), version, retained.clone())
            .unwrap()
            .source,
    )
    .unwrap();
    doc.set_text(detail, "changed private detail").unwrap();
    let newer = evidence(&doc, &page, version);
    let same_index = index::encode_index(
        &validate(page.clone(), version, newer.clone())
            .unwrap()
            .source,
    )
    .unwrap();
    assert_eq!(
        old.digest, same_index.digest,
        "index alone does not prove detail"
    );
    let mut corrupt = retained.clone();
    corrupt.snapshot = newer.snapshot.clone();
    assert!(matches!(
        validate(page.clone(), version, corrupt),
        Err(MemoryCodecError::InvalidSnapshot)
    ));
    let mut corrupt = retained;
    corrupt.snapshot = newer.snapshot;
    corrupt.checksum = newer.checksum;
    assert!(
        matches!(
            validate(page, version, corrupt),
            Err(MemoryCodecError::InvalidSnapshot)
        ),
        "VV/frontiers still bind the retained position"
    );
}

#[test]
fn history_reader_checks_identity_and_encoded_caps() {
    let (doc, page, version, _, _) = fixture();
    let retained = evidence(&doc, &page, version);
    assert!(validate(page.clone(), version, retained.clone()).is_ok());
    assert!(matches!(
        validate(page.clone(), DocumentVersionId::new(), retained.clone()),
        Err(MemoryCodecError::InvalidCapture)
    ));
    let mut wrong = retained.clone();
    wrong.document = koru_store_core::ids::DocumentId::new();
    assert!(matches!(
        validate(page.clone(), version, wrong),
        Err(MemoryCodecError::InvalidCapture)
    ));
    let mut huge = retained.clone();
    huge.frontiers = vec![0; MAX_BOUNDED_HISTORY_POSITION_BYTES + 1];
    assert!(matches!(
        validate(page.clone(), version, huge),
        Err(MemoryCodecError::TooLarge(MemoryBudget::HistoryMetadata))
    ));
    let mut wrong = page;
    wrong.workspace = koru_ai_core::WorkspaceId::new();
    assert!(matches!(
        validate(wrong, version, retained),
        Err(MemoryCodecError::InvalidSnapshot)
    ));
}
