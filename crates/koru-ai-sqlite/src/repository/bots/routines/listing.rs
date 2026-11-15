// SPDX-License-Identifier: MPL-2.0
//! Bounded observational snapshots. No value here supplies execution authority.
use super::*;
use occurrence::recent::RecentSnapshot;
#[cfg(test)]
mod tests;

#[derive(Clone, PartialEq, Eq)]
pub struct RoutineListSnapshot {
    pub metadata: BotPolicyMetadata,
    pub profile: ProfileId,
    pub model: BotRoutineModelObservation,
    pub roster: BotRoutineCreationPage,
    after: Option<ScheduleId>,
    limit: u8,
    model_fields: (Option<String>, Option<String>, Option<String>),
}
impl std::fmt::Debug for RoutineListSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineListSnapshot([REDACTED])")
    }
}

/// Every semantic field outside the already verified seed/grant encodings is
/// retained exactly, including first VV bytes and parsed immutable receipt.
/// Seed/grant bodies are discarded before the next identity is decoded.
#[derive(Clone, PartialEq, Eq)]
pub struct RoutineListCheckpoint {
    route: koru_delegate_core::RoutineRouteIdentity,
    store_request: Uuid,
    expected: u64,
    fingerprint: RoutineDigest32,
    grant_digest: RoutineDigest32,
    origin: Option<Vec<u8>>,
    state: BotRoutineCreationState,
    refusal: Option<BotRoutineCreationRefusal>,
    created: i64,
    updated: i64,
    receipt_stage: String,
    receipt: BotRoutineCreationReceipt,
    recent: Result<RecentSnapshot, BotError>,
}
impl std::fmt::Debug for RoutineListCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineListCheckpoint([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RoutineReadSubject {
    pub metadata: BotPolicyMetadata,
    pub profile: ProfileId,
    pub model: BotRoutineModelObservation,
    model_fields: (Option<String>, Option<String>, Option<String>),
}
impl std::fmt::Debug for RoutineReadSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineReadSubject([REDACTED])")
    }
}

