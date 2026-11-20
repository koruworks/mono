// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

type TurnRow = (String, String, String, u64, String, String);
pub(crate) fn turn_row(connection: &rusqlite::Connection, turn: Turn) -> Result<TurnRow, Error> {
    let row: Option<TurnRow> = connection
        .query_row(
            "SELECT thread,bot,state,generation,worker_incarnation,session FROM turns WHERE id=?1",
            [turn.id.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    row.filter(|r| {
        r.3 == turn.generation && r.4 == turn.worker.to_string() && r.5 == turn.session.to_string()
    })
    .ok_or(Error::StaleTurn)
}
fn active(connection: &rusqlite::Connection, turn: Turn) -> Result<(Uuid, Uuid), Error> {
    let row = turn_row(connection, turn)?;
    if row.2 != "running" {
        return Err(Error::StaleTurn);
    }
    Ok((id(row.0)?, id(row.1)?))
}

impl Database {
    /// The worker carrier supplies a registered incarnation and session. The
    /// actor admits only one running turn for this pair; other threads can run.
    pub fn claim_messages(
        &mut self,
        bot: Uuid,
        thread: Uuid,
        worker: Uuid,
        session: Uuid,
        resume: Resume,
        budget: usize,
        now: i64,
    ) -> Result<Option<Delivery>, Error> {
        // A caller that left no room for ordinary framing has a context/config
        // error. It must not turn valid user work into a terminal refusal.
        if budget < 1024 {
            return Err(Error::InputTooSmall);
        }
        let mut limit = DELIVERY_LIMIT;
        loop {
            match self.claim_bounded(bot, thread, worker, session, &resume, budget, now, limit) {
                Err(Error::InputTooSmall) if limit > 1 => limit /= 2,
                result => return result,
            }
        }
    }

    fn claim_bounded(
        &mut self,
        bot: Uuid,
        thread: Uuid,
        worker: Uuid,
        session: Uuid,
        resume: &Resume,
        budget: usize,
        now: i64,
        limit: usize,
    ) -> Result<Option<Delivery>, Error> {
        if worker.is_nil() || session.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bots b JOIN threads t ON t.id=?2 JOIN participants p ON p.thread=t.id LEFT JOIN team ON team.owner=b.owner WHERE b.id=?1 AND b.paused=0 AND b.lifecycle='active' AND t.lifecycle='active' AND p.participant=b.id AND p.kind='bot' AND COALESCE(team.paused,0)=0)",params![bot.to_string(),thread.to_string()],|r|r.get(0))?;
        if !allowed {
            return Ok(None);
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE bot=?1 AND thread=?2 AND state IN ('running','needs_you','parked')) OR EXISTS(SELECT 1 FROM operations WHERE bot=?1 AND thread=?2 AND (state='leased' OR (state='unresolved' AND json_extract(CAST(recovery AS TEXT),'$.continued_by') IS NULL)))",params![bot.to_string(),thread.to_string()],|r|r.get(0))?;
        if busy {
            return Ok(None);
        }
        expire(&tx, now)?;
        let owed:Vec<(String,String)>=tx.prepare("SELECT o.id,o.message FROM obligations o JOIN inbox i ON i.id=o.id JOIN messages m ON m.id=o.message WHERE o.bot=?1 AND o.thread=?2 AND o.state IN ('owed','delivered') AND (o.state='delivered' OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms') IS NULL OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms')>=?3) AND NOT EXISTS(SELECT 1 FROM routine_occurrences ro WHERE ro.message=m.id AND ro.state='queued' AND ro.deadline_ms<=?3) ORDER BY m.sequence,o.id LIMIT ?4")?.query_map(params![bot.to_string(),thread.to_string(),now,limit as u64],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        if owed.is_empty() {
            tx.commit()?;
            return Ok(None);
        }
        let generation: u64 = tx.query_row(
            "SELECT COALESCE(MAX(generation),0)+1 FROM turns WHERE bot=?1 AND thread=?2",
            params![bot.to_string(), thread.to_string()],
            |r| r.get(0),
        )?;
        let turn = Turn {
            id: Uuid::new_v4(),
            generation,
            worker,
            session,
        };
        tx.execute("UPDATE questions SET state='cancelled',delivery_state='sent' WHERE state='open' AND turn IN (SELECT id FROM turns WHERE bot=?1 AND thread=?2)",params![bot.to_string(),thread.to_string()])?;
        let grant = crate::routines::grant(&tx, bot, thread)?;
        tx.execute("INSERT INTO turns(id,thread,bot,session,grant,state,generation,worker_incarnation,details,created_at_ms,reply_message) VALUES (?1,?2,?3,?4,?5,'running',?6,?7,x'7b7d',?8,?9)",params![turn.id.to_string(),thread.to_string(),bot.to_string(),session.to_string(),grant,generation,worker.to_string(),now,Uuid::new_v4().to_string()])?;
        let boundary: u64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE thread=?1",
            [thread.to_string()],
            |r| r.get(0),
        )?;
        let mut delivery = Delivery {
            id: Uuid::new_v4(),
            turn,
            history_boundary: boundary,
            context: vec![],
            work: vec![],
            older_history_available: false,
            continuation: false,
            uncertain: false,
            resume_accepted: vec![],
        };
        let found: BTreeSet<_> = match &resume {
            Resume::Rebuilt => BTreeSet::new(),
            Resume::Resumed { found_deliveries } => found_deliveries.iter().copied().collect(),
        };
        let mut carried = vec![];
        let mut fresh = vec![];
        for (obligation, message) in &owed {
            // A carried delivery is an engine record, not another provider
            // input. Any original input retained by this session can prove the
            // obligation, even after several interrupted resumes.
            let previous:Option<(String,Vec<u8>)>=tx.query_row("SELECT d.state,d.payload FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery JOIN turns t ON t.id=d.turn WHERE x.obligation=?1 ORDER BY t.generation DESC,d.id LIMIT 1",[obligation],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            delivery.continuation |= previous.is_some();
            // Join the small proof set rather than loading every historical
            // delivery payload again on each resume.
            let proven_ids:Vec<String>=tx.prepare("SELECT x.delivery FROM delivery_obligations x JOIN json_each(CAST(?2 AS TEXT)) f ON f.value=x.delivery WHERE x.obligation=?1")?.query_map(params![obligation,encode(&found)?],|r|r.get(0))?.collect::<Result<_,_>>()?;
            let proven = !proven_ids.is_empty();
            for prior in proven_ids {
                delivery.resume_accepted.push(id(prior)?);
            }
            if !proven && let Some((state, payload)) = &previous {
                delivery.uncertain |= matches!(resume, Resume::Resumed { .. })
                    || matches!(state.as_str(), "offered" | "unknown")
                    || decode::<Delivery>(payload)?.uncertain;
            }
            if proven {
                carried.push(obligation.clone());
            } else {
                fresh.push(obligation.clone());
                let row: Message = tx
                    .query_row(
                        "SELECT sequence,sender,kind,payload,accepted_at_ms FROM messages WHERE id=?1",
                        [message],
                        |r| {
                            Ok((
                                r.get::<_, u64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, Vec<u8>>(3)?,
                                r.get::<_, i64>(4)?,
                            ))
                        },
                    )
                    .map_err(Error::from)
                    .and_then(|(sequence, sender, kind, payload, created_at_ms)| {
                        Ok(Message {
                            id: id(message.clone())?,
                            sequence,
                            sender,
                            kind,
                            content: decode(&payload)?,
                            created_at_ms,
                        })
                    })?;
                delivery.work.push(history::input(row));
            }
        }
        delivery.resume_accepted.sort();
        delivery.resume_accepted.dedup();
        if matches!(resume, Resume::Resumed { .. }) {
            reconcile_deliveries(&tx, bot, thread, &found)?;
        }
        let read_point: u64 = if matches!(resume, Resume::Rebuilt) {
            0
        } else {
            tx.query_row("SELECT read_sequence FROM participants WHERE thread=?1 AND participant=?2 AND kind='bot'",params![thread.to_string(),bot.to_string()],|r|r.get(0))?
        };
        let history = history::read(
            &tx,
            thread,
            read_point,
            boundary,
            HISTORY_LIMIT + 1,
            true,
            Some(bot),
            budget,
        )?;
        let earliest = history
            .last()
            .map_or(boundary.saturating_add(1), |m| m.sequence);
        delivery.older_history_available = history.len() > HISTORY_LIMIT || tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE thread=?1 AND sequence>?2 AND sequence<?3 AND NOT EXISTS(SELECT 1 FROM obligations o WHERE o.message=m.id AND o.bot=?4 AND o.state IN ('owed','delivered')))",params![thread.to_string(),read_point,earliest,bot.to_string()],|r|r.get::<_,bool>(0))?;
        for message in history.into_iter().take(HISTORY_LIMIT).rev() {
            delivery.context.push(history::input(message));
        }
        if let Err(error) = history::bound(&mut delivery, budget) {
            if error == Error::InputTooSmall && limit == 1 && fresh.len() == 1 {
                let obligation = fresh.remove(0);
                // Undo this tentative turn and all resume reconciliation.
                // The actor owns the connection across the following atomic
                // refusal, which rechecks the obligation's exact scope.
                tx.rollback()?;
                self.refuse_oversized(bot, thread, &obligation, now)?;
            }
            return Err(error);
        }
        if matches!(resume, Resume::Rebuilt) {
            tx.execute("UPDATE deliveries SET state='superseded' WHERE turn IN (SELECT id FROM turns WHERE bot=?1 AND thread=?2 AND id<>?3) AND state<>'superseded' AND EXISTS(SELECT 1 FROM delivery_obligations x JOIN obligations o ON o.id=x.obligation WHERE x.delivery=deliveries.id AND o.state IN ('owed','delivered'))",params![bot.to_string(),thread.to_string(),turn.id.to_string()])?;
        }
        if !carried.is_empty() {
            let carried_id = Uuid::new_v4();
            tx.execute("INSERT INTO deliveries(id,turn,session,history_boundary,state,payload) VALUES (?1,?2,?3,?4,'accepted',?5)",params![carried_id.to_string(),turn.id.to_string(),session.to_string(),read_point,encode(&delivery)?])?;
            for obligation in carried {
                tx.execute(
                    "INSERT INTO delivery_obligations VALUES (?1,?2)",
                    params![carried_id.to_string(), obligation],
                )?;
            }
        }
        tx.execute("INSERT INTO deliveries(id,turn,session,history_boundary,state,payload) VALUES (?1,?2,?3,?4,'offered',?5)",params![delivery.id.to_string(),turn.id.to_string(),session.to_string(),boundary,encode(&delivery)?])?;
        for obligation in fresh {
            tx.execute(
                "INSERT INTO delivery_obligations VALUES (?1,?2)",
                params![delivery.id.to_string(), obligation],
            )?;
        }
        for (obligation, message) in &owed {
            tx.execute(
                "UPDATE obligations SET state='delivered' WHERE id=?1",
                [obligation],
            )?;
            tx.execute("UPDATE routine_occurrences SET state='running',updated_at_ms=?2 WHERE message=?1 AND state='queued'",params![message,now])?;
        }
        tx.commit()?;
        Ok(Some(delivery))
    }

    pub fn delivery_accepted(&mut self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        let tx = self.connection.transaction()?;
        let (thread, bot, state, _, _, _) = turn_row(&tx, turn)?;
        if !matches!(state.as_str(), "running" | "answered") {
            return Err(Error::StaleTurn);
        }
        let boundary:Option<u64>=tx.query_row("SELECT history_boundary FROM deliveries WHERE id=?1 AND turn=?2 AND session=?3 AND state IN ('offered','unknown','accepted')",params![delivery.to_string(),turn.id.to_string(),turn.session.to_string()],|r|r.get(0)).optional()?;
        let boundary = boundary.ok_or(Error::StaleTurn)?;
        tx.execute(
            "UPDATE deliveries SET state='accepted' WHERE id=?1",
            [delivery.to_string()],
        )?;
        tx.execute("UPDATE participants SET read_sequence=MAX(read_sequence,?1) WHERE thread=?2 AND participant=?3 AND kind='bot'",params![boundary,thread,bot])?;
        // Late confirmation after Publish records evidence only. It never
        // answers obligations retroactively or changes a published reply.
        tx.commit()?;
        Ok(())
    }

    pub fn mark_delivery_unknown(&mut self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        active(&self.connection, turn)?;
        self.connection.execute("UPDATE deliveries SET state='unknown' WHERE id=?1 AND turn=?2 AND session=?3 AND state='offered'",params![delivery.to_string(),turn.id.to_string(),turn.session.to_string()])?;
        Ok(())
    }

    /// A replacement worker cannot borrow the old worker's current turn.
    /// Operations are intentionally untouched; P1.6 owns their settlement.
    pub fn interrupt_worker(&mut self, worker: Uuid) -> Result<usize, Error> {
        let tx = self.connection.transaction()?;
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE worker_incarnation=?1",
            [worker.to_string()],
        )?;
        tx.execute("UPDATE deliveries SET state='unknown' WHERE state='offered' AND turn IN (SELECT id FROM turns WHERE worker_incarnation=?1 AND state='running')",[worker.to_string()])?;
        let count = tx.execute(
            "UPDATE turns SET state='interrupted' WHERE worker_incarnation=?1 AND state='running'",
            [worker.to_string()],
        )?;
        crate::questions::recover(&tx, Some(worker))?;
        tx.commit()?;
        Ok(count)
    }
    /// The session owner has confirmed this provider process ended. This
    /// cannot interrupt another conversation or a replacement generation.
    pub fn interrupt_turn(&mut self, turn: Turn) -> Result<(), Error> {
        let tx = self.connection.transaction()?;
        let row = turn_row(&tx, turn)?;
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE session=?1 AND worker_incarnation=?2",
            params![turn.session.to_string(), turn.worker.to_string()],
        )?;
        if row.2 == "running" {
            tx.execute(
                "UPDATE deliveries SET state='unknown' WHERE turn=?1 AND state='offered'",
                [turn.id.to_string()],
            )?;
            tx.execute(
                "UPDATE turns SET state='interrupted' WHERE id=?1",
                [turn.id.to_string()],
            )?;
        }
        crate::questions::recover(&tx, Some(turn.worker))?;
        tx.commit()?;
        Ok(())
    }
    /// Only at engine startup, after exclusive binding and parent containment
    /// have established that every previous worker incarnation is gone.
    pub fn recover_interrupted_turns(&mut self) -> Result<usize, Error> {
        let tx = self.connection.transaction()?;
        tx.execute("UPDATE credentials SET revoked=1 WHERE revoked=0", [])?;
        tx.execute("UPDATE deliveries SET state='unknown' WHERE state='offered' AND turn IN (SELECT id FROM turns WHERE state='running')",[])?;
        let count = tx.execute(
            "UPDATE turns SET state='interrupted' WHERE state='running'",
            [],
        )?;
        crate::questions::recover(&tx, None)?;
        tx.commit()?;
        Ok(count)
    }
    pub fn expire_messages(&mut self, now: i64) -> Result<usize, Error> {
        let tx = self.connection.transaction()?;
        let count = expire(&tx, now)?;
        tx.commit()?;
        Ok(count)
    }
}

