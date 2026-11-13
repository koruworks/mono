// SPDX-License-Identifier: MPL-2.0
use super::*;

fn original(f: &Fixture) -> Result<koru_store_core::ports::BoundedDocumentVersion> {
    f.adapter.memory_original_seed_history(
        f.identity.owner,
        f.reader,
        &f.identity,
        &f.binding,
        &Cancellation::default(),
    )
}

#[test]
fn original_seed_write_base_never_creates_and_reuses_actual_retained_origin() {
    let f = Fixture::new();
    assert!(original(&f).is_err());
    let page = f.binding.page.clone();
    assert!(
        f.memory
            .with_store(move |store| store
                .document_entry(page.workspace, page.document)
                .unwrap()
                .is_none())
            .unwrap()
    );
    let PreparedMemoryPage::Confirmed(created) = f.prepare() else {
        panic!()
    };
    let actual_origin = f.origin();
    assert_ne!(
        seed::seed_update(&f.identity).unwrap(),
        actual_origin.initial_update
    );
    let first = original(&f).unwrap();
    let second = original(&f).unwrap();
    assert_eq!(first.id, second.id);
    assert!(
        koru_delegate_core::same_native_edit_revision(
            &created.original_edit_revision,
            &first.version_vector
        )
        .unwrap()
    );
    let version = DocumentVersionId::from_uuid(first.id.as_uuid());
    let (_, recipe) = crate::bots::memory_edit_recipe(
        f.binding.page.clone(),
        version,
        first,
        &koru_mcp_server::native::memory::MemoryRecipeOperation::Remember {
            summary: "Fact".into(),
            detail: "Detail".into(),
        },
    )
    .unwrap();
    assert_eq!(recipe.document, f.binding.page.document);
    assert_eq!(f.origin(), actual_origin);
}

#[test]
fn edited_seed_conflicts_but_explicit_exact_history_remains_available() {
    let f = Fixture::new();
    f.prepare();
    let seed = original(&f).unwrap();
    let version = DocumentVersionId::from_uuid(seed.id.as_uuid());
    f.add_fact("Human edit");
    assert!(matches!(
        original(&f),
        Err(MemoryAuthorityError::Authority(BotError::RevisionConflict))
    ));
    let retained = f
        .adapter
        .memory_edit_history(
            f.identity.owner,
            f.reader,
            &f.binding,
            version,
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(retained, seed);
    assert!(history::validate(f.binding.page.clone(), version, retained).is_ok());
}

#[test]
fn write_history_ports_reject_profile_owner_identity_and_cancellation_changes() {
    let f = Fixture::new();
    f.prepare();
    let seed = original(&f).unwrap();
    let version = DocumentVersionId::from_uuid(seed.id.as_uuid());
    assert!(
        f.adapter
            .memory_edit_history(
                f.identity.owner,
                f.other,
                &f.binding,
                version,
                &Cancellation::default()
            )
            .is_err()
    );
    let mut owner = f.identity.owner;
    owner.device = DeviceId::new();
    assert!(
        f.adapter
            .memory_edit_history(
                owner,
                f.reader,
                &f.binding,
                version,
                &Cancellation::default()
            )
            .is_err()
    );
    let mut identity = f.identity.clone();
    identity.store_request = BotRequestId::mint();
    assert!(
        f.adapter
            .memory_original_seed_history(
                f.identity.owner,
                f.reader,
                &identity,
                &f.binding,
                &Cancellation::default()
            )
            .is_err()
    );
    let mut binding = f.binding.clone();
    binding.creation_request = BotRequestId::mint();
    assert!(
        f.adapter
            .memory_original_seed_history(
                f.identity.owner,
                f.reader,
                &f.identity,
                &binding,
                &Cancellation::default()
            )
            .is_err()
    );
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(
        f.adapter
            .memory_edit_history(f.identity.owner, f.reader, &f.binding, version, &cancelled)
            .is_err()
    );
    assert!(
        f.adapter
            .memory_edit_history(
                f.identity.owner,
                f.reader,
                &f.binding,
                DocumentVersionId::new(),
                &Cancellation::default()
            )
            .is_err()
    );
    assert_eq!(original(&f).unwrap(), seed);
}

#[test]
fn original_seed_route_refuses_actual_corrupt_retained_dedup_row() {
    let f = Fixture::new();
    f.prepare();
    let origin = f.origin();
    let doc = seed::validate_seed_document(&f.identity, &origin.initial_update).unwrap();
    let vv = doc.doc().oplog_vv().encode();
    let frontiers = doc.doc().oplog_frontiers().encode();
    let page = f.binding.page.clone();
    let poisoned = f
        .memory
        .with_store(move |store| {
            // The ordinary history writer records supplied bytes. Use the real seed
            // position with a malformed snapshot so the next capture must confront
            // an actual retained dedup row, rather than a fabricated port response.
            let retained = store
                .record_history_state(
                    page.workspace,
                    page.document,
                    b"malformed retained snapshot",
                    &vv,
                    &frontiers,
                    HistoryReason::Manual,
                    Timestamp::from_millis(8),
                )
                .unwrap();
            let row = store
                .bounded_document_version(page.workspace, retained.version)
                .unwrap()
                .unwrap();
            assert_eq!(row.snapshot, b"malformed retained snapshot");
            retained.version
        })
        .unwrap();
    assert!(matches!(
        original(&f),
        Err(MemoryAuthorityError::HistoryUnavailable)
    ));
    let page = f.binding.page.clone();
    let retained = f
        .memory
        .with_store(move |store| {
            store
                .bounded_document_version(page.workspace, poisoned)
                .unwrap()
                .unwrap()
        })
        .unwrap();
    assert_eq!(
        retained.snapshot, b"malformed retained snapshot",
        "refusal does not replace retained evidence"
    );
    assert_eq!(f.origin(), origin);
}
