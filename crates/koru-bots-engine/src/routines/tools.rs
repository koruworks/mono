// SPDX-License-Identifier: MPL-2.0
//! Bot-authored definitions use the live arrival binding and engine receipts.
//! New definitions start on with narrow access; later enable/pause and access changes remain person actions.
use super::*;
use crate::authority::{Call, Resolution};
use serde_json::{Value, json};

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "create_routine" | "edit_routine" | "list_routines" | "run_routine"
    )
}

pub fn catalog() -> Vec<Value> {
    let rule = json!({"oneOf":[
        {"type":"object","properties":{"type":{"const":"floating"},"cron":{"type":"string"}},"required":["type","cron"],"additionalProperties":false},
        {"type":"object","properties":{"type":{"const":"zoned"},"cron":{"type":"string"},"zone":{"type":"string"}},"required":["type","cron","zone"],"additionalProperties":false}
    ]});
    vec![
        json!({"name":"create_routine","description":"Create an enabled Routine for yourself with default narrow access. Use a five-field cron and either the device's floating time or an explicit IANA zone. Obtain expected_bot_revision from list_routines. Reuse id and identical arguments after uncertainty. Explain the timing and task. The person can review timing, pause it or change access in Routines; do not broaden its access or restart an existing paused Routine.","inputSchema":{"type":"object","properties":{"id":{"type":"string","format":"uuid"},"expected_bot_revision":{"type":"integer","minimum":1},"name":{"type":"string"},"objective":{"type":"string"},"recurrence":rule},"required":["id","expected_bot_revision","name","objective","recurrence"],"additionalProperties":false}}),
        json!({"name":"edit_routine","description":"Update your Routine's name, objective and timing using the revision from list_routines. Its on/off setting and separate access grant stay unchanged. Reuse id and identical arguments after uncertainty.","inputSchema":{"type":"object","properties":{"id":{"type":"string","format":"uuid"},"routine":{"type":"string","format":"uuid"},"revision":{"type":"integer","minimum":1},"name":{"type":"string"},"objective":{"type":"string"},"recurrence":rule},"required":["id","routine","revision","name","objective","recurrence"],"additionalProperties":false}}),
        json!({"name":"list_routines","description":"List your Routines, their exact revisions, timing and on/off state. Pass next as after for another page.","inputSchema":{"type":"object","properties":{"after":{"type":["string","null"],"format":"uuid"}},"additionalProperties":false}}),
        json!({"name":"run_routine","description":"Request one run of your existing Routine using its revision from list_routines, without changing its on/off setting or access. Reuse id and identical arguments after uncertainty. The returned occurrence is the durable queued or skipped receipt, not a completed result. Queued work waits for capacity: briefly acknowledge its actual state and finish this main turn. Do not perform the Routine objective here or poll while holding capacity. Results appear in its existing conversation and occurrence cards.","inputSchema":{"type":"object","properties":{"id":{"type":"string","format":"uuid"},"routine":{"type":"string","format":"uuid"},"revision":{"type":"integer","minimum":1}},"required":["id","routine","revision"],"additionalProperties":false}}),
    ]
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Mutation {
    RunNow {
        id: Uuid,
        routine: Uuid,
        revision: u32,
    },
    Create {
        id: Uuid,
        expected_bot_revision: u64,
        name: String,
        objective: String,
        recurrence: RecurrenceRule,
    },
    Edit {
        id: Uuid,
        routine: Uuid,
        revision: u32,
        name: String,
        objective: String,
        recurrence: RecurrenceRule,
    },
}

fn metadata(c: &Connection, bot: Uuid) -> Result<(String, String, u64), Error> {
    c.query_row("SELECT b.owner,p.participant,b.revision FROM bots b JOIN threads t ON t.bot=b.id AND t.kind='main' JOIN participants p ON p.thread=t.id AND p.kind='person' WHERE b.id=?1 AND b.lifecycle='active'",[bot.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))
        .optional()?.ok_or(Error::NotAllowed)
}

pub fn execute(
    db: &mut Database,
    identity: &Resolution,
    call: &Call,
    now: i64,
) -> Result<Value, Error> {
    let row = messages::turns::turn_row(&db.connection, identity.binding.turn)?;
    if row.2 != "running"
        || row.0 != identity.thread.to_string()
        || row.1 != identity.bot.to_string()
    {
        return Err(Error::StaleTurn);
    }
    let (owner, sender, bot_revision) = metadata(&db.connection, identity.bot)?;
    if call.tool() == "list_routines" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            after: Option<Uuid>,
        }
        let args: Arguments =
            serde_json::from_value(call.arguments().clone()).map_err(|_| Error::InvalidRequest)?;
        let page = db.routines(
            &Person { owner, sender },
            identity.bot,
            args.after,
            PAGE_LIMIT,
        )?;
        return Ok(json!({"bot_revision":bot_revision,"items":page.items,"next":page.next}));
    }
    let mut arguments = call.arguments().clone();
    let action = match call.tool() {
        "create_routine" => "create",
        "edit_routine" => "edit",
        "run_routine" => "run_now",
        _ => return Err(Error::NotAllowed),
    };
    let object = arguments.as_object_mut().ok_or(Error::InvalidRequest)?;
    if object.contains_key("action") {
        return Err(Error::InvalidRequest);
    }
    object.insert("action".into(), json!(action));
    let request: Mutation = serde_json::from_value(arguments).map_err(|_| Error::InvalidRequest)?;
    let (command, change) = match request {
        Mutation::RunNow {
            id,
            routine,
            revision,
        } => (
            id,
            Change::RunNow {
                bot: identity.bot,
                routine,
                revision,
            },
        ),
        Mutation::Create {
            id,
            expected_bot_revision,
            name,
            objective,
            recurrence,
        } => (
            id,
            Change::Create {
                bot: identity.bot,
                expected_bot_revision,
                name,
                objective,
                recurrence,
                documents: vec![],
            },
        ),
        Mutation::Edit {
            id,
            routine,
            revision,
            name,
            objective,
            recurrence,
        } => (
            id,
            Change::Edit {
                bot: identity.bot,
                routine,
                revision,
                name,
                objective,
                recurrence,
            },
        ),
    };
    if command.is_nil() || encode(&change)?.len() > crate::person::MAX_COMMAND_BYTES {
        return Err(Error::InvalidRequest);
    }
    let subject = format!("bot:{}:{}", identity.bot, identity.thread);
    let digest = messages::receipts::digest(call.tool(), call.arguments())?;
    let tx = db.connection.transaction()?;
    if let Some(answer) = messages::receipts::find::<Value>(&tx, &subject, command, &digest)? {
        return Ok(answer);
    }
    let changed = control::apply(&tx, &owner, &sender, command, &change, now)?;
    let answer = if call.tool() == "run_routine" {
        serde_json::to_value(changed).map_err(|_| Error::InvalidRequest)?
    } else {
        json!({"routine":changed.routine})
    };
    messages::receipts::save(&tx, &subject, command, &digest, &answer, now)?;
    tx.commit()?;
    Ok(answer)
}
