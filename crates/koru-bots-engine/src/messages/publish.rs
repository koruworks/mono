// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::params;

impl Database {
    pub fn publish_reply(
        &mut self,
        request: &Publish,
        now: i64,
    ) -> Result<Receipt<Published>, Error> {
        if request.command.is_nil()
            || encode(request)?.len() > koru_bot_client_wire::MAX_FRAME_BYTES
        {
            return Err(Error::InvalidRequest);
        }
        let digest = receipts::digest("publish", request)?;
        let tx = self.connection.transaction()?;
        let (thread, bot, state, _, _, _) = turns::turn_row(&tx, request.turn)?;
        let sender = format!("worker:{}", request.turn.worker);
        if !matches!(state.as_str(), "running" | "answered") {
            return Err(Error::StaleTurn);
        }
        if let Some(answer) = receipts::find(&tx, &sender, request.command, &digest)? {
            return Ok(Receipt {
                answer,
                replayed: true,
            });
        }
        if state != "running" {
            return Err(Error::StaleTurn);
        }
        let outputs = crate::operations::outputs::collect(&tx, request.turn)?;
        // Callers may repeat known references, but cannot invent or alter them.
        // Empty is normal: the engine derives all output references itself.
        if request
            .outputs
            .iter()
            .any(|output| !outputs.contains(output))
        {
            return Err(Error::InvalidRequest);
        }
        let message = Uuid::new_v4();
        let thread_id = id(thread.clone())?;
        let content = Content {
            body: request.body.clone(),
            attachments: vec![],
            references: vec![],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        };
        tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'words',?5,'published',?6)",params![message.to_string(),thread,next_sequence(&tx,thread_id)?,bot,encode(&content)?,now])?;
        for output in &outputs {
            crate::operations::outputs::attach(&tx, message, output)?;
        }
        let answered=tx.execute("UPDATE obligations SET state='answered',answered_by=?1 WHERE state='delivered' AND id IN (SELECT x.obligation FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery WHERE d.turn=?2 AND d.state='accepted')",params![message.to_string(),request.turn.id.to_string()])? as u64;
        tx.execute("DELETE FROM inbox WHERE id IN (SELECT x.obligation FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery JOIN obligations o ON o.id=x.obligation WHERE d.turn=?1 AND o.state IN ('answered','expired'))",[request.turn.id.to_string()])?;
        tx.execute(
            "UPDATE turns SET state='answered',details=CAST(json_set(CAST(details AS TEXT),'$.published_message',?2) AS BLOB) WHERE id=?1",
            params![request.turn.id.to_string(), message.to_string()],
        )?;
        let unfinished:Vec<(String,Vec<u8>)>=tx.prepare("SELECT id,payload FROM deliveries WHERE turn=?1 AND state IN ('offered','unknown')")?.query_map([request.turn.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        for (id, payload) in unfinished {
            let mut delivery: Delivery = decode(&payload)?;
            delivery.uncertain = true;
            tx.execute(
                "UPDATE deliveries SET state='unknown',payload=?1 WHERE id=?2",
                params![encode(&delivery)?, id],
            )?;
        }
        // One child-thread reference in its Bot's main conversation, committed
        // with the answer. A crash cannot publish the answer without the card.
        let main: Option<String> = tx
            .query_row(
                "SELECT id FROM threads WHERE bot=?1 AND kind='main'",
                [&bot],
                |r| r.get(0),
            )
            .optional()?;
        let main = main.ok_or(Error::StorageUnavailable)?;
        if main != thread {
            let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE thread=?1 AND kind='thread_reference' AND json_extract(CAST(payload AS TEXT),'$.references[0].thread')=?2)",params![main,thread],|r|r.get(0))?;
            if !existing {
                let reference = Content {
                    body: String::new(),
                    attachments: vec![],
                    references: vec![serde_json::json!({"thread":thread})],
                    in_reply_to: None,
                    deadline_ms: None,
                    reply_to: ReplyTo::Thread,
                };
                tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'thread_reference',?5,'published',?6)",params![Uuid::new_v4().to_string(),main,next_sequence(&tx,id(main.clone())?)?,bot,encode(&reference)?,now])?;
            }
        }
        let answer = Published { message, answered };
        receipts::save(&tx, &sender, request.command, &digest, &answer, now)?;
        tx.commit()?;
        Ok(Receipt {
            answer,
            replayed: false,
        })
    }

    pub fn stop_turn(
        &mut self,
        person: &Person,
        command: Uuid,
        turn: Turn,
        now: i64,
    ) -> Result<Receipt<Stopped>, Error> {
        if command.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let digest = receipts::digest("stop", &turn)?;
        let subject = person.receipt_subject();
        let tx = self.connection.transaction()?;
        let row = turns::turn_row(&tx, turn)?;
        authorize_thread(&tx, person, id(row.0)?)?;
        if let Some(answer) = receipts::find(&tx, &subject, command, &digest)? {
            return Ok(Receipt {
                answer,
                replayed: true,
            });
        }
        let newer:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE thread=(SELECT thread FROM turns WHERE id=?1) AND bot=(SELECT bot FROM turns WHERE id=?1) AND generation>?2)",params![turn.id.to_string(),turn.generation],|r|r.get(0))?;
        if newer {
            return Err(Error::StaleTurn);
        }
        if !matches!(
            row.2.as_str(),
            "running" | "interrupted" | "needs_you" | "stopped"
        ) {
            return Err(Error::StaleTurn);
        }
        let cancelled=tx.execute("UPDATE obligations SET state='cancelled' WHERE state IN ('owed','delivered') AND id IN (SELECT x.obligation FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery WHERE d.turn=?1)",[turn.id.to_string()])? as u64;
        tx.execute("DELETE FROM inbox WHERE id IN (SELECT x.obligation FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery WHERE d.turn=?1)",[turn.id.to_string()])?;
        tx.execute(
            "UPDATE turns SET state='stopped',stopped=1 WHERE id=?1",
            [turn.id.to_string()],
        )?;
        let answer = Stopped { cancelled };
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE session=?1",
            [turn.session.to_string()],
        )?;
        tx.execute("UPDATE questions SET state='cancelled',delivery_state='sent' WHERE turn=?1 AND (state='open' OR delivery_state='pending')",[turn.id.to_string()])?;
        receipts::save(&tx, &subject, command, &digest, &answer, now)?;
        tx.commit()?;
        Ok(Receipt {
            answer,
            replayed: false,
        })
    }
}

use rusqlite::OptionalExtension;
