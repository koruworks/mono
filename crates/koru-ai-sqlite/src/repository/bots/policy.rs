// SPDX-License-Identifier: MPL-2.0

//! Desired access and its durable effect-barrier acknowledgement share one writer.
use super::*;
use koru_delegate_core::{BotPolicyBarrier, BotPolicyBarrierReceipt, BotPolicyBarrierState};
use rusqlite::Connection;

pub struct BotAccessRecoveryPage {
    pub scanned_through: Option<i64>,
    pub pending: Vec<(i64, BotId, UserScopeId, BotRequestId)>,
}

pub(crate) fn require_policy_settled(conn: &Connection, bot: BotId) -> Result<(), BotError> {
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE bot_id=?1 AND command_kind='update_access' AND stage='prepared')",
        [bot.to_storage()], |r| r.get(0),
    ).map_err(bad)?;
    if pending {
        // Still being acknowledged: not a reused request id.
        Err(BotError::Settling)
    } else {
        Ok(())
    }
}

impl Repository {
    pub fn bot_access_receipt(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<BotAccessReceipt>, BotError> {
        // A read: an archived Bot's receipts stay readable by their owner; a
        // trashed Bot's do not.
        let (current, lifecycle) = self.bot_any_lifecycle(bot)?.ok_or(BotError::NotFound)?;
        if lifecycle == BotLifecycle::Trashed
            || current.user_scope != owner.user_scope
            || current.owner_device != owner.device
        {
            return Err(BotError::Forbidden);
        }
        let row: Option<(String, String, String, String, Option<String>)> = self.connection.query_row(
            "SELECT bot_id,command_kind,stage,fingerprint,outcome_json FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
            params![owner.user_scope.to_storage(),request.to_storage()],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional().map_err(bad)?;
        let Some((id, kind, stage, fingerprint, json)) = row else {
            return Ok(None);
        };
        if kind != "update_access" {
            return Ok(None);
        }
        let json = json.ok_or(BotError::Storage)?;
        if id != bot.to_storage()
            || json.len() > 1024 * 1024
            || fingerprint.len() != 64
            || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(BotError::Storage);
        }
        let outcome: BotReceiptOutcome = serde_json::from_str(&json).map_err(bad)?;
        outcome.validate().map_err(|_| BotError::Storage)?;
        let (snapshot, barrier) = match outcome {
            BotReceiptOutcome::AccessPolicy {
                version: 1,
                bot,
                barrier,
            } if matches!(stage.as_str(), "prepared" | "completed") => (bot, Some(barrier)),
            BotReceiptOutcome::Configuration { version: 1, bot } if stage == "completed" => {
                (bot, None)
            }
            _ => return Err(BotError::Storage),
        };
        if snapshot.id != bot
            || snapshot.user_scope != owner.user_scope
            || snapshot.owner_device != owner.device
        {
            return Err(BotError::Storage);
        }
        Ok(Some(BotAccessReceipt {
            request,
            fingerprint,
            bot: snapshot,
            barrier,
            completed: stage == "completed",
        }))
    }

    pub fn prepare_bot_access(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        expected: u64,
        barrier: &BotPolicyBarrier,
    ) -> Result<BotAccessReceipt, BotError> {
        let owner = BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        };
        if let Some(old) = self.bot_access_receipt(owner, bot.id, request)? {
            return if old.fingerprint == fingerprint {
                Ok(old)
            } else {
                Err(BotError::Conflict)
            };
        }
        let current = self.bot(bot.id)?.ok_or(BotError::NotFound)?;
        if expected == 0
            || expected >= i64::MAX as u64
            || current.revision != expected
            || current.policy_revision >= i64::MAX as u64
        {
            return Err(BotError::RevisionConflict);
        }
        let mut desired = current;
        desired.access = bot.access;
        desired.selected_pages = bot.selected_pages.clone();
        // The caller states the whole grant set. A command that did not
        // mention connections passes the Bot's current set, so nothing is
        // removed; the writer accepts a new grant only for a usable one.
        desired.connections = bot.connections.clone();
        desired.revision += 1;
        desired.policy_revision += 1;
        desired.updated_at_ms = bot.updated_at_ms;
        if &desired != bot {
            return Err(BotError::Invalid);
        }
        validate_access_policy(bot, barrier)?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(BotError::Invalid);
        }
        self.save_bot_record(
            bot,
            profile,
            request,
            fingerprint,
            Some(expected),
            Some(barrier),
        )?;
        self.bot_access_receipt(owner, bot.id, request)?
            .ok_or(BotError::Storage)
    }

    pub fn acknowledge_bot_access(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
        observed: &BotPolicyBarrierReceipt,
        now: i64,
    ) -> Result<BotAccessReceipt, BotError> {
        let old = self
            .bot_access_receipt(owner, bot, request)?
            .ok_or(BotError::NotFound)?;
        if old.barrier.as_ref() != Some(&observed.barrier)
            || observed.state != BotPolicyBarrierState::Effective
        {
            return Err(BotError::Conflict);
        }
        if old.completed {
            return Ok(old);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1 AND user_scope_id=?2 AND owner_device_id=?3 AND lifecycle='active' AND revision=?4 AND policy_revision=?5)",
            params![bot.to_storage(), owner.user_scope.to_storage(), owner.device.to_storage(), old.bot.revision, old.bot.policy_revision], |r| r.get(0),
        ).map_err(bad)?;
        if !valid {
            return Err(BotError::RevisionConflict);
        }
        let changed = tx.execute(
            "UPDATE bot_command_receipts SET stage='completed',updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2 AND bot_id=?3 AND stage='prepared' AND command_kind='update_access' AND fingerprint=?5",
            params![owner.user_scope.to_storage(),request.to_storage(),bot.to_storage(),now,old.fingerprint],
        ).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        tx.commit().map_err(bad)?;
        Ok(BotAccessReceipt {
            completed: true,
            ..old
        })
    }

    /// One page of prepared access changes, read through the ai18 partial
    /// index (`bot_pending_access`), so settled history is never walked.
    pub fn pending_bot_access(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<BotAccessRecoveryPage, BotError> {
        let mut query = self.connection.prepare(
            "SELECT rowid,bot_id,user_scope_id,request_id FROM bot_command_receipts INDEXED BY bot_pending_access WHERE rowid>?1 AND command_kind='update_access' AND stage='prepared' ORDER BY rowid LIMIT ?2",
        ).map_err(bad)?;
        let rows = query
            .query_map(params![after, limit.clamp(1, 64)], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(bad)?;
        let mut page = BotAccessRecoveryPage {
            scanned_through: None,
            pending: Vec::new(),
        };
        for row in rows {
            let (row, bot, scope, request) = row.map_err(bad)?;
            page.scanned_through = Some(row);
            page.pending.push((
                row,
                bot.parse().map_err(bad)?,
                UserScopeId::parse(&scope).ok_or(BotError::Storage)?,
                request.parse().map_err(bad)?,
            ));
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests;
