// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::messages::{Content, ReplyTo, next_sequence};

impl Admission {
    /// One durable notice per waiting period, not per actor tick or queued
    /// message. A thread flag avoids scanning retained history.
    pub(super) fn notify_waiters(&self, db: &Database, now: u64) -> Result<(), Error> {
        let occupied =
            serde_json::to_string(&self.sessions.keys().map(|key| key.bot).collect::<Vec<_>>())
                .map_err(|_| Error::InvalidRequest)?;
        for _ in 0..crate::messages::PAGE_LIMIT {
            let Some(key) = self.candidate(db, now, false, true)? else {
                break;
            };
            let tx = db.connection.unchecked_transaction()?;
            let running = tx.prepare("SELECT DISTINCT b.id,b.name FROM bots b JOIN json_each(?1) live ON live.value=b.id WHERE b.owner=(SELECT owner FROM bots WHERE id=?2) ORDER BY b.id")?
                .query_map(params![occupied,key.bot.to_string()], |r| Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?})))?
                .collect::<Result<Vec<_>,_>>()?;
            let note = Content {
                body: String::new(),
                attachments: vec![],
                references: vec![
                    serde_json::json!({"notice":"waiting_for_capacity","running_bots":running,"limit":self.limit}),
                ],
                in_reply_to: None,
                deadline_ms: None,
                reply_to: ReplyTo::Thread,
            };
            tx.execute(
                "UPDATE threads SET capacity_wait_notified=1 WHERE id=?1",
                [key.thread.to_string()],
            )?;
            tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES(?1,?2,?3,?4,'setup_note',?5,'published',?6)",
                params![Uuid::new_v4().to_string(),key.thread.to_string(),next_sequence(&tx,key.thread)?,key.bot.to_string(),serde_json::to_vec(&note).map_err(|_| Error::InvalidRequest)?,now.min(i64::MAX as u64) as i64])?;
            tx.commit()?;
        }
        Ok(())
    }
}
