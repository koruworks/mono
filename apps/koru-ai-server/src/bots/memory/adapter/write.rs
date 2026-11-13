// SPDX-License-Identifier: MPL-2.0
//! Bounded encoded evidence stays between the adapter and its background job.
use super::*;
use koru_store_core::ports::BoundedDocumentVersion;

pub(in crate::bots) fn exact(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    binding: &BotMemoryBinding,
    version: DocumentVersionId,
    cancellation: &Cancellation,
) -> Result<BoundedDocumentVersion> {
    binding_authority(adapter, owner, binding)?;
    exact_evidence(adapter, owner, reader, &binding.page, version, cancellation)
}

pub(in crate::bots) fn original_seed(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotMemoryIdentity,
    binding: &BotMemoryBinding,
    cancellation: &Cancellation,
) -> Result<BoundedDocumentVersion> {
    binding_authority(adapter, owner, binding)?;
    identity.validate()?;
    if identity.owner != owner
        || identity.bot != binding.bot
        || identity.workspace != binding.page.workspace
        || identity.candidate != binding.page.document
        || identity.creation_request != binding.creation_request
    {
        return Err(BotError::Forbidden.into());
    }
    cancelled(cancellation)?;
    let page = binding.page.clone();
    let cancel = cancellation.clone();
    let origin = adapter
        .client
        .with_store(move |store| {
            live_authority(store, owner, reader, &page, &cancel)?;
            match store
                .bounded_creation_origin(page.workspace, page.document)
                .map_err(store_error)?
            {
                CreationOriginLookup::Known(origin) => Ok(origin),
                CreationOriginLookup::Missing | CreationOriginLookup::Unknown => {
                    Err(MemoryAuthorityError::HistoryUnavailable)
                }
            }
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    if !origin_matches(identity, &origin) {
        return Err(MemoryAuthorityError::HistoryUnavailable);
    }
    let original = seed::validate_seed_document(identity, &origin.initial_update).map_err(codec)?;
    let snapshot = original
        .export_snapshot()
        .map_err(|_| MemoryAuthorityError::HistoryUnavailable)?;
    let vv = original.doc().oplog_vv();
    let frontiers = original.doc().oplog_frontiers();
    let version_vector = vv.encode();
    let encoded_frontiers = frontiers.encode();
    if snapshot.len() > SNAPSHOT_LIMIT
        || version_vector.len() > 65536
        || encoded_frontiers.len() > 65536
    {
        return Err(BotError::ContextTooLarge.into());
    }
    let page = binding.page.clone();
    let cancel = cancellation.clone();
    let expected = vv.clone();
    let evidence = adapter
        .client
        .with_store(move |store| {
            live_authority(store, owner, reader, &page, &cancel)?;
            if store
                .version(page.workspace, page.document)
                .map_err(store_error)?
                != expected
            {
                return Err(BotError::RevisionConflict.into());
            }
            let retained = store
                .record_history_state(
                    page.workspace,
                    page.document,
                    &snapshot,
                    &version_vector,
                    &encoded_frontiers,
                    HistoryReason::Manual,
                    koru_store_core::model::Timestamp::now(),
                )
                .map_err(store_error)?;
            store
                .bounded_document_version(page.workspace, retained.version)
                .map_err(store_error)?
                .ok_or(MemoryAuthorityError::HistoryUnavailable)
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    // The shared P3c decoder proves the retained bytes, including the dedup case.
    // Comparing the decoded position additionally binds them to the actual
    // retained original update, never a regenerated seed or current snapshot.
    let version = DocumentVersionId::from_uuid(evidence.id.as_uuid());
    let verified =
        history::validate(binding.page.clone(), version, evidence.clone()).map_err(codec)?;
    if verified.doc.doc().oplog_vv() != vv || verified.doc.doc().oplog_frontiers() != frontiers {
        return Err(MemoryAuthorityError::HistoryUnavailable);
    }
    recheck(adapter, owner, reader, &binding.page, cancellation)?;
    Ok(evidence)
}
