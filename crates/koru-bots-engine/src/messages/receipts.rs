// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

/// Sorting is independent of serde_json's workspace-unified map-order feature.
pub(super) fn digest(kind: &str, value: &impl Serialize) -> Result<[u8; 32], Error> {
    fn canonical(value: &serde_json::Value, out: &mut Vec<u8>) -> Result<(), Error> {
        match value {
            serde_json::Value::Object(object) => {
                out.push(b'{');
                let mut entries: Vec<_> = object.iter().collect();
                entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    out.extend(encode(key)?);
                    out.push(b':');
                    canonical(value, out)?;
                }
                out.push(b'}');
            }
            serde_json::Value::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    canonical(value, out)?;
                }
                out.push(b']');
            }
            value => out.extend(encode(value)?),
        }
        Ok(())
    }
    let value = serde_json::to_value(value).map_err(|_| Error::InvalidRequest)?;
    let mut bytes = b"koru-bots-command-v1\0".to_vec();
    bytes.extend(kind.as_bytes());
    bytes.push(0);
    canonical(&value, &mut bytes)?;
    Ok(Sha256::digest(&bytes).into())
}

pub(super) fn find<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    sender: &str,
    command: Uuid,
    digest: &[u8; 32],
) -> Result<Option<T>, Error> {
    let row: Option<(Vec<u8>, Option<Vec<u8>>)> = connection
        .query_row(
            "SELECT digest,answer FROM receipts WHERE sender=?1 AND id=?2",
            rusqlite::params![sender, command.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((old, _)) if old.as_slice() != digest => Err(Error::Conflict),
        Some((_, None)) => Err(Error::Expired),
        Some((_, Some(answer))) => decode(&answer).map(Some),
    }
}
pub(super) fn save(
    connection: &rusqlite::Connection,
    sender: &str,
    command: Uuid,
    digest: &[u8; 32],
    answer: &impl Serialize,
    now: i64,
) -> Result<(), Error> {
    connection.execute(
        "INSERT INTO receipts(sender,id,digest,answer,accepted_at_ms) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            sender,
            command.to_string(),
            digest.as_slice(),
            encode(answer)?,
            now
        ],
    )?;
    Ok(())
}
impl Database {
    /// Opportunistic retention, called with normal engine work, never a timer.
    /// Tombstones are retained for the authenticated sender's whole lifetime.
    pub fn prune_receipt_answers(&self, now: i64) -> Result<usize, Error> {
        Ok(self.connection.execute(
            "UPDATE receipts SET answer=NULL WHERE rowid IN (SELECT rowid FROM receipts WHERE answer IS NOT NULL AND accepted_at_ms<=?1 ORDER BY accepted_at_ms LIMIT 64)",
            [now.saturating_sub(RETENTION_MS)],
        )?)
    }
}
