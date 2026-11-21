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
    let mut tools = vec![
        json!({
            "name":"read_routine_result",
            "description":"Read the exact retained Routine result attached to a person's message in this Bot's main conversation. Use routine_context.occurrence and pass next as offset until complete. The excerpt can be partial; this tool reads the full result without granting access to linked documents or other Bots.",
            "inputSchema":{"type":"object","properties":{"occurrence":{"type":"string","format":"uuid"},"offset":{"type":"integer","minimum":0}},"required":["occurrence"],"additionalProperties":false}
        }),
        json!({
            "name": "read_older_messages",
            "description": "Read an ordered page of messages in this conversation. Use the last sequence as after to read the next page.",
            "inputSchema": {"type":"object", "properties":{"after":{"type":"integer","minimum":0}}, "additionalProperties":false}
        }),
        json!({
            "name":"message_bot",
        "description":"Ask another Bot without waiting. Briefly tell the person which Bots you are asking and why before calling this tool. Omit thread to open a side thread; its answer continues your originating conversation. After acceptance, give one short confirmation for the group of requests. When replies arrive, answer the person's original question using their actual contents; say which replies are still outstanding and never claim a Bot replied without a received message. The UI shows delivery and reply progress, so do not repeat those status lines. source_message must name a work item offered to this turn. Reuse id and identical arguments after uncertainty.",
            "inputSchema":{"type":"object","properties":{"id":{"type":"string","format":"uuid"},"to":{"type":"string","format":"uuid"},"source_message":{"type":"string","format":"uuid"},"body":{"type":"string"},"thread":{"type":["string","null"],"format":"uuid"},"deadline_ms":{"type":["integer","null"]}},"required":["id","to","source_message","body"],"additionalProperties":false}
        }),
        json!({
            "name":"list_bots",
            "description":"List active Bots in your workspace, with addresses usable by message_bot. Pass next as after for another page.",
            "inputSchema":{"type":"object","properties":{"after":{"type":["string","null"],"format":"uuid"}},"additionalProperties":false}
        }),
    ];
    tools.extend(crate::routines::tools::catalog());
    tools
}

pub(crate) fn execute(
    db: &mut Database,
    secret: &Secret,
    binding: Uuid,
    call: &Call,
    now: i64,
) -> Result<Value, Error> {
    let identity = db.resolve_call(secret, binding, call)?;
    if crate::routines::tools::handles(call.tool()) {
        return crate::routines::tools::execute(db, &identity, call, now);
    }
    match call.tool() {
        "read_routine_result" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                occurrence: Uuid,
                #[serde(default)]
                offset: usize,
            }
            let args: Arguments = serde_json::from_value(call.arguments().clone())
                .map_err(|_| Error::InvalidRequest)?;
            let detail = crate::messages::routine_context::read(
                &db.connection,
                identity.bot,
                identity.thread,
                args.occurrence,
                args.offset,
            )?;
            let response = json!({"occurrence":args.occurrence,"reply":detail.item.reply,"text":detail.text,"offset":detail.offset,"next":detail.next});
            if serde_json::to_vec(&response)
                .map_err(|_| Error::InvalidRequest)?
                .len()
                > crate::limits::MAX_PAYLOAD_BYTES
            {
                return Err(Error::InputTooSmall);
            }
            Ok(response)
        }
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
