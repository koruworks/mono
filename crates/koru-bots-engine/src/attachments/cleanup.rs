// SPDX-License-Identifier: MPL-2.0
//! Tombstone before deleting bytes. A concurrent Send either binds first and
//! survives, or sees the tombstone and refuses; no acknowledged file is removed.
use super::*;
const PAGE: usize = 64;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sweep {
    pub garbage: Vec<(Uuid, Uuid)>,
    pub next: Option<Uuid>,
}
impl Database {
    pub(super) fn expire_attachments(
        &mut self,
        who: &Person,
        after: Option<Uuid>,
        cutoff: i64,
    ) -> Result<Sweep, Error> {
        let tx = self.connection.transaction()?;
        let rows: Vec<(String, String, Vec<u8>)> = tx.prepare(
            "SELECT a.id,a.thread,a.record FROM attachments a JOIN threads t ON t.id=a.thread JOIN bots b ON b.id=t.bot WHERE b.owner=?1 AND a.message IS NULL AND a.removed=0 AND (?2 IS NULL OR a.id>?2) ORDER BY a.id LIMIT ?3"
        )?.query_map(params![who.owner, after.map(|id| id.to_string()), PAGE as u64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<Result<_,_>>()?;
        let mut result = Sweep {
            garbage: vec![],
            next: None,
        };
        for (id, thread, bytes) in &rows {
            let record: Record =
                serde_json::from_slice(bytes).map_err(|_| Error::StorageUnavailable)?;
            if record.attachment.created_at.millis() < cutoff {
                tx.execute(
                    "UPDATE attachments SET removed=1 WHERE id=?1 AND message IS NULL",
                    [id],
                )?;
                result.garbage.push((
                    Uuid::parse_str(thread).map_err(|_| Error::StorageUnavailable)?,
                    Uuid::parse_str(id).map_err(|_| Error::StorageUnavailable)?,
                ));
            }
        }
        if rows.len() == PAGE {
            result.next = rows
                .last()
                .map(|row| Uuid::parse_str(&row.0).map_err(|_| Error::StorageUnavailable))
                .transpose()?;
        }
        tx.commit()?;
        Ok(result)
    }
    /// The trusted gateway only proposes old, canonical filesystem entries.
    /// Unknown ids receive tombstones too, preventing late upload resurrection.
    pub(super) fn collect_attachment(
        &mut self,
        who: &Person,
        thread: Uuid,
        id: Uuid,
        cutoff: i64,
    ) -> Result<bool, Error> {
        if id.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        messages::authorize_thread(&tx, who, thread)?;
        let row: Option<(String, Option<String>, bool, Vec<u8>)> = tx
            .query_row(
                "SELECT thread,message,removed,record FROM attachments WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((actual_thread, message, removed, bytes)) = row {
            if actual_thread != thread.to_string() {
                return Err(Error::NotAllowed);
            }
            if message.is_some() {
                return Ok(false);
            }
            if !removed {
                let record: Record =
                    serde_json::from_slice(&bytes).map_err(|_| Error::StorageUnavailable)?;
                if record.attachment.created_at.millis() >= cutoff {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE attachments SET removed=1 WHERE id=?1",
                    [id.to_string()],
                )?;
            }
        } else {
            tx.execute(
                "INSERT INTO attachments(id,thread,record,removed) VALUES (?1,?2,x'7b7d',1)",
                params![id.to_string(), thread.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
