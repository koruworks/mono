// SPDX-License-Identifier: MPL-2.0
use super::jobs::{Evidence, Fresh, Phase};
use super::*;

impl WorkerState {
    fn reject_occurrence_incoming(&mut self, incoming: Incoming, code: RoutineBridgeRefusal) {
        if matches!(&incoming.request, Request::Recover(_)) {
            self.occurrences.recovery.finished(true);
        }
        incoming.reply.refuse(code);
    }
    pub(in crate::worker) fn begin_routine_occurrence(&mut self, incoming: Incoming) {
        let (owner, epoch) = self.routine_recovery.owner_epoch();
        self.occurrences.synchronize(owner, epoch);
        let identity = match incoming.request.identity().and_then(|identity| {
            incoming.context.validate()?;
            Ok(identity)
        }) {
            Ok(identity) => identity,
            Err(error) => {
                self.reject_occurrence_incoming(incoming, error);
                return;
            }
        };
        let slot = incoming.request.slot();
        if owner != Some(ai_owner(slot)) {
            let code = if owner.is_some() {
                RoutineBridgeRefusal::Forbidden
            } else {
                RoutineBridgeRefusal::Unavailable
            };
            self.reject_occurrence_incoming(incoming, code);
            return;
        }
        if self
            .occurrences
            .active
            .values()
            .any(|active| active.key.request_id == incoming.context.request_id)
        {
            self.reject_occurrence_incoming(incoming, RoutineBridgeRefusal::Conflict);
            return;
        }
        let Some(serial) = self.occurrences.serial.checked_add(1) else {
            self.reject_occurrence_incoming(incoming, RoutineBridgeRefusal::Unavailable);
            return;
        };
        self.occurrences.serial = serial;
        let key = Key {
            request_id: incoming.context.request_id,
            serial,
            owner_epoch: epoch,
            slot,
            identity,
            phase: 0,
        };
        self.occurrences.active.insert(
            serial,
            Active {
                key: key.clone(),
                cancellation: incoming.context.cancellation.clone(),
            },
        );
        let operation = Operation {
            key,
            request: incoming.request,
            reply: incoming.reply,
            context: incoming.context,
            _permit: incoming.permit,
        };
        let phase = self.initial_occurrence_phase(&operation);
        match phase {
            Ok(phase) => self.queue_occurrence(operation, phase),
            Err(error) => self.refuse_occurrence(operation, error),
        }
    }

