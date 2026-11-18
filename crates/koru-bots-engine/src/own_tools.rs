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
    vec![json!({
        "name": "read_older_messages",
        "description": "Read an ordered page of messages in this conversation. Use the last sequence as after to read the next page.",
        "inputSchema": {"type":"object", "properties":{"after":{"type":"integer","minimum":0}}, "additionalProperties":false}
    })]
}

pub(crate) fn execute(
    db: &mut Database,
    secret: &Secret,
    binding: Uuid,
    call: &Call,
) -> Result<Value, Error> {
    let identity = db.resolve_call(secret, binding, call)?;
    match call.tool() {
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
