// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn create(
    c: &Connection,
    who: &Person,
    origin: Uuid,
    branch: Uuid,
    through: Uuid,
    title: &str,
    now: i64,
) -> Result<(), Error> {
    if branch.is_nil() || through.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let thread = read::thread(c, who, origin)?;
    messages::authorize_bot(c, who, thread.bot)?;
    let sequence: Option<u64> = c.query_row("SELECT sequence FROM messages WHERE id=?1 AND thread=?2 AND kind='words' AND sender<>'engine' AND sender NOT IN (SELECT participant FROM participants WHERE thread=?2 AND kind='person') AND state IN ('published','stopped')",params![through.to_string(),origin.to_string()],|r|r.get(0)).optional()?;
    let sequence = sequence.ok_or(Error::NotAllowed)?;
    let used: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1)",
        [branch.to_string()],
        |r| r.get(0),
    )?;
    if used {
        return Err(Error::Conflict);
    }
    c.execute("INSERT INTO threads(id,bot,kind,title,lifecycle,origin_thread,origin_message,created_at_ms,updated_at_ms) VALUES (?1,?2,'branch',?3,'active',?4,?5,?6,?6)",params![branch.to_string(),thread.bot.to_string(),title,origin.to_string(),through.to_string(),now])?;
    c.execute("INSERT INTO participants(thread,participant,kind) SELECT ?1,participant,kind FROM participants WHERE thread=?2",params![branch.to_string(),origin.to_string()])?;
    c.execute(
        "INSERT OR IGNORE INTO participants(thread,participant,kind) VALUES (?1,?2,'person')",
        params![branch.to_string(), who.sender],
    )?;
    c.execute("INSERT INTO conversation_settings(thread,bot,revision,settings) SELECT ?1,bot,1,settings FROM conversation_settings WHERE thread=?2",params![branch.to_string(),origin.to_string()])?;
    let rows = c.prepare("SELECT id,sequence,sender,kind,payload,state,accepted_at_ms,COALESCE((SELECT source FROM message_copies WHERE message=m.id),id) FROM messages m WHERE thread=?1 AND sequence<=?2 ORDER BY sequence")?.query_map(params![origin.to_string(),sequence],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Vec<u8>>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,String>(7)?)))?.collect::<Result<Vec<_>,_>>()?;
    let ids: std::collections::HashMap<Uuid, Uuid> = rows
        .iter()
        .map(|row| Ok((id(row.0.clone())?, Uuid::new_v4())))
        .collect::<Result<_, Error>>()?;
    for (old, seq, sender, kind, bytes, state, at, source) in rows {
        let new = ids[&id(old)?];
        let mut content: messages::Content = decode(&bytes)?;
        content.in_reply_to = content.in_reply_to.and_then(|id| ids.get(&id).copied());
        for attachment in &mut content.attachments {
            let mut a: koru_ai_core::ChatAttachment = serde_json::from_value(attachment.clone())
                .map_err(|_| Error::StorageUnavailable)?;
            a.message_id = Some(koru_ai_core::MessageId::from_uuid(new));
            *attachment = value(a)?;
        }
        c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![new.to_string(),branch.to_string(),seq,sender,kind,encode(&content)?,state,at])?;
        c.execute(
            "INSERT INTO message_copies(message,source) VALUES (?1,?2)",
            params![new.to_string(), source],
        )?;
    }
    let main = read::bot(c, who, thread.bot)?.main_thread;
    crate::messages::collaboration::reference(c, origin, branch, thread.bot, now)?;
    if main != origin {
        crate::messages::collaboration::reference(c, main, branch, thread.bot, now)?;
    }
    Ok(())
}
