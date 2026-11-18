// SPDX-License-Identifier: MPL-2.0
//! Bounded window lists over maintained thread metadata, never transcript scans.
use super::*;
use rusqlite::{Connection, params};

fn limit(limit: u16) -> Result<usize, Error> {
    if limit == 0 || limit > 100 {
        Err(Error::InvalidRequest)
    } else {
        Ok(usize::from(limit))
    }
}
fn threads(
    c: &Connection,
    who: &Person,
    mut keys: Vec<String>,
    limit: usize,
) -> Result<(Vec<Thread>, bool), Error> {
    let more = keys.len() > limit;
    keys.truncate(limit);
    let items = keys
        .into_iter()
        .map(|key| read::thread(c, who, id(key)?))
        .collect::<Result<Vec<_>, _>>()?;
    if encode(&items)?.len() > MAX_COMMAND_BYTES {
        return Err(Error::InputTooSmall);
    }
    Ok((items, more))
}
pub(super) fn conversations(
    c: &Connection,
    who: &Person,
    bot: Uuid,
    after: Option<ConversationCursor>,
    count: u16,
) -> Result<Value, Error> {
    let count = limit(count)?;
    messages::authorize_bot(c, who, bot)?;
    let keys = c.prepare("SELECT t.id FROM threads t JOIN participants p ON p.thread=t.id WHERE t.bot=?1 AND t.lifecycle='active' AND p.kind='person' AND p.participant=?2 AND (?3 IS NULL OR (t.kind='main')<?4 OR ((t.kind='main')=?4 AND (t.updated_at_ms<?5 OR (t.updated_at_ms=?5 AND t.id>?3)))) ORDER BY (t.kind='main') DESC,t.updated_at_ms DESC,t.id LIMIT ?6")?
        .query_map(params![bot.to_string(), who.sender, after.as_ref().map(|v| v.thread.to_string()), after.as_ref().map(|v| v.main), after.as_ref().map(|v| v.updated_at_ms), count+1], |r| r.get(0))?
        .collect::<Result<Vec<String>,_>>()?;
    let (items, more) = threads(c, who, keys, count)?;
    let next = if more {
        items.last().map(|thread| ConversationCursor {
            main: thread.kind == "main",
            updated_at_ms: thread.updated_at_ms,
            thread: thread.id,
        })
    } else {
        None
    };
    value(serde_json::json!({"items":items,"next":next}))
}
pub(super) fn trashed(
    c: &Connection,
    who: &Person,
    workspace: Uuid,
    after: Option<Uuid>,
    count: u16,
) -> Result<Value, Error> {
    let count = limit(count)?;
    let keys = c.prepare("SELECT t.id FROM threads t JOIN bots b ON b.id=t.bot JOIN participants p ON p.thread=t.id WHERE b.owner=?1 AND json_extract(CAST(b.access AS TEXT),'$.workspace')=?2 AND (t.lifecycle='trashed' OR b.lifecycle='trashed') AND p.kind='person' AND p.participant=?3 AND (?4 IS NULL OR t.id>?4) ORDER BY t.id LIMIT ?5")?
        .query_map(params![who.owner, workspace.to_string(), who.sender, after.map(|id|id.to_string()), count+1], |r| r.get(0))?
        .collect::<Result<Vec<String>,_>>()?;
    let (items, more) = threads(c, who, keys, count)?;
    let next = if more {
        items.last().map(|thread| thread.id)
    } else {
        None
    };
    value(serde_json::json!({"items":items,"next":next}))
}
pub(super) fn activity(c: &Connection, who: &Person, workspace: Uuid) -> Result<Value, Error> {
    // Only current turns count: an old needs-you turn cannot eclipse a newer
    // completed turn. Queued inbox work counts even while the Bot is paused.
    let mut statement = c.prepare("SELECT t.bot,t.id,EXISTS(SELECT 1 FROM turns x WHERE x.thread=t.id AND x.state='needs_you' AND NOT EXISTS(SELECT 1 FROM turns newer WHERE newer.thread=x.thread AND newer.bot=x.bot AND newer.generation>x.generation)) AS needs_you FROM threads t JOIN bots b ON b.id=t.bot JOIN participants p ON p.thread=t.id WHERE b.owner=?1 AND json_extract(CAST(b.access AS TEXT),'$.workspace')=?2 AND b.lifecycle='active' AND t.lifecycle='active' AND p.kind='person' AND p.participant=?3 AND (EXISTS(SELECT 1 FROM inbox i WHERE i.thread=t.id) OR EXISTS(SELECT 1 FROM turns x WHERE x.thread=t.id AND x.state IN ('queued','running','needs_you','parked','interrupted') AND NOT EXISTS(SELECT 1 FROM turns newer WHERE newer.thread=x.thread AND newer.bot=x.bot AND newer.generation>x.generation))) ORDER BY t.bot,t.id LIMIT 201")?;
    let mut rows = statement
        .query_map(params![who.owner, workspace.to_string(), who.sender], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?
        .map(|row| {
            let (bot, thread, needs_you) = row?;
            Ok(serde_json::json!({"bot":id(bot)?,"thread":id(thread)?,"needs_you":needs_you}))
        })
        .collect::<Result<Vec<Value>, Error>>()?;
    let truncated = rows.len() > 200;
    rows.truncate(200);
    value(serde_json::json!({"items":rows,"truncated":truncated}))
}
