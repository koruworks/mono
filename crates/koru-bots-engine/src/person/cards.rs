// SPDX-License-Identifier: MPL-2.0
//! Bounded navigation/output projection; no duplicate operation or file facts.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
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
    // Fan-out can produce many dispatches per card. Bound the entire page's
    // returned prose as well as each individual preview; exhausted excerpts
    // retain their identity and continuation link with `truncated: true`.
    let mut reply_bytes_left = 256 * 1024;
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
            // The navigation reference alone names the thread's starter, not
            // the Bot being asked. Preserve the actual recipients and causal
            // turn so the UI can group a fan-out without guessing from prose.
            let dispatches = c.prepare("SELECT a.origin_turn,a.message,a.recipient,b.name,o.state,o.answered_by,r.payload,origin.reply_message FROM bot_dispatches a JOIN messages m ON m.id=a.message JOIN bots b ON b.id=a.recipient JOIN obligations o ON o.message=a.message AND o.bot=a.recipient LEFT JOIN turns origin ON origin.id=a.origin_turn AND origin.thread=a.origin_thread LEFT JOIN messages r ON r.id=o.answered_by AND r.thread=m.thread AND r.sender=a.recipient AND r.kind='words' AND r.state='published' AND o.state='answered' WHERE a.origin_thread=?1 AND m.thread=?2 ORDER BY m.sequence,a.recipient LIMIT 64")?
                .query_map(params![thread.to_string(),target.id.to_string()], |r| {
                    Ok((json!({"group":r.get::<_,String>(0)?,"message":r.get::<_,String>(1)?,"recipient":r.get::<_,String>(2)?,"name":r.get::<_,String>(3)?,"state":r.get::<_,String>(4)?,"reply":r.get::<_,Option<String>>(5)?,"origin_reply":r.get::<_,Option<String>>(7)?}), r.get::<_,Option<Vec<u8>>>(6)?))
                })?.collect::<Result<Vec<_>,_>>()?;
            let dispatches = dispatches
                .into_iter()
                .map(|(mut dispatch, payload)| {
                    if let Some(payload) = payload {
                        let content: crate::messages::Content = decode(&payload)?;
                        let message = koru_ai_core::MessageId::from_uuid(id(dispatch["reply"]
                            .as_str()
                            .ok_or(Error::StorageUnavailable)?
                            .to_owned())?);
                        if let Some(mut preview) =
                            koru_ai_core::bots::BotReplyPreview::new(message, &content.body)
                        {
                            if preview.markdown.len() > reply_bytes_left {
                                let mut end = reply_bytes_left;
                                while !preview.markdown.is_char_boundary(end) {
                                    end -= 1;
                                }
                                preview.markdown.truncate(end);
                                preview.truncated = true;
                            }
                            reply_bytes_left -= preview.markdown.len();
                            dispatch["preview"] = json!(preview);
                        }
                    }
                    Ok(dispatch)
                })
                .collect::<Result<Vec<_>, Error>>()?;
            card["dispatches"] = json!(dispatches);
            // One completed occurrence, not the accumulated count of every
            // firing in this persistent Routine thread. The card id is the
            // durable publication link, so later replies cannot replace it.
            let routine:Option<(String,String,i64,bool,String,Vec<u8>)>=c.query_row("SELECT s.id,r.id,r.occurrence_ms,r.manual,m.id,m.payload FROM routine_occurrences r JOIN routines s ON s.id=r.routine JOIN messages m ON m.id=r.reply AND m.thread=s.thread AND m.sender=s.bot AND m.kind='words' AND m.state='published' WHERE r.card=?1 AND s.thread=?2 AND r.state='answered'",params![key,target.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            if let Some((routine, occurrence, at_ms, manual, reply, payload)) = routine {
                let content: crate::messages::Content = decode(&payload)?;
                let mut preview = koru_ai_core::bots::BotReplyPreview::new(
                    koru_ai_core::MessageId::from_uuid(id(reply.clone())?),
                    &content.body,
                );
                if let Some(preview) = &mut preview {
                    if preview.markdown.len() > reply_bytes_left {
                        let mut end = reply_bytes_left;
                        while !preview.markdown.is_char_boundary(end) {
                            end -= 1;
                        }
                        preview.markdown.truncate(end);
                        preview.truncated = true;
                    }
                    reply_bytes_left -= preview.markdown.len();
                }
                card["routine"] = json!({"routine":routine,"occurrence":occurrence,"at_ms":at_ms,"manual":manual,"reply":reply,"preview":preview});
            }
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
