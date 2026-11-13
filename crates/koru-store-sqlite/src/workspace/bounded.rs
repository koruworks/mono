// SPDX-License-Identifier: MPL-2.0

//! CASE gates bound the bytes transferred into Rust, not SQLite page access.
use super::*;
use koru_store_core::ports::*;
use rusqlite::types::ValueRef;

fn corrupt() -> Error {
    Error::new(
        Code::CorruptState,
        "invalid bounded history or origin evidence",
    )
}

// Every variable column is projected as typeof, encoded length, guarded value.
// Wrong types are withheld by SQL and distinguished from missing/oversize here.
fn field<'a>(row: &'a rusqlite::Row<'_>, at: usize, kind: &str, max: usize) -> Result<&'a [u8]> {
    let actual: String = row.get(at).map_err(|_| corrupt())?;
    if actual != kind {
        return Err(corrupt());
    }
    let length: i64 = row.get(at + 1).map_err(|_| corrupt())?;
    let length = usize::try_from(length).map_err(|_| corrupt())?;
    if length > max {
        return Err(Error::new(
            Code::PayloadTooLarge,
            "encoded history or origin exceeds its budget",
        ));
    }
    let value = row.get_ref(at + 2).map_err(|_| corrupt())?;
    let bytes = match (kind, value) {
        ("text", ValueRef::Text(value)) | ("blob", ValueRef::Blob(value)) => value,
        _ => return Err(corrupt()),
    };
    if length == 0 || bytes.len() != length {
        return Err(corrupt());
    }
    Ok(bytes)
}

fn text(row: &rusqlite::Row<'_>, at: usize, max: usize) -> Result<String> {
    std::str::from_utf8(field(row, at, "text", max)?)
        .map(str::to_owned)
        .map_err(|_| corrupt())
}

pub(super) fn document_version(
    connection: &Connection,
    id: DocumentVersionId,
) -> Result<Option<BoundedDocumentVersion>> {
    connection.query_row(
        "SELECT
          typeof(id),length(CAST(id AS BLOB)),CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=36 THEN id END,
          typeof(document_id),length(CAST(document_id AS BLOB)),CASE WHEN typeof(document_id)='text' AND length(CAST(document_id AS BLOB))<=36 THEN document_id END,
          typeof(snapshot),length(snapshot),CASE WHEN typeof(snapshot)='blob' AND length(snapshot)<=?2 THEN snapshot END,
          typeof(version_vector),length(version_vector),CASE WHEN typeof(version_vector)='blob' AND length(version_vector)<=?3 THEN version_vector END,
          typeof(frontiers),length(frontiers),CASE WHEN typeof(frontiers)='blob' AND length(frontiers)<=?3 THEN frontiers END,
          typeof(checksum),length(CAST(checksum AS BLOB)),CASE WHEN typeof(checksum)='text' AND length(CAST(checksum AS BLOB))<=64 THEN checksum END,
          captured_at
         FROM document_versions WHERE id=?1",
        params![id.to_storage(), MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES as i64, MAX_BOUNDED_HISTORY_POSITION_BYTES as i64],
        |row| Ok((|| {
            let raw_id = text(row, 0, 36)?;
            let raw_document = text(row, 3, 36)?;
            let stored_id = DocumentVersionId::parse(&raw_id).ok_or_else(corrupt)?;
            let document = DocumentId::parse(&raw_document).ok_or_else(corrupt)?;
            let checksum = text(row, 15, 64)?;
            if stored_id != id || stored_id.to_storage() != raw_id || document.to_storage() != raw_document
                || raw_id.len() != 36 || raw_document.len() != 36 || checksum.len() != 64
                || !checksum.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(corrupt());
            }
            Ok(BoundedDocumentVersion {
                id: stored_id, document,
                snapshot: field(row, 6, "blob", MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES)?.to_vec(),
                version_vector: field(row, 9, "blob", MAX_BOUNDED_HISTORY_POSITION_BYTES)?.to_vec(),
                frontiers: field(row, 12, "blob", MAX_BOUNDED_HISTORY_POSITION_BYTES)?.to_vec(),
                checksum,
                captured_at: Timestamp::from_millis(row.get(18).map_err(|_| corrupt())?),
            })
        })()),
    ).optional().map_err(sql_failed)?.transpose()
}

pub(super) fn creation_origin(
    connection: &Connection,
    document: DocumentId,
) -> Result<CreationOriginLookup> {
    let known = connection.query_row(
        "SELECT
          typeof(scope),length(CAST(scope AS BLOB)),CASE WHEN typeof(scope)='text' AND length(CAST(scope AS BLOB))<=?2 THEN scope END,
          typeof(request_key),length(CAST(request_key AS BLOB)),CASE WHEN typeof(request_key)='text' AND length(CAST(request_key AS BLOB))<=?3 THEN request_key END,
          typeof(request_hash),length(CAST(request_hash AS BLOB)),CASE WHEN typeof(request_hash)='text' AND length(CAST(request_hash AS BLOB))<=?4 THEN request_hash END,
          typeof(initial_update),length(initial_update),CASE WHEN typeof(initial_update)='blob' AND length(initial_update)<=?5 THEN initial_update END,
          created_at
         FROM document_creation_origins WHERE document_id=?1",
        params![document.to_storage(), MAX_CREATION_SCOPE_BYTES as i64, MAX_CREATION_KEY_BYTES as i64, MAX_CREATION_HASH_BYTES as i64, MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES as i64],
        |row| Ok((|| -> Result<CreationOrigin> {
            Ok(CreationOrigin {
                document,
                scope: text(row, 0, MAX_CREATION_SCOPE_BYTES)?,
                request_key: text(row, 3, MAX_CREATION_KEY_BYTES)?,
                request_hash: text(row, 6, MAX_CREATION_HASH_BYTES)?,
                initial_update: field(row, 9, "blob", MAX_BOUNDED_HISTORY_SNAPSHOT_BYTES)?.to_vec(),
                created_at: Timestamp::from_millis(row.get(12).map_err(|_| corrupt())?),
            })
        })()),
    ).optional().map_err(sql_failed)?.transpose()?;
    if let Some(origin) = known {
        return Ok(CreationOriginLookup::Known(origin));
    }
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM loro_documents WHERE id=?1)",
            [document.to_storage()],
            |row| row.get(0),
        )
        .map_err(sql_failed)?;
    Ok(if exists {
        CreationOriginLookup::Unknown
    } else {
        CreationOriginLookup::Missing
    })
}
