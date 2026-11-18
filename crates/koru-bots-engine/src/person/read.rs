// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn bot(c: &Connection, who: &Person, key: Uuid) -> Result<Bot, Error> {
    owner(c, who, key)?;
    let row=c.query_row("SELECT name,settings,access,role_page,knowledge_pages,memory_page,paused,lifecycle,revision,created_at_ms,updated_at_ms,(SELECT id FROM threads WHERE bot=b.id AND kind='main'),avatar,folder_frozen_at_ms,policy_revision,lifecycle_holder,may_ask FROM bots b WHERE id=?1",[key.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Vec<u8>>(4)?,r.get::<_,Option<String>>(5)?,r.get(6)?,r.get::<_,String>(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get::<_,String>(11)?,r.get(12)?,r.get(13)?,r.get(14)?,r.get::<_,Option<Vec<u8>>>(15)?,r.get(16)?)))?;
    Ok(Bot {
        id: key,
        definition: Definition {
            name: row.0,
            avatar: row.12,
            configuration: decode(&row.1)?,
            access: decode(&row.2)?,
            role: row.3.map(id).transpose()?,
            knowledge: decode(&row.4)?,
            memory: row.5.map(id).transpose()?,
        },
        paused: row.6,
        may_ask: row.16,
        lifecycle: lifecycle(row.7)?,
        held_by: row.15.as_deref().map(decode).transpose()?,
        revision: row.8,
        policy_revision: row.14,
        created_at_ms: row.9,
        updated_at_ms: row.10,
        folder_frozen_at_ms: row.13,
        main_thread: id(row.11)?,
    })
}
pub(super) fn thread(c: &Connection, who: &Person, key: Uuid) -> Result<Thread, Error> {
    messages::authorize_thread(c, who, key)?;
    let row=c.query_row("SELECT bot,title,kind,lifecycle,revision,origin_thread,origin_message,lifecycle_holder,created_at_ms,updated_at_ms FROM threads WHERE id=?1",[key.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<Vec<u8>>>(7)?,r.get(8)?,r.get(9)?)))?;
    let settings = crate::configuration::conversation(c, id(row.0.clone())?, key)?;
    Ok(Thread {
        id: key,
        bot: id(row.0)?,
        title: row.1,
        kind: row.2,
        lifecycle: lifecycle(row.3)?,
        held_by: row.7.as_deref().map(decode).transpose()?,
        revision: row.4,
        origin_thread: row.5.map(id).transpose()?,
        origin_message: row.6.map(id).transpose()?,
        created_at_ms: row.8,
        updated_at_ms: row.9,
        settings,
    })
}
pub(super) fn team(c: &Connection, who: &Person) -> Result<Team, Error> {
    Ok(c.query_row(
        "SELECT paused,revision FROM team WHERE owner=?1",
        [&who.owner],
        |r| {
            Ok(Team {
                paused: r.get(0)?,
                revision: r.get(1)?,
            })
        },
    )
    .optional()?
    .unwrap_or(Team {
        paused: false,
        revision: 0,
    }))
}
fn page<T: Serialize>(
    keys: Vec<String>,
    mut get: impl FnMut(Uuid) -> Result<T, Error>,
) -> Result<Value, Error> {
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut last = None;
    let mut more = false;
    for key in keys {
        if items.len() == 64 {
            more = true;
            break;
        }
        let key = id(key)?;
        let item = get(key)?;
        let size = encode(&item)?.len();
        if size > koru_bot_client_wire::MAX_FRAME_BYTES / 2 {
            return Err(Error::InputTooSmall);
        }
        if bytes + size > koru_bot_client_wire::MAX_FRAME_BYTES / 2 {
            more = true;
            break;
        }
        bytes += size;
        items.push(item);
        last = Some(key);
    }
    value(serde_json::json!({"items":items,"next":if more {last}else{None}}))
}
pub(super) fn query(db: &Database, who: &Person, query: Query) -> Result<Value, Error> {
    let c = &db.connection;
    match query {
        Query::ConversationSettings { bot, thread } => {
            owner(c, who, bot)?;
            messages::authorize_thread(c, who, thread)?;
            value(crate::configuration::conversation(c, bot, thread)?)
        }
        Query::ControlReceipt { bot, request } => {
            owner(c, who, bot)?;
            let token: Option<String> = c.query_row("SELECT control_kind FROM receipts WHERE sender=?1 AND id=?2 AND control_bot=?3",
                params![who.receipt_subject(),request.to_string(),bot.to_string()],|r|r.get(0)).optional()?;
            let kind = match token.as_deref().ok_or(Error::NotAllowed)? {
                "pause" => ControlKind::Pause,
                "resume" => ControlKind::Resume,
                "access" => ControlKind::Access,
                _ => return Err(Error::StorageUnavailable),
            };
            value(ControlReceipt { request, bot, kind })
        }
        Query::ChangeReceipt { id, action } => {
            change::authorize(c, who, &action)?;
            let digest = messages::receipts::digest("person-change", &action)?;
            let found = messages::receipts::find::<Value>(c, &who.receipt_subject(), id, &digest)?;
            value(found.map(|answer| Receipt {
                answer,
                replayed: true,
            }))
        }
        Query::Bot { bot: key } => value(bot(c, who, key)?),
        Query::Bots {
            workspace,
            after,
            lifecycle,
        } => {
            let keys=c.prepare("SELECT id FROM bots WHERE owner=?1 AND json_extract(CAST(access AS TEXT),'$.workspace')=?2 AND lifecycle=?3 AND (?4 IS NULL OR id>?4) ORDER BY id LIMIT 65")?.query_map(params![who.owner,workspace.to_string(),lifecycle.token(),after.map(|x|x.to_string())],|r|r.get(0))?.collect::<Result<Vec<String>,_>>()?;
            page(keys, |key| bot(c, who, key))
        }
        Query::Threads {
            bot,
            after,
            lifecycle,
        } => {
            owner(c, who, bot)?;
            let keys=c.prepare("SELECT t.id FROM threads t JOIN participants p ON p.thread=t.id WHERE t.bot=?1 AND p.kind='person' AND p.participant=?2 AND t.lifecycle=?3 AND (?4 IS NULL OR t.id>?4) ORDER BY t.id LIMIT 65")?.query_map(params![bot.to_string(),who.sender,lifecycle.token(),after.map(|x|x.to_string())],|r|r.get(0))?.collect::<Result<Vec<String>,_>>()?;
            page(keys, |key| thread(c, who, key))
        }
        Query::Thread { thread: key } => {
            let thread = thread(c, who, key)?;
            // Terminal history lives in messages. Only current attention/work
            // is projected here, bounded separately from the transcript.
            let turns=c.prepare("SELECT id,bot,state,generation,session,worker_incarnation,details,reply_message,created_at_ms FROM turns t WHERE thread=?1 AND state IN ('running','needs_you','parked','interrupted') AND NOT EXISTS(SELECT 1 FROM turns newer WHERE newer.thread=t.thread AND newer.bot=t.bot AND newer.generation>t.generation) ORDER BY created_at_ms,id LIMIT 64")?.query_map([key.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u64>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Vec<u8>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,i64>(8)?)))?.map(|row|{let row=row?; Ok(ActiveTurn {id:id(row.0)?,bot:id(row.1)?,state:row.2,generation:row.3,session:row.4.map(id).transpose()?,worker:row.5.map(id).transpose()?,details:decode::<Value>(&row.6)?,reply_message:row.7.map(id).transpose()?,created_at_ms:row.8})}).collect::<Result<Vec<_>,Error>>()?;
            let waiting: u64 = c.query_row(
                "SELECT count(*) FROM inbox i JOIN obligations o ON o.id=i.id WHERE i.thread=?1 AND o.state='owed'",
                [key.to_string()],
                |r| r.get(0),
            )?;
            let head_sequence = c.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE thread=?1",
                [key.to_string()],
                |r| r.get(0),
            )?;
            value(ThreadView {
                origin: thread.origin_thread.map(|origin| {
                    let origin= self::thread(c,who,origin)?;
                    let bot=self::bot(c,who,origin.bot)?;
                    Ok::<_,Error>(Origin{thread:origin.id,bot:bot.id,name:bot.definition.name,title:origin.title,main:origin.kind=="main"})
                }).transpose()?,
                joined: c.query_row("SELECT EXISTS(SELECT 1 FROM participants WHERE thread=?1 AND participant=?2 AND kind='person')",params![key.to_string(),who.sender],|r|r.get(0))?,
                participants: c.prepare("SELECT b.id,b.name FROM participants p JOIN bots b ON b.id=p.participant WHERE p.thread=?1 AND p.kind='bot' ORDER BY b.id")?.query_map([key.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.map(|row|{let (bot,name)=row?;Ok(Participant{bot:id(bot)?,name})}).collect::<Result<_,Error>>()?,
                thread,
                turns,
                waiting_messages: waiting,
                head_sequence,
            })
        }
        Query::History { thread, after } => value(db.read_messages(who, thread, after)?),
        Query::RecentHistory { thread, before } => {
            value(db.read_recent_messages(who, thread, before)?)
        }
        Query::Questions { thread } => value(db.read_questions(who, thread)?),
        Query::OperationAttention { thread, after } => {
            value(db.operation_attention(who, thread, after)?)
        }
        Query::Cards { thread, after } => cards::read(c, who, thread, after),
        Query::Outputs { thread, after } => {
            messages::authorize_thread(c, who, thread)?;
            let keys=c.prepare("SELECT id FROM outputs WHERE thread=?1 AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT 65")?.query_map(params![thread.to_string(),after.map(|x|x.to_string())],|r|r.get(0))?.collect::<Result<Vec<String>,_>>()?;
            page(keys, |key| {
                let row = c.query_row(
                    "SELECT message,operation,kind,reference FROM outputs WHERE id=?1",
                    [key.to_string()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Vec<u8>>(3)?,
                        ))
                    },
                )?;
                value(
                    serde_json::json!({"id":key,"message":row.0,"operation":row.1,"kind":row.2,"reference":decode::<Value>(&row.3)?}),
                )
            })
        }
        Query::Team => value(team(c, who)?),
        Query::Conversations { bot, after, limit } => {
            lists::conversations(c, who, bot, after, limit)
        }
        Query::TrashedConversations {
            workspace,
            after,
            limit,
        } => lists::trashed(c, who, workspace, after, limit),
        Query::Activity { workspace } => lists::activity(c, who, workspace),
        Query::LifecycleOperation { bot, request } => {
            value(lifecycle_ops::read(c, who, bot, request)?)
        }
    }
}
