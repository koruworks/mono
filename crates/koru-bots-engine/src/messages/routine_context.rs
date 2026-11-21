// SPDX-License-Identifier: MPL-2.0
//! Retained result authority follows the original main-conversation card.
use super::*;
use crate::routines::review;
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(test)]
mod tests;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub occurrence: Uuid,
    pub card: Uuid,
    pub reply: Uuid,
    pub excerpt: String,
    pub partial: bool,
}

fn owns_main(c: &Connection, bot: Uuid, thread: Uuid) -> Result<bool, Error> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 AND bot=?2 AND kind='main')",
        params![thread.to_string(), bot.to_string()],
        |r| r.get(0),
    )?)
}

pub(super) fn resolve(
    c: &Connection,
    bot: Uuid,
    thread: Uuid,
    card: Uuid,
) -> Result<Option<Context>, Error> {
    let (kind, bytes): (String, Vec<u8>) = c
        .query_row(
            "SELECT kind,payload FROM messages WHERE id=?1 AND thread=?2",
            params![card.to_string(), thread.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::InvalidRequest)?;
    if kind != "thread_reference" {
        return Ok(None);
    }
    let content: Content = decode(&bytes)?;
    let mut refs = content
        .references
        .iter()
        .filter(|v| v["kind"] == "routine_result");
    let Some(reference) = refs.next() else {
        return Ok(None);
    };
    if refs.next().is_some() || !owns_main(c, bot, thread)? {
        return Err(Error::NotAllowed);
    }
    let occurrence: Uuid = serde_json::from_value(reference["occurrence"].clone())
        .map_err(|_| Error::InvalidRequest)?;
    let detail = review::detail(c, bot, occurrence, 0)?;
    let original_thread: String = c.query_row(
        "SELECT thread FROM routines WHERE id=?1",
        [detail.item.routine.to_string()],
        |r| r.get(0),
    )?;
    if detail.item.card != card
        || reference["reply"] != detail.item.reply.to_string()
        || reference["thread"] != original_thread
    {
        return Err(Error::NotAllowed);
    }
    Ok(Some(Context {
        occurrence,
        card,
        reply: detail.item.reply,
        excerpt: detail.item.preview,
        partial: detail.item.truncated,
    }))
}

pub(super) fn handled(
    c: &Connection,
    bot: Uuid,
    context: &Context,
    message: Uuid,
    now: i64,
) -> Result<(), Error> {
    c.execute("UPDATE routine_occurrences SET review='handled',reviewed_at_ms=?2,handled_by_message=?3 WHERE id=?1 AND (review IS NULL OR review<>'handled')",
        params![context.occurrence.to_string(), now, message.to_string()])?;
    review::invalidate(c, bot, now)
}

pub(super) fn enrich(
    c: &Connection,
    bot: Uuid,
    thread: Uuid,
    delivery: &mut Delivery,
) -> Result<(), Error> {
    // A guest Bot may read ordinary shared words, but the reference does not
    // grant it the owning Bot's Routine result or any linked workspace access.
    if !owns_main(c, bot, thread)? {
        return Ok(());
    }
    for message in delivery.context.iter_mut().chain(&mut delivery.work) {
        let Some(card) = message.content.in_reply_to else {
            continue;
        };
        let from_person: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM participants WHERE thread=?1 AND participant=?2 AND kind='person')",
            params![thread.to_string(), message.sender], |r| r.get(0))?;
        if from_person {
            message.routine_context = resolve(c, bot, thread, card)?;
        }
    }
    Ok(())
}

pub(crate) fn read(
    c: &Connection,
    bot: Uuid,
    thread: Uuid,
    occurrence: Uuid,
    offset: usize,
) -> Result<review::Detail, Error> {
    if !owns_main(c, bot, thread)? {
        return Err(Error::NotAllowed);
    }
    let detail = review::detail(c, bot, occurrence, offset)?;
    let available: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages m JOIN participants p ON p.thread=m.thread AND p.participant=m.sender AND p.kind='person'
         WHERE m.thread=?1 AND m.kind='words' AND m.state='accepted' AND json_extract(CAST(m.payload AS TEXT),'$.in_reply_to')=?2)",
        params![thread.to_string(), detail.item.card.to_string()], |r| r.get(0))?;
    if !available || resolve(c, bot, thread, detail.item.card)?.is_none() {
        return Err(Error::NotAllowed);
    }
    Ok(detail)
}
