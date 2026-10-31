// SPDX-License-Identifier: MPL-2.0

//! A reader's private, device-local view state per document
//! ([`crate::document_private`]): read, change, and clean up after the
//! document is permanently deleted.
//!
//! None of this touches the shared document: no update, no version vector,
//! no edit time, no history, no commit signal and no sync-dirty mark. A
//! caller must not wake replication for it either.

use super::*;
use crate::document_private::{DocumentPrivate, PrivateChange, PrivateState};

impl<C: Catalog, B: Backend> Store<C, B> {
    /// `owner`'s private state for `document`. Absent state is empty and
    /// writes nothing. The document must be listed in the workspace's
    /// manifest — trashed is fine — and the Profile must be active.
    pub fn document_private_state(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        owner: ProfileId,
    ) -> Result<PrivateState> {
        self.require_private_target(workspace, document, owner)?;
        let open = &self.open[&workspace];
        match open.repo.private_snapshot(document, owner)? {
            Some(bytes) => Ok(DocumentPrivate::load(&bytes, document, owner)?.state()),
            None => Ok(PrivateState::default()),
        }
    }

    /// Apply a batch of explicit values. All or nothing: the batch is
    /// validated before anything is read, merged into the stored state, and
    /// saved in one statement. A batch that changes nothing saves nothing —
    /// including the first one, so an unchanged default never creates a row.
    pub fn apply_document_private_changes(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        owner: ProfileId,
        changes: &[PrivateChange],
        at: Timestamp,
    ) -> Result<PrivateState> {
        DocumentPrivate::validate(changes)?;
        self.require_private_target(workspace, document, owner)?;
        let open = self.open.get_mut(&workspace).expect("checked open");
        let mut unit = match open.repo.private_snapshot(document, owner)? {
            Some(bytes) => DocumentPrivate::load(&bytes, document, owner)?,
            None => DocumentPrivate::fresh(document, owner)?,
        };
        if unit.apply(changes)? {
            open.repo
                .put_private_snapshot(document, owner, &unit.snapshot()?, at)?;
        }
        Ok(unit.state())
    }

    /// Which readers hold a private row for `document`, listed or not.
    /// Content-free, for diagnostics and for proving a cleanup ran.
    pub fn document_private_owners(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<ProfileId>> {
        self.ensure_open(workspace)?;
        Ok(self.open[&workspace]
            .repo
            .private_snapshots(document)?
            .into_iter()
            .map(|(owner, _)| owner)
            .collect())
    }

    /// The Profile is active, the workspace opens, and its manifest lists the
    /// document. A write queued before a permanent delete therefore fails
    /// here instead of resurrecting a row for a document that is gone.
    fn require_private_target(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        owner: ProfileId,
    ) -> Result<()> {
        self.require_profile(owner)?;
        self.ensure_open(workspace)?;
        let open = &self.open[&workspace];
        if document == open.manifest_id || !open.manifest.contains(document) {
            return Err(Error::bare(Code::DocumentNotFound));
        }
        Ok(())
    }
}

/// Drop private rows for documents `entries` no longer lists. Called after
/// every manifest commit — a permanent delete, local or arriving through
/// sync, is a manifest commit — and when a workspace opens with a sound
/// manifest, which finishes a cleanup a crash interrupted. Trashed documents
/// are still listed, so their folds survive a restore.
pub(super) fn prune_private_state(
    workspace: WorkspaceId,
    repo: &mut dyn WorkspaceRepo,
    entries: &[crate::model::DocumentEntry],
) {
    let listed: Vec<DocumentId> = entries.iter().map(|entry| entry.id).collect();
    if let Err(error) = repo.prune_private_snapshots(&listed) {
        tracing::warn!(
            target: "koru::store",
            %workspace,
            code = error.code.as_str(),
            "pruning private document state failed; the next open retries"
        );
    }
}
