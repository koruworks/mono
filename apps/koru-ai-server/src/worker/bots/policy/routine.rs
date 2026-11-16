// SPDX-License-Identifier: MPL-2.0

//! Fresh Routine proof binds local materialization to independent owner truth.
//! Retained terminal effect recovery does not use this lookup.
use super::*;
use koru_ai_core::bots::{
    BotRoutineCreation, BotRoutineCreationIdentity, BotRoutineCreationState,
    BotRoutineCurrentGrant, BotRoutineStoredPreparation,
};
use koru_delegate_core::{RoutineClaimObservation, RoutineLinkState, RoutinePolicyBarrier};

#[derive(Clone, PartialEq, Eq)]
pub(in crate::worker) struct RoutineLookup {
    occurrence: BotRoutineStoredPreparation,
    creation: BotRoutineCreation,
    /// The grant the Routine runs under now, from the one reader.
    current: BotRoutineCurrentGrant,
}

/// The Routine's independent policy barrier, built from the grant it runs
/// under now: the one function shared by Linked-native proof, preclaim and the
/// access edit. `current` comes from the one reader
/// (`Repository::routine_current_grant`), never from today's Bot settings.
///
/// For revision 1 the request is the creation request, so no barrier
/// installed before an access edit existed changes. For an edited grant it is
/// the edit's own request; the ledger allows one request per grant revision
/// and refuses a lower revision. Callers validate the complete retained Bound
/// identity and current authority.
pub(in crate::worker) fn current_barrier(
    identity: &BotRoutineCreationIdentity,
    current: &BotRoutineCurrentGrant,
) -> RoutinePolicyBarrier {
    RoutinePolicyBarrier {
        owner: identity.owner,
        bot: identity.bot,
        schedule: identity.schedule,
        request: current.request.into(),
        grant_revision: current.grant.grant_revision,
        policy: current.grant.policy.clone(),
        // Global lifecycle fencing remains separate; Schedule Pause does not
        // stop an admitted Run.
        effects_allowed: true,
    }
}

impl RoutineLookup {
    pub(in crate::worker::bots) fn execution_profile(
        &self,
        submission: &koru_delegate_core::BotSubmission,
    ) -> ApiResult<koru_ai_core::ProfileId> {
        if self.occurrence.prepared.submission != *submission {
            return Err(bot_failure(BotError::Forbidden));
        }
        match submission.request.requested_by {
            koru_delegate_core::ActorRef::Person { profile } => Ok(profile),
            _ => Err(bot_failure(BotError::Forbidden)),
        }
    }
    pub(in crate::worker::bots) fn barrier(&self) -> RoutinePolicyBarrier {
        current_barrier(&self.creation.identity, &self.current)
    }

    pub(in crate::worker::bots) fn verify_delegate(
        &self,
        delegate: &dyn BotDelegatePort,
        deadline: Instant,
    ) -> ApiResult<()> {
        let prepared = &self.occurrence.prepared;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(unavailable_failure());
        }
        let reply = delegate
            .execute_within(
                DelegateCommand::ReadRoutineClaim {
                    slot: prepared.slot,
                },
                remaining,
            )
            .map_err(|_| unavailable_failure())?;
        if Instant::now() >= deadline {
            return Err(unavailable_failure());
        }
        match reply {
            BotReply::RoutineClaim {
                observation: RoutineClaimObservation::Claimed { record },
            } if record.valid()
                && record.link_state == (RoutineLinkState::Linked {})
                && record.claim.matches_prepared(prepared)
                && record.submission_version == prepared.submission_version
                && record.submission == prepared.submission =>
            {
                Ok(())
            }
            _ => Err(bot_failure(BotError::Forbidden)),
        }
    }
}

/// Reuse the single bounded repository decoder; never infer the subject from
/// chat association or deserialize a second receipt/capture vocabulary here.
pub(in crate::worker::bots) fn snapshot(
    repository: &Repository,
    bot: &Bot,
    request: &NativePolicyRequest,
) -> ApiResult<Option<Box<RoutineLookup>>> {
    if request.provenance.owner.user_scope != bot.user_scope
        || request.provenance.owner.device != bot.owner_device
        || request.provenance.bot != bot.id
        || request.provenance.config_revision > bot.revision
        || request.provenance.policy_revision > bot.policy_revision
    {
        return Err(bot_failure(BotError::Forbidden));
    }
    let Some(subject) = request.provenance.routine else {
        let stored = repository
            .bot_submission_for_run(owner(bot), bot.id, request.run)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if stored.intent.submission.provenance != request.provenance
            || stored.intent.submission.policy != request.captured
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        return Ok(None);
    };
    let occurrence = repository
        .routine_occurrence_for_run(owner(bot), bot.id, request.run)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
    let creation = repository
        .bot_routine_creation(owner(bot), bot.id, subject.schedule)
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
    // There is no grant history: only the current grant can be read. An
    // access edit is refused while a Run of the Routine is unsettled, so a
    // live Run's capture always equals it. A capture at any other revision
    // is refused.
    let current = repository
        .routine_current_grant(owner(bot), bot.id, subject.schedule)
        .map_err(bot_failure)?;
    let prepared = &occurrence.prepared;
    if creation.state != BotRoutineCreationState::Bound
        || creation.identity.home != bot.home_workspace
        || prepared.submission.provenance != request.provenance
        || prepared.submission.policy != request.captured
        || prepared.ids.run != request.run
        || occurrence.capture.creation != creation.identity.route()
        || occurrence.capture.grant != current.grant
        || subject.grant_revision != current.grant.grant_revision
    {
        return Err(bot_failure(BotError::Forbidden));
    }
    Ok(Some(Box::new(RoutineLookup {
        occurrence,
        creation,
        current,
    })))
}

pub(super) fn same_canonical(current: &Bot, prior: &Bot) -> bool {
    owner(current) == owner(prior)
        && current.id == prior.id
        && current.revision == prior.revision
        && current.policy_revision == prior.policy_revision
        && current.home_workspace == prior.home_workspace
        && current.config == prior.config
        && current.access == prior.access
        && current.selected_pages == prior.selected_pages
}

pub(in crate::worker::bots) fn recheck(
    repository: &Repository,
    current_bot: Option<&Bot>,
    prior_bot: &Bot,
    request: &NativePolicyRequest,
    expected: Option<&RoutineLookup>,
) -> ApiResult<()> {
    let bot = current_bot
        .filter(|bot| same_canonical(bot, prior_bot))
        .ok_or_else(|| bot_failure(BotError::RevisionConflict))?;
    let current = snapshot(repository, bot, request)?;
    if current.as_deref() != expected {
        return Err(bot_failure(BotError::RevisionConflict));
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
