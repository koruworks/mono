// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Database {
    /// Read the exact receipt before gateway revalidation of new-send effects.
    /// Current message authority still applies; changed/expired ids refuse.
    pub fn message_receipt(
        &self,
        person: &Person,
        envelope: &Envelope,
    ) -> Result<Option<Receipt<Accepted>>, Error> {
        authorize_bot(&self.connection, person, envelope.to)?;
        if let Some(thread) = envelope.thread {
            authorize_thread(&self.connection, person, thread)?;
        }
        let digest = receipts::digest("accept", envelope)?;
        Ok(receipts::find(
            &self.connection,
            &person.receipt_subject(),
            envelope.id,
            &digest,
        )?
        .map(|answer| Receipt {
            answer,
            replayed: true,
        }))
    }
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
            || encode(envelope)?.len() > crate::limits::MAX_PAYLOAD_BYTES
        {
            return Err(Error::InvalidRequest);
        }
        skills::selected(&envelope.references)?;
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
        let context = envelope
            .in_reply_to
            .map(|card| routine_context::resolve(&tx, envelope.to, thread, card))
            .transpose()?
            .flatten();
        let answer = Accepted {
            message: Uuid::new_v4(),
            thread,
            sequence: next_sequence(&tx, thread)?,
        };
        let mut recipients = koru_ai_core::prompt_links::mentioned_bots(&envelope.body);
        if recipients.len() > 8 {
            return Err(Error::InvalidRequest);
        }
        if recipients.is_empty() {
            let starter: String = tx.query_row(
                "SELECT bot FROM threads WHERE id=?1",
                [thread.to_string()],
                |r| r.get(0),
            )?;
            recipients.push(id(starter)?);
        }
        // Result authority is scoped to this Bot. Do not accept a contextual
        // instruction whose mentions redirect its work to a different Bot.
        if context.is_some() && recipients.iter().any(|recipient| *recipient != envelope.to) {
            return Err(Error::NotAllowed);
        }
        for recipient in &recipients {
            if *recipient != envelope.to {
                collaboration::permitted(&tx, envelope.to, *recipient)?;
            }
            collaboration::participant(&tx, thread, *recipient)?;
        }
        let content = Content {
            body: envelope.body.clone(),
            attachments: crate::attachments::prepare(
                &tx,
                thread,
                answer.message,
                &envelope.attachments,
            )?,
            references: envelope.references.clone(),
            in_reply_to: envelope.in_reply_to,
            deadline_ms: envelope.deadline_ms,
            reply_to: envelope.reply_to,
        };
        let payload = encode(&content)?;
        // Replacing short attachment ids with durable metadata must not make
        // an acknowledged message impossible to read through the carrier.
        if payload.len() > crate::person::MAX_COMMAND_BYTES {
            return Err(Error::InvalidRequest);
        }
        tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'words',?5,'accepted',?6)",rusqlite::params![answer.message.to_string(),thread.to_string(),answer.sequence,person.sender,payload,now])?;
        crate::attachments::bind(&tx, thread, answer.message, &envelope.attachments)?;
        if let Some(context) = &context {
            routine_context::handled(&tx, envelope.to, context, answer.message, now)?;
        }
        let expired = envelope.deadline_ms.is_some_and(|deadline| now > deadline);
        // Reading a side thread never joins it. Sending is the person's
        // explicit intervention and records membership with the message.
        tx.execute(
            "INSERT OR IGNORE INTO participants(thread,participant,kind) VALUES (?1,?2,'person')",
            rusqlite::params![thread.to_string(), person.sender],
        )?;
        collaboration::person_message(&tx, thread)?;
        let chain = collaboration::chain(&tx, thread)?;
        for recipient in recipients {
            tx.execute(
                "INSERT OR IGNORE INTO chain_bots(chain,bot) VALUES (?1,?2)",
                rusqlite::params![chain.to_string(), recipient.to_string()],
            )?;
            collaboration::wake(&tx, thread, answer.message, recipient, expired, now)?;
            tx.execute("UPDATE questions SET state='cancelled',delivery_state='sent' WHERE state='open' AND json_extract(CAST(prompt AS TEXT),'$.delivery')='next_message' AND turn IN (SELECT id FROM turns WHERE thread=?1 AND bot=?2 AND state='answered')",rusqlite::params![thread.to_string(),recipient.to_string()])?;
        }
        receipts::save(&tx, &subject, envelope.id, &digest, &answer, now)?;
        tx.commit()?;
        Ok(Receipt {
            answer,
            replayed: false,
        })
    }
}