impl Repository {
    pub fn routine_read_subject(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<RoutineReadSubject, BotError> {
        let metadata = self.bot_policy_metadata(owner, bot)?;
        if metadata.lifecycle == BotLifecycle::Trashed {
            return Err(BotError::NotFound);
        }
        if metadata.pending_lifecycle.is_some() {
            return Err(BotError::Unavailable);
        }
        let profile: String = self.connection.query_row("SELECT CASE WHEN typeof(profile_id)='text' AND length(CAST(profile_id AS BLOB))=36 THEN profile_id END FROM chats WHERE id=?1", [metadata.main_chat.to_storage()], |r| r.get(0)).map_err(bad)?;
        let profile = ProfileId::parse(&profile).ok_or(BotError::Storage)?;
        let model_fields: (Option<String>, Option<String>, Option<String>) = self.connection.query_row("SELECT
            CASE WHEN typeof(provider)='text' AND length(CAST(provider AS BLOB)) BETWEEN 1 AND 64 THEN provider END,
            CASE WHEN typeof(model)='text' AND length(CAST(model AS BLOB)) BETWEEN 1 AND 512 THEN model END,
            CASE WHEN typeof(reasoning)='text' AND length(CAST(reasoning AS BLOB))<=32 THEN reasoning END
            FROM bots WHERE bot_id=?1", [bot.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(bad)?;
        let model = (|| {
            let value = ModelSelection {
                provider: AiProviderId::parse(model_fields.0.clone()?).ok()?,
                requested_model: model_fields.1.clone()?,
                default_reasoning: ReasoningChoice::parse(model_fields.2.as_deref()?)?,
            };
            validate_bot_model(&value).ok()?;
            Some(BotRoutineModelObservation::Configured {
                provider: value.provider,
                requested_model: value.requested_model,
            })
        })()
        .unwrap_or(BotRoutineModelObservation::Unavailable {});
        Ok(RoutineReadSubject {
            metadata,
            profile,
            model,
            model_fields,
        })
    }

    pub fn routine_list_snapshot(
        &self,
        owner: BotOwner,
        bot: BotId,
        after: Option<ScheduleId>,
        limit: u8,
    ) -> Result<RoutineListSnapshot, BotError> {
        validate_routine_list_request(bot, after, limit)?;
        let RoutineReadSubject {
            metadata,
            profile,
            model,
            model_fields,
        } = self.routine_read_subject(owner, bot)?;
        let roster = page::read(&self.connection, owner, bot, after, limit)?;
        Ok(RoutineListSnapshot {
            metadata,
            profile,
            model,
            roster,
            after,
            limit,
            model_fields,
        })
    }

    /// Exactly one full bounded identity may leave the writer for a Store read.
    pub fn routine_list_entry(
        &self,
        snapshot: &RoutineListSnapshot,
        index: usize,
    ) -> Result<
        (
            BotRoutineCreation,
            RoutineListCheckpoint,
            BotRoutineListItem,
        ),
        BotError,
    > {
        let summary = snapshot.roster.items.get(index).ok_or(BotError::Invalid)?;
        let owner = snapshot.metadata.owner;
        let bot = snapshot.metadata.id;
        let c = bounded::by_schedule(&self.connection, owner, bot, summary.route.schedule)?
            .ok_or(BotError::Storage)?;
        if c.identity.route() != summary.route
            || c.observed() != summary.observed
            || c.refusal != summary.refusal
            || c.identity.seed.name.as_str() != summary.name
            || c.identity.grant.grant_revision != summary.grant_revision
            || c.identity.grant.source_bot_policy_revision != summary.source_bot_policy_revision
        {
            return Err(BotError::Conflict);
        }
        let receipt = bounded::receipt(&self.connection, owner, bot, c.identity.creation_request)?
            .ok_or(BotError::Storage)?;
        let recent = if c.state == BotRoutineCreationState::Bound {
            occurrence::recent::read(self, owner, bot, c.identity.schedule, c.identity.home)
        } else {
            Err(BotError::Unavailable)
        };
        let item = BotRoutineListItem {
            schedule_id: c.identity.schedule,
            creation: BotRoutineCreationObservation {
                request_id: c.identity.creation_request,
                state: c.state,
                refusal: c.refusal,
                updated_at_ms: c.updated_at_ms,
            },
            grant: BotRoutineGrantSummary {
                source_bot_policy_revision: c.identity.grant.source_bot_policy_revision,
                grant_revision: c.identity.grant.grant_revision,
                access: match c.identity.grant.policy.access {
                    koru_delegate_core::BotAccess::Default => BotAccess::Default,
                    koru_delegate_core::BotAccess::Workspace => BotAccess::Workspace,
                },
                selected_page_count: c
                    .identity
                    .grant
                    .policy
                    .selected_pages
                    .len()
                    .try_into()
                    .map_err(bad)?,
            },
            live: BotRoutineLiveObservation::Unavailable {},
            local_conversations: recent
                .as_ref()
                .map(|r| r.observation())
                .unwrap_or(BotRoutineLocalConversations::Unavailable {}),
        };
        let checkpoint = RoutineListCheckpoint {
            route: c.identity.route(),
            store_request: c.identity.store_request,
            expected: c.identity.expected_bot_revision,
            fingerprint: c.identity.command_fingerprint,
            grant_digest: c.identity.grant.digest,
            origin: c.origin_edit_revision.clone(),
            state: c.state,
            refusal: c.refusal,
            created: c.created_at_ms,
            updated: c.updated_at_ms,
            receipt_stage: receipt.stage,
            receipt: receipt.outcome,
            recent,
        };
        Ok((c, checkpoint, item))
    }

    /// Re-decodes bodies sequentially. Never accumulates all selected seeds.
    pub fn validate_routine_list(
        &self,
        snapshot: &RoutineListSnapshot,
        checkpoints: &[RoutineListCheckpoint],
    ) -> Result<(), BotError> {
        if checkpoints.len() != snapshot.roster.items.len()
            || self.routine_list_snapshot(
                snapshot.metadata.owner,
                snapshot.metadata.id,
                snapshot.after,
                snapshot.limit,
            )? != *snapshot
        {
            return Err(BotError::Conflict);
        }
        for (index, expected) in checkpoints.iter().enumerate() {
            let (_, actual, _) = self.routine_list_entry(snapshot, index)?;
            if actual != *expected {
                return Err(BotError::Conflict);
            }
        }
        Ok(())
    }
}
