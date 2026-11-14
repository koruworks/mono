// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Repository {
    /// Reject only positively decoded canonical drift, in the same transaction
    /// as the exact pending-row CAS and terminal receipt. Unknown/corrupt source
    /// or owner/lifecycle refusal never becomes disagreement evidence.
    pub fn reject_changed_bot_routine_creation(
        &mut self,
        owner: BotOwner,
        entry: &BotRoutineRecoveryIdentity,
        now: i64,
    ) -> Result<BotRoutineCreation, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        visible(&tx, owner, entry.bot)?;
        let mut creation = bounded::by_schedule(&tx, owner, entry.bot, entry.schedule)?
            .ok_or(BotError::NotFound)?;
        if creation.observed() != entry.observed
            || !matches!(
                creation.state,
                BotRoutineCreationState::Prepared | BotRoutineCreationState::Unconfirmed
            )
        {
            return Err(BotError::Conflict);
        }
        settled(&tx, owner, entry.bot)?;
        let source = bounded::source(&tx, owner, entry.bot)?;
        // matches_source short-circuits on a changed revision. Validate all of
        // today's template/grant fields first, even when the revision differs.
        creation.identity.validate_current_source(&source)?;
        if creation.identity.matches_source(&source)? {
            return Err(BotError::Conflict);
        }
        creation.state = BotRoutineCreationState::Conflict;
        creation.refusal = Some(BotRoutineCreationRefusal::AuthorityChanged);
        persist_transition(&tx, &mut creation, entry.observed, now)?;
        tx.commit().map_err(bad)?;
        Ok(creation)
    }

    /// Scheduling hint only, never owner or effect authority.
    pub fn bot_routine_recovery_needed(&self) -> Result<bool, BotError> {
        self.connection.query_row("SELECT EXISTS(SELECT 1 FROM bot_routines WHERE creation_state IS NULL OR typeof(creation_state)<>'text' OR creation_state NOT IN ('bound','conflict'))", [], |r| r.get(0)).map_err(bad)
    }

    /// Only the selected row's bounded identity is decoded. Every Store job
    /// and completion rechecks canonical lifecycle/config/grant/Profile here.
    pub fn bot_routine_recovery_candidate(
        &self,
        owner: BotOwner,
        entry: &BotRoutineRecoveryIdentity,
    ) -> Result<(BotRoutineCreation, koru_ai_core::ProfileId), BotError> {
        settled(&self.connection, owner, entry.bot)?;
        let creation = bounded::by_schedule(&self.connection, owner, entry.bot, entry.schedule)?
            .ok_or(BotError::NotFound)?;
        if creation.observed() != entry.observed
            || !matches!(
                creation.state,
                BotRoutineCreationState::Prepared | BotRoutineCreationState::Unconfirmed
            )
        {
            return Err(BotError::Conflict);
        }
        let source = bounded::source(&self.connection, owner, entry.bot)?;
        if !creation.identity.matches_source(&source)? {
            return Err(BotError::RevisionConflict);
        }
        Ok((creation, source.profile))
    }

    /// A fixed rowid high-water bounds each pass. At most sixteen narrow rows;
    /// corrupt identities advance as per-entry errors, never head-of-line block.
    /// This may inspect unrelated rows and is not a constant query-work claim.
    pub fn bot_routine_recovery_page(
        &self,
        owner: BotOwner,
        cursor: Option<BotRoutineRecoveryCursor>,
    ) -> Result<BotRoutineRecoveryPage, BotError> {
        let (after, through) = if let Some(cursor) = cursor {
            if cursor.owner != owner || cursor.after < 0 || cursor.through < cursor.after {
                return Err(BotError::Invalid);
            }
            (cursor.after, cursor.through)
        } else {
            let through: i64 = self.connection.query_row("SELECT COALESCE(MAX(rowid),0) FROM bot_routines WHERE user_scope_id=?1 AND owner_device_id=?2",
                params![owner.user_scope.to_storage(), owner.device.to_storage()], |r| r.get(0)).map_err(bad)?;
            if through < 0 {
                return Err(BotError::Storage);
            }
            (0, through)
        };
        let mut query = self.connection.prepare("SELECT rowid,
            CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
            CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END,
            CASE WHEN typeof(creation_state)='text' AND length(CAST(creation_state AS BLOB))<=16 THEN creation_state END, updated_at_ms
            FROM bot_routines NOT INDEXED WHERE rowid>?1 AND rowid<=?2 AND user_scope_id=?3 AND owner_device_id=?4
            AND (creation_state IS NULL OR typeof(creation_state)<>'text' OR creation_state NOT IN ('bound','conflict')) ORDER BY rowid LIMIT 17").map_err(bad)?;
        let rows = query
            .query_map(
                params![
                    after,
                    through,
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |r| {
                    let rowid = r.get::<_, i64>(0)?;
                    let entry = (|| {
                        Ok(BotRoutineRecoveryIdentity {
                            bot: r.get::<_, String>(1).map_err(bad)?.parse().map_err(bad)?,
                            schedule: r.get::<_, String>(2).map_err(bad)?.parse().map_err(bad)?,
                            observed: BotRoutineCreationObserved {
                                state: state(&r.get::<_, String>(3).map_err(bad)?)?,
                                updated_at_ms: r.get(4).map_err(bad)?,
                            },
                        })
                    })();
                    Ok((rowid, entry))
                },
            )
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        let next = (rows.len() > 16).then(|| BotRoutineRecoveryCursor {
            owner,
            after: rows[15].0,
            through,
        });
        Ok(BotRoutineRecoveryPage {
            entries: rows.into_iter().take(16).map(|(_, entry)| entry).collect(),
            next,
        })
    }
}
