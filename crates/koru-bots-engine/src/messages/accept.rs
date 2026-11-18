// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Database {
    /// Acceptance is one transaction. The caller has a durable receipt only
    /// after its message, inbox row and obligation have all committed.
    pub fn accept_message(
        &mut self,
        person: &Person,
        envelope: &Envelope,
        now: i64,
    ) -> Result<Receipt<Accepted>, Error> {
        if envelope.id.is_nil()
            || envelope.to.is_nil()
            || encode(envelope)?.len() > koru_bot_client_wire::MAX_FRAME_BYTES
        {
            return Err(Error::InvalidRequest);
        }
        let digest = receipts::digest("accept", envelope)?;
        let subject = person.receipt_subject();
        let tx = self.connection.transaction()?;
        authorize_bot(&tx, person, envelope.to)?;
        if let Some(thread) = envelope.thread {
            authorize_thread(&tx, person, thread)?;
        }
        if let Some(answer) = receipts::find(&tx, &subject, envelope.id, &digest)? {
            return Ok(Receipt {
                answer,
                replayed: true,
            });
        }
        let thread = if let Some(thread) = envelope.thread {
            let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM threads t JOIN participants p ON p.thread=t.id WHERE t.id=?1 AND t.lifecycle='active' AND p.participant=?2 AND p.kind='bot')",rusqlite::params![thread.to_string(),envelope.to.to_string()],|r|r.get(0))?;
            if !allowed {
                return Err(Error::NotAllowed);
            }
            thread
        } else {
            let thread = Uuid::new_v4();
            tx.execute("INSERT INTO threads(id,bot,kind,title,lifecycle,created_at_ms,updated_at_ms) VALUES (?1,?2,'conversation','','active',?3,?3)",rusqlite::params![thread.to_string(),envelope.to.to_string(),now])?;
            tx.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'person'),(?1,?3,'bot')",rusqlite::params![thread.to_string(),person.sender,envelope.to.to_string()])?;
            crate::configuration::seed(&tx, envelope.to, thread)?;
            thread
        };
        if let Some(reply) = envelope.in_reply_to {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND thread=?2)",
                rusqlite::params![reply.to_string(), thread.to_string()],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::InvalidRequest);
            }
        }
        let content = Content {
            body: envelope.body.clone(),
            attachments: envelope.attachments.clone(),
            references: envelope.references.clone(),
            in_reply_to: envelope.in_reply_to,
            deadline_ms: envelope.deadline_ms,
            reply_to: envelope.reply_to,
        };
        let answer = Accepted {
            message: Uuid::new_v4(),
            thread,
            sequence: next_sequence(&tx, thread)?,
        };
        tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'words',?5,'accepted',?6)",rusqlite::params![answer.message.to_string(),thread.to_string(),answer.sequence,person.sender,encode(&content)?,now])?;
        let obligation = Uuid::new_v4();
        let expired = envelope.deadline_ms.is_some_and(|deadline| now > deadline);
        tx.execute(
            "UPDATE bots SET folder_frozen_at_ms=COALESCE(folder_frozen_at_ms,?2) WHERE id=?1",
            rusqlite::params![envelope.to.to_string(), now],
        )?;
        tx.execute("INSERT INTO obligations(id,thread,message,bot,state,accepted_at_ms) VALUES (?1,?2,?3,?4,?5,?6)",rusqlite::params![obligation.to_string(),thread.to_string(),answer.message.to_string(),envelope.to.to_string(),if expired{"expired"}else{"owed"},now])?;
        if !expired {
            tx.execute(
                "INSERT INTO inbox(id,thread,message,bot,accepted_at_ms) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    obligation.to_string(),
                    thread.to_string(),
                    answer.message.to_string(),
                    envelope.to.to_string(),
                    now
                ],
            )?;
        }
        tx.execute("UPDATE questions SET state='cancelled',delivery_state='sent' WHERE state='open' AND json_extract(CAST(prompt AS TEXT),'$.delivery')='next_message' AND turn IN (SELECT id FROM turns WHERE thread=?1 AND bot=?2 AND state='answered')",rusqlite::params![thread.to_string(),envelope.to.to_string()])?;
        receipts::save(&tx, &subject, envelope.id, &digest, &answer, now)?;
        tx.commit()?;
        Ok(Receipt {
            answer,
            replayed: false,
        })
    }
}
