// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{
    Database,
    messages::{self, receipts},
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

fn keys(connection: &Connection, thread: Uuid) -> Result<Vec<Key>, Error> {
    connection.prepare("SELECT scope,bot,id FROM operations WHERE thread=?1 AND (state='leased' OR (state='unresolved' AND json_extract(CAST(recovery AS TEXT),'$.continued_by') IS NULL)) ORDER BY scope,bot,id")?
        .query_map([thread.to_string()], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?
        .map(|row| { let (scope,bot,operation)=row?; Ok(Key { scope,bot:Uuid::parse_str(&bot).map_err(|_|Error::StorageUnavailable)?,thread,operation:Uuid::parse_str(&operation).map_err(|_|Error::StorageUnavailable)? }) }).collect()
}

impl Database {
    /// Called only with a receipt from Store's exact permanent deny-and-drain.
    /// Persisting this barrier precedes evidence lookup; restart can resume it.
    pub fn record_operation_drain(
        &mut self,
        key: &Key,
        drain: NativeEffectDrainReceipt,
    ) -> Result<Record, Error> {
        let tx = self.connection.transaction()?;
        let mut record = ledger::load(&tx, key)?.ok_or(Error::InvalidRequest)?;
        if record.lease.is_none()
            || drain.binding != record.store
            || drain.drained_at < drain.fenced_at
        {
            return Err(Error::InvalidRequest);
        }
        if let Some(prior) = &record.recovery.drain {
            if prior != &drain {
                return Err(Error::Conflict);
            }
        } else {
            record.recovery.drain = Some(drain);
            outcomes::save(&tx, &record)?;
        }
        tx.commit()?;
        Ok(record)
    }

    /// None means a successful native lookup returned Unconfirmed. Transport
    /// failure must not call this method. It keeps the lease pending instead.
    pub fn reconcile_operation(
        &mut self,
        key: &Key,
        verified: Option<Outcome>,
    ) -> Result<Record, Error> {
        let tx = self.connection.transaction()?;
        let mut record = ledger::load(&tx, key)?.ok_or(Error::InvalidRequest)?;
        if record.recovery.drain.is_none() {
            return Err(Error::RecoveryPending);
        }
        if let Some(outcome) = verified {
            if !matches!(outcome, Outcome::Applied { .. }) {
                return Err(Error::InvalidRequest);
            }
            outcomes::observe(&tx, &mut record, outcome)?;
        } else if record.state == State::Leased {
            record.state = State::Unresolved;
            outcomes::save(&tx, &record)?;
            let lease = record.lease.as_ref().ok_or(Error::StorageUnavailable)?;
            tx.execute("UPDATE turns SET state='needs_you' WHERE id=?1 AND state IN ('running','interrupted','parked') AND stopped=0",[lease.binding.turn.id.to_string()])?;
            tx.execute(
                "UPDATE credentials SET revoked=1 WHERE session=?1",
                [lease.binding.turn.session.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(record)
    }

    /// Continue acknowledges exactly the currently unresolved set. Old keys
    /// remain terminal and fenced; fresh work must read their pages first.
    pub fn continue_operations(
        &mut self,
        person: &messages::Person,
        command: Uuid,
        thread: Uuid,
        expected: &[Key],
        now: i64,
    ) -> Result<messages::Receipt<Continued>, Error> {
        if !v4(command) || expected.is_empty() {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        messages::authorize_thread(&tx, person, thread)?;
        let subject = person.receipt_subject();
        let mut expected = expected.to_vec();
        expected.sort_by_key(|k| (k.scope.clone(), k.bot, k.operation));
        if expected.windows(2).any(|p| p[0] == p[1])
            || expected
                .iter()
                .any(|k| k.thread != thread || k.scope != person.owner)
        {
            return Err(Error::InvalidRequest);
        }
        let digest = receipts::digest("continue-operations", &(thread, &expected))?;
        if let Some(answer) = receipts::find(&tx, &subject, command, &digest)? {
            return Ok(messages::Receipt {
                answer,
                replayed: true,
            });
        }
        let current = keys(&tx, thread)?;
        if current != expected {
            return Err(Error::RecoveryPending);
        }
        let mut references: BTreeMap<Uuid, Vec<Value>> = BTreeMap::new();
        for key in &current {
            let mut record = ledger::load(&tx, key)?.ok_or(Error::StorageUnavailable)?;
            if record.state != State::Unresolved || record.recovery.drain.is_none() {
                return Err(Error::RecoveryPending);
            }
            references.entry(key.bot).or_default().push(serde_json::json!({"workspace":record.store.workspace,"document":record.store.document,"operation":key.operation,"state":"unresolved","observations":record.recovery.observations}));
            record.recovery.continued_by = Some(command);
            outcomes::save(&tx, &record)?;
            tx.execute(
                "UPDATE turns SET state='interrupted' WHERE id=?1 AND state='needs_you'",
                [record
                    .lease
                    .as_ref()
                    .ok_or(Error::StorageUnavailable)?
                    .binding
                    .turn
                    .id
                    .to_string()],
            )?;
        }
        let mut answer = Continued { messages: vec![] };
        for (bot, references) in references {
            messages::authorize_bot(&tx, person, bot)?;
            let message = Uuid::new_v4();
            let content=messages::Content { body:"Continue after the person acknowledged uncertain page writes. Read each referenced page before acting. These operations remain unresolved and permanently fenced. Do not retry their operation IDs. Any new write requires a new operation ID.".into(),attachments:vec![],references,in_reply_to:None,deadline_ms:None,reply_to:messages::ReplyTo::Thread };
            tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,'engine','words',?4,'accepted',?5)",params![message.to_string(),thread.to_string(),messages::next_sequence(&tx,thread)?,encode(&content)?,now])?;
            let obligation = Uuid::new_v4();
            tx.execute("INSERT INTO obligations(id,thread,message,bot,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'owed',?5)",params![obligation.to_string(),thread.to_string(),message.to_string(),bot.to_string(),now])?;
            tx.execute(
                "INSERT INTO inbox(id,thread,message,bot,accepted_at_ms) VALUES (?1,?2,?3,?4,?5)",
                params![
                    obligation.to_string(),
                    thread.to_string(),
                    message.to_string(),
                    bot.to_string(),
                    now
                ],
            )?;
            answer.messages.push(message);
        }
        receipts::save(&tx, &subject, command, &digest, &answer, now)?;
        tx.commit()?;
        Ok(messages::Receipt {
            answer,
            replayed: false,
        })
    }
}
