// SPDX-License-Identifier: MPL-2.0
//! Batched owner-list facts. Only indexed published main words can be a preview.
use super::*;
use koru_ai_core::bots::{BotMainPreview, BotSummary};
use rusqlite::{Connection, params};
use std::collections::HashMap;

pub(super) fn page(
    c: &Connection,
    who: &Person,
    workspace: Uuid,
    keys: &[String],
) -> Result<HashMap<Uuid, BotSummary>, Error> {
    let sql = format!(
        "{} SELECT b.id,m.id,m.accepted_at_ms,
        substr(json_extract(CAST(m.payload AS TEXT),'$.body'),1,512),
        (SELECT count(*) FROM routine_occurrences o JOIN routines r ON r.id=o.routine
         WHERE r.bot=b.id AND o.review='pending'),
        (SELECT count(*) FROM eligible e WHERE e.bot=b.id AND e.needs_you)
        FROM bots b JOIN threads main ON main.bot=b.id AND main.kind='main'
        LEFT JOIN messages m ON m.id=(SELECT id FROM messages
          WHERE thread=main.id AND sender=b.id AND kind='words' AND state='published'
          ORDER BY sequence DESC LIMIT 1)
        WHERE b.owner=?1 AND json_extract(CAST(b.access AS TEXT),'$.workspace')=?2
          AND b.id IN (SELECT value FROM json_each(?4))",
        lists::ELIGIBLE
    );
    c.prepare(&sql)?
        .query_map(
            params![
                who.owner,
                workspace.to_string(),
                who.sender,
                serde_json::to_string(keys).map_err(|_| Error::InvalidRequest)?
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, u64>(4)?,
                    r.get::<_, u64>(5)?,
                ))
            },
        )?
        .map(|row| {
            let (bot, message, at, text, pending_results, open_requests) = row?;
            Ok((
                id(bot)?,
                BotSummary {
                    latest: match (message, at, text) {
                        (Some(message), Some(at_ms), Some(text)) => Some(BotMainPreview {
                            message: koru_ai_core::MessageId::from_uuid(id(message)?),
                            at_ms,
                            text,
                        }),
                        _ => None,
                    },
                    pending_results,
                    open_requests,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;
