// SPDX-License-Identifier: MPL-2.0

//! One validation/import path for ordinary and resource-retained updates.
use super::*;
use crate::{lifecycle::UpdateRetention, ports::UpdateOrigin};

impl<C: Catalog, B: Backend> Store<C, B> {
    /// Validate an update and queue it for the next commit.
    ///
    /// **Nothing is durable when this returns.** It imports into the live
    /// document — which is the validation, and is why a malformed update never
    /// reaches the journal — and adds it to the open commit window. The
    /// acknowledgement belongs to [`flush`](Self::flush).
    pub fn apply_update<O: Into<crate::ports::UpdateOrigin>>(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        bytes: Vec<u8>,
        at: Timestamp,
        origin: O,
    ) -> Result<()> {
        self.apply_update_owned(workspace, document, bytes, at, origin.into(), None)
    }

    /// Queue an update while retaining its caller-owned resource with the bytes.
    /// Validation failure/known frames release the supplied resource; a queued
    /// write retains it across failed flushes until successful commit or Store
    /// disposal. This does not make apply durable: acknowledgement still waits
    /// for flush. The resource is ephemeral, never serialized into the journal.
    pub fn apply_update_retained(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        bytes: Vec<u8>,
        at: Timestamp,
        origin: UpdateOrigin,
        retention: impl Send + Sync + 'static,
    ) -> Result<()> {
        self.apply_update_owned(
            workspace,
            document,
            bytes,
            at,
            origin,
            Some(Box::new(retention)),
        )
    }

    fn apply_update_owned(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        bytes: Vec<u8>,
        at: Timestamp,
        origin: UpdateOrigin,
        retention: Option<UpdateRetention>,
    ) -> Result<()> {
        if self.document_purge_frozen(workspace, document)? {
            return Err(Error::bare(Code::InvalidRequest));
        }
        // The import below decodes Loro bytes, whose own warnings (an
        // inconsistent tree says "Missing in parent's children") name no
        // document; the span puts both ids on them.
        let _span =
            tracing::info_span!("doc", workspace = %workspace, document = %document).entered();
        self.ensure_document(workspace, document)?;
        let open = self.open.get_mut(&workspace).expect("just opened");

        // Already have it? Then there is nothing to persist and nothing to
        // acknowledge beyond what is already acknowledged. Retries happen, and
        // they must cost bytes rather than correctness.
        let known = if open.is_manifest(document) {
            open.manifest.frame_is_known(&bytes)
        } else {
            open.documents[&document].frame_is_known(&bytes)
        };
        let cloud = matches!(origin, crate::ports::UpdateOrigin::Cloud { .. });
        if known && !cloud {
            return Ok(());
        }

        // The validation step. A malformed update is refused here, before
        // anything is written, so the journal can never hold bytes that will
        // not replay.
        if cloud && open.is_manifest(document) {
            open.manifest.import_cloud(&bytes)?;
        } else if cloud {
            open.documents
                .get_mut(&document)
                .expect("just opened")
                .import_cloud(&bytes)?;
        } else if open.is_manifest(document) {
            open.manifest.import(&bytes)?;
        } else {
            open.documents
                .get_mut(&document)
                .expect("just opened")
                .import(&bytes)?;
        }

        if known {
            // Even an already-known Cloud frame is fully validated, rather than
            // trusting only the unchecked header used by the ordinary fast path.
            return Ok(());
        }
        open.windows
            .entry(document)
            .or_insert_with(|| CommitWindow::new(document))
            .push_retained(bytes, at, origin, retention);
        Ok(())
    }
}
