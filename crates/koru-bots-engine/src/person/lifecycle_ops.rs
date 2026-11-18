// SPDX-License-Identifier: MPL-2.0
//! Exact lifecycle reversal, in the same transaction as cancellation and the
//! visible Bot/thread change. Ordinary workspace pages are never moved.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn change_thread(
    c: &Connection,
    who: &Person,
    command: Uuid,
    prior: Thread,
    lifecycle: Lifecycle,
    source: Option<Uuid>,
    now: i64,
) -> Result<Value, Error> {
    if prior.kind == "main" {
        return Err(Error::NotAllowed);
    }
    messages::authorize_bot(c, who, prior.bot)?;
    let thread = prior.id;
    let (lifecycle, holder) = apply(
        c,
        who,
        LifecycleOperation {
            request: command,
            bot: prior.bot,
            thread: Some(thread),
            prior: prior.lifecycle,
            lifecycle,
            prior_holder: prior.held_by,
            source,
            reversed_by: None,
            completed_at_ms: now,
        },
    )?;
    if source.is_none() {
        change::retire(c, None, Some(thread))?;
    }
    c.execute(
        "UPDATE threads SET lifecycle=?2,lifecycle_holder=?3,revision=revision+1 WHERE id=?1",
        params![
            thread.to_string(),
            lifecycle.token(),
            holder.as_ref().map(encode).transpose()?
        ],
    )?;
    value(read::thread(c, who, thread)?)
}

pub(super) fn read(
    c: &Connection,
    who: &Person,
    bot: Uuid,
    request: Uuid,
) -> Result<LifecycleOperation, Error> {
    owner(c, who, bot)?;
    let bytes: Option<Vec<u8>> = c
        .query_row(
            "SELECT record FROM lifecycle_changes WHERE owner=?1 AND request=?2 AND bot=?3",
            params![who.owner, request.to_string(), bot.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    decode(&bytes.ok_or(Error::NotAllowed)?)
}

pub(super) fn apply(
    c: &Connection,
    who: &Person,
    mut operation: LifecycleOperation,
) -> Result<(Lifecycle, Option<LifecycleHolder>), Error> {
    let holder = if let Some(source) = operation.source {
        if operation.lifecycle != Lifecycle::Active
            || operation.prior_holder.as_ref().map(|holder| holder.request) != Some(source)
        {
            return Err(Error::Conflict);
        }
        let mut original = read(c, who, operation.bot, source)?;
        if original.thread != operation.thread
            || original.reversed_by.is_some()
            || original.source.is_some()
        {
            return Err(Error::Conflict);
        }
        operation.lifecycle = original.prior;
        original.reversed_by = Some(operation.request);
        c.execute(
            "UPDATE lifecycle_changes SET record=?3 WHERE owner=?1 AND request=?2",
            params![who.owner, source.to_string(), encode(&original)?],
        )?;
        original.prior_holder
    } else {
        // Archive an active subject; Trash an active or archived subject.
        // Restore always names the precise operation holding it.
        if operation.lifecycle == Lifecycle::Active
            || operation.lifecycle == operation.prior
            || operation.prior == Lifecycle::Trashed
        {
            return Err(Error::NotAllowed);
        }
        Some(LifecycleHolder {
            request: operation.request,
            lifecycle: operation.lifecycle,
            completed_at_ms: operation.completed_at_ms,
        })
    };
    c.execute(
        "INSERT INTO lifecycle_changes(owner,request,bot,thread,record) VALUES (?1,?2,?3,?4,?5)",
        params![
            who.owner,
            operation.request.to_string(),
            operation.bot.to_string(),
            operation.thread.map(|id| id.to_string()),
            encode(&operation)?
        ],
    )?;
    Ok((operation.lifecycle, holder))
}
