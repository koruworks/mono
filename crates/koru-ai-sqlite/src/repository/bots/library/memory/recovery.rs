// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Repository {
    /// Startup hint only: no owner or mutation authority is inferred from this.
    pub fn bot_memory_recovery_needed(&self) -> Result<bool, BotError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_memory_creation_intents
             WHERE state IS NULL OR typeof(state)<>'text' OR state NOT IN ('bound','conflict','abandoned'))",
            [], |row| row.get(0),
        ).map_err(bad)
    }

    pub fn bot_memory_mutation_ready(&self, owner: BotOwner, bot: BotId) -> Result<bool, BotError> {
        match settled(&self.connection, owner, bot) {
            Ok(_) => Ok(true),
            Err(BotError::Settling | BotError::Conflict | BotError::RevisionConflict) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Re-read one inventory entry under current mutation guards before any
    /// Store effect. A scan position or old token never grants authority.
    pub fn bot_memory_recovery_candidate(
        &self,
        owner: BotOwner,
        entry: &BotMemoryRecoveryIdentity,
    ) -> Result<(BotMemoryCreation, u64), BotError> {
        let g = settled(&self.connection, owner, entry.bot)?;
        let creation =
            creation_on(&self.connection, owner, entry.bot, g.home)?.ok_or(BotError::NotFound)?;
        if creation.identity.creation_request != entry.request
            || creation.observed() != entry.observed
        {
            return Err(BotError::Conflict);
        }
        if !matches!(
            creation.state,
            BotMemoryState::Prepared | BotMemoryState::Unconfirmed
        ) {
            return Err(BotError::Conflict);
        }
        bound_memory_on(&self.connection, owner, entry.bot, g.home)?;
        Ok((creation, g.revision))
    }

    /// Column-only bounded recovery inventory. The rowid seek avoids sorting a
    /// payload-bearing projection, but may inspect unrelated rows. No constant
    /// query-work claim is made. Bad identities stay visible as per-entry errors.
    pub fn bot_memory_recovery_page(
        &self,
        owner: BotOwner,
        cursor: Option<BotMemoryRecoveryCursor>,
    ) -> Result<BotMemoryRecoveryPage, BotError> {
        let (after, through) = if let Some(cursor) = cursor {
            if cursor.owner != owner || cursor.after < 0 || cursor.through < cursor.after {
                return Err(BotError::Invalid);
            }
            (cursor.after, cursor.through)
        } else {
            let through: i64 = self
                .connection
                .query_row(
                    "SELECT COALESCE(MAX(rowid),0) FROM bot_memory_creation_intents
                 WHERE user_scope_id=?1 AND owner_device_id=?2",
                    params![owner.user_scope.to_storage(), owner.device.to_storage()],
                    |r| r.get(0),
                )
                .map_err(bad)?;
            if through < 0 {
                return Err(BotError::Storage);
            }
            (0, through)
        };
        let mut query = self.connection.prepare(
            "SELECT rowid,
             CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
             CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END,
             CASE WHEN typeof(state)='text' AND length(CAST(state AS BLOB))<=16 THEN state END, updated_at_ms
             FROM bot_memory_creation_intents NOT INDEXED
             WHERE rowid>?1 AND rowid<=?2 AND user_scope_id=?3 AND owner_device_id=?4
             AND (state IS NULL OR typeof(state)<>'text' OR state NOT IN ('bound','conflict','abandoned'))
             ORDER BY rowid LIMIT 17",
        ).map_err(bad)?;
        let rows = query
            .query_map(
                params![
                    after,
                    through,
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |r| {
                    // Position decoding must succeed independently of corrupt identity
                    // columns; no malformed identity aborts the rest of this page.
                    let rowid = r.get::<_, i64>(0)?;
                    let entry = (|| {
                        let bot = r.get::<_, String>(1).map_err(bad)?.parse().map_err(bad)?;
                        let request = r.get::<_, String>(2).map_err(bad)?.parse().map_err(bad)?;
                        let state = BotMemoryState::parse(&r.get::<_, String>(3).map_err(bad)?)
                            .ok_or(BotError::Storage)?;
                        let updated_at_ms = r.get::<_, i64>(4).map_err(bad)?;
                        Ok(BotMemoryRecoveryIdentity {
                            bot,
                            request,
                            observed: BotMemoryObserved {
                                state,
                                updated_at_ms,
                            },
                        })
                    })();
                    Ok((rowid, entry))
                },
            )
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        let next = if rows.len() > 16 {
            Some(BotMemoryRecoveryCursor {
                owner,
                after: rows[15].0,
                through,
            })
        } else {
            None
        };
        Ok(BotMemoryRecoveryPage {
            entries: rows.into_iter().take(16).map(|(_, entry)| entry).collect(),
            next,
        })
    }
}
