// SPDX-License-Identifier: MPL-2.0
//! Trusted local native writes. No public transport can opt into this seam.
use super::*;
mod drain;

/// Whether a failed conditional write reached its effect-bearing phase.
#[derive(Debug)]
pub enum NativeUpdateError {
    Refused(Error),
    Unconfirmed(Error),
}
impl NativeUpdateError {
    pub fn code(&self) -> Code {
        match self {
            Self::Refused(error) | Self::Unconfirmed(error) => error.code(),
        }
    }
}
impl From<Error> for NativeUpdateError {
    fn from(error: Error) -> Self {
        Self::Refused(error)
    }
}

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Compare the decoded current version and apply one complete causal frame
    /// within the sole Store owner. No live mutation occurs on validation failure.
    /// Success includes a pinned exact version after the existing durable flush.
    /// A failure after admission may be uncertain; callers must not blindly replay.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_update_at_version(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        expected_version: &[u8],
        update: Vec<u8>,
        at: Timestamp,
        origin: crate::ports::UpdateOrigin,
    ) -> std::result::Result<DocumentVersion, NativeUpdateError> {
        if expected_version.is_empty()
            || expected_version.len() > 65_536
            || update.is_empty()
            || update.len() > 16 * 1024 * 1024
        {
            return Err(
                Error::new(Code::InvalidRequest, "invalid conditional update bounds").into(),
            );
        }
        let expected = loro::VersionVector::decode(expected_version)
            .map_err(|_| Error::new(Code::InvalidRequest, "invalid expected version"))?;
        self.ensure_document(workspace, document)?;
        let open = &self.open[&workspace];
        if open.is_manifest(document) {
            return Err(Error::new(
                Code::InvalidRequest,
                "native writes cannot change the manifest",
            )
            .into());
        }
        let live = &open.documents[&document];
        if live.version() != expected {
            return Err(Error::new(Code::IdempotencyConflict, "document revision changed").into());
        }
        // Shared complete-causality preflight stages on a fork and rejects
        // Loro's successful-but-pending imports, including partial application.
        crate::lifecycle::validate_cloud_import(live.doc(), &update)?;
        self.apply_update(workspace, document, update, at, origin)
            .map_err(NativeUpdateError::Unconfirmed)?;
        self.flush(workspace, document, at)
            .map_err(NativeUpdateError::Unconfirmed)?;
        let marker = self
            .capture_history(
                workspace,
                document,
                None,
                HistoryReason::McpAfter,
                at,
                None,
                None,
                None,
            )
            .map_err(NativeUpdateError::Unconfirmed)?;
        self.document_version(workspace, marker.version)
            .map_err(NativeUpdateError::Unconfirmed)?
            .ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    "committed native version is unavailable",
                )
            })
            .map_err(NativeUpdateError::Unconfirmed)
    }
}
