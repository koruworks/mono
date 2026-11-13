// SPDX-License-Identifier: MPL-2.0

//! Post-Hello canonical replay evidence. Never completes an operation receipt.
use super::*;
use koru_delegate_core::{BotAdmissionBarrier, BotBootAdmission, BotPolicyBarrier};

pub struct BotBootSnapshot {
    pub bot: Bot,
    pub admission: BotAdmissionBarrier,
    pub pending_access: Option<BotPolicyBarrier>,
}

fn exact(bot: &Bot, barrier: &BotAdmissionBarrier) -> bool {
    barrier.owner.user_scope == bot.user_scope
        && barrier.owner.device == bot.owner_device
        && barrier.bot == bot.id
        && barrier.epoch == bot.revision
        && barrier.paused == bot.paused
}

fn merge_exact(
    slot: &mut Option<BotAdmissionBarrier>,
    barrier: BotAdmissionBarrier,
) -> Result<(), BotError> {
    if slot.as_ref().is_some_and(|known| *known != barrier) {
        return Err(BotError::Conflict);
    }
    *slot = Some(barrier);
    Ok(())
}

impl Repository {
    /// The caller observed the delegate boot before entering this writer read.
    /// A carried tuple supplies only an otherwise lost opaque request UUID;
    /// desired state and revisions always come from this current canonical Bot.
    pub fn bot_boot_snapshot(
        &self,
        owner: BotOwner,
        id: BotId,
        hint: &BotBootAdmission,
    ) -> Result<BotBootSnapshot, BotError> {
        let (bot, _) = self.bot_any_lifecycle(id)?.ok_or(BotError::NotFound)?;
        if bot.user_scope != owner.user_scope || bot.owner_device != owner.device {
            return Err(BotError::Forbidden);
        }
        if bot.revision == 0
            || bot.revision > i64::MAX as u64
            || bot.policy_revision == 0
            || bot.policy_revision > i64::MAX as u64
        {
            return Err(BotError::Storage);
        }
        let current = match hint {
            BotBootAdmission::Unavailable {} => return Err(BotError::Unavailable),
            BotBootAdmission::Missing {} => None,
            BotBootAdmission::Present { barrier } => {
                if barrier.owner.user_scope != owner.user_scope
                    || barrier.owner.device != owner.device
                    || barrier.bot != id
                {
                    return Err(BotError::Forbidden);
                }
                if barrier.epoch == 0
                    || barrier.epoch > bot.revision
                    || (barrier.epoch == bot.revision && barrier.paused != bot.paused)
                {
                    return Err(BotError::RevisionConflict);
                }
                (barrier.epoch == bot.revision).then_some(barrier.clone())
            }
        };
        let mut known = self.boot_pause(&bot)?;
        self.boot_refusals(&bot, &mut known)?;
        // An open lifecycle's required pause may not be replaced with a
        // convenient delegate key or with a synthetic creation-correlated one.
        if let Some((scope, request)) = self.open_bot_lifecycle(id)? {
            let op = self
                .bot_lifecycle_operation(scope, request)?
                .ok_or(BotError::Storage)?;
            if op.owner != owner || op.bot != id {
                return Err(BotError::Forbidden);
            }
            if op.barrier_revision == Some(bot.revision) {
                let request = op.pause_request.ok_or(BotError::Storage)?;
                if known
                    .as_ref()
                    .is_none_or(|barrier| barrier.request != request)
                {
                    return Err(BotError::Storage);
                }
            }
        }
        let admission = match (known, current) {
            (Some(known), Some(current)) if known != current => return Err(BotError::Conflict),
            (Some(known), _) => known,
            (None, Some(current)) => current,
            (None, None) => BotAdmissionBarrier {
                owner: koru_delegate_core::BotOwner {
                    user_scope: owner.user_scope,
                    device: owner.device,
                },
                bot: id,
                request: self.boot_creation_request(&bot)?,
                epoch: bot.revision,
                paused: bot.paused,
            },
        };
        let pending_access = self.boot_pending_access(&bot)?;
        Ok(BotBootSnapshot {
            bot,
            admission,
            pending_access,
        })
    }