    fn occurrence_row(
        &self,
        operation: &Operation,
    ) -> Result<Option<BotRoutineStoredPreparation>, RoutineBridgeRefusal> {
        match &operation.key.identity {
            Identity::Semantic(hash) => self
                .repository
                .read_routine_preparation(&operation.key.slot, *hash),
            Identity::Claim(claim) => self.repository.routine_preparation_for_claim(claim),
        }
        .map_err(refusal)
    }
    fn occurrence_home(&self, operation: &Operation) -> Result<WorkspaceId, RoutineBridgeRefusal> {
        self.repository
            .routine_occurrence_home(ai_owner(operation.key.slot), operation.key.slot.bot)
            .map_err(refusal)
    }
    fn observation_phase(
        &self,
        operation: &Operation,
        row: Option<&BotRoutineStoredPreparation>,
    ) -> Result<Phase, RoutineBridgeRefusal> {
        let home = self.occurrence_home(operation)?;
        let claim = matches!(
            &operation.request,
            Request::Finalize(_) | Request::Link(_) | Request::Recover(_)
        ) || row.is_some_and(|row| row.stage == BotRoutineStoredStage::Linked);
        Ok(Phase::Observe { home, claim })
    }
    fn initial_occurrence_phase(
        &self,
        operation: &Operation,
    ) -> Result<Phase, RoutineBridgeRefusal> {
        // This is the first repository access: no current model, Store or
        // captured content is read before exact immutable slot replay.
        let row = self.occurrence_row(operation)?;
        let Request::Prepare(request) = &operation.request else {
            return self.observation_phase(operation, row.as_ref());
        };
        if row.is_some() {
            return self.observation_phase(operation, row.as_ref());
        }
        if !self
            .repository
            .routine_preparation_capacity(&request.slot)
            .map_err(refusal)?
        {
            return Err(RoutineBridgeRefusal::Capacity);
        }
        let (creation, canonical, profile) = self
            .repository
            .routine_occurrence_source(&request.slot)
            .map_err(refusal)?;
        if canonical.home != request.workspace {
            return Err(RoutineBridgeRefusal::Forbidden);
        }
        let bot = self
            .repository
            .bot(request.slot.bot)
            .map_err(refusal)?
            .ok_or(RoutineBridgeRefusal::Stale)?;
        if bot.user_scope != request.slot.owner.user_scope
            || bot.owner_device != request.slot.owner.device
            || !canonical.matches_bot(&bot)
        {
            return Err(RoutineBridgeRefusal::Conflict);
        }
        let documents = request
            .schedule_definition
            .template
            .context
            .iter()
            .map(|pointer| match pointer {
                RoutineScheduleContextPointer::Document {
                    workspace,
                    document,
                } => Ok(BotPage {
                    workspace: *workspace,
                    document: *document,
                }),
                _ => Err(RoutineBridgeRefusal::Invalid),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let library = library::LibrarySources::read(&self.repository, &bot, documents)
            .map_err(|_| RoutineBridgeRefusal::Forbidden)?;
        Ok(Phase::Capture(Box::new(Fresh {
            bot,
            creation,
            canonical,
            profile,
            library,
        })))
    }

    fn queue_occurrence(&mut self, mut operation: Operation, phase: Phase) {
        if !operation.context.live() {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
            return;
        }
        let Some(next) = operation.key.phase.checked_add(1) else {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
            return;
        };
        operation.key.phase = next;
        if let Some(active) = self.occurrences.active.get_mut(&operation.key.serial) {
            active.key = operation.key.clone();
        }
        let (Some(authority), Some(delegate)) =
            (self.bot_authority.clone(), self.bot_delegate.clone())
        else {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
            return;
        };
        let read = matches!(&phase, Phase::Observe { .. });
        let job = OccurrenceJob {
            operation,
            phase,
            authority,
            delegate,
        };
        let Some(pool) = &self.provider_admin else {
            self.refuse_occurrence(job.operation, RoutineBridgeRefusal::Unavailable);
            return;
        };
        let job = if read {
            ProviderAdminJob::RoutineRead(job)
        } else {
            ProviderAdminJob::RoutineMutation(job)
        };
        if let Err(error) = pool.try_submit(job) {
            let code = if error.failure.code == "ai_worker_saturated" {
                RoutineBridgeRefusal::Capacity
            } else {
                RoutineBridgeRefusal::Unavailable
            };
            match error.job {
                ProviderAdminJob::RoutineRead(job) | ProviderAdminJob::RoutineMutation(job) => {
                    self.refuse_occurrence(job.operation, code)
                }
                _ => unreachable!(),
            }
        }
    }
    fn end_occurrence(&mut self, operation: &Operation, owed: bool) {
        self.occurrences.finish(operation, owed);
    }
    fn refuse_occurrence(&mut self, operation: Operation, code: RoutineBridgeRefusal) {
        self.end_occurrence(&operation, true);
        if operation.key.owner_epoch == self.occurrences.epoch
            && self.occurrences.owner == Some(ai_owner(operation.key.slot))
            && matches!(
                &operation.request,
                Request::Prepare(_) | Request::Finalize(_)
            )
        {
            self.occurrences.recovery.wake();
        }
        operation.reply.refuse(code);
    }
    fn answer_occurrence(
        &mut self,
        operation: Operation,
        row: Option<&BotRoutineStoredPreparation>,
        observation: Option<&RoutineClaimObservation>,
    ) {
        self.end_occurrence(&operation, false);
        replies::answer(operation, row, observation);
    }

    pub(in crate::worker) fn complete_routine_occurrence(&mut self, result: OccurrenceResult) {
        let (owner, epoch) = self.routine_recovery.owner_epoch();
        self.occurrences.synchronize(owner, epoch);
        let operation = result.operation;
        if !self.occurrences.current(&operation.key)
            || !operation.context.live()
            || operation.context.request_id != operation.key.request_id
            || operation.request.slot() != operation.key.slot
            || operation.request.identity().as_ref() != Ok(&operation.key.identity)
        {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
            return;
        }
        let evidence = match result.evidence {
            Ok(evidence) => evidence,
            Err(error) => {
                self.refuse_occurrence(operation, error);
                return;
            }
        };
        match evidence {
            Evidence::Captured(captured) => {
                if !matches!(&operation.request, Request::Prepare(_))
                    || captured.slot != operation.key.slot
                    || captured.semantic_hash().map(Identity::Semantic).as_ref()
                        != Ok(&operation.key.identity)
                {
                    self.refuse_occurrence(operation, RoutineBridgeRefusal::Conflict);
                    return;
                }
                match self
                    .repository
                    .prepare_routine_occurrence(&captured, Timestamp::now().millis())
                {
                    Ok(BotRoutinePreparationAdmission::Ready(row)) => {
                        self.occurrences.recovery.wake();
                        match self.observation_phase(&operation, Some(&row)) {
                            Ok(phase) => self.queue_occurrence(operation, phase),
                            Err(error) => self.refuse_occurrence(operation, error),
                        }
                    }
                    Ok(BotRoutinePreparationAdmission::Capacity) => {
                        self.refuse_occurrence(operation, RoutineBridgeRefusal::Capacity)
                    }
                    Err(error) => self.refuse_occurrence(operation, refusal(error)),
                }
            }
            Evidence::Observed {
                authority,
                observation,
            } => {
                let row = match self.occurrence_row(&operation) {
                    Ok(row) => row,
                    Err(error) => {
                        self.refuse_occurrence(operation, error);
                        return;
                    }
                };
                if authority.owner != operation.key.slot.owner
                    || self.occurrence_home(&operation) != Ok(authority.home)
                {
                    self.refuse_occurrence(operation, RoutineBridgeRefusal::Forbidden);
                    return;
                }
                if matches!(
                    &operation.request,
                    Request::Finalize(_) | Request::Recover(_)
                ) {
                    self.mutate_occurrence(operation, row, authority, observation);
                } else {
                    self.answer_occurrence(operation, row.as_ref(), observation.as_ref());
                }
            }
            Evidence::Acknowledged {
                authority,
                record,
                admission,
            } => {
                self.complete_occurrence_ack(operation, authority, record, admission);
            }
        }
    }

    fn mutate_occurrence(
        &mut self,
        operation: Operation,
        row: Option<BotRoutineStoredPreparation>,
        authority: BotRoutineFinalizeAuthority,
        observation: Option<RoutineClaimObservation>,
    ) {
        let Some(row) = row else {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Stale);
            return;
        };
        let Some(observation) = observation else {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
            return;
        };
        let record = match replies::exact_record(&operation, &row, &observation) {
            Ok(record) => record,
            Err(error) => {
                if matches!(&operation.request, Request::Recover(_))
                    && row.stage == BotRoutineStoredStage::Prepared
                {
                    let reason = match &observation {
                        RoutineClaimObservation::Absent { .. } => {
                            BotRoutineCloseReason::ExpiredUnclaimed
                        }
                        RoutineClaimObservation::Skipped { .. } => {
                            BotRoutineCloseReason::SlotSkipped
                        }
                        RoutineClaimObservation::Claimed { .. } => {
                            BotRoutineCloseReason::OtherWinner
                        }
                    };
                    match self.repository.close_unclaimed_preparation(
                        &row.prepared.slot,
                        row.prepared.semantic_hash,
                        reason,
                        &observation,
                        Timestamp::now().millis(),
                    ) {
                        Ok(closed) => {
                            self.answer_occurrence(operation, Some(&closed), Some(&observation))
                        }
                        Err(_) => self.refuse_occurrence(operation, error),
                    }
                } else if matches!(&operation.request, Request::Recover(_))
                    && matches!(
                        row.stage,
                        BotRoutineStoredStage::Closed(BotRoutineStoredClosure::Unclaimed { .. })
                    )
                {
                    self.answer_occurrence(operation, Some(&row), Some(&observation));
                } else {
                    self.refuse_occurrence(operation, error);
                }
                return;
            }
        };
        let stored = match self.repository.finalize_routine_occurrence(
            record,
            &authority,
            PROMPT_TEMPLATE_VERSION,
            Timestamp::now().millis(),
        ) {
            Ok(BotRoutinePreparationAdmission::Ready(row)) => row,
            Ok(BotRoutinePreparationAdmission::Capacity) => {
                self.refuse_occurrence(operation, RoutineBridgeRefusal::Capacity);
                return;
            }
            Err(error) => {
                self.refuse_occurrence(operation, refusal(error));
                return;
            }
        };
        if matches!(record.link_state, RoutineLinkState::Terminal { .. }) {
            self.answer_occurrence(operation, Some(&stored), Some(&observation));
            return;
        }
        let outcome = match stored.stage {
            BotRoutineStoredStage::Linked => RoutineLinkOutcome::Linked {},
            BotRoutineStoredStage::Closed(BotRoutineStoredClosure::ClaimedTerminal { reason }) => {
                RoutineLinkOutcome::Terminal { reason }
            }
            _ => {
                self.refuse_occurrence(operation, RoutineBridgeRefusal::Conflict);
                return;
            }
        };
        let acknowledgement = RoutineLinkAcknowledgement {
            claim: record.claim.clone(),
            outcome,
        };
        self.queue_occurrence(
            operation,
            Phase::Acknowledge {
                home: authority.home,
                acknowledgement,
            },
        );
    }

    fn complete_occurrence_ack(
        &mut self,
        operation: Operation,
        authority: BotRoutineFinalizeAuthority,
        record: RoutineClaimRecord,
        admission: Option<koru_delegate_core::BotAdmissionView>,
    ) {
        let row = match self.occurrence_row(&operation) {
            Ok(Some(row)) => row,
            _ => {
                self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable);
                return;
            }
        };
        let observation = RoutineClaimObservation::Claimed { record };
        let record = match replies::exact_record(&operation, &row, &observation) {
            Ok(record) => record,
            Err(error) => {
                self.refuse_occurrence(operation, error);
                return;
            }
        };
        if authority.owner != operation.key.slot.owner
            || self.occurrence_home(&operation) != Ok(authority.home)
        {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Forbidden);
            return;
        }
        if matches!(record.link_state, RoutineLinkState::Terminal { .. }) {
            match self.repository.finalize_routine_occurrence(
                record,
                &authority,
                PROMPT_TEMPLATE_VERSION,
                Timestamp::now().millis(),
            ) {
                Ok(BotRoutinePreparationAdmission::Ready(row)) => {
                    self.answer_occurrence(operation, Some(&row), Some(&observation))
                }
                _ => self.refuse_occurrence(operation, RoutineBridgeRefusal::Unavailable),
            }
            return;
        }
        if row.stage != BotRoutineStoredStage::Linked
            || record.link_state != (RoutineLinkState::Linked {})
        {
            self.refuse_occurrence(operation, RoutineBridgeRefusal::Conflict);
            return;
        }
        let acknowledged = (|| {
            let receipt = self
                .repository
                .bot_readable_submission_for_run(
                    ai_owner(row.prepared.slot),
                    row.prepared.slot.bot,
                    row.prepared.ids.run,
                )
                .map_err(refusal)?
                .ok_or(RoutineBridgeRefusal::Unavailable)?;
            if receipt.stage == BotReceiptStage::Prepared {
                let admission = admission
                    .as_ref()
                    .ok_or(RoutineBridgeRefusal::Unavailable)?;
                self.repository
                    .acknowledge_bot_submission(
                        ai_owner(row.prepared.slot),
                        row.prepared.slot.bot,
                        row.prepared.ids.submission,
                        admission,
                        Timestamp::now().millis(),
                    )
                    .map_err(refusal)?;
            } else if receipt.stage != BotReceiptStage::Completed {
                return Err(RoutineBridgeRefusal::Conflict);
            }
            Ok(())
        })();
        match acknowledged {
            Ok(()) => self.answer_occurrence(operation, Some(&row), Some(&observation)),
            Err(error) => self.refuse_occurrence(operation, error),
        }
    }
}
