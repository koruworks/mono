// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{OptionalExtension, params};

impl Database {
    /// One fresh obligation, offered once to this running turn. An uncertain
    /// carrier response is never permission to send it again. Its obligation
    /// stays open until Publish includes an explicit acceptance.
    pub fn offer_steer(
        &mut self,
        turn: Turn,
        budget: usize,
        now: i64,
    ) -> Result<Option<Delivery>, Error> {
        if budget < 1024 {
            return Err(Error::InputTooSmall);
        }
        let tx = self.connection.transaction()?;
        let (thread, bot, state, _, _, _) = turns::turn_row(&tx, turn)?;
        if state != "running" {
            return Err(Error::StaleTurn);
        }
        let thread = id(thread)?;
        let bot = id(bot)?;
        let next: Option<(String, String, u64, String, String, Vec<u8>)> = tx.query_row(
            "SELECT o.id,m.id,m.sequence,m.sender,m.kind,m.payload FROM obligations o JOIN inbox i ON i.id=o.id JOIN messages m ON m.id=o.message WHERE o.thread=?1 AND o.bot=?2 AND o.state='owed' AND (json_extract(CAST(m.payload AS TEXT),'$.deadline_ms') IS NULL OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms')>=?4) AND NOT EXISTS(SELECT 1 FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery WHERE x.obligation=o.id AND d.turn=?3) ORDER BY m.sequence,o.id LIMIT 1",
            params![thread.to_string(),bot.to_string(),turn.id.to_string(),now],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).optional()?;
        let Some((obligation, message, sequence, sender, kind, payload)) = next else {
            return Ok(None);
        };
        let boundary: u64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE thread=?1",
            [thread.to_string()],
            |r| r.get(0),
        )?;
        let read: u64 = tx.query_row("SELECT read_sequence FROM participants WHERE thread=?1 AND participant=?2 AND kind='bot'",params![thread.to_string(),bot.to_string()],|r|r.get(0))?;
        let history = history::read(
            &tx,
            thread,
            read,
            boundary,
            HISTORY_LIMIT,
            true,
            Some(bot),
            budget,
        )?;
        let older_history_available = read > 0 || history.len() == HISTORY_LIMIT;
        let mut delivery = Delivery {
            id: Uuid::new_v4(),
            turn,
            history_boundary: boundary,
            context: history.into_iter().rev().map(history::input).collect(),
            work: vec![history::input(Message {
                id: id(message)?,
                sequence,
                sender,
                kind,
                content: decode(&payload)?,
            })],
            older_history_available,
            continuation: false,
            uncertain: false,
            resume_accepted: vec![],
        };
        // A structured input too large to steer waits for normal admission,
        // where the ordinary terminal-refusal rule handles it.
        if let Err(error) = history::bound(&mut delivery, budget) {
            if error == Error::InputTooSmall {
                return Ok(None);
            }
            return Err(error);
        }
        tx.execute("INSERT INTO deliveries(id,turn,session,history_boundary,state,payload) VALUES (?1,?2,?3,?4,'offered',?5)",params![delivery.id.to_string(),turn.id.to_string(),turn.session.to_string(),boundary,encode(&delivery)?])?;
        tx.execute(
            "INSERT INTO delivery_obligations(delivery,obligation) VALUES (?1,?2)",
            params![delivery.id.to_string(), obligation],
        )?;
        tx.execute(
            "UPDATE obligations SET state='delivered' WHERE id=?1",
            [obligation],
        )?;
        tx.commit()?;
        Ok(Some(delivery))
    }

    /// An adapter's definite refusal is different from a lost acknowledgement.
    /// Keep the message owed and remember not to offer it again to this turn.
    pub fn reject_steer(&mut self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        let tx = self.connection.transaction()?;
        let row = turns::turn_row(&tx, turn)?;
        if !matches!(row.2.as_str(), "running" | "answered") {
            return Err(Error::StaleTurn);
        }
        let found: Option<String> = tx
            .query_row(
                "SELECT state FROM deliveries WHERE id=?1 AND turn=?2 AND session=?3",
                params![
                    delivery.to_string(),
                    turn.id.to_string(),
                    turn.session.to_string()
                ],
                |r| r.get(0),
            )
            .optional()?;
        match found.as_deref() {
            Some("offered" | "unknown" | "rejected") => {}
            _ => return Err(Error::StaleTurn),
        }
        tx.execute(
            "UPDATE deliveries SET state='rejected' WHERE id=?1",
            [delivery.to_string()],
        )?;
        tx.execute("UPDATE obligations SET state='owed' WHERE state='delivered' AND id IN (SELECT obligation FROM delivery_obligations WHERE delivery=?1)",[delivery.to_string()])?;
        tx.commit()?;
        Ok(())
    }
}
