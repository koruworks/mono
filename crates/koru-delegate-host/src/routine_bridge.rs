// SPDX-License-Identifier: MPL-2.0

//! Trusted bootstrap projection of the existing finite private bridge. No launch.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use koru_api_client::routine_bridge::{RoutineBridgeClient, RoutineBridgeTransportError};
use koru_delegate_api::bots::BotDelegateAuthority;
use koru_delegate_api::routines::*;
use koru_delegate_core::*;
use koru_delegate_runtime::{
    Port, PortError, RoutineBridge, RoutineBridgeFailure, RoutineBridgeResult,
    RoutinePreparationObservation, RoutinePreparedReply,
};
use koru_delegate_schedule::{
    ScheduleDefinition, routine_definition_digest, routine_semantic_digest,
};

pub(crate) struct HostRoutineBridge {
    client: Option<RoutineBridgeClient>,
    stopped: Arc<AtomicBool>,
}

fn error(code: &'static str) -> PortError {
    PortError::new(Port::Schedules, code, "Routine bridge phase is unavailable")
}
fn refused(code: RoutineBridgeRefusal) -> RoutineBridgeFailure {
    RoutineBridgeFailure::Refused(error(match code {
        RoutineBridgeRefusal::Invalid => "routine_invalid",
        RoutineBridgeRefusal::Unauthorized => "routine_unauthorized",
        RoutineBridgeRefusal::Unavailable => "routine_unavailable",
        RoutineBridgeRefusal::Capacity => "routine_capacity",
        RoutineBridgeRefusal::Conflict => "routine_conflict",
        RoutineBridgeRefusal::Stale => "routine_stale",
        RoutineBridgeRefusal::Forbidden => "routine_forbidden",
        RoutineBridgeRefusal::ContextTooLarge => "routine_context_too_large",
    }))
}
fn request<T: RoutineBridgePayload>(payload: T) -> RoutineBridgeResult<RoutineBridgeEnvelope<T>> {
    RoutineBridgeEnvelope::new(payload)
        .map_err(|_| RoutineBridgeFailure::NotSent(error("routine_invalid_request")))
}

impl HostRoutineBridge {
    pub(crate) fn new(
        gateway: &koru_api_client::Client,
        authority: Option<&BotDelegateAuthority>,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        // Keep routing/expiry installed even if the optional HTTP capability
        // cannot be constructed. Never fall back to an ordinary bearer.
        let client =
            authority.and_then(|a| gateway.routine_bridge(a.routine_bridge_authority()).ok());
        Self { client, stopped }
    }

    fn call<T>(
        &self,
        deadline: Instant,
        send: impl FnOnce(
            &RoutineBridgeClient,
        ) -> Result<RoutineBridgeEnvelope<T>, RoutineBridgeTransportError>,
    ) -> RoutineBridgeResult<T> {
        if self.stopped() || Instant::now() >= deadline {
            return Err(RoutineBridgeFailure::NotSent(error(
                "routine_phase_expired",
            )));
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| RoutineBridgeFailure::NotSent(error("routine_bridge_unavailable")))?;
        let answer = send(client).map_err(|error_value| match error_value {
            RoutineBridgeTransportError::NotSent(_) => {
                RoutineBridgeFailure::NotSent(error("routine_not_sent"))
            }
            RoutineBridgeTransportError::Unknown(_) => {
                RoutineBridgeFailure::Unknown(error("routine_completion_unknown"))
            }
        });
        // A completed physical response cannot renew the original slice or
        // authorize a subsequent phase after Stop. The client still owns its
        // permit until physical completion; there is no detached request here.
        if self.stopped() || Instant::now() >= deadline {
            return match answer {
                Err(failure @ RoutineBridgeFailure::NotSent(_)) => Err(failure),
                _ => Err(RoutineBridgeFailure::Unknown(error(
                    "routine_phase_expired",
                ))),
            };
        }
        answer.map(|reply| reply.payload)
    }
}

