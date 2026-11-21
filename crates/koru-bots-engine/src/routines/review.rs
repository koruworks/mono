// SPDX-License-Identifier: MPL-2.0
//! Review is a projection of retained occurrences, not a second result ledger.
use super::*;
use crate::limits::{
    MAX_REVIEW_PAGE_ENTRIES, MAX_ROUTINE_EXCERPT_BYTES, MAX_ROUTINE_RESULT_CHUNK_BYTES,
};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Cursor {
    pub at_ms: i64,
    pub id: Uuid,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Item {
    pub occurrence: Uuid,
    pub routine: Uuid,
    pub name: String,
    pub card: Uuid,
    pub reply: Uuid,
    pub published_at_ms: i64,
    pub review: Option<String>,
    pub preview: String,
    pub truncated: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Page {
    pub bot: Uuid,
    pub count: u64,
    pub items: Vec<Item>,
    pub next: Option<Cursor>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Detail {
    pub bot: Uuid,
    pub item: Item,
    pub text: String,
    pub offset: usize,
    pub next: Option<usize>,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineReview(<redacted>)")
    }
}

pub(crate) fn prefix(text: &str, bound: usize) -> &str {
    let mut end = bound.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn result(c: &Connection, bot: Uuid, key: Uuid) -> Result<(Item, String), Error> {
    let row = c
        .query_row(
            "SELECT r.id,r.name,o.card,o.reply,m.accepted_at_ms,o.review,m.payload
         FROM routine_occurrences o JOIN routines r ON r.id=o.routine
         JOIN messages m ON m.id=o.reply AND m.thread=r.thread AND m.sender=r.bot
         JOIN messages card ON card.id=o.card AND card.kind='thread_reference' AND card.state='published' AND card.sender=r.bot
         JOIN threads main ON main.id=card.thread AND main.bot=r.bot AND main.kind='main'
         WHERE o.id=?1 AND r.bot=?2 AND m.kind='words' AND m.state='published'",
            params![key.to_string(), bot.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::NotAllowed)?;
    let content: messages::Content = decode(&row.6)?;
    let preview = prefix(&content.body, MAX_ROUTINE_EXCERPT_BYTES).to_owned();
    Ok((
        Item {
            occurrence: key,
            routine: id(row.0)?,
            name: row.1,
            card: id(row.2)?,
            reply: id(row.3)?,
            published_at_ms: row.4,
            review: row.5,
            truncated: preview.len() < content.body.len(),
            preview,
        },
        content.body,
    ))
}

impl Database {
    pub fn routine_reviews(
        &self,
        who: &Person,
        bot: Uuid,
        before: Option<Cursor>,
    ) -> Result<Page, Error> {
        authorize(&self.connection, who, bot)?;
        let count = self.connection.query_row(
            "SELECT count(*) FROM routine_occurrences o JOIN routines r ON r.id=o.routine WHERE r.bot=?1 AND o.review='pending'",
            [bot.to_string()], |r| r.get(0))?;
        let keys = self
            .connection
            .prepare(
                "SELECT o.id FROM routine_occurrences o JOIN routines r ON r.id=o.routine
             JOIN messages m ON m.id=o.reply WHERE r.bot=?1 AND o.review='pending'
             AND (?2 IS NULL OR m.accepted_at_ms<?2 OR (m.accepted_at_ms=?2 AND o.id<?3))
             ORDER BY m.accepted_at_ms DESC,o.id DESC LIMIT ?4",
            )?
            .query_map(
                params![
                    bot.to_string(),
                    before.as_ref().map(|v| v.at_ms),
                    before.as_ref().map(|v| v.id.to_string()),
                    MAX_REVIEW_PAGE_ENTRIES + 1
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut more = keys.len() > MAX_REVIEW_PAGE_ENTRIES;
        let mut page = Page {
            bot,
            count,
            items: vec![],
            next: None,
        };
        // Include framing/cursor and JSON escaping, not just raw preview bytes.
        for key in keys.into_iter().take(MAX_REVIEW_PAGE_ENTRIES) {
            page.items.push(result(&self.connection, bot, id(key)?)?.0);
            if encode(&page)?.len() + 256 > crate::limits::MAX_PAGE_BYTES / 2 {
                page.items.pop();
                more = true;
                break;
            }
        }
        if more {
            page.next = page.items.last().map(|v| Cursor {
                at_ms: v.published_at_ms,
                id: v.occurrence,
            });
            if page.next.is_none() {
                return Err(Error::InputTooSmall);
            }
        }
        Ok(page)
    }

    pub fn routine_review(
        &self,
        who: &Person,
        bot: Uuid,
        occurrence: Uuid,
        offset: usize,
    ) -> Result<Detail, Error> {
        authorize(&self.connection, who, bot)?;
        detail(&self.connection, bot, occurrence, offset)
    }

    pub fn skip_routine_review(
        &mut self,
        who: &Person,
        command: Uuid,
        bot: Uuid,
        occurrence: Uuid,
        now: i64,
    ) -> Result<Receipt<Item>, Error> {
        if command.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let digest = messages::receipts::digest("skip-routine-review", &(bot, occurrence))?;
        let subject = who.receipt_subject();
        let tx = self.connection.transaction()?;
        authorize(&tx, who, bot)?;
        if let Some(answer) = messages::receipts::find(&tx, &subject, command, &digest)? {
            return Ok(Receipt {
                answer,
                replayed: true,
            });
        }
        result(&tx, bot, occurrence)?;
        let changed = tx.execute("UPDATE routine_occurrences SET review='skipped',reviewed_at_ms=?2 WHERE id=?1 AND review='pending'",params![occurrence.to_string(),now])?;
        if changed != 0 {
            invalidate(&tx, bot, now)?;
        }
        let answer = result(&tx, bot, occurrence)?.0;
        messages::receipts::save(&tx, &subject, command, &digest, &answer, now)?;
        tx.commit()?;
        Ok(Receipt {
            answer,
            replayed: false,
        })
    }
}

pub(crate) fn invalidate(c: &Connection, bot: Uuid, now: i64) -> Result<(), Error> {
    c.execute(
        "UPDATE threads SET updated_at_ms=MAX(updated_at_ms+1,?2) WHERE bot=?1 AND kind='main'",
        params![bot.to_string(), now],
    )?;
    Ok(())
}

pub(crate) fn detail(
    c: &Connection,
    bot: Uuid,
    occurrence: Uuid,
    offset: usize,
) -> Result<Detail, Error> {
    let (item, body) = result(c, bot, occurrence)?;
    if offset > body.len() || !body.is_char_boundary(offset) {
        return Err(Error::InvalidRequest);
    }
    let text = prefix(&body[offset..], MAX_ROUTINE_RESULT_CHUNK_BYTES).to_owned();
    let end = offset + text.len();
    Ok(Detail {
        bot,
        item,
        text,
        offset,
        next: (end < body.len()).then_some(end),
    })
}

/// A declaration belongs to the live turn, never to a later retry/replacement turn.
pub(crate) fn declare(
    db: &mut Database,
    turn: messages::Turn,
    bot: Uuid,
    command: Uuid,
    occurrence: Uuid,
    needs_person: bool,
    now: i64,
) -> Result<Value, Error> {
    if command.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let digest =
        messages::receipts::digest("set-routine-attention", &(turn, occurrence, needs_person))?;
    let subject = format!("routine-attention:{}", turn.id);
    let tx = db.connection.transaction()?;
    let row = messages::turns::turn_row(&tx, turn)?;
    if row.1 != bot.to_string() {
        return Err(Error::NotAllowed);
    }
    if let Some(answer) = messages::receipts::find(&tx, &subject, command, &digest)? {
        return Ok(answer);
    }
    if row.2 != "running" {
        return Err(Error::StaleTurn);
    }
    let eligible: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM routine_occurrences r JOIN routines s ON s.id=r.routine
         JOIN obligations o ON o.message=r.message AND o.bot=s.bot AND o.thread=s.thread
         JOIN delivery_obligations x ON x.obligation=o.id JOIN deliveries d ON d.id=x.delivery
         WHERE r.id=?1 AND s.bot=?2 AND s.thread=?3 AND r.reply IS NULL AND o.state='delivered'
         AND d.turn=?4 AND d.state='accepted')",
        params![
            occurrence.to_string(),
            bot.to_string(),
            row.0,
            turn.id.to_string()
        ],
        |r| r.get(0),
    )?;
    if !eligible {
        return Err(Error::NotAllowed);
    }
    tx.execute("UPDATE turns SET details=CAST(json_set(CAST(details AS TEXT),?2,json(?3)) AS BLOB) WHERE id=?1",
        params![turn.id.to_string(),format!("$.routine_attention.\"{occurrence}\""),if needs_person { "true" } else { "false" }])?;
    let answer = json!({"occurrence":occurrence,"needs_person":needs_person});
    messages::receipts::save(&tx, &subject, command, &digest, &answer, now)?;
    tx.commit()?;
    Ok(answer)
}

pub(crate) fn published(
    c: &Connection,
    turn: messages::Turn,
    occurrence: &str,
) -> Result<(), Error> {
    let bytes: Vec<u8> = c.query_row(
        "SELECT details FROM turns WHERE id=?1",
        [turn.id.to_string()],
        |r| r.get(0),
    )?;
    let details: Value = decode(&bytes)?;
    let required = details
        .get("routine_attention")
        .and_then(|v| v.get(occurrence))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    c.execute(
        "UPDATE routine_occurrences SET review=?2 WHERE id=?1 AND review IS NULL",
        params![
            occurrence,
            if required { "pending" } else { "not_required" }
        ],
    )?;
    Ok(())
}
