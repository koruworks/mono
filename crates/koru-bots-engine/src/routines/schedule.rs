// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_recurrence::{OCCURRENCE_PRECISION_MS, PlannedAction, plan_schedule};
use koru_time_zone::SystemZone;

pub(super) fn enqueue(
    c: &Connection,
    row: &Routine,
    at: i64,
    manual: bool,
    now: i64,
) -> Result<Uuid, Error> {
    if !manual {
        let existing:Option<String>=c.query_row("SELECT id FROM routine_occurrences WHERE routine=?1 AND occurrence_ms=?2 AND manual=0",params![row.id.to_string(),at],|r|r.get(0)).optional()?;
        if let Some(existing) = existing {
            return id(existing);
        }
    }
    // A bounded settlement pass may not yet have visited another elapsed
    // scheduled occurrence. Such unclaimed work cannot make this minute busy.
    let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM obligations o WHERE thread=?1 AND state IN ('owed','delivered') AND NOT (o.state='owed' AND EXISTS(SELECT 1 FROM routine_occurrences r WHERE r.message=o.message AND r.state='queued' AND r.deadline_ms<=?2))) OR EXISTS(SELECT 1 FROM turns WHERE thread=?1 AND state IN ('running','needs_you','parked')) OR EXISTS(SELECT 1 FROM operations WHERE thread=?1 AND (state='leased' OR (state='unresolved' AND json_extract(CAST(recovery AS TEXT),'$.continued_by') IS NULL)))",params![row.thread.to_string(),now],|r|r.get(0))?;
    let key = Uuid::new_v4();
    if busy {
        record(c, row, key, at, manual, "skipped", Some("busy"), None, now)?;
        return Ok(key);
    }
    let message = Uuid::new_v4();
    let deadline = (!manual).then(|| at.saturating_add(OCCURRENCE_PRECISION_MS));
    let mut references = vec![serde_json::json!({"routine":row.id,"occurrence":key})];
    references.extend(row.access.selected_pages.iter().map(|document|serde_json::json!({"kind":"page","workspace":row.access.workspace,"document":document})));
    let content = messages::Content {
        body: row.objective.clone(),
        attachments: vec![],
        references,
        in_reply_to: None,
        deadline_ms: deadline,
        reply_to: messages::ReplyTo::Thread,
    };
    c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,'routine','words',?4,'accepted',?5)",params![message.to_string(),row.thread.to_string(),messages::next_sequence(c,row.thread)?,encode(&content)?,now])?;
    messages::collaboration::wake(c, row.thread, message, row.bot, false, now)?;
    record(c, row, key, at, manual, "queued", None, Some(message), now)?;
    c.execute(
        "UPDATE routine_occurrences SET deadline_ms=?2 WHERE id=?1",
        params![key.to_string(), deadline],
    )?;
    Ok(key)
}
fn record(
    c: &Connection,
    row: &Routine,
    key: Uuid,
    at: i64,
    manual: bool,
    state: &str,
    reason: Option<&str>,
    message: Option<Uuid>,
    now: i64,
) -> Result<(), Error> {
    c.execute("INSERT OR IGNORE INTO routine_occurrences(id,routine,definition_revision,occurrence_ms,manual,state,reason,message,recorded_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",params![key.to_string(),row.id.to_string(),row.revision,at,manual,state,reason,message.map(|x|x.to_string()),now])?;
    Ok(())
}
pub(super) fn cancel_queued(
    c: &Connection,
    routine: Uuid,
    reason: &str,
    now: i64,
) -> Result<(), Error> {
    c.execute("UPDATE obligations SET state='cancelled' WHERE state='owed' AND message IN (SELECT message FROM routine_occurrences WHERE routine=?1 AND state='queued')",[routine.to_string()])?;
    c.execute("DELETE FROM inbox WHERE message IN (SELECT message FROM routine_occurrences WHERE routine=?1 AND state='queued') AND id IN (SELECT id FROM obligations WHERE state='cancelled')",[routine.to_string()])?;
    c.execute("UPDATE routine_occurrences SET state='cancelled',reason=?2,updated_at_ms=?3 WHERE routine=?1 AND state='queued'",params![routine.to_string(),reason,now])?;
    Ok(())
}
impl Database {
    /// At most sixteen definitions and sixteen waiting occurrences per turn.
    /// `after` is a bounded restart/zone-change walk, not an in-memory queue.
    pub(crate) fn evaluate_routines(
        &mut self,
        now: i64,
        zone: &SystemZone,
        scan: Option<Option<Uuid>>,
    ) -> Result<Option<Uuid>, Error> {
        let tx = self.connection.transaction()?;
        settle(&tx, now)?;
        let keys = if let Some(after) = scan {
            tx.prepare("SELECT id,bot FROM routines WHERE enabled=1 AND (?1 IS NULL OR id>?1) ORDER BY id LIMIT 16")?.query_map([after.map(|x|x.to_string())],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?
        } else {
            tx.prepare("SELECT id,bot FROM routines WHERE enabled=1 AND next_due_ms<=?1 ORDER BY next_due_ms,id LIMIT 16")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?
        };
        let mut last = None;
        for (key, bot) in &keys {
            let row = read(&tx, id(bot.clone())?, id(key.clone())?)?;
            let plan = plan_schedule(
                &row.recurrence,
                row.effective_at_ms,
                row.observed_through_ms,
                now,
                zone,
            )
            .map_err(|_| Error::InvalidRequest)?;
            let mut observed = row.observed_through_ms;
            for action in plan.actions {
                let at = match action {
                    PlannedAction::Due { occurrence_ms } => {
                        enqueue(&tx, &row, occurrence_ms, false, now)?;
                        occurrence_ms
                    }
                    PlannedAction::Missed { occurrence_ms } => {
                        record(
                            &tx,
                            &row,
                            Uuid::new_v4(),
                            occurrence_ms,
                            false,
                            "missed",
                            Some("late"),
                            None,
                            now,
                        )?;
                        now
                    }
                };
                observed = Some(observed.map_or(at, |old| old.max(at)));
            }
            if observed != row.observed_through_ms || plan.next_deadline_ms != row.next_due_ms {
                tx.execute(
                    "UPDATE routines SET observed_through_ms=?2,next_due_ms=?3 WHERE id=?1",
                    params![key, observed, plan.next_deadline_ms],
                )?;
            }
            last = Some(row.id);
        }
        tx.commit()?;
        Ok(if keys.len() == 16 { last } else { None })
    }
    pub(crate) fn routine_deadline(&self) -> Result<Option<i64>, Error> {
        Ok(self.connection.query_row("SELECT MIN(at) FROM (SELECT MIN(next_due_ms) at FROM routines WHERE enabled=1 UNION ALL SELECT MIN(deadline_ms) at FROM routine_occurrences WHERE state='queued')",[],|r|r.get(0))?)
    }
}

/// Reconcile bounded occurrence facts against the same obligations used by
/// admission. An already delivered/running request never expires mid-work.
fn settle(c: &Connection, now: i64) -> Result<(), Error> {
    let rows=c.prepare("SELECT r.id,r.message,o.state FROM routine_occurrences r JOIN obligations o ON o.message=r.message WHERE r.state IN ('queued','running') AND (o.state IN ('answered','cancelled','expired') OR (r.state='queued' AND o.state='delivered') OR (r.state='queued' AND r.deadline_ms<=?1 AND o.state='owed')) ORDER BY r.id LIMIT 16")?.query_map([now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (key, message, state) in rows {
        let (outcome, reason) = match state.as_str() {
            "delivered" => ("running", None),
            "answered" => ("answered", None),
            "cancelled" => ("cancelled", None),
            _ => ("missed", Some("capacity")),
        };
        if outcome == "missed" {
            c.execute(
                "UPDATE obligations SET state='expired' WHERE message=?1 AND state='owed'",
                [&message],
            )?;
            c.execute("DELETE FROM inbox WHERE message=?1", [&message])?;
        }
        c.execute(
            "UPDATE routine_occurrences SET state=?2,reason=?3,updated_at_ms=?4 WHERE id=?1",
            params![key, outcome, reason, now],
        )?;
    }
    Ok(())
}

/// Each firing gets its own main-conversation card, atomically with its reply.
pub(crate) fn published(
    c: &Connection,
    turn: messages::Turn,
    reply: Uuid,
    now: i64,
) -> Result<(), Error> {
    let rows=c.prepare("SELECT r.id,s.thread,s.bot FROM routine_occurrences r JOIN routines s ON s.id=r.routine JOIN obligations o ON o.message=r.message JOIN delivery_obligations x ON x.obligation=o.id JOIN deliveries d ON d.id=x.delivery WHERE d.turn=?1 AND o.answered_by=?2 AND r.card IS NULL GROUP BY r.id ORDER BY r.id LIMIT 32")?.query_map(params![turn.id.to_string(),reply.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (key, thread, bot) in rows {
        let main: String = c.query_row(
            "SELECT id FROM threads WHERE bot=?1 AND kind='main'",
            [&bot],
            |r| r.get(0),
        )?;
        let card = Uuid::new_v4();
        let content = messages::Content {
            body: String::new(),
            attachments: vec![],
            references: vec![
                serde_json::json!({"thread":thread,"occurrence":key,"reply":reply,"kind":"routine_result"}),
            ],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: messages::ReplyTo::Thread,
        };
        c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'thread_reference',?5,'published',?6)",params![card.to_string(),main,messages::next_sequence(c,id(main.clone())?)?,bot,encode(&content)?,now])?;
        c.execute("UPDATE routine_occurrences SET state='answered',reply=?2,card=?3,updated_at_ms=?4 WHERE id=?1",params![key,reply.to_string(),card.to_string(),now])?;
        super::review::published(c, turn, &key)?;
        super::review::invalidate(c, id(bot.clone())?, now)?;
    }
    Ok(())
}
