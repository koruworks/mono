// SPDX-License-Identifier: MPL-2.0

//! A strict detached replay; damaged rows remain untouched for normal recovery.
use super::*;
use koru_store_core::service::{DetachedDocument, DocumentInspectionBudget};

fn charge(remaining: &mut u64, length: i64) -> Result<()> {
    let length = u64::try_from(length).map_err(|_| Error::bare(Code::CorruptState))?;
    *remaining = remaining
        .checked_sub(length)
        .ok_or_else(|| Error::bare(Code::PayloadTooLarge))?;
    Ok(())
}
fn apply(doc: &loro::LoroDoc, bytes: &[u8]) -> Result<()> {
    let status = doc
        .import(bytes)
        .map_err(|_| Error::bare(Code::CorruptState))?;
    if status.pending.is_some() {
        return Err(Error::bare(Code::CorruptState));
    }
    Ok(())
}

impl SqliteWorkspace {
    pub(crate) fn inspect_prepared_document(
        self,
        workspace: WorkspaceId,
        document: DocumentId,
        budget: DocumentInspectionBudget,
    ) -> Result<DetachedDocument> {
        let mut metadata = u64::from(budget.metadata_bytes);
        let mut payload = budget.payload_bytes;
        if metadata == 0 || payload == 0 || budget.journal_rows == 0 {
            return Err(Error::bare(Code::PayloadTooLarge));
        }
        // Bound allocations before using the ordinary row decoder. The pinned
        // query-only snapshot prevents a writer replacing bytes after this check.
        let fields: Option<(i64, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT length(version_vector)+length(frontiers)+length(kind),latest_checkpoint_seq FROM loro_documents WHERE id=?1",
                [document.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_failed)?;
        let (size, checkpoint_sequence) =
            fields.ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        if checkpoint_sequence.is_some_and(|seq| seq < 0) {
            return Err(Error::bare(Code::CorruptState));
        }
        charge(&mut metadata, size)?;
        let state = self
            .document(document)?
            .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
        if state.kind == DocumentKind::Manifest {
            return Err(Error::bare(Code::InvalidRequest));
        }
        let doc = loro::LoroDoc::new();
        if let Some(sequence) = state.latest_checkpoint_seq {
            let sizes: Option<(i64,i64)> = self.connection.query_row(
                "SELECT length(snapshot),length(version_vector)+length(frontiers)+length(checksum) FROM loro_checkpoints WHERE document_id=?1 AND seq=?2",
                params![document.to_storage(),sequence as i64], |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional().map_err(sql_failed)?;
            let (bytes, fields) = sizes.ok_or_else(|| Error::bare(Code::CorruptState))?;
            charge(&mut payload, bytes)?;
            charge(&mut metadata, fields)?;
            let checkpoint = self
                .checkpoint(document, sequence)?
                .ok_or_else(|| Error::bare(Code::CorruptState))?;
            if checksum(&checkpoint.snapshot) != checkpoint.checksum {
                return Err(Error::bare(Code::CorruptState));
            }
            apply(&doc, &checkpoint.snapshot)?;
            let expected = loro::VersionVector::decode(&checkpoint.version_vector)
                .map_err(|_| Error::bare(Code::CorruptState))?;
            let frontiers = loro::Frontiers::decode(&checkpoint.frontiers)
                .map_err(|_| Error::bare(Code::CorruptState))?;
            if doc.state_vv() != expected || doc.state_frontiers() != frontiers {
                return Err(Error::bare(Code::CorruptState));
            }
        }
        let after = state.latest_checkpoint_seq.unwrap_or(0);
        let mut headers = self.connection.prepare(
            "SELECT length(update_bytes) FROM loro_updates WHERE document_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3"
        ).map_err(sql_failed)?;
        let rows = headers
            .query_map(
                params![
                    document.to_storage(),
                    after as i64,
                    i64::from(budget.journal_rows) + 1
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_failed)?;
        for (index, length) in rows.enumerate() {
            if index >= budget.journal_rows as usize {
                return Err(Error::bare(Code::PayloadTooLarge));
            }
            charge(&mut payload, length.map_err(sql_failed)?)?;
        }
        // One blob at a time. Origin and projection columns neither identify
        // this captured state nor need to be loaded for content inspection.
        let mut updates = self.connection.prepare(
            "SELECT update_bytes FROM loro_updates WHERE document_id=?1 AND seq>?2 ORDER BY seq"
        ).map_err(sql_failed)?;
        let rows = updates
            .query_map(params![document.to_storage(), after as i64], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map_err(sql_failed)?;
        let mut has_committed_content = state.latest_checkpoint_seq.is_some();
        for bytes in rows {
            has_committed_content = true;
            apply(&doc, &bytes.map_err(sql_failed)?)?;
        }
        if !has_committed_content {
            return Err(Error::bare(Code::VersionUnavailable));
        }
        DetachedDocument::from_replayed(
            workspace,
            document,
            doc,
            state.version_vector,
            &state.frontiers,
        )
    }
}
