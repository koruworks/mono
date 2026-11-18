// SPDX-License-Identifier: MPL-2.0
//! Bounded navigation/output projection; no duplicate operation or file facts.
use super::*;
use rusqlite::{Connection, params};
use serde_json::json;

pub(super) fn read(
    c: &Connection,
    who: &Person,
    thread: Uuid,
    after: Option<Uuid>,
) -> Result<Value, Error> {
    let current = read::thread(c, who, thread)?;
    // Main is also the Bot's artifacts list. Other threads show their own
    // outputs and immutable copies. Every row retains the original output id.
    let rows = c.prepare("SELECT id,sequence,message,kind,target,reference FROM (
        SELECT m.id,m.sequence,m.id AS message,'thread' AS kind,json_extract(CAST(m.payload AS TEXT),'$.references[0].thread') AS target,NULL AS reference FROM messages m WHERE m.thread=?1 AND m.kind='thread_reference'
        UNION ALL
        SELECT o.id,COALESCE(m.sequence,cp.sequence,0),COALESCE(m.id,cp.id,o.message),'output',NULL,o.reference FROM outputs o
        LEFT JOIN messages m ON m.id=o.message AND m.thread=?1
        LEFT JOIN (SELECT copied.id,copied.sequence,mc.source FROM message_copies mc JOIN messages copied ON copied.id=mc.message WHERE copied.thread=?1) cp ON cp.source=o.message
        WHERE m.id IS NOT NULL OR cp.id IS NOT NULL OR (?3='main' AND EXISTS(SELECT 1 FROM participants p JOIN threads t ON t.id=p.thread WHERE p.thread=o.thread AND p.participant=?4 AND p.kind='bot' AND t.lifecycle<>'trashed'))
        ) WHERE (?2 IS NULL OR id>?2) ORDER BY id LIMIT 65")?.query_map(params![thread.to_string(),after.map(|x|x.to_string()),current.kind,current.bot.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<Vec<u8>>>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
    let more = rows.len() > 64;
    let mut items = vec![];
    let mut next = None;
    for (key, sequence, message, kind, target, reference) in rows.into_iter().take(64) {
        next = Some(id(key.clone())?);
        let mut card = json!({"id":key,"sequence":sequence,"message":message,"kind":kind,"title":"","thread":null,"bot":null,"bot_name":"","workspace":null,"document":null,"messages":0,"outputs":0,"state":""});
        if let Some(target) = target {
            let target = read::thread(c, who, id(target)?)?;
            let bot = read::bot(c, who, target.bot)?;
            card["thread"] = json!(target.id);
            card["bot"] = json!(target.bot);
            card["bot_name"] = json!(bot.definition.name);
            card["title"] = json!(target.title);
            if card["title"] == "" {
                let names=c.prepare("SELECT b.name FROM participants p JOIN bots b ON b.id=p.participant WHERE p.thread=?1 AND p.kind='bot' ORDER BY b.id LIMIT 3")?.query_map([target.id.to_string()],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
                let mut title = names.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
                if names.len() > 2 {
                    title.push_str(", …");
                }
                card["title"] = json!(title);
            }
            card["state"] = json!(target.lifecycle);
            card["messages"] = json!(c.query_row(
                "SELECT count(*) FROM messages WHERE thread=?1 AND kind='words'",
                [target.id.to_string()],
                |r| r.get::<_, u64>(0)
            )?);
            card["outputs"] = json!(c.query_row(
                "SELECT count(*) FROM outputs WHERE thread=?1",
                [target.id.to_string()],
                |r| r.get::<_, u64>(0)
            )?);
        } else if let Some(reference) = reference {
            let reference: Value = decode(&reference)?;
            card["workspace"] = reference.get("workspace").cloned().unwrap_or(Value::Null);
            card["document"] = reference.get("document").cloned().unwrap_or(Value::Null);
            card["title"] = reference.get("title").cloned().unwrap_or(json!(""));
        }
        items.push(card);
    }
    Ok(json!({"items":items,"next":if more {next}else{None}}))
}
