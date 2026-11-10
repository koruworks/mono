// SPDX-License-Identifier: MPL-2.0
use super::*;

fn state(
    snapshot: &[u8],
    vv: &[u8],
    frontiers: &[u8],
) -> Result<(loro::VersionVector, loro::Frontiers)> {
    let invalid = || Error::new(Code::CorruptState, "invalid immutable history evidence");
    let version = loro::VersionVector::decode(vv).map_err(|_| invalid())?;
    let frontier = loro::Frontiers::decode(frontiers).map_err(|_| invalid())?;
    let doc = loro::LoroDoc::new();
    let imported = doc.import(snapshot).map_err(|_| invalid())?;
    if imported.pending.is_some() || doc.oplog_vv() != version || doc.oplog_frontiers() != frontier
    {
        return Err(invalid());
    }
    Ok((version, frontier))
}
fn validate_capture(capture: &HistoryCapture<'_>) -> Result<()> {
    state(capture.snapshot, capture.version_vector, capture.frontiers)?;
    Ok(())
}
fn validate_once(
    capture: &HistoryCapture<'_>,
    entry: &HistoryEntry,
    version: &DocumentVersion,
) -> Result<()> {
    if checksum(&version.snapshot) != version.checksum {
        return Err(Error::new(Code::CorruptState, "history checksum differs"));
    }
    let actual = state(
        &version.snapshot,
        &version.version_vector,
        &version.frontiers,
    )?;
    let expected = state(capture.snapshot, capture.version_vector, capture.frontiers)?;
    if entry.document != capture.document
        || version.document != capture.document
        || entry.version != version.id
        || entry.reason != capture.reason
        || entry.pinned != capture.reason.pinned()
        || entry.boundary.as_deref() != capture.boundary
        || actual != expected
    {
        return Err(Error::new(
            Code::IdempotencyConflict,
            "history identity names different evidence",
        ));
    }
    Ok(())
}
impl SqliteWorkspace {
    pub(super) fn capture_history_inner(
        &mut self,
        capture: &HistoryCapture<'_>,
        identity: Option<HistoryEntryId>,
    ) -> Result<HistoryEntry> {
        if identity.is_some_and(|id| {
            id.as_uuid().get_version_num() != 4 || id.as_uuid().as_bytes()[8] & 0xc0 != 0x80
        }) {
            return Err(Error::new(
                Code::InvalidRequest,
                "history identity must be v4",
            ));
        }
        self.require_document(capture.document)?;
        if checksum(capture.snapshot) != capture.checksum {
            return Err(Error::new(
                Code::CorruptState,
                "the history version checksum does not match its bytes",
            ));
        }
        let document = capture.document.to_storage();
        let tx = self.connection.transaction().map_err(sql_failed)?;
        if let Some(identity) = identity {
            let existing=tx.query_row(
                "SELECT id,document_id,version_id,reason,recorded_at,label,message,boundary,pinned FROM document_history WHERE id=?1",
                params![identity.to_storage()],read_history_entry).optional().map_err(sql_failed)?.transpose()?;
            if let Some(entry) = existing {
                let version=tx.query_row(
                    "SELECT id,document_id,snapshot,version_vector,frontiers,checksum,captured_at FROM document_versions WHERE id=?1",
                    params![entry.version.to_storage()],read_document_version).map_err(sql_failed)??;
                validate_once(capture, &entry, &version)?;
                tx.commit().map_err(sql_failed)?;
                return Ok(entry);
            }
            validate_capture(capture)?;
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM document_versions WHERE document_id = ?1 AND frontiers = ?2",
                params![document, capture.frontiers],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_failed)?;
        let version = match existing.as_deref().and_then(DocumentVersionId::parse) {
            Some(id) => id,
            None => {
                let id = DocumentVersionId::new();
                tx.execute(
                    "INSERT INTO document_versions
                       (id, document_id, snapshot, version_vector, frontiers, checksum, captured_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id.to_storage(),
                        document,
                        capture.snapshot,
                        capture.version_vector,
                        capture.frontiers,
                        capture.checksum,
                        capture.recorded_at.millis()
                    ],
                )
                .map_err(sql_failed)?;
                id
            }
        };
        let entry = HistoryEntry {
            id: identity.unwrap_or_default(),
            document: capture.document,
            version,
            reason: capture.reason,
            recorded_at: capture.recorded_at,
            label: capture.label.map(str::to_owned),
            message: capture.message.map(str::to_owned),
            boundary: capture.boundary.map(str::to_owned),
            pinned: capture.reason.pinned(),
        };
        if identity.is_some() {
            let retained=tx.query_row("SELECT id,document_id,snapshot,version_vector,frontiers,checksum,captured_at FROM document_versions WHERE id=?1",
                params![entry.version.to_storage()],read_document_version).map_err(sql_failed)??;
            validate_once(capture, &entry, &retained)?;
        }
        tx.execute(
            "INSERT INTO document_history
               (id, document_id, version_id, reason, recorded_at, label, message, boundary, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                entry.id.to_storage(),
                document,
                entry.version.to_storage(),
                entry.reason.as_str(),
                entry.recorded_at.millis(),
                entry.label,
                entry.message,
                entry.boundary,
                i64::from(entry.pinned)
            ],
        )
        .map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(entry)
    }
}
