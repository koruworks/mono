// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Repository {
    /// Canonical local materialization, not current delegate/native authority.
    /// All consumers share the bounded row/receipt/capture/association decoder.
    pub fn routine_occurrence_for_run(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: koru_delegate_core::RunId,
    ) -> Result<Option<BotRoutineStoredPreparation>, BotError> {
        visible(&self.connection, owner, bot)?;
        let raw:Option<(String,String,String,String,i64)>=self.connection.query_row("SELECT
            CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
            CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
            CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
            CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END,occurrence_ms
            FROM bot_routine_preparations WHERE run_id=?1",[run.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(bad)?;
        let Some((scope, device, subject, schedule, occurrence_ms)) = raw else {
            return Ok(None);
        };
        if scope != owner.user_scope.to_storage()
            || device != owner.device.to_storage()
            || subject != bot.to_storage()
        {
            return Err(BotError::Forbidden);
        }
        let slot = RoutineSlot {
            owner: koru_delegate_core::BotOwner {
                user_scope: owner.user_scope,
                device: owner.device,
            },
            bot,
            schedule: schedule.parse().map_err(bad)?,
            occurrence_ms,
        };
        let row = rows::read(&self.connection, &slot)?.ok_or(BotError::Storage)?;
        if row.prepared.ids.run != run || row.stage != BotRoutineStoredStage::Linked {
            return Err(BotError::Conflict);
        }
        Ok(Some(row))
    }

    pub fn routine_occurrence_recovery_page(
        &self,
        owner: BotOwner,
        cursor: Option<BotRoutineOccurrenceCursor>,
    ) -> Result<BotRoutineOccurrenceRecoveryPage, BotError> {
        let (after, through) = match cursor {
            Some(c) if c.owner == owner && c.after >= 0 && c.after <= c.through => {
                (c.after, c.through)
            }
            Some(_) => return Err(BotError::Invalid),
            None => (
                0,
                self.connection
                    .query_row(
                        "SELECT COALESCE(MAX(rowid),0) FROM bot_routine_preparations",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(bad)?,
            ),
        };
        // A narrow high-water walk also covers Closed terminal decisions with
        // lost acknowledgements. There is no stored ack bit: repeated exact
        // terminal acks are harmless. This bounds allocation, not SQL scan work.
        let mut statement=self.connection.prepare("SELECT d.rowid,
            CASE WHEN typeof(d.bot_id)='text' AND length(CAST(d.bot_id AS BLOB))=36 THEN d.bot_id END,
            CASE WHEN typeof(d.schedule_id)='text' AND length(CAST(d.schedule_id AS BLOB))=36 THEN d.schedule_id END,
            d.occurrence_ms,
            CASE WHEN typeof(d.semantic_hash)='blob' AND length(d.semantic_hash)=32 THEN d.semantic_hash END
            FROM bot_routine_preparations d NOT INDEXED
            WHERE d.rowid>?1 AND d.rowid<=?2 AND d.user_scope_id=?3 AND d.owner_device_id=?4
            AND (d.stage IN ('prepared','closed') OR (d.stage='linked' AND EXISTS(SELECT 1 FROM bot_command_receipts r WHERE r.user_scope_id=d.user_scope_id AND r.request_id=d.submission_request_id AND r.command_kind='routine_occurrence' AND r.stage IN ('prepared','accepted'))))
            ORDER BY d.rowid LIMIT 17").map_err(bad)?;
        let scanned = statement
            .query_map(
                params![
                    after,
                    through,
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |r| {
                    let id = r.get::<_, i64>(0)?;
                    // Preserve the cursor even when another projected field is corrupt.
                    let item = (|| -> Result<BotRoutineOccurrenceRecoveryEntry, BotError> {
                        let bot = r.get::<_, String>(1).map_err(bad)?.parse().map_err(bad)?;
                        let schedule = r.get::<_, String>(2).map_err(bad)?.parse().map_err(bad)?;
                        let occurrence_ms = r.get(3).map_err(bad)?;
                        let bytes: Vec<u8> = r.get(4).map_err(bad)?;
                        let slot = RoutineSlot {
                            owner: koru_delegate_core::BotOwner {
                                user_scope: owner.user_scope,
                                device: owner.device,
                            },
                            bot,
                            schedule,
                            occurrence_ms,
                        };
                        if !slot.valid() {
                            return Err(BotError::Storage);
                        }
                        Ok(BotRoutineOccurrenceRecoveryEntry {
                            slot,
                            semantic_hash: RoutineDigest32::from_bytes(
                                bytes.try_into().map_err(|_| BotError::Storage)?,
                            ),
                        })
                    })();
                    Ok((id, item))
                },
            )
            .map_err(bad)?;
        let mut entries = Vec::new();
        let mut last = after;
        let mut more = false;
        for item in scanned {
            let (id, item) = item.map_err(bad)?;
            if entries.len() == 16 {
                more = true;
                break;
            }
            last = id;
            entries.push(item);
        }
        Ok(BotRoutineOccurrenceRecoveryPage {
            entries,
            next: more.then_some(BotRoutineOccurrenceCursor {
                owner,
                after: last,
                through,
            }),
        })
    }
}
