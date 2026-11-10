// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::ports::{CreationOrigin, CreationOriginLookup};

/// Strict content-first success. The immutable origin accompanies the existing
/// creation result; neither value replaces current workspace authorization.
pub struct ProvenSeededCreation {
    pub creation: SeededCreation,
    pub origin: CreationOrigin,
}
impl<C: Catalog, B: Backend> Store<C, B> {
    /// Read retained local creation evidence without interpreting synced content.
    pub fn creation_origin(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<CreationOriginLookup> {
        self.ensure_open(workspace)?;
        self.open[&workspace].repo.creation_origin(document)
    }

    /// Retain the exact locally proven creation state without changing the live
    /// page or repairing its manifest. Repeated calls deduplicate the version.
    #[allow(clippy::too_many_arguments)]
    pub fn retain_creation_version(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        scope: &str,
        key: &str,
        hash: &str,
        at: Timestamp,
    ) -> Result<DocumentVersion> {
        let CreationOriginLookup::Known(origin) = self.creation_origin(workspace, document)? else {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "creation origin is unproven",
            ));
        };
        if origin.scope != scope
            || origin.request_key != key
            || origin.request_hash != hash
            || scope.is_empty()
            || scope.len() > 256
            || origin.initial_update.is_empty()
            || origin.initial_update.len() > 16 * 1024 * 1024
        {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "creation origin binding differs",
            ));
        }
        let mut original = LiveDocument::empty(document);
        crate::lifecycle::validate_cloud_import(original.doc(), &origin.initial_update)?;
        original.import(&origin.initial_update)?;
        let snapshot = original.export_snapshot()?;
        let identity = HistoryEntryId::parse(key)
            .filter(|id| {
                id.to_storage() == key
                    && id.as_uuid().get_version_num() == 4
                    && id.as_uuid().get_variant() == uuid::Variant::RFC4122
            })
            .ok_or_else(|| {
                Error::new(
                    Code::InvalidRequest,
                    "native creation key must be a canonical history identity",
                )
            })?;
        let version = original.encoded_version();
        let frontiers = original.encoded_frontiers();
        let checksum = crate::lifecycle::checksum(&snapshot);
        let entry = self
            .open
            .get_mut(&workspace)
            .expect("origin opened workspace")
            .repo
            .capture_history_once(
                &HistoryCapture {
                    document,
                    snapshot: &snapshot,
                    version_vector: &version,
                    frontiers: &frontiers,
                    checksum: &checksum,
                    reason: HistoryReason::McpAfter,
                    recorded_at: at,
                    label: None,
                    message: None,
                    boundary: Some("native_creation"),
                },
                identity,
            )?;
        self.document_version(workspace, entry.version)?
            .ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    "retained creation version is unavailable",
                )
            })
    }

    /// Internal strict candidate recovery for a trusted native semantic owner.
    /// Existing ordinary seeded-create routes retain their original behavior.
    #[allow(clippy::too_many_arguments)]
    pub fn create_seeded_document_with_origin(
        &mut self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        scope: &str,
        idempotency_key: &str,
        request_hash: &str,
        candidate: DocumentId,
        initial_update: Vec<u8>,
        attribution: Option<&str>,
        at: Timestamp,
    ) -> Result<ProvenSeededCreation> {
        if scope.is_empty()
            || scope.len() > 256
            || initial_update.is_empty()
            || initial_update.len() > 16 * 1024 * 1024
        {
            return Err(Error::new(
                Code::InvalidRequest,
                "invalid strict creation input",
            ));
        }
        let (creation, origin) = self.create_seeded_document_inner(
            workspace,
            title,
            kind,
            parent,
            index,
            scope,
            idempotency_key,
            request_hash,
            Some(candidate),
            initial_update,
            attribution,
            at,
            true,
        )?;
        let origin =
            origin.ok_or_else(|| Error::new(Code::CorruptState, "strict creation lacks origin"))?;
        Ok(ProvenSeededCreation { creation, origin })
    }
}
