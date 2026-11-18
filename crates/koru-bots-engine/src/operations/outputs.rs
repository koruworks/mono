// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::messages::{Output, OutputKind, Turn};
use rusqlite::{Connection, params};

fn output(record: &Record) -> Result<Output, Error> {
    let Some(Outcome::Applied { evidence, .. }) = &record.outcome else {
        return Err(Error::StorageUnavailable);
    };
    outcomes::validate(record, record.outcome.as_ref().unwrap())?;
    Ok(Output {
        id: record.output,
        operation: record.key.operation,
        kind: OutputKind::Page,
        reference: serde_json::to_value(evidence).map_err(|_| Error::StorageUnavailable)?,
    })
}

/// The executing lease's turn owns the output, even when an earlier turn
/// recorded the intent. Provider prose and caller-chosen references are unused.
pub(crate) fn collect(connection: &Connection, turn: Turn) -> Result<Vec<Output>, Error> {
    let keys:Vec<Key>=connection.prepare("SELECT scope,bot,thread,id FROM operations WHERE state='applied' AND json_extract(CAST(lease AS TEXT),'$.binding.turn.id')=?1 ORDER BY id")?
        .query_map([turn.id.to_string()], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?
        .map(|row| { let (scope,bot,thread,operation)=row?; let id=|s:&str|Uuid::parse_str(s).map_err(|_|Error::StorageUnavailable);
            Ok(Key{scope,bot:id(&bot)?,thread:id(&thread)?,operation:id(&operation)?}) }).collect::<Result<_,Error>>()?;
    keys.iter()
        .map(|key| {
            let record = ledger::load(connection, key)?.ok_or(Error::StorageUnavailable)?;
            if record.lease.as_ref().map(|l| l.binding.turn) != Some(turn) {
                return Err(Error::StorageUnavailable);
            }
            output(&record)
        })
        .collect()
}

pub(crate) fn attach(connection: &Connection, message: Uuid, output: &Output) -> Result<(), Error> {
    let changed = connection.execute(
        "UPDATE outputs SET message=?2 WHERE id=?1 AND operation=?3 AND reference=?4 AND EXISTS(SELECT 1 FROM messages m JOIN bots b ON b.id=m.sender WHERE m.id=?2 AND b.owner=outputs.scope AND b.id=outputs.bot AND m.thread=outputs.thread)",
        params![
            output.id.to_string(),
            message.to_string(),
            output.operation.to_string(),
            encode(&output.reference)?
        ],
    )?;
    if changed != 1 {
        return Err(Error::StorageUnavailable);
    }
    Ok(())
}

/// Record the applied fact as a typed history entry, even if the provider never
/// publishes. A replacement session can read it with the ordinary history
/// budget/cursor. It is evidence, not new work or a fabricated assistant reply.
/// Publish subsequently attaches the stable output to its exact reply.
pub(super) fn record_output(connection: &Connection, record: &Record) -> Result<(), Error> {
    let turn = record
        .lease
        .as_ref()
        .ok_or(Error::StorageUnavailable)?
        .binding
        .turn;
    let message:Option<String>=connection.query_row("SELECT COALESCE(json_extract(CAST(details AS TEXT),'$.published_message'),json_extract(CAST(details AS TEXT),'$.stopped_message')) FROM turns WHERE id=?1 AND generation=?2 AND worker_incarnation=?3 AND session=?4",
        params![turn.id.to_string(),turn.generation,turn.worker.to_string(),turn.session.to_string()],|r|r.get(0))?;
    let published = message
        .map(|id| Uuid::parse_str(&id).map_err(|_| Error::StorageUnavailable))
        .transpose()?;
    let output = output(record)?;
    let notice = Uuid::new_v4();
    let Some(Outcome::Applied { evidence, .. }) = &record.outcome else {
        return Err(Error::StorageUnavailable);
    };
    let content = crate::messages::Content {
        body: String::new(),
        attachments: vec![],
        references: vec![
            serde_json::json!({"notice":"operation_applied","turn":turn.id,"output":output.id,"operation":output.operation,"reference":output.reference}),
        ],
        in_reply_to: None,
        deadline_ms: None,
        reply_to: crate::messages::ReplyTo::Thread,
    };
    connection.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'setup_note',?5,'published',?6)",
        params![notice.to_string(),record.key.thread.to_string(),crate::messages::next_sequence(connection,record.key.thread)?,record.key.bot.to_string(),encode(&content)?,evidence.confirmed_at_ms])?;
    connection.execute("INSERT INTO outputs(id,message,operation,scope,bot,thread,kind,reference) VALUES (?1,?2,?3,?4,?5,?6,'page',?7)",
        params![output.id.to_string(),published.unwrap_or(notice).to_string(),record.key.operation.to_string(),record.key.scope,record.key.bot.to_string(),record.key.thread.to_string(),encode(&output.reference)?])?;
    Ok(())
}