    fn boot_pause(&self, bot: &Bot) -> Result<Option<BotAdmissionBarrier>, BotError> {
        let mut query = self
            .connection
            .prepare_cached(
                "SELECT request_id FROM bot_command_receipts INDEXED BY bot_command_guard
             WHERE bot_id=?1 AND user_scope_id=?2 AND command_kind IN ('pause','resume')
               AND expected_revision=?3 LIMIT 2",
            )
            .map_err(bad)?;
        let requests = query
            .query_map(
                params![
                    bot.id.to_storage(),
                    bot.user_scope.to_storage(),
                    bot.revision - 1
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(bad)?;
        let mut found = None;
        for request in requests {
            let request = request.map_err(bad)?.parse().map_err(bad)?;
            let receipt = admission::pause_on(&self.connection, bot.user_scope, request)?
                .ok_or(BotError::Storage)?;
            if !exact(bot, &receipt.intent.barrier) {
                return Err(BotError::Conflict);
            }
            merge_exact(&mut found, receipt.intent.barrier)?;
        }
        Ok(found)
    }

    fn boot_refusals(
        &self,
        bot: &Bot,
        found: &mut Option<BotAdmissionBarrier>,
    ) -> Result<(), BotError> {
        // prepare_bot_submission_tx caps prepared intents at 50. Project only
        // fixed-field outcomes, never the task-bearing submission_json. An
        // oversize marker cannot disappear as a filtered or NULL outcome.
        let mut query = self
            .connection
            .prepare_cached(
                "SELECT request_id,captured_config_revision,
                COALESCE(length(CAST(outcome_json AS BLOB))>8192,0),
                CASE WHEN length(CAST(outcome_json AS BLOB))<=8192 THEN outcome_json END
             FROM bot_command_receipts INDEXED BY bot_command_guard
             WHERE bot_id=?1 AND user_scope_id=?2 AND command_kind IN ('start','continue')
               AND stage='prepared' LIMIT 51",
            )
            .map_err(bad)?;
        let rows = query
            .query_map(
                params![bot.id.to_storage(), bot.user_scope.to_storage()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .map_err(bad)?;
        for (n, row) in rows.enumerate() {
            let (request, captured, oversize, outcome) = row.map_err(bad)?;
            let _: BotRequestId = request.parse().map_err(bad)?;
            if n >= 50 || oversize || captured == 0 || captured > i64::MAX as u64 {
                return Err(BotError::Storage);
            }
            let Some(outcome) = outcome else {
                continue;
            };
            let BotExecutionOutcome::RefusalFence {
                version: 1,
                round,
                barrier,
                ..
            } = serde_json::from_str(&outcome).map_err(bad)?
            else {
                return Err(BotError::Storage);
            };
            if round == 0
                || barrier.epoch <= captured
                || barrier.epoch > bot.revision
                || barrier.owner.user_scope != bot.user_scope
                || barrier.owner.device != bot.owner_device
                || barrier.bot != bot.id
            {
                return Err(BotError::Storage);
            }
            if barrier.epoch == bot.revision {
                if !exact(bot, &barrier) {
                    return Err(BotError::Conflict);
                }
                merge_exact(found, barrier)?;
            }
        }
        Ok(())
    }

    fn boot_creation_request(&self, bot: &Bot) -> Result<BotRequestId, BotError> {
        let mut query = self.connection.prepare_cached(
            "SELECT request_id,CASE WHEN length(CAST(outcome_json AS BLOB))<=1048576 THEN outcome_json END
             FROM bot_command_receipts INDEXED BY bot_command_guard
             WHERE bot_id=?1 AND user_scope_id=?2 AND command_kind='create' AND stage='completed' LIMIT 2").map_err(bad)?;
        let rows = query
            .query_map(
                params![bot.id.to_storage(), bot.user_scope.to_storage()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(bad)?;
        let mut found = None;
        for row in rows {
            if found.is_some() {
                return Err(BotError::Conflict);
            }
            let (request, json) = row.map_err(bad)?;
            let request = request.parse::<BotRequestId>().map_err(bad)?;
            let outcome: BotReceiptOutcome =
                serde_json::from_str(&json.ok_or(BotError::Storage)?).map_err(bad)?;
            outcome.validate().map_err(|_| BotError::Storage)?;
            let authentic = match outcome {
                BotReceiptOutcome::Configuration {
                    version: 1,
                    bot: created,
                } => {
                    validate_config(&created.config, created.home_workspace)
                        .map_err(|_| BotError::Storage)?;
                    created.id == bot.id
                        && created.user_scope == bot.user_scope
                        && created.owner_device == bot.owner_device
                        && created.home_workspace == bot.home_workspace
                        && created.main_chat == bot.main_chat
                        && created.revision == 1
                }
                BotReceiptOutcome::CreationV2 {
                    version: 2,
                    creation,
                } => {
                    creation.ready()
                        && !creation.abandoned()
                        && creation.bot == bot.id
                        && creation.user_scope == bot.user_scope
                        && creation.owner_device == bot.owner_device
                        && creation.home_workspace == bot.home_workspace
                        && creation.main_chat == bot.main_chat
                        && self
                            .bot_creation(bot.user_scope, request)?
                            .is_some_and(|r| {
                                r.stage == BotReceiptStage::Completed && !r.record.abandoned()
                            })
                }
                _ => false,
            };
            if !authentic {
                return Err(BotError::Storage);
            }
            found = Some(request);
        }
        found.ok_or(BotError::Storage)
    }

    fn boot_pending_access(&self, bot: &Bot) -> Result<Option<BotPolicyBarrier>, BotError> {
        let mut query = self.connection.prepare_cached(
            "SELECT request_id FROM bot_command_receipts INDEXED BY bot_command_guard
             WHERE bot_id=?1 AND user_scope_id=?2 AND command_kind='update_access' AND stage='prepared' LIMIT 2").map_err(bad)?;
        let rows = query
            .query_map(
                params![bot.id.to_storage(), bot.user_scope.to_storage()],
                |row| row.get::<_, String>(0),
            )
            .map_err(bad)?;
        let mut found = None;
        for row in rows {
            if found.is_some() {
                return Err(BotError::Conflict);
            }
            let request = row.map_err(bad)?.parse().map_err(bad)?;
            let receipt = self
                .bot_access_receipt(
                    BotOwner {
                        user_scope: bot.user_scope,
                        device: bot.owner_device,
                    },
                    bot.id,
                    request,
                )?
                .ok_or(BotError::Storage)?;
            if receipt.bot.revision != bot.revision
                || receipt.bot.policy_revision != bot.policy_revision
            {
                return Err(BotError::RevisionConflict);
            }
            found = Some(receipt.barrier.ok_or(BotError::Storage)?);
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests;
