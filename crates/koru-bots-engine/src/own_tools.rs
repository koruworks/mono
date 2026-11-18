// SPDX-License-Identifier: MPL-2.0

//! Engine tools use the same arrival binding as workspace tools. A provider
//! cannot nominate another Bot or conversation in its arguments.
use crate::{
    Database, Error,
    authority::{Call, Secret},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn catalog() -> Vec<Value> {
    vec![
        json!({
            "name": "read_older_messages",
            "description": "Read an ordered page of messages in this conversation. Use the last sequence as after to read the next page.",
            "inputSchema": {"type":"object", "properties":{"after":{"type":"integer","minimum":0}}, "additionalProperties":false}
        }),
        json!({
            "name":"message_bot",
        "description":"Ask another Bot without waiting. Omit thread to open a side thread; its answer continues your originating conversation. source_message must name a work item offered to this turn. Reuse id and identical arguments after uncertainty.",
            "inputSchema":{"type":"object","properties":{"id":{"type":"string","format":"uuid"},"to":{"type":"string","format":"uuid"},"source_message":{"type":"string","format":"uuid"},"body":{"type":"string"},"thread":{"type":["string","null"],"format":"uuid"},"deadline_ms":{"type":["integer","null"]}},"required":["id","to","source_message","body"],"additionalProperties":false}
        }),
        json!({
            "name":"list_bots",
            "description":"List active Bots in your workspace, with addresses usable by message_bot. Pass next as after for another page.",
            "inputSchema":{"type":"object","properties":{"after":{"type":["string","null"],"format":"uuid"}},"additionalProperties":false}
        }),
    ]
}

pub(crate) fn execute(
    db: &mut Database,
    secret: &Secret,
    binding: Uuid,
    call: &Call,
    now: i64,
) -> Result<Value, Error> {
    let identity = db.resolve_call(secret, binding, call)?;
    match call.tool() {
        "message_bot" => {
            let request = serde_json::from_value(call.arguments().clone())
                .map_err(|_| Error::InvalidRequest)?;
            crate::messages::collaboration::ask(db, &identity, &request, now)
        }
        "list_bots" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                after: Option<Uuid>,
            }
            let args: Arguments = serde_json::from_value(call.arguments().clone())
                .map_err(|_| Error::InvalidRequest)?;
            let mut items: Vec<Value> = db.connection.prepare("SELECT b.id,b.name FROM bots a JOIN bots b ON b.owner=a.owner WHERE a.id=?1 AND b.lifecycle='active' AND b.id<>a.id AND b.id>?2 AND json_extract(CAST(a.access AS TEXT),'$.workspace')=json_extract(CAST(b.access AS TEXT),'$.workspace') ORDER BY b.id LIMIT 65")?
                .query_map(rusqlite::params![identity.bot.to_string(), args.after.map(|id| id.to_string()).unwrap_or_default()], |r| Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?})))?.collect::<Result<_,_>>()?;
            let more = items.len() > 64;
            items.truncate(64);
            let next = if more {
                items.last().map(|v| v["id"].clone())
            } else {
                None
            };
            Ok(json!({"items":items,"next":next}))
        }
        "read_older_messages" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                #[serde(default)]
                after: u64,
            }
            let args: Arguments = serde_json::from_value(call.arguments().clone())
                .map_err(|_| Error::InvalidRequest)?;
            let messages = db.read_messages_for_bot(identity.bot, identity.thread, args.after)?;
            Ok(json!({"messages": messages}))
        }
        _ => Err(Error::NotAllowed),
    }
}
