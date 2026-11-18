// SPDX-License-Identifier: MPL-2.0
//! Bot asks are durable messages, never held provider calls. All helpers run
//! inside the caller's engine transaction and perform no external work.
use super::*;
use crate::authority::Resolution;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ask {
    pub id: Uuid,
    pub to: Uuid,
    /// Work offered to this turn, not an arbitrary context message.
    pub source_message: Uuid,
    pub body: String,
    /// Omitted opens a side thread; supplied must be the caller's own thread.
    pub thread: Option<Uuid>,
    pub deadline_ms: Option<i64>,
}

pub(crate) fn chain(c: &Connection, thread: Uuid) -> Result<Uuid, Error> {
    let found: Option<String> = c
        .query_row(
            "SELECT chain FROM thread_chains WHERE thread=?1",
            [thread.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(found) = found {
        return id(found);
    }
    c.execute(
        "INSERT INTO bot_chains(id) VALUES (?1)",
        [thread.to_string()],
    )?;
    c.execute(
        "INSERT INTO thread_chains(thread,chain) VALUES (?1,?1)",
        [thread.to_string()],
    )?;
    Ok(thread)
}

pub(crate) fn person_message(c: &Connection, thread: Uuid) -> Result<(), Error> {
    let chain = chain(c, thread)?;
    c.execute(
        "UPDATE bot_chains SET messages=0 WHERE id=?1",
        [chain.to_string()],
    )?;
    // A person's intervention starts a new automatic exchange, while child
    // threads retain the same accounting owner and cannot reset it themselves.
    c.execute("DELETE FROM chain_bots WHERE chain=?1", [chain.to_string()])?;
    Ok(())
}

pub(crate) fn permitted(c: &Connection, from: Uuid, to: Uuid) -> Result<(), Error> {
    let allowed: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM bots a JOIN bots b ON b.owner=a.owner WHERE a.id=?1 AND b.id=?2 AND a.lifecycle='active' AND b.lifecycle='active' AND json_extract(CAST(a.access AS TEXT),'$.workspace')=json_extract(CAST(b.access AS TEXT),'$.workspace'))",
        params![from.to_string(),to.to_string()], |r| r.get(0))?;
    if allowed {
        Ok(())
    } else {
        Err(Error::NotAllowed)
    }
}

pub(crate) fn participant(c: &Connection, thread: Uuid, bot: Uuid) -> Result<(), Error> {
    if c.execute(
        "INSERT OR IGNORE INTO participants(thread,participant,kind) VALUES (?1,?2,'bot')",
        params![thread.to_string(), bot.to_string()],
    )? > 0
    {
        crate::configuration::seed(c, bot, thread)?;
    }
    Ok(())
}

pub(crate) fn wake(
    c: &Connection,
    thread: Uuid,
    message: Uuid,
    bot: Uuid,
    expired: bool,
    now: i64,
) -> Result<(), Error> {
    let obligation = Uuid::new_v4();
    c.execute("INSERT INTO obligations(id,thread,message,bot,state,accepted_at_ms) VALUES (?1,?2,?3,?4,?5,?6)",
        params![obligation.to_string(),thread.to_string(),message.to_string(),bot.to_string(),if expired {"expired"} else {"owed"},now])?;
    if !expired {
        c.execute(
            "INSERT INTO inbox(id,thread,message,bot,accepted_at_ms) VALUES (?1,?2,?3,?4,?5)",
            params![
                obligation.to_string(),
                thread.to_string(),
                message.to_string(),
                bot.to_string(),
                now
            ],
        )?;
    }
    c.execute(
        "UPDATE bots SET folder_frozen_at_ms=COALESCE(folder_frozen_at_ms,?2) WHERE id=?1",
        params![bot.to_string(), now],
    )?;
    Ok(())
}

pub(crate) fn reference(
    c: &Connection,
    thread: Uuid,
    child: Uuid,
    sender: Uuid,
    now: i64,
) -> Result<(), Error> {
    if thread == child {
        return Ok(());
    }
    let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE thread=?1 AND kind='thread_reference' AND json_extract(CAST(payload AS TEXT),'$.references[0].thread')=?2)",
        params![thread.to_string(),child.to_string()], |r| r.get(0))?;
    if !exists {
        let content = Content {
            body: String::new(),
            attachments: vec![],
            references: vec![json!({"thread":child})],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        };
        c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'thread_reference',?5,'published',?6)",
            params![Uuid::new_v4().to_string(),thread.to_string(),next_sequence(c,thread)?,sender.to_string(),encode(&content)?,now])?;
    }
    Ok(())
}

