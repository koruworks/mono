// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_core::WorkspaceDoc;
use koru_store_core::ports::{BoundedDocumentVersion, MAX_BOUNDED_HISTORY_POSITION_BYTES};

/// Validated encoded history and page structure, never current read authority.
/// Private fields prevent a detail caller from substituting another document.
pub(in crate::bots) struct VerifiedMemory {
    pub(super) doc: WorkspaceDoc,
    pub(super) source: MemoryIndexSource,
}

/// Must execute off the Store owner. Even a well-formed port response is checked
/// again: fakes/other repositories must not bypass the encoded import limits.
pub(in crate::bots) fn validate(
    page: BotPage,
    version: DocumentVersionId,
    evidence: BoundedDocumentVersion,
) -> Result<VerifiedMemory> {
    if evidence.snapshot.len() > SNAPSHOT_LIMIT {
        return Err(MemoryCodecError::TooLarge(MemoryBudget::EncodedSnapshot));
    }
    if evidence.version_vector.len() > MAX_BOUNDED_HISTORY_POSITION_BYTES
        || evidence.frontiers.len() > MAX_BOUNDED_HISTORY_POSITION_BYTES
    {
        return Err(MemoryCodecError::TooLarge(MemoryBudget::HistoryMetadata));
    }
    if evidence.id.as_uuid() != version.as_uuid()
        || evidence.document.as_uuid() != page.document.as_uuid()
        || evidence.snapshot.is_empty()
    {
        return Err(MemoryCodecError::InvalidCapture);
    }
    let imported = koru_store_core::lifecycle::validate_history_evidence(
        &evidence.snapshot,
        &evidence.version_vector,
        &evidence.frontiers,
        &evidence.checksum,
    )
    .map_err(|_| MemoryCodecError::InvalidSnapshot)?;
    let doc = WorkspaceDoc::adopt(imported).map_err(|_| MemoryCodecError::InvalidSnapshot)?;
    let source = observe::observe_document(page, version, &doc)?;
    Ok(VerifiedMemory { doc, source })
}

#[cfg(test)]
pub(super) mod tests;
