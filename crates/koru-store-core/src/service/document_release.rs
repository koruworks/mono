// SPDX-License-Identifier: MPL-2.0
//! A final tombstone freezes the old identity before another window can write.
use super::*;
use loro::{LoroDoc, ValueOrContainer};

fn key(workspace: WorkspaceId, document: DocumentId) -> String {
    format!("document.purged.{workspace}.{document}")
}

impl<C: Catalog, B: Backend> Store<C, B> {
    pub(super) fn document_purge_frozen(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool> {
        Ok(self.catalog.state(&key(workspace, document))?.is_some())
    }

    /// Returns a durable source snapshot after permanently refusing further
    /// writes to this identity. A retained system tombstone is mandatory.
    pub fn freeze_document_purge(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        at: Timestamp,
    ) -> Result<Vec<u8>> {
        let entries = self.documents(workspace)?;
        if document == self.manifest_id(workspace)?
            || entries
                .iter()
                .any(|e| e.id == document && e.kind == "koru-system")
        {
            return Err(Error::bare(Code::InvalidRequest));
        }
        let mut purged = false;
        for system in entries.iter().filter(|e| e.kind == "koru-system") {
            self.flush(workspace, system.id, at)?;
            // Manifest-first sync may leave other SystemDoc rows without a
            // body. Only present evidence can freeze this source.
            let snapshot = match self.snapshot(workspace, system.id) {
                Ok(snapshot) => snapshot,
                Err(error) if error.code() == Code::DocumentNotFound => continue,
                Err(error) => return Err(error),
            };
            let doc = LoroDoc::new();
            doc.import(&snapshot)
                .map_err(|_| Error::bare(Code::CorruptState))?;
            let Some(ValueOrContainer::Container(container)) = doc.get_map("pages").get("purged")
            else {
                continue;
            };
            let map = container
                .into_map()
                .map_err(|_| Error::bare(Code::CorruptState))?;
            purged |= map.get(&document.to_storage()).is_some();
        }
        if !purged {
            return Err(Error::bare(Code::InvalidRequest));
        }
        self.flush(workspace, document, at)?;
        let snapshot = self.snapshot(workspace, document)?;
        self.catalog.set_state(&key(workspace, document), "1")?;
        Ok(snapshot)
    }

    /// A kept copy must include every frozen operation and have a committed
    /// manifest placement. Delete quotes the exact version the user decided.
    pub fn release_document_purge(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        expected: &loro::VersionVector,
        kept: Option<DocumentId>,
        at: Timestamp,
    ) -> Result<bool> {
        if !self.document_purge_frozen(workspace, document)? {
            return Err(Error::bare(Code::InvalidRequest));
        }
        if self.locate(document)?.is_none() {
            return Ok(false);
        }
        let current = match self.version(workspace, document) {
            Ok(current) => current,
            Err(error) if error.code() == Code::DocumentNotFound => {
                // A crash can fall between deleting the workspace bytes and
                // clearing their catalog routing row. The durable frozen marker
                // and absent manifest row prove this is completion of release.
                if self
                    .documents(workspace)?
                    .iter()
                    .any(|entry| entry.id == document)
                {
                    return Err(error);
                }
                self.catalog.forget_location(document)?;
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if &current != expected {
            return Err(Error::bare(Code::IdempotencyConflict));
        }
        if let Some(kept) = kept {
            if kept == document
                || !self.documents(workspace)?.iter().any(|e| e.id == kept)
                || !self.version(workspace, kept)?.includes_vv(expected)
            {
                return Err(Error::bare(Code::IdempotencyConflict));
            }
            self.flush(workspace, kept, at)?;
        }
        let manifest = self.manifest_id(workspace)?;
        let open = self.open.get_mut(&workspace).expect("opened source");
        if open.manifest.contains(document) {
            let before = open.manifest.version();
            // Children have their own deletion evidence and decisions. Detach
            // them before removing this row so they cannot be silently lost.
            for child in open
                .manifest
                .entries()
                .into_iter()
                .filter(|e| e.parent == Some(document))
            {
                let end = open
                    .manifest
                    .entries()
                    .iter()
                    .filter(|entry| entry.parent.is_none())
                    .count();
                open.manifest.move_document(child.id, None, end)?;
            }
            open.manifest.remove_document(document)?;
            let delta = open.manifest.export_updates_since(&before)?;
            commit_one(
                open,
                workspace,
                &mut self.committed,
                &mut self.sync_dirty,
                manifest,
                delta,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            open.repo.reindex(&open.manifest.entries())?;
        }
        self.flush(workspace, manifest, at)?;
        let open = self.open.get_mut(&workspace).expect("opened source");
        open.documents.remove(&document);
        open.windows.remove(&document);
        open.repo.drop_document(document)?;
        self.catalog.forget_location(document)?;
        Ok(true)
    }
}