impl RoutineBridge for HostRoutineBridge {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn prepare(
        &mut self,
        slot: &RoutineSlot,
        workspace: WorkspaceId,
        definition: &ScheduleDefinition,
        owner_handoff: Option<HandoffId>,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutinePreparedReply> {
        let semantic = routine_semantic_digest(slot, definition, workspace, owner_handoff)
            .map_err(|_| RoutineBridgeFailure::NotSent(error("routine_invalid_request")))?;
        let definition_digest = routine_definition_digest(definition, workspace, owner_handoff)
            .map_err(|_| RoutineBridgeFailure::NotSent(error("routine_invalid_request")))?;
        let request = request(PrepareOccurrence {
            slot: *slot,
            workspace,
            schedule_definition: definition.clone(),
            owner_handoff,
        })?;
        match self.call(deadline, |client| client.prepare_until(&request, deadline))? {
            PrepareOccurrenceReply::Prepared { preparation }
                if preparation.semantic_hash == semantic
                    && preparation.definition.digest == definition_digest =>
            {
                Ok(RoutinePreparedReply::Prepared(preparation))
            }
            PrepareOccurrenceReply::Closed {
                semantic_hash,
                reason,
                ..
            } if semantic_hash == semantic => Ok(RoutinePreparedReply::Closed { reason }),
            PrepareOccurrenceReply::Refused { code } => Err(refused(code)),
            _ => Err(RoutineBridgeFailure::Unknown(error(
                "routine_response_mismatch",
            ))),
        }
    }

    fn read_preparation(
        &mut self,
        slot: &RoutineSlot,
        semantic_hash: RoutineDigest32,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutinePreparationObservation> {
        let request = request(ReadPreparation {
            slot: *slot,
            semantic_hash,
        })?;
        match self.call(deadline, |client| {
            client.read_preparation_until(&request, deadline)
        })? {
            ReadPreparationReply::Prepared { preparation } => {
                Ok(RoutinePreparationObservation::Prepared(preparation))
            }
            ReadPreparationReply::Linked { claim, .. } => {
                Ok(RoutinePreparationObservation::Linked { claim })
            }
            ReadPreparationReply::Closed { reason, .. } => {
                Ok(RoutinePreparationObservation::Closed { reason })
            }
            ReadPreparationReply::Missing { .. } => Ok(RoutinePreparationObservation::Missing),
            ReadPreparationReply::Refused { code } => Err(refused(code)),
        }
    }

    fn synchronize_preclaim(
        &mut self,
        prepared: &RoutinePrepared,
        deadline: Instant,
    ) -> RoutineBridgeResult<(BotAdmissionBarrier, FreshBotPolicyProof)> {
        if !prepared.valid() {
            return Err(RoutineBridgeFailure::NotSent(error(
                "routine_invalid_request",
            )));
        }
        let request = request(SynchronizeRoutinePreclaim {
            slot: prepared.slot,
            semantic_hash: prepared.semantic_hash,
            preparation_hash: prepared.preparation_hash,
        })?;
        match self.call(deadline, |client| {
            client.synchronize_preclaim_until(&request, deadline)
        })? {
            SynchronizeRoutinePreclaimReply::Ready {
                admission, proof, ..
            } => {
                let p = &prepared.submission.provenance;
                if !proof.effects_allowed_for(&prepared.submission)
                    || proof.global.config_revision != p.config_revision
                    || proof.global.policy_revision != p.policy_revision
                    || admission.epoch != p.config_revision
                    || proof.routine.as_ref().zip(p.routine.as_ref()).is_none_or(
                        |(current, captured)| current.grant_revision != captured.grant_revision,
                    )
                {
                    return Err(RoutineBridgeFailure::Unknown(error(
                        "routine_response_mismatch",
                    )));
                }
                Ok((admission, proof))
            }
            SynchronizeRoutinePreclaimReply::Refused { code } => Err(refused(code)),
        }
    }

    fn finalize(
        &mut self,
        claim: &RoutineClaimIdentity,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutineLinkOutcome> {
        let request = request(FinalizeOccurrence {
            claim: claim.clone(),
        })?;
        match self.call(deadline, |client| client.finalize_until(&request, deadline))? {
            FinalizeOccurrenceReply::Linked { .. } => Ok(RoutineLinkOutcome::Linked {}),
            FinalizeOccurrenceReply::Terminal { reason, .. } => {
                Ok(RoutineLinkOutcome::Terminal { reason })
            }
            FinalizeOccurrenceReply::Refused { code } => Err(refused(code)),
        }
    }

    fn read_link(
        &mut self,
        claim: &RoutineClaimIdentity,
        deadline: Instant,
    ) -> RoutineBridgeResult<Option<RoutineLinkOutcome>> {
        let request = request(ReadOccurrenceLink {
            claim: claim.clone(),
        })?;
        match self.call(deadline, |client| {
            client.read_link_until(&request, deadline)
        })? {
            ReadOccurrenceLinkReply::Linked { .. } => Ok(Some(RoutineLinkOutcome::Linked {})),
            ReadOccurrenceLinkReply::Terminal { reason, .. } => {
                Ok(Some(RoutineLinkOutcome::Terminal { reason }))
            }
            ReadOccurrenceLinkReply::Missing { .. } => Ok(None),
            ReadOccurrenceLinkReply::Refused { code } => Err(refused(code)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
