// SPDX-License-Identifier: MPL-2.0

use super::*;
use rusqlite::{OptionalExtension, params};

impl Database {
    /// A single message whose structured attachments/references cannot fit is
    /// terminal. Keep the original content and a typed notice in the thread;
    /// the window renders the notice in the person's language.
    pub(super) fn refuse_oversized(
        &mut self,
        bot: Uuid,
        thread: Uuid,
        obligation: &str,
        now: i64,
    ) -> Result<(), Error> {
        let tx = self.connection.transaction()?;
        let message: Option<String> = tx.query_row(
            "SELECT message FROM obligations WHERE id=?1 AND bot=?2 AND thread=?3 AND state IN ('owed','delivered')",
            params![obligation,bot.to_string(),thread.to_string()], |r| r.get(0),
        ).optional()?;
        let Some(message) = message else {
            return Ok(());
        };
        let message = id(message)?;
        let notice = Content {
            body: String::new(),
            attachments: vec![],
            references: vec![serde_json::json!({"notice":"input_too_large","message":message})],
            in_reply_to: Some(message),
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        };
        tx.execute(
            "UPDATE obligations SET state='refused' WHERE id=?1",
            [obligation],
        )?;
        tx.execute("DELETE FROM inbox WHERE id=?1", [obligation])?;
        tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'setup_note',?5,'published',?6)",
            params![Uuid::new_v4().to_string(),thread.to_string(),next_sequence(&tx,thread)?,bot.to_string(),encode(&notice)?,now])?;
        tx.commit()?;
        Ok(())
    }
}