pub(crate) fn ask(
    db: &mut Database,
    identity: &Resolution,
    request: &Ask,
    now: i64,
) -> Result<Value, Error> {
    if request.id.is_nil()
        || request.to.is_nil()
        || request.source_message.is_nil()
        || request.body.trim().is_empty()
        || encode(request)?.len() > crate::person::MAX_COMMAND_BYTES
        || request.to == identity.bot
        || request.thread.is_some_and(|t| t != identity.thread)
    {
        return Err(Error::InvalidRequest);
    }
    let subject = format!("bot:{}:{}", identity.bot, identity.thread);
    let digest = receipts::digest("message-bot", request)?;
    let tx = db.connection.transaction()?;
    let row = turns::turn_row(&tx, identity.binding.turn)?;
    if row.2 != "running"
        || row.0 != identity.thread.to_string()
        || row.1 != identity.bot.to_string()
    {
        return Err(Error::StaleTurn);
    }
    if let Some(answer) = receipts::find::<Value>(&tx, &subject, request.id, &digest)? {
        return Ok(answer);
    }
    permitted(&tx, identity.bot, request.to)?;
    let may_ask: bool = tx.query_row(
        "SELECT may_ask FROM bots WHERE id=?1",
        [identity.bot.to_string()],
        |r| r.get(0),
    )?;
    if !may_ask {
        return Err(Error::NotAllowed);
    }
    // Tool arrival may beat the provider's acceptance observation. As with a
    // native write, a bound effect may start from offered work; it does not
    // turn that delivery into accepted evidence or answer its obligations.
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM obligations o JOIN delivery_obligations x ON x.obligation=o.id JOIN deliveries d ON d.id=x.delivery WHERE o.message=?1 AND o.bot=?2 AND o.thread=?3 AND o.state='delivered' AND d.turn=?4 AND d.state IN ('offered','accepted'))",
        params![request.source_message.to_string(),identity.bot.to_string(),identity.thread.to_string(),identity.binding.turn.id.to_string()], |r| r.get(0))?;
    if !valid {
        return Err(Error::NotAllowed);
    }
    let chain = chain(&tx, identity.thread)?;
    let count: u64 = tx.query_row(
        "SELECT messages FROM bot_chains WHERE id=?1",
        [chain.to_string()],
        |r| r.get(0),
    )?;
    let bots: u64 = tx.query_row("SELECT count(*) FROM (SELECT bot FROM chain_bots WHERE chain=?1 UNION SELECT ?2 UNION SELECT ?3)",
        params![chain.to_string(),identity.bot.to_string(),request.to.to_string()], |r| r.get(0))?;
    let recipients: u64 = tx.query_row(
        "SELECT count(*) FROM (SELECT bot FROM turn_recipients WHERE turn=?1 UNION SELECT ?2)",
        params![identity.binding.turn.id.to_string(), request.to.to_string()],
        |r| r.get(0),
    )?;
    if count >= 20 || bots > 8 || recipients > 3 {
        limit_notice(&tx, chain, identity.bot, identity.binding.turn, now)?;
        let answer = json!({"status":"refused","code":"bot_chain_limit"});
        receipts::save(&tx, &subject, request.id, &digest, &answer, now)?;
        tx.commit()?;
        return Ok(answer);
    }
    let thread = if let Some(thread) = request.thread {
        thread
    } else {
        let thread = Uuid::new_v4();
        tx.execute("INSERT INTO threads(id,bot,kind,title,lifecycle,origin_thread,origin_message,created_at_ms,updated_at_ms) VALUES (?1,?2,'between_bots','','active',?3,?4,?5,?5)",
            params![thread.to_string(),identity.bot.to_string(),identity.thread.to_string(),request.source_message.to_string(),now])?;
        tx.execute(
            "INSERT INTO thread_chains(thread,chain) VALUES (?1,?2)",
            params![thread.to_string(), chain.to_string()],
        )?;
        participant(&tx, thread, identity.bot)?;
        thread
    };
    participant(&tx, thread, request.to)?;
    let (source_thread, source): (String, Vec<u8>) = tx.query_row(
        "SELECT thread,payload FROM messages WHERE id=?1",
        [request.source_message.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let source: Content = decode(&source)?;
    let deadline = match (source.deadline_ms, request.deadline_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let message = Uuid::new_v4();
    let content = Content {
        body: request.body.clone(),
        attachments: vec![],
        references: vec![
            json!({"kind":"origin","thread":identity.thread,"message":request.source_message}),
        ],
        in_reply_to: if thread.to_string() == source_thread {
            Some(request.source_message)
        } else {
            None
        },
        deadline_ms: deadline,
        reply_to: ReplyTo::Thread,
    };
    let sequence = next_sequence(&tx, thread)?;
    tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'words',?5,'accepted',?6)",
        params![message.to_string(),thread.to_string(),sequence,identity.bot.to_string(),encode(&content)?,now])?;
    tx.execute("INSERT INTO bot_dispatches(message,origin_thread,origin_message,origin_turn,sender,recipient) VALUES (?1,?2,?3,?4,?5,?6)",
        params![message.to_string(),identity.thread.to_string(),request.source_message.to_string(),identity.binding.turn.id.to_string(),identity.bot.to_string(),request.to.to_string()])?;
    tx.execute(
        "INSERT OR IGNORE INTO turn_recipients(turn,bot) VALUES (?1,?2)",
        params![identity.binding.turn.id.to_string(), request.to.to_string()],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO chain_bots(chain,bot) VALUES (?1,?2),(?1,?3)",
        params![
            chain.to_string(),
            identity.bot.to_string(),
            request.to.to_string()
        ],
    )?;
    tx.execute(
        "UPDATE bot_chains SET messages=messages+1 WHERE id=?1",
        [chain.to_string()],
    )?;
    wake(
        &tx,
        thread,
        message,
        request.to,
        deadline.is_some_and(|d| now > d),
        now,
    )?;
    reference(&tx, identity.thread, thread, identity.bot, now)?;
    let main: String = tx.query_row(
        "SELECT id FROM threads WHERE bot=?1 AND kind='main'",
        [identity.bot.to_string()],
        |r| r.get(0),
    )?;
    reference(&tx, id(main)?, thread, identity.bot, now)?;
    let answer = json!({"status":"accepted","message":message,"thread":thread,"sequence":sequence});
    receipts::save(&tx, &subject, request.id, &digest, &answer, now)?;
    tx.commit()?;
    Ok(answer)
}

