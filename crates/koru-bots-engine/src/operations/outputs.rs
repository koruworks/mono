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

pub(crate) fn insert(connection: &Connection, message: Uuid, output: &Output) -> Result<(), Error> {
    let changed = connection.execute(
        "INSERT INTO outputs(id,message,operation,scope,bot,thread,kind,reference) SELECT ?1,m.id,?3,b.owner,b.id,m.thread,'page',?4 FROM messages m JOIN bots b ON b.id=m.sender WHERE m.id=?2",
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

/// Publish and the first Applied observation serialize in the same database.
/// If Publish won, attach the stable output to that exact reply now; never to
/// the newest reply in the thread. Stopped turns have no published reply.
pub(super) fn attach_if_published(connection: &Connection, record: &Record) -> Result<(), Error> {
    let turn = record
        .lease
        .as_ref()
        .ok_or(Error::StorageUnavailable)?
        .binding
        .turn;
    let message:Option<String>=connection.query_row("SELECT json_extract(CAST(details AS TEXT),'$.published_message') FROM turns WHERE id=?1 AND generation=?2 AND worker_incarnation=?3 AND session=?4",
        params![turn.id.to_string(),turn.generation,turn.worker.to_string(),turn.session.to_string()],|r|r.get(0))?;
    if let Some(message) = message {
        insert(
            connection,
            Uuid::parse_str(&message).map_err(|_| Error::StorageUnavailable)?,
            &output(record)?,
        )?;
    }
    Ok(())
}
