// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) fn read(
    connection: &rusqlite::Connection,
    thread: Uuid,
    after: u64,
    before: u64,
    limit: usize,
    descending: bool,
    exclude_owed_to: Option<Uuid>,
    byte_budget: usize,
) -> Result<Vec<Message>, Error> {
    // SQL is chosen from two static statements, never interpolated content.
    let sql = if descending {
        "SELECT id,sequence,sender,kind,payload,accepted_at_ms FROM messages m WHERE thread=?1 AND sequence>?2 AND sequence<=?3 AND (?5 IS NULL OR NOT EXISTS(SELECT 1 FROM obligations o WHERE o.message=m.id AND o.bot=?5 AND o.state IN ('owed','delivered'))) ORDER BY sequence DESC LIMIT ?4"
    } else {
        "SELECT id,sequence,sender,kind,payload,accepted_at_ms FROM messages m WHERE thread=?1 AND sequence>?2 AND sequence<=?3 AND (?5 IS NULL OR NOT EXISTS(SELECT 1 FROM obligations o WHERE o.message=m.id AND o.bot=?5 AND o.state IN ('owed','delivered'))) ORDER BY sequence LIMIT ?4"
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        rusqlite::params![
            thread.to_string(),
            after,
            before,
            limit as u64,
            exclude_owed_to.map(|id| id.to_string())
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        },
    )?;
    let mut result = vec![];
    let mut bytes = 0usize;
    for row in rows {
        let (key, sequence, sender, kind, payload, created_at_ms) = row?;
        if !result.is_empty() && bytes.saturating_add(payload.len()) > byte_budget {
            break;
        }
        bytes = bytes.saturating_add(payload.len());
        result.push(Message {
            id: id(key)?,
            sequence,
            sender,
            kind,
            content: decode(&payload)?,
            created_at_ms,
        });
    }
    Ok(result)
}
pub(super) fn input(message: Message) -> InputMessage {
    InputMessage {
        id: message.id,
        sequence: message.sequence,
        sender: message.sender,
        content: message.content,
        truncated: false,
        routine_context: None,
    }
}

/// The budget includes all serialized framing and uncertainty metadata. Work
/// identities survive truncation; history can be read later with the tool.
pub(super) fn bound(delivery: &mut Delivery, budget: usize) -> Result<(), Error> {
    work_skills(delivery)?;
    let work_files: usize = delivery
        .work
        .iter()
        .map(|m| m.content.attachments.len())
        .sum();
    let maximum = koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE;
    // Mandatory files are never dropped. The caller retries a smaller batch;
    // context can be read later through the ordinary history tool.
    if work_files > maximum {
        return Err(Error::InputTooSmall);
    }
    while work_files
        + delivery
            .context
            .iter()
            .map(|m| m.content.attachments.len())
            .sum::<usize>()
        > maximum
    {
        delivery.context.remove(0);
        delivery.older_history_available = true;
    }
    while delivery.context.len() + delivery.work.len() > HISTORY_LIMIT {
        delivery.context.remove(0);
        delivery.older_history_available = true;
    }
    // First reserve a fair content share for every work item, so one enormous
    // request cannot consume the other 31 requests' entire input budget.
    let mut metadata = delivery.clone();
    metadata.context.clear();
    for message in &mut metadata.work {
        message.content.body.clear();
        message.truncated = true;
        if let Some(context) = &mut message.routine_context {
            context.excerpt.clear();
            context.partial = true;
        }
    }
    let size = |delivery: &Delivery| -> Result<usize, Error> {
        Ok(encode(delivery)?.len() + crate::workspace::input::budget(delivery)?)
    };
    let share = budget.saturating_sub(size(&metadata)?) / delivery.work.len().max(1);
    for message in &mut delivery.work {
        cut(message, share);
    }
    while size(delivery)? > budget && !delivery.context.is_empty() {
        delivery.context.remove(0);
        delivery.older_history_available = true;
    }
    while size(delivery)? > budget {
        let Some(message) = delivery
            .work
            .iter_mut()
            .filter(|m| text_bytes(m) > 0)
            .max_by_key(|m| text_bytes(m))
        else {
            return Err(Error::InputTooSmall);
        };
        cut(message, text_bytes(message) / 2);
    }
    Ok(())
}
fn text_bytes(message: &InputMessage) -> usize {
    message.content.body.len()
        + message
            .routine_context
            .as_ref()
            .map_or(0, |c| c.excerpt.len())
}
fn cut(message: &mut InputMessage, mut bytes: usize) {
    if let Some(context) = &mut message.routine_context {
        let keep = crate::routines::review::prefix(
            &context.excerpt,
            bytes.saturating_sub(message.content.body.len()),
        )
        .len();
        if keep < context.excerpt.len() {
            context.excerpt.truncate(keep);
            context.partial = true;
        }
        bytes = bytes.saturating_sub(context.excerpt.len());
    }
    if message.content.body.len() <= bytes {
        return;
    }
    while bytes > 0 && !message.content.body.is_char_boundary(bytes) {
        bytes -= 1;
    }
    message.content.body.truncate(bytes);
    message.truncated = true;
}

impl Database {
    /// Open at the recent end without reading the entire transcript. A page
    /// is ascending for display; its first sequence is the exclusive bound
    /// for the preceding page, so concurrent appends do not move that cursor.
    pub fn read_recent_messages(
        &self,
        person: &Person,
        thread: Uuid,
        before: Option<u64>,
    ) -> Result<Vec<Message>, Error> {
        authorize_thread(&self.connection, person, thread)?;
        if before == Some(0) {
            return Err(Error::InvalidRequest);
        }
        let upper = before.map_or(i64::MAX as u64, |sequence| sequence - 1);
        let mut messages = read(
            &self.connection,
            thread,
            0,
            upper.min(i64::MAX as u64),
            PAGE_LIMIT,
            true,
            None,
            crate::limits::MAX_PAGE_BYTES,
        )?;
        messages.reverse();
        Ok(messages)
    }
    pub fn read_messages(
        &self,
        person: &Person,
        thread: Uuid,
        after: u64,
    ) -> Result<Vec<Message>, Error> {
        authorize_thread(&self.connection, person, thread)?;
        read(
            &self.connection,
            thread,
            after,
            i64::MAX as u64,
            PAGE_LIMIT,
            false,
            None,
            crate::limits::MAX_PAGE_BYTES,
        )
    }
    /// The worker carrier must resolve the current Bot identity before calling.
    pub fn read_messages_for_bot(
        &self,
        bot: Uuid,
        thread: Uuid,
        after: u64,
    ) -> Result<Vec<Message>, Error> {
        let allowed:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM participants WHERE thread=?1 AND participant=?2 AND kind='bot')",rusqlite::params![thread.to_string(),bot.to_string()],|r|r.get(0))?;
        if !allowed {
            return Err(Error::NotAllowed);
        }
        read(
            &self.connection,
            thread,
            after,
            i64::MAX as u64,
            PAGE_LIMIT,
            false,
            None,
            crate::limits::MAX_PAGE_BYTES,
        )
    }
}
