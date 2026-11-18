// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{Connection, params};

fn name(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() || text.chars().count() > 256 {
        Err(Error::InvalidRequest)
    } else {
        Ok(())
    }
}
fn pages(role: Option<Uuid>, knowledge: &[Uuid], memory: Option<Uuid>) -> Result<(), Error> {
    if knowledge.len() > 16
        || role
            .iter()
            .chain(knowledge)
            .chain(memory.iter())
            .any(Uuid::is_nil)
        || knowledge
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != knowledge.len()
    {
        Err(Error::InvalidRequest)
    } else {
        Ok(())
    }
}
fn policy(access: &CapturedBotPolicy) -> Result<(), Error> {
    if !access.valid()
        || access.workspace.as_uuid().is_nil()
        || access.selected_pages.iter().any(|id| id.as_uuid().is_nil())
        || !access.connections.is_empty()
    {
        Err(Error::InvalidRequest)
    } else {
        Ok(())
    }
}
fn authorize(c: &Connection, who: &Person, action: &Change) -> Result<(), Error> {
    match action {
        Change::Create { bot, .. } => {
            let other: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND owner<>?2)",
                params![bot.to_string(), who.owner],
                |r| r.get(0),
            )?;
            if other {
                Err(Error::NotAllowed)
            } else {
                Ok(())
            }
        }
        Change::Configure { bot, .. }
        | Change::Library { bot, .. }
        | Change::Access { bot, .. }
        | Change::Pause { bot, .. }
        | Change::BotLifecycle { bot, .. }
        | Change::Conversation { bot, .. } => owner(c, who, *bot),
        Change::ThreadLifecycle { thread, .. }
        | Change::RenameThread { thread, .. }
        | Change::RetrySetup { thread, .. } => messages::authorize_thread(c, who, *thread),
        Change::Team { .. } => Ok(()),
    }
}
fn revision(
    c: &Connection,
    who: &Person,
    key: Uuid,
    expected: u64,
    active: bool,
) -> Result<Bot, Error> {
    let bot = read::bot(c, who, key)?;
    if bot.revision != expected {
        return Err(Error::Conflict);
    }
    if active && bot.lifecycle != Lifecycle::Active {
        return Err(Error::NotAllowed);
    }
    Ok(bot)
}
fn updated(c: &Connection, bot: Uuid, now: i64) -> Result<(), Error> {
    c.execute(
        "UPDATE bots SET revision=revision+1,updated_at_ms=?2 WHERE id=?1",
        params![bot.to_string(), now],
    )?;
    Ok(())
}
pub(super) fn conversation(
    c: &Connection,
    who: &Person,
    bot: Uuid,
    kind: &str,
    title: &str,
    now: i64,
) -> Result<Uuid, Error> {
    let thread = Uuid::new_v4();
    c.execute("INSERT INTO threads(id,bot,kind,title,lifecycle,created_at_ms) VALUES (?1,?2,?3,?4,'active',?5)",params![thread.to_string(),bot.to_string(),kind,title,now])?;
    c.execute(
        "INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'bot'),(?1,?3,'person')",
        params![thread.to_string(), bot.to_string(), who.sender],
    )?;
    Ok(thread)
}
/// Archive/trash cancel queued work as well as delivered work. Messages and
/// operation evidence remain; restore never reconstructs an inbox.
fn retire(c: &Connection, bot: Option<Uuid>, thread: Option<Uuid>) -> Result<(), Error> {
    let bot = bot.map(|id| id.to_string());
    let thread = thread.map(|id| id.to_string());
    c.execute("UPDATE obligations SET state='cancelled' WHERE state IN ('owed','delivered') AND (?1 IS NULL OR bot=?1) AND (?2 IS NULL OR thread=?2)",params![bot,thread])?;
    c.execute(
        "DELETE FROM inbox WHERE (?1 IS NULL OR bot=?1) AND (?2 IS NULL OR thread=?2)",
        params![bot, thread],
    )?;
    c.execute("UPDATE turns SET state='stopped',stopped=1 WHERE state IN ('queued','running','needs_you','parked','interrupted') AND (?1 IS NULL OR bot=?1) AND (?2 IS NULL OR thread=?2)",params![bot,thread])?;
    c.execute("UPDATE credentials SET revoked=1 WHERE (?1 IS NULL OR bot=?1) AND (?2 IS NULL OR thread=?2)",params![bot,thread])?;
    c.execute("UPDATE questions SET state='cancelled' WHERE state='open' AND turn IN (SELECT id FROM turns WHERE stopped=1 AND (?1 IS NULL OR bot=?1) AND (?2 IS NULL OR thread=?2))",params![bot,thread])?;
    Ok(())
}
pub(super) fn apply(
    db: &mut Database,
    who: &Person,
    command: Uuid,
    action: &Change,
    now: i64,
) -> Result<Receipt<Value>, Error> {
    if command.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let digest = messages::receipts::digest("person-change", action)?;
    let subject = who.receipt_subject();
    let tx = db.connection.transaction()?;
    authorize(&tx, who, action)?;
    if let Some(answer) = messages::receipts::find(&tx, &subject, command, &digest)? {
        return Ok(Receipt {
            answer,
            replayed: true,
        });
    }
    let answer = match action {
        Change::Create { bot, definition: d } => {
            if bot.is_nil() {
                return Err(Error::InvalidRequest);
            }
            name(&d.name)?;
            pages(d.role, &d.knowledge, d.memory)?;
            policy(&d.access)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1)",
                [bot.to_string()],
                |r| r.get(0),
            )?;
            if exists {
                return Err(Error::Conflict);
            }
            tx.execute("INSERT INTO bots(id,owner,name,settings,access,role_page,knowledge_pages,memory_page,visibility,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'private',?9,?9)",params![bot.to_string(),who.owner,d.name,encode(&d.configuration)?,encode(&d.access)?,d.role.map(|id|id.to_string()),encode(&d.knowledge)?,d.memory.map(|id|id.to_string()),now])?;
            conversation(&tx, who, *bot, "main", "", now)?;
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Configure {
            bot,
            revision: rev,
            name: label,
            configuration,
        } => {
            revision(&tx, who, *bot, *rev, true)?;
            name(label)?;
            tx.execute(
                "UPDATE bots SET name=?2,settings=?3 WHERE id=?1",
                params![bot.to_string(), label, encode(configuration)?],
            )?;
            updated(&tx, *bot, now)?;
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Library {
            bot,
            revision: rev,
            role,
            knowledge,
            memory,
        } => {
            revision(&tx, who, *bot, *rev, true)?;
            pages(*role, knowledge, *memory)?;
            tx.execute(
                "UPDATE bots SET role_page=?2,knowledge_pages=?3,memory_page=?4 WHERE id=?1",
                params![
                    bot.to_string(),
                    role.map(|id| id.to_string()),
                    encode(knowledge)?,
                    memory.map(|id| id.to_string())
                ],
            )?;
            updated(&tx, *bot, now)?;
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Access {
            bot,
            revision: rev,
            access,
        } => {
            let prior = revision(&tx, who, *bot, *rev, true)?;
            policy(access)?;
            if prior.definition.access.workspace != access.workspace {
                return Err(Error::NotAllowed);
            }
            tx.execute(
                "UPDATE bots SET access=?2 WHERE id=?1",
                params![bot.to_string(), encode(access)?],
            )?;
            updated(&tx, *bot, now)?;
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Pause {
            bot,
            revision: rev,
            paused,
        } => {
            let prior = revision(&tx, who, *bot, *rev, true)?;
            if prior.paused != *paused {
                tx.execute(
                    "UPDATE bots SET paused=?2 WHERE id=?1",
                    params![bot.to_string(), paused],
                )?;
                updated(&tx, *bot, now)?;
            }
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Team { revision, paused } => {
            let prior = read::team(&tx, who)?;
            if prior.revision != *revision {
                return Err(Error::Conflict);
            }
            if prior.paused != *paused {
                tx.execute("INSERT INTO team(owner,paused,revision) VALUES (?1,?2,?3) ON CONFLICT(owner) DO UPDATE SET paused=excluded.paused,revision=excluded.revision",params![who.owner,paused,revision+1])?;
            }
            value(read::team(&tx, who)?)?
        }
        Change::BotLifecycle {
            bot,
            revision: rev,
            lifecycle,
        } => {
            let prior = revision(&tx, who, *bot, *rev, false)?;
            if prior.lifecycle != *lifecycle {
                if *lifecycle != Lifecycle::Active {
                    retire(&tx, Some(*bot), None)?;
                }
                tx.execute(
                    "UPDATE bots SET lifecycle=?2 WHERE id=?1",
                    params![bot.to_string(), lifecycle.token()],
                )?;
                updated(&tx, *bot, now)?;
            }
            value(read::bot(&tx, who, *bot)?)?
        }
        Change::Conversation { bot, title } => {
            messages::authorize_bot(&tx, who, *bot)?;
            if title.chars().count() > 256 {
                return Err(Error::InvalidRequest);
            }
            let thread = conversation(&tx, who, *bot, "conversation", title, now)?;
            let main = read::bot(&tx, who, *bot)?.main_thread;
            let content = messages::Content {
                body: String::new(),
                attachments: vec![],
                references: vec![serde_json::json!({"thread":thread})],
                in_reply_to: None,
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            };
            tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'thread_reference',?5,'published',?6)",params![Uuid::new_v4().to_string(),main.to_string(),messages::next_sequence(&tx,main)?,who.sender,encode(&content)?,now])?;
            value(read::thread(&tx, who, thread)?)?
        }
        Change::ThreadLifecycle {
            thread,
            revision,
            lifecycle,
        } => {
            let prior = read::thread(&tx, who, *thread)?;
            if prior.revision != *revision {
                return Err(Error::Conflict);
            }
            if prior.kind == "main" {
                return Err(Error::NotAllowed);
            }
            if prior.lifecycle != *lifecycle {
                if *lifecycle != Lifecycle::Active {
                    retire(&tx, None, Some(*thread))?;
                }
                tx.execute(
                    "UPDATE threads SET lifecycle=?2,revision=revision+1 WHERE id=?1",
                    params![thread.to_string(), lifecycle.token()],
                )?;
            }
            value(read::thread(&tx, who, *thread)?)?
        }
        Change::RenameThread {
            thread,
            revision,
            title,
        } => {
            name(title)?;
            let prior = read::thread(&tx, who, *thread)?;
            if prior.revision != *revision {
                return Err(Error::Conflict);
            }
            tx.execute(
                "UPDATE threads SET title=?2,revision=revision+1 WHERE id=?1",
                params![thread.to_string(), title],
            )?;
            value(read::thread(&tx, who, *thread)?)?
        }
        Change::RetrySetup { thread, turn } => {
            let current = read::thread(&tx, who, *thread)?;
            if current.lifecycle != Lifecycle::Active {
                return Err(Error::NotAllowed);
            }
            messages::authorize_bot(&tx, who, current.bot)?;
            let changed=tx.execute("UPDATE turns SET state='interrupted' WHERE id=?1 AND thread=?2 AND state='needs_you' AND stopped=0 AND json_extract(CAST(details AS TEXT),'$.setup_failure') IS NOT NULL AND NOT EXISTS(SELECT 1 FROM turns newer WHERE newer.thread=turns.thread AND newer.bot=turns.bot AND newer.generation>turns.generation)",params![turn.to_string(),thread.to_string()])?;
            if changed != 1 {
                return Err(Error::StaleTurn);
            }
            serde_json::json!({"turn":turn})
        }
    };
    messages::receipts::save(&tx, &subject, command, &digest, &answer, now)?;
    tx.commit()?;
    Ok(Receipt {
        answer,
        replayed: false,
    })
}
