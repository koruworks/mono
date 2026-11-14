// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::RoutineRouteIdentity;

impl Repository {
    /// Live Schedule keyset, not a snapshot roster or a current Schedule read.
    /// Column-only projection never materializes retained seed/grant payloads.
    pub fn bot_routine_creation_page(
        &self,
        owner: BotOwner,
        bot: BotId,
        after: Option<ScheduleId>,
        limit: u8,
    ) -> Result<BotRoutineCreationPage, BotError> {
        if !(1..=16).contains(&limit) {
            return Err(BotError::Invalid);
        }
        visible(&self.connection, owner, bot)?;
        let mut query = self.connection.prepare("SELECT
            CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END,
            CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
            CASE WHEN typeof(creation_request_id)='text' AND length(CAST(creation_request_id AS BLOB))=36 THEN creation_request_id END,
            CASE WHEN typeof(seed_hash)='blob' AND length(seed_hash)=32 THEN seed_hash END,
            CASE WHEN typeof(seed_title)='text' AND length(CAST(seed_title AS BLOB)) BETWEEN 1 AND 256 THEN seed_title END,
            CASE WHEN typeof(creation_state)='text' AND length(CAST(creation_state AS BLOB))<=16 THEN creation_state END,
            CASE WHEN refusal IS NULL THEN NULL WHEN typeof(refusal)='text' AND length(CAST(refusal AS BLOB))<=32 THEN refusal ELSE 'invalid' END,
            created_at_ms,updated_at_ms,source_bot_policy_revision,grant_revision
            FROM bot_routines WHERE user_scope_id=?1 AND owner_device_id=?2 AND bot_id=?3 AND schedule_id>?4
            ORDER BY schedule_id LIMIT ?5").map_err(bad)?;
        let mut items = query
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    bot.to_storage(),
                    after.map(|id| id.to_storage()).unwrap_or_default(),
                    u32::from(limit) + 1
                ],
                |r| {
                    let invalid = || rusqlite::Error::InvalidQuery;
                    let hash: [u8; 32] =
                        r.get::<_, Vec<u8>>(3)?.try_into().map_err(|_| invalid())?;
                    let route = RoutineRouteIdentity {
                        owner: koru_delegate_core::BotOwner {
                            user_scope: owner.user_scope,
                            device: owner.device,
                        },
                        bot,
                        schedule: r.get::<_, String>(0)?.parse().map_err(|_| invalid())?,
                        workspace: WorkspaceId::parse(&r.get::<_, String>(1)?)
                            .ok_or_else(invalid)?,
                        creation_request: r.get::<_, String>(2)?.parse().map_err(|_| invalid())?,
                        seed_hash: RoutineDigest32::from_bytes(hash),
                        created_at_ms: r.get(7)?,
                    };
                    if !route.valid() {
                        return Err(invalid());
                    }
                    Ok(BotRoutineCreationSummary {
                        route,
                        name: r.get(4)?,
                        observed: BotRoutineCreationObserved {
                            state: state(&r.get::<_, String>(5)?).map_err(|_| invalid())?,
                            updated_at_ms: r.get(8)?,
                        },
                        refusal: r
                            .get::<_, Option<String>>(6)?
                            .map(|v| refusal(&v).map_err(|_| invalid()))
                            .transpose()?,
                        source_bot_policy_revision: r.get(9)?,
                        grant_revision: r.get(10)?,
                    })
                },
            )
            .map_err(bad)?
            .map(|r| r.map_err(bad))
            .collect::<Result<Vec<_>, _>>()?;
        let more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        for item in &items {
            let r = bounded::receipt(&self.connection, owner, bot, item.route.creation_request)?
                .ok_or(BotError::Storage)?;
            let (expected_state, expected_stage) = match item.observed.state {
                BotRoutineCreationState::Prepared | BotRoutineCreationState::Unconfirmed => {
                    (BotRoutineCreationState::Prepared, "prepared")
                }
                BotRoutineCreationState::Bound => (BotRoutineCreationState::Bound, "completed"),
                BotRoutineCreationState::Conflict => {
                    (BotRoutineCreationState::Conflict, "rejected")
                }
            };
            if item.observed.updated_at_ms < item.route.created_at_ms
                || item.grant_revision == 0
                || item.source_bot_policy_revision == 0
                || (item.observed.state == BotRoutineCreationState::Conflict)
                    != item.refusal.is_some()
                || r.stage != expected_stage
                || r.outcome.version != 1
                || r.outcome.route != item.route
                || r.outcome.state != expected_state
                || r.outcome.refusal != item.refusal
            {
                return Err(BotError::Storage);
            }
        }
        let next = more.then(|| items.last().expect("positive limit").route.schedule);
        Ok(BotRoutineCreationPage { items, next })
    }
}