fn expire(connection: &rusqlite::Connection, now: i64) -> Result<usize, Error> {
    // A bounded batch; claim independently excludes expired work even if an
    // older backlog still awaits this maintenance pass.
    let ids:Vec<String>=connection.prepare("SELECT o.id FROM obligations o JOIN messages m ON m.id=o.message WHERE o.state='owed' AND json_extract(CAST(m.payload AS TEXT),'$.deadline_ms')<?1 ORDER BY o.accepted_at_ms,o.id LIMIT 64")?.query_map([now],|r|r.get(0))?.collect::<Result<_,_>>()?;
    for id in &ids {
        connection.execute("UPDATE obligations SET state='expired' WHERE id=?1", [id])?;
        connection.execute("DELETE FROM inbox WHERE id=?1", [id])?;
    }
    Ok(ids.len())
}

fn reconcile_deliveries(
    connection: &rusqlite::Connection,
    bot: Uuid,
    thread: Uuid,
    found: &BTreeSet<Uuid>,
) -> Result<(), Error> {
    let proof = encode(found)?;
    connection.execute(
        "UPDATE deliveries SET state=CASE WHEN id IN (SELECT value FROM json_each(CAST(?3 AS TEXT))) THEN 'accepted' ELSE 'unknown' END
         WHERE turn IN (SELECT id FROM turns WHERE bot=?1 AND thread=?2) AND EXISTS(
           SELECT 1 FROM delivery_obligations x JOIN obligations o ON o.id=x.obligation
           WHERE x.delivery=deliveries.id AND o.state IN ('owed','delivered'))",
        params![bot.to_string(),thread.to_string(),proof],
    )?;
    connection.execute(
        "UPDATE participants SET read_sequence=MAX(read_sequence,COALESCE((
           SELECT MAX(d.history_boundary) FROM deliveries d JOIN turns t ON t.id=d.turn
           WHERE t.bot=?1 AND t.thread=?2 AND d.id IN (SELECT value FROM json_each(CAST(?3 AS TEXT)))
         ),0)) WHERE thread=?2 AND participant=?1 AND kind='bot'",
        params![bot.to_string(),thread.to_string(),proof],
    )?;
    Ok(())
}