/// Called after marking accepted obligations answered, before committing
/// Publish. Continuations point to the single stored side-thread answer.
pub(crate) fn published(
    c: &Connection,
    turn: Turn,
    message: Uuid,
    thread: Uuid,
    now: i64,
) -> Result<(), Error> {
    let chain = chain(c, thread)?;
    c.execute(
        "UPDATE bot_chains SET messages=messages+1 WHERE id=?1",
        [chain.to_string()],
    )?;
    let count: u64 = c.query_row(
        "SELECT messages FROM bot_chains WHERE id=?1",
        [chain.to_string()],
        |r| r.get(0),
    )?;
    if count > 20 {
        let bot: String = c.query_row(
            "SELECT bot FROM turns WHERE id=?1",
            [turn.id.to_string()],
            |r| r.get(0),
        )?;
        limit_notice(c, chain, id(bot)?, turn, now)?;
        return Ok(());
    }
    let targets: Vec<(String,String)> = c.prepare("SELECT DISTINCT a.origin_thread,a.sender FROM bot_dispatches a JOIN obligations o ON o.message=a.message AND o.bot=a.recipient JOIN turns t ON t.id=a.origin_turn JOIN threads h ON h.id=a.origin_thread JOIN messages m ON m.id=a.message WHERE o.answered_by=?1 AND t.stopped=0 AND h.lifecycle='active' AND (json_extract(CAST(m.payload AS TEXT),'$.deadline_ms') IS NULL OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms')>=?2) AND EXISTS(SELECT 1 FROM delivery_obligations x JOIN deliveries d ON d.id=x.delivery WHERE x.obligation=o.id AND d.turn=?3 AND d.state='accepted')")?
        .query_map(params![message.to_string(),now,turn.id.to_string()], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
    for (origin, asker) in targets {
        let origin = id(origin)?;
        let asker = id(asker)?;
        // A Bot explicitly trashed or archived cannot be revived by an answer.
        let active: bool = c.query_row(
            "SELECT lifecycle='active' FROM bots WHERE id=?1",
            [asker.to_string()],
            |r| r.get(0),
        )?;
        if active {
            wake(c, origin, message, asker, false, now)?;
        }
    }
    Ok(())
}

fn limit_notice(
    c: &Connection,
    thread: Uuid,
    bot: Uuid,
    turn: Turn,
    now: i64,
) -> Result<(), Error> {
    let exists: bool=c.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE thread=?1 AND kind='setup_note' AND json_extract(CAST(payload AS TEXT),'$.references[0].notice')='bot_chain_limit' AND json_extract(CAST(payload AS TEXT),'$.references[0].turn')=?2)",params![thread.to_string(),turn.id.to_string()],|r|r.get(0))?;
    if !exists {
        let content = Content {
            body: String::new(),
            attachments: vec![],
            references: vec![json!({"notice":"bot_chain_limit","turn":turn.id})],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        };
        c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'setup_note',?5,'published',?6)", params![Uuid::new_v4().to_string(),thread.to_string(),next_sequence(c,thread)?,bot.to_string(),encode(&content)?,now])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
