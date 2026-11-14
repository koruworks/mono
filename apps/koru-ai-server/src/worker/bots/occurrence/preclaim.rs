// SPDX-License-Identifier: MPL-2.0
//! Explicit current synchronization, separate from immutable Prepare replay.
use super::jobs::{Evidence, Phase};
use super::*;
use koru_delegate_core::{
    ActorRef, BotAdmissionBarrier, BotBootAdmission, BotPolicyBarrier, BotPolicyBarrierState,
    FreshBotPolicyProof, RoutinePolicyBarrier,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Snapshot {
    row: BotRoutineStoredPreparation,
    creation: BotRoutineCreation,
    metadata: BotPolicyMetadata,
}
impl Snapshot {
    fn profile(&self) -> Result<koru_ai_core::ProfileId, RoutineBridgeRefusal> {
        let ActorRef::Person { profile } =
            self.row.capture.schedule_definition.template.requested_by
        else {
            return Err(RoutineBridgeRefusal::Forbidden);
        };
        if self
            .creation
            .identity
            .seed
            .header
            .definition
            .template
            .requested_by
            != (ActorRef::Person { profile })
        {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        Ok(profile)
    }
}

pub(super) fn snapshot(
    repository: &Repository,
    request: &SynchronizeRoutinePreclaim,
) -> Result<Snapshot, RoutineBridgeRefusal> {
    let (row, creation, metadata) = repository
        .routine_preclaim_snapshot(&request.slot, request.semantic_hash)
        .map_err(refusal)?
        .ok_or(RoutineBridgeRefusal::Stale)?;
    if row.prepared.slot != request.slot
        || row.prepared.preparation_hash != request.preparation_hash
        || row.capture.creation != creation.identity.route()
        || row.capture.grant != creation.identity.grant
    {
        return Err(RoutineBridgeRefusal::Conflict);
    }
    if metadata.lifecycle != BotLifecycle::Active
        || metadata.paused
        || metadata.pending_lifecycle.is_some()
        || metadata.pending_change.is_some()
    {
        return Err(RoutineBridgeRefusal::Unavailable);
    }
    if row.capture.canonical.config_revision != metadata.revision
        || row.capture.canonical.policy_revision != metadata.policy_revision
    {
        return Err(RoutineBridgeRefusal::Conflict);
    }
    let snapshot = Snapshot {
        row,
        creation,
        metadata,
    };
    snapshot.profile()?;
    Ok(snapshot)
}

#[derive(Clone)]
pub(super) struct Hint {
    snapshot: Snapshot,
    hint: BotBootAdmission,
}
#[derive(Clone)]
pub(super) struct Install {
    observed: Hint,
    admission: BotAdmissionBarrier,
    global: BotPolicyBarrier,
    routine: RoutinePolicyBarrier,
}

fn validate_install(
    repository: &Repository,
    request: &SynchronizeRoutinePreclaim,
    install: &Install,
) -> Result<(), RoutineBridgeRefusal> {
    if snapshot(repository, request)? != install.observed.snapshot {
        return Err(RoutineBridgeRefusal::Conflict);
    }
    let admission = repository
        .bot_admission_barrier(&install.observed.snapshot.metadata, &install.observed.hint)
        .map_err(refusal)?;
    // H and installed A are retained. A changed selector never becomes a
    // replacement success tuple, even when H's old epoch/absence is stale.
    if admission != install.admission {
        return Err(RoutineBridgeRefusal::Conflict);
    }
    Ok(())
}

impl OccurrenceJob {
    fn preclaim_authority(&self, snapshot: &Snapshot) -> Result<(), RoutineBridgeRefusal> {
        let current = self.current(snapshot.metadata.home_workspace)?;
        if current.profile != snapshot.profile()? {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        self.live()
    }

    pub(super) fn preclaim_read(
        &self,
        snapshot: &Snapshot,
    ) -> Result<Evidence, RoutineBridgeRefusal> {
        self.preclaim_authority(snapshot)?;
        let slot = self.operation.key.slot;
        let hint = self
            .delegate
            .read_admission_barrier(
                slot.owner,
                slot.bot,
                self.operation
                    .context
                    .deadline
                    .saturating_duration_since(Instant::now()),
            )
            .map_err(|_| RoutineBridgeRefusal::Unavailable)?;
        self.live()?;
        self.preclaim_authority(snapshot)?;
        Ok(Evidence::PreclaimRead(Box::new(Hint {
            snapshot: snapshot.clone(),
            hint,
        })))
    }

    pub(super) fn preclaim_install(
        &self,
        install: &Install,
    ) -> Result<Evidence, RoutineBridgeRefusal> {
        self.preclaim_authority(&install.observed.snapshot)?;
        let answer = self.call(DelegateCommand::Barrier {
            barrier: install.admission.clone(),
        })?;
        self.live()?;
        if !matches!(answer, BotReply::Barrier { barrier } if barrier == install.admission) {
            return Err(RoutineBridgeRefusal::Conflict);
        }
        let answer = self.call(DelegateCommand::SynchronizePolicy {
            barrier: install.global.clone(),
        })?;
        self.live()?;
        match answer {
            BotReply::PolicyBarrier { receipt } if receipt.barrier == install.global => {
                if receipt.state != BotPolicyBarrierState::Effective {
                    return Err(RoutineBridgeRefusal::Unavailable);
                }
            }
            _ => return Err(RoutineBridgeRefusal::Conflict),
        }
        let answer = self.call(DelegateCommand::SynchronizeRoutinePolicy {
            barrier: install.routine.clone(),
        })?;
        self.live()?;
        match answer {
            BotReply::RoutinePolicyBarrier { barrier, state } if barrier == install.routine => {
                if state != BotPolicyBarrierState::Effective {
                    return Err(RoutineBridgeRefusal::Unavailable);
                }
            }
            _ => return Err(RoutineBridgeRefusal::Conflict),
        }
        self.preclaim_authority(&install.observed.snapshot)?;
        Ok(Evidence::PreclaimInstalled(Box::new(install.clone())))
    }
}

impl WorkerState {
    fn recheck_preclaim(
        &self,
        operation: &Operation,
        observed: &Snapshot,
    ) -> Result<(), RoutineBridgeRefusal> {
        if !operation.context.live() {
            return Err(RoutineBridgeRefusal::Unavailable);
        }
        let Request::Preclaim(request) = &operation.request else {
            return Err(RoutineBridgeRefusal::Conflict);
        };
        if snapshot(&self.repository, request)? != *observed {
            return Err(RoutineBridgeRefusal::Conflict);
        }
        Ok(())
    }

    pub(super) fn complete_preclaim_read(&mut self, operation: Operation, observed: Hint) {
        let install = (|| {
            self.recheck_preclaim(&operation, &observed.snapshot)?;
            let metadata = &observed.snapshot.metadata;
            let admission = self
                .repository
                .bot_admission_barrier(metadata, &observed.hint)
                .map_err(refusal)?;
            if admission.owner != operation.key.slot.owner
                || admission.bot != operation.key.slot.bot
                || admission.epoch != metadata.revision
                || admission.paused
            {
                return Err(RoutineBridgeRefusal::Conflict);
            }
            let global = policy::global_barrier(
                metadata.owner,
                metadata.id,
                metadata.revision,
                metadata.policy_revision,
                metadata.policy(),
            )
            .map_err(|_| RoutineBridgeRefusal::Unavailable)?;
            let routine = policy::initial_routine_barrier(&observed.snapshot.creation.identity);
            Ok(Install {
                observed,
                admission,
                global,
                routine,
            })
        })();
        match install {
            Ok(install) => {
                self.queue_occurrence(operation, Phase::PreclaimInstall(Box::new(install)))
            }
            Err(code) => self.refuse_occurrence(operation, code),
        }
    }

    pub(super) fn complete_preclaim_install(&mut self, operation: Operation, install: Install) {
        let result = (|| {
            if !operation.context.live() {
                return Err(RoutineBridgeRefusal::Unavailable);
            }
            let Request::Preclaim(request) = &operation.request else {
                return Err(RoutineBridgeRefusal::Conflict);
            };
            validate_install(&self.repository, request, &install)?;
            let reply = SynchronizeRoutinePreclaimReply::Ready {
                slot: request.slot,
                semantic_hash: request.semantic_hash,
                preparation_hash: request.preparation_hash,
                admission: install.admission,
                proof: FreshBotPolicyProof {
                    global: install.global,
                    routine: Some(install.routine),
                },
            };
            if !reply.matches_request(request) {
                return Err(RoutineBridgeRefusal::Conflict);
            }
            if !operation.context.live() {
                return Err(RoutineBridgeRefusal::Unavailable);
            }
            Ok(reply)
        })();
        match result {
            Ok(reply) => {
                self.end_occurrence(&operation, false);
                if let Reply::Preclaim(response) = operation.reply {
                    response.send(Ok(reply));
                }
            }
            Err(code) => self.refuse_occurrence(operation, code),
        }
    }
}

#[cfg(test)]
mod tests;
