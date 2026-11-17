// SPDX-License-Identifier: MPL-2.0

//! Credential checks happen at the private server boundary; this owner repeats
//! the trusted device binding and owns all effects and queue transitions.
use super::*;
use koru_delegate_api::bots::{BotCommand, BotObservation, BotReply};
use koru_delegate_core::{BotRunKey, WorkspaceScope};

fn unavailable(code: &'static str) -> Failure {
    Failure::new(code, "The Bot operation is not available.", false)
}
pub(super) fn bot_failure(error: RunError) -> Failure {
    match error {
        RunError::Port(error)
            if matches!(
                error.code.as_str(),
                "bot_native_effects_pending"
                    | "bot_delivery_pending"
                    | "bot_stop_unconfirmed"
                    | "bot_interrupt_pending"
            ) =>
        {
            Failure::new(
                error.code,
                "The Bot operation is still being reconciled.",
                true,
            )
        }
        // IPC 18: each new private refusal has its one fixed text. None
        // interpolates a value and none is retryable.
        RunError::Port(error)
            if koru_delegate_api::bots::private_failure(&error.code).is_some() =>
        {
            koru_delegate_api::bots::private_failure(&error.code).expect("checked above")
        }
        // IPC 19: each child refusal has its one fixed text. Exactly one,
        // `bot_child_wait_closing`, is retryable.
        RunError::Port(error) if koru_delegate_api::bots::child_failure(&error.code).is_some() => {
            koru_delegate_api::bots::child_failure(&error.code).expect("checked above")
        }
        RunError::Port(error) if error.code.starts_with("bot_") => {
            Failure::new(error.code, "The Bot operation was refused.", false)
        }
        error => failure_of(&error),
    }
}
/// The exact retained decision as the IPC16 reply carries it.
fn decision_view(
    record: koru_delegate_sqlite::DecisionRecord,
) -> koru_delegate_api::bots::DecisionReceiptView {
    koru_delegate_api::bots::DecisionReceiptView {
        receipt: record.receipt,
        wait: record.wait.wait,
        wait_phase: record.wait.phase,
        wait_revision: record.wait.revision,
        native_response: record.native_response,
        resume: record.resume,
    }
}
impl Actor {
    pub(super) fn refuse_ordinary_bot_route(&mut self, command: &Command) -> bool {
        let (run, workspace, reply) = match command {
            Command::Create { request, reply } => {
                if matches!(request.scope, WorkspaceScope::BotFolder { .. }) {
                    let _ =
                        reply.send(Response::Failure(unavailable("bot_private_route_required")));
                    return true;
                }
                // A new UUID has no document. An existing Bot intent must not
                // be retried through ordinary Create even with changed scope.
                match self.orchestrator.is_bot_run(request.run, None) {
                    Ok(true) => {
                        let _ = reply
                            .send(Response::Failure(unavailable("bot_private_route_required")));
                        return true;
                    }
                    Ok(false) | Err(RunError::UnknownRun) => return false,
                    Err(error) => {
                        let _ = reply.send(Response::Failure(failure_of(&error)));
                        return true;
                    }
                }
            }
            Command::Run { run, window, reply } => (*run, window.workspace, reply),
            Command::Cancel { run, reply }
            | Command::Resume { run, reply, .. }
            | Command::Rate { run, reply, .. }
            | Command::Trash { run, reply, .. }
            | Command::Restore { run, reply }
            | Command::Purge { run, reply } => (*run, None, reply),
            Command::Stream {
                run,
                workspace,
                sink,
                ..
            } => {
                if !matches!(self.orchestrator.is_bot_run(*run, *workspace), Ok(false)) {
                    let _ = sink.try_send(RunStreamItem::Closed);
                    return true;
                }
                return false;
            }
            _ => return false,
        };
        match self.orchestrator.is_bot_run(run, workspace) {
            Ok(false) | Err(RunError::UnknownRun) => false,
            result => {
                let failure = match result {
                    Err(error) => failure_of(&error),
                    Ok(true) if matches!(command, Command::Purge { .. }) => Failure::new(
                        "bot_retained_state",
                        "This Bot Run retains task and recovery state and cannot be permanently deleted.",
                        false,
                    ),
                    _ => unavailable("bot_private_route_required"),
                };
                let _ = reply.send(Response::Failure(failure));
                true
            }
        }
    }

    pub(super) fn bot_command(&mut self, command: BotCommand) -> Result<BotReply, Failure> {
        if self.device.as_uuid().is_nil()
            || command
                .owner()
                .is_some_and(|owner| owner.device != self.device)
        {
            return Err(unavailable("bot_owner_mismatch"));
        }
        // IPC 18: a private command that is malformed, or over its 16 KiB
        // bound, is refused before anything reads the ledger.
        if command.private_valid() == Some(false) {
            return Err(unavailable("bot_invalid"));
        }
        // IPC 19: the same for a child command.
        if command.child_valid() == Some(false) {
            return Err(unavailable("bot_invalid"));
        }
        let device = self.device;
        let reply = match command {
            // IPC 17 owner controls belong to the scheduler coordinator, which
            // owns the Schedule document write; the server routes them there
            // before this actor is reached. These arms exist because the match
            // is exhaustive. They refuse, write nothing, and never wait on a
            // Store commit.
            BotCommand::ControlRoutine { .. } | BotCommand::TestRoutine { .. } => {
                return Err(Failure::new(
                    "schedule_unavailable",
                    "Routine controls are served by the schedule coordinator.",
                    true,
                ));
            }
            BotCommand::ReadRoutineClaim { slot } => BotReply::RoutineClaim {
                observation: self
                    .orchestrator
                    .read_routine_claim(&slot, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::AcknowledgeRoutineLink { acknowledgement } => {
                let record = self
                    .orchestrator
                    .acknowledge_routine_link(&acknowledgement, device)
                    .map_err(bot_failure)?;
                if record.link_state == (koru_delegate_core::RoutineLinkState::Linked {}) {
                    self.wake_bots_after_replay();
                }
                BotReply::RoutineLinkAcknowledged { record }
            }
            BotCommand::RegisterRoutineRoute { identity } => BotReply::RoutineRoute {
                identity: self
                    .orchestrator
                    .register_routine_route(&identity, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::Hello { owner, scan } => BotReply::Hello {
                hello: self
                    .orchestrator
                    .bot_hello(owner, scan, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::SynchronizeBootBot {
                boot,
                barrier,
                policy,
            } => {
                let receipt = self
                    .orchestrator
                    .synchronize_boot_bot(boot, barrier, policy, device)
                    .map_err(bot_failure)?;
                if receipt.state == koru_delegate_core::BotBootState::Ready {
                    self.wake_bots_after_replay();
                }
                BotReply::BootSynchronized { receipt }
            }
            BotCommand::PendingNativeEffects {
                owner,
                bot,
                after,
                limit,
            } => BotReply::NativePendingEffects {
                page: self
                    .orchestrator
                    .pending_native_effects(owner, bot, after.as_ref(), limit, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::CheckNativeEffectDrain { key, reason } => {
                BotReply::NativeDrainEligibility {
                    target: self
                        .orchestrator
                        .check_native_effect_drain(&key, &reason, device)
                        .map_err(bot_failure)?,
                }
            }
            BotCommand::ResolveRunAuthority { run, generation } => BotReply::RunAuthority {
                authority: self
                    .orchestrator
                    .resolve_run_tool_authority(run, generation, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::EffectFence { fence } => BotReply::EffectFence {
                policy: self
                    .orchestrator
                    .native_effect_fence(&fence, device)
                    .map_err(bot_failure)?,
                fence,
            },
            BotCommand::SynchronizePolicy { barrier } => BotReply::PolicyBarrier {
                receipt: self
                    .orchestrator
                    .synchronize_native_policy(barrier, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ReconcilePolicyBarrier { key } => BotReply::PolicyBarrier {
                receipt: self
                    .orchestrator
                    .reconcile_native_policy(&key, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::SynchronizeRoutinePolicy { barrier } => {
                let state = self
                    .orchestrator
                    .synchronize_routine_policy(barrier.clone(), device)
                    .map_err(bot_failure)?;
                self.wake_bots_after_replay();
                BotReply::RoutinePolicyBarrier { barrier, state }
            }
            BotCommand::PrepareNativeResult { request } => BotReply::NativeResult {
                intent: self
                    .orchestrator
                    .prepare_native_effect(&request, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ReadMemoryEditAllocation {
                run,
                operation_id,
                caller_fingerprint,
            } => {
                match self
                    .orchestrator
                    .read_memory_edit_allocation(run, operation_id, caller_fingerprint, device)
                    .map_err(bot_failure)?
                {
                    Some(allocation) => BotReply::MemoryEditAllocation { allocation },
                    None => BotReply::Missing,
                }
            }
            BotCommand::AllocateMemoryEdit { request } => BotReply::MemoryEditAllocation {
                allocation: self
                    .orchestrator
                    .allocate_memory_edit(&request, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ResolveMemoryEditBase {
                key,
                caller_fingerprint,
                authority,
                expected_version,
                expected_revision,
                current_config_revision,
                current_policy_revision,
            } => BotReply::MemoryEditAllocation {
                allocation: self
                    .orchestrator
                    .resolve_memory_edit_base(
                        &key,
                        caller_fingerprint,
                        &koru_delegate_core::MemoryBaseResolution::Resolved {
                            authority,
                            version: expected_version,
                            expected_revision,
                        },
                        current_config_revision,
                        current_policy_revision,
                        device,
                    )
                    .map_err(bot_failure)?,
            },
            BotCommand::PrepareMemoryEdit {
                request,
                caller_fingerprint,
                authority,
            } => BotReply::NativeResult {
                intent: self
                    .orchestrator
                    .prepare_memory_edit(&request, caller_fingerprint, &authority, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ReconcileNativeResult { key } => match self
                .orchestrator
                .reconcile_native_result_if_present(&key, device)
                .map_err(bot_failure)?
            {
                Some(intent) => BotReply::NativeResult { intent },
                None => BotReply::Missing,
            },
            BotCommand::ConfirmNativeResult { request } => BotReply::NativeResult {
                intent: self
                    .orchestrator
                    .confirm_native_result(&request, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::Submit { submission } => {
                let run = submission.request.run;
                let admission = self
                    .orchestrator
                    .bot_submit(submission, device)
                    .map_err(bot_failure)?;
                self.orchestrator.settle(run).map_err(bot_failure)?;
                BotReply::Admission { admission }
            }
            BotCommand::Reconcile { key } => match self
                .orchestrator
                .bot_reconcile(&key, device)
                .map_err(bot_failure)?
            {
                Some(admission) => BotReply::Admission { admission },
                None => BotReply::Missing,
            },
            BotCommand::Barrier { barrier } => BotReply::Barrier {
                barrier: self
                    .orchestrator
                    .bot_barrier(&barrier, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ReadAdmissionBarrier { owner, bot } => BotReply::AdmissionBarrier {
                owner,
                bot,
                admission: self
                    .orchestrator
                    .bot_read_admission_barrier(owner, bot, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::UnsettledWork { owner, bot, limit } => BotReply::UnsettledWork {
                work: self
                    .orchestrator
                    .bot_unsettled(owner, bot, limit, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::Read { key, window } => {
                self.orchestrator
                    .bot_read_allowed(&key, device)
                    .map_err(bot_failure)?;
                BotReply::Run {
                    view: self.view(
                        key.run,
                        window.workspace,
                        window.from,
                        window.bounded_len(),
                    )?,
                }
            }
            BotCommand::Observe { fence, after } => {
                let observation = self
                    .orchestrator
                    .bot_observe(&fence, device, after)
                    .map_err(bot_failure)?;
                BotReply::Observation {
                    observation: BotObservation { fence, observation },
                }
            }
            BotCommand::BeginTurn { fence, input } => {
                let turn = self
                    .orchestrator
                    .bot_begin_turn(&fence, device, input)
                    .map_err(bot_failure)?;
                self.orchestrator.settle(fence.run).map_err(bot_failure)?;
                BotReply::Turn { turn }
            }
            BotCommand::Steer { fence, input } => BotReply::Turn {
                turn: self
                    .orchestrator
                    .bot_steer(&fence, device, input)
                    .map_err(bot_failure)?,
            },
            BotCommand::AnswerApproval {
                fence,
                request,
                approval,
                decision,
                choice,
            } => {
                self.orchestrator
                    .bot_answer_approval_keyed(&fence, device, request, approval, decision, choice)
                    .map_err(bot_failure)?;
                BotReply::Applied
            }
            BotCommand::AnswerQuestions {
                fence,
                request,
                answers,
            } => {
                self.orchestrator
                    .bot_answer_questions(&fence, device, request, answers)
                    .map_err(bot_failure)?;
                BotReply::Applied
            }
            BotCommand::Interrupt { key, fence } => {
                // A pending reply may follow a durable status change; watchers
                // learn it either way. The caller wakes the queue (handle).
                let result = self
                    .orchestrator
                    .bot_interrupt(&key, device, fence.as_ref());
                self.notify(key.run);
                result.map_err(bot_failure)?;
                BotReply::Applied
            }
            BotCommand::Stop { key, fence } => {
                self.orchestrator
                    .bot_stop(&key, device, fence.as_ref())
                    .map_err(bot_failure)?;
                self.notify(key.run);
                BotReply::Applied
            }
            BotCommand::SkipQuestions { fence, request } => {
                self.orchestrator
                    .bot_skip_questions(&fence, device, request)
                    .map_err(bot_failure)?;
                BotReply::Applied
            }
            BotCommand::IngestAttachment {
                fence,
                attachment_id,
                source,
                data_base64,
            } => BotReply::Attachment {
                attachment: self
                    .orchestrator
                    .bot_ingest_attachment(&fence, device, attachment_id, source, data_base64)
                    .map_err(bot_failure)?,
            },
            BotCommand::RemoveAttachment {
                fence,
                attachment_id,
            } => BotReply::Attachment {
                attachment: self
                    .orchestrator
                    .bot_remove_attachment(&fence, device, attachment_id)
                    .map_err(bot_failure)?,
            },
            BotCommand::ListAttachments { fence } => BotReply::Attachments {
                attachments: self
                    .orchestrator
                    .bot_attachments(&fence, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::AcknowledgeDelivery { key, receipt } => {
                self.orchestrator
                    .bot_acknowledge_delivery(&key, device, &receipt)
                    .map_err(bot_failure)?;
                self.notify(key.run);
                self.wake_bots();
                BotReply::Applied
            }
            BotCommand::PrepareNativeDecision {
                fence,
                approval,
                proof,
                boot,
            } => BotReply::Decision {
                decision: decision_view(
                    self.orchestrator
                        .bot_prepare_native_decision(&fence, device, approval, &proof, boot)
                        .map_err(bot_failure)?,
                ),
            },
            BotCommand::PrepareEditDecision {
                fence,
                edit,
                proof,
                boot,
            } => {
                let edit = koru_delegate_runtime::DecisionEditRequest {
                    target: edit.target,
                    action: edit.action,
                    operation_id: edit.operation_id,
                    edit_fingerprint: edit.edit_fingerprint,
                    expected_revision: edit.expected_revision.as_slice().to_vec(),
                    payload: edit.payload.as_slice().to_vec(),
                };
                BotReply::Decision {
                    decision: decision_view(
                        self.orchestrator
                            .bot_prepare_edit_decision(&fence, device, &edit, &proof, boot)
                            .map_err(bot_failure)?,
                    ),
                }
            }
            BotCommand::AnswerDecision {
                answer,
                proof,
                boot,
            } => {
                // The private Bot channel is the trusted human route: the
                // server admitted its credential, `owner()` bound the device,
                // and the ledger binds the answer to the decision's own human,
                // exact revision and binding digest in its writer.
                let answer = koru_delegate_sqlite::AnswerDecision {
                    owner: answer.owner,
                    run: answer.run,
                    decision: answer.decision,
                    expected_revision: answer.expected_revision,
                    binding_digest: answer.binding_digest,
                    request: answer.request,
                    human: answer.human,
                    choice: answer.choice,
                    authority_digest: answer.authority_digest,
                };
                let record = self
                    .orchestrator
                    .bot_answer_decision(&answer, device, &proof, boot)
                    .map_err(bot_failure)?;
                self.notify(answer.run);
                BotReply::Decision {
                    decision: decision_view(record),
                }
            }
            BotCommand::ConsumeDecisionEdit {
                fence,
                request,
                payload,
                proof,
                boot,
            } => {
                let leased = self
                    .orchestrator
                    .bot_consume_decision_edit(
                        &fence,
                        device,
                        &request,
                        payload.as_slice(),
                        &proof,
                        boot,
                    )
                    .map_err(bot_failure)?;
                // A retained row never carries Execute; only the first fresh
                // commit, which returned its one lease, does.
                let mut intent = leased.intent.view();
                if leased.execute.is_some() {
                    intent.disposition = koru_delegate_core::NativeResultDisposition::Execute;
                }
                BotReply::NativeResult { intent }
            }
            BotCommand::ReadBotDecisionWait {
                owner,
                bot,
                run,
                wait_id,
                decision_id,
            } => match self
                .orchestrator
                .bot_read_decision_wait_and_resume(owner, bot, run, wait_id, decision_id, device)
                .map_err(bot_failure)?
            {
                (Some(observation), resume) => BotReply::DecisionWait {
                    observation,
                    resume,
                },
                // The exact owner/subject read succeeded and found no frozen
                // cleanup intent; an error above is never reported as Missing.
                (None, _) => BotReply::Missing,
            },
            BotCommand::AnnouncePrivateDispatcher { owner, dispatcher } => {
                let resolved = self
                    .orchestrator
                    .bot_announce_private_dispatcher(owner, dispatcher, device)
                    .map_err(bot_failure)?;
                BotReply::PrivateDispatcher {
                    dispatcher,
                    resolved,
                }
            }
            BotCommand::PreparePrivateDecision {
                fence,
                operation,
                proof,
                boot,
            } => BotReply::Decision {
                decision: decision_view(
                    self.orchestrator
                        .bot_prepare_private_decision(&fence, device, &operation, &proof, boot)
                        .map_err(bot_failure)?,
                ),
            },
            BotCommand::ConsumePrivateDecision {
                fence,
                dispatcher,
                operation,
                proof,
                boot,
            } => BotReply::PrivateEffect {
                effect: self
                    .orchestrator
                    .bot_consume_private_decision(
                        &fence, device, dispatcher, &operation, &proof, boot,
                    )
                    .map_err(bot_failure)?,
            },
            BotCommand::PreparePrivateEffect {
                fence,
                dispatcher,
                operation,
                proof,
                boot,
            } => BotReply::PrivateEffect {
                effect: self
                    .orchestrator
                    .bot_prepare_private_effect(
                        &fence, device, dispatcher, &operation, &proof, boot,
                    )
                    .map_err(bot_failure)?,
            },
            BotCommand::AdvancePrivateEffect {
                key,
                dispatcher,
                transition,
            } => {
                let effect = self
                    .orchestrator
                    .bot_advance_private_effect(&key, dispatcher, &transition, device)
                    .map_err(bot_failure)?;
                // A final row may have settled its decision; watchers of the
                // Run learn it, and the owed Control is published next turn.
                self.notify(key.run);
                BotReply::PrivateEffect { effect }
            }
            BotCommand::ReadPrivateEffect {
                run,
                operation_id,
                dispatcher,
            } => match self
                .orchestrator
                .bot_read_private_effect(run, operation_id, dispatcher, device)
                .map_err(bot_failure)?
            {
                Some(effect) => BotReply::PrivateEffect { effect },
                // The Run's exact owner read succeeded and found no effect;
                // an error above is never reported as Missing.
                None => BotReply::Missing,
            },
            BotCommand::RequestChild { request, boot } => {
                use koru_delegate_api::bots::ChildRequestedOutcome;
                use koru_delegate_core::children::ChildRequestOutcome;
                let outcome = self
                    .orchestrator
                    .bot_request_child(&request, device, boot)
                    .map_err(bot_failure)?;
                let (outcome, requested) = match outcome {
                    ChildRequestOutcome::Created(requested) => {
                        (ChildRequestedOutcome::Created, requested)
                    }
                    ChildRequestOutcome::AlreadyCreated(requested) => {
                        (ChildRequestedOutcome::AlreadyCreated, requested)
                    }
                    // A refusal committed nothing. Its code is final on the
                    // wire, except a wait that is still closing.
                    ChildRequestOutcome::Refused(refused) => {
                        return Err(koru_delegate_api::bots::child_refused(refused));
                    }
                };
                self.notify(request.parent.run);
                BotReply::ChildRequested {
                    outcome,
                    wait: requested.wait,
                    enrollment: requested.enrollment,
                    receipts: requested.receipts,
                }
            }
            BotCommand::ReadChildRequests { subject } => {
                let read = self
                    .orchestrator
                    .bot_read_child_requests(&subject, device)
                    .map_err(bot_failure)?;
                BotReply::ChildRequests {
                    wait: read.wait,
                    requests: read.requests,
                }
            }
            BotCommand::ReadBotChildWait {
                owner,
                bot,
                run,
                wait_id,
            } => match self
                .orchestrator
                .bot_read_child_wait(owner, bot, run, wait_id, device)
                .map_err(bot_failure)?
            {
                Some(observation) => BotReply::ChildWait { observation },
                // The exact owner and Run read succeeded and found no such
                // wait; an error above is never reported as unknown.
                None => {
                    return Err(koru_delegate_api::bots::child_failure(
                        koru_delegate_api::bots::CHILD_WAIT_UNKNOWN,
                    )
                    .expect("a listed code"));
                }
            },
            BotCommand::TakeOver { .. } | BotCommand::HandBack { .. } => {
                return Err(unavailable("bot_control_unsupported"));
            }
            BotCommand::Trash { key, lifecycle } | BotCommand::Restore { key, lifecycle } => {
                if !lifecycle.valid_for(&key) {
                    return Err(unavailable("bot_fenced"));
                }
                return Err(unavailable("bot_lifecycle_unavailable"));
            }
        };
        Ok(reply)
    }

    pub(super) fn wake_bots(&mut self) {
        if !self.bot_drain_pending {
            self.bot_queue_cursor = 0;
            self.bot_drain_pending = self.commands.send(Command::BotDrain).is_ok();
        }
    }
    fn wake_bots_after_replay(&mut self) {
        if self.bot_drain_pending {
            // Finish the current sweep first: resetting its cursor for every
            // replay would starve later entries. One additional pass is enough.
            self.bot_drain_again = true;
        } else {
            self.wake_bots();
        }
    }
    pub(super) fn drain_one_bot(&mut self) {
        // One lightweight candidate and at most its one selected payload per
        // mailbox turn. Paused/busy entries advance the cursor, never starve
        // later pages. A completed sweep sleeps until a relevant event.
        let candidate = match self.orchestrator.bot_candidates(self.bot_queue_cursor, 1) {
            Ok(rows) => rows.into_iter().next(),
            Err(_) => None,
        };
        let Some(candidate) = candidate else {
            self.bot_drain_pending = false;
            if std::mem::take(&mut self.bot_drain_again) {
                self.wake_bots();
            }
            return;
        };
        self.bot_queue_cursor = candidate.sequence;
        // Existing schedule admission is exclusive across its claim boundary.
        // Retained schedule claims must be resolved before Bot dispatch too.
        if self.schedule_admission.is_none() {
            let reserved: Option<Vec<_>> = self
                .claimed_schedule_retries
                .iter()
                .map(|reservation| self.reservation_identity(reservation))
                .collect();
            if let Some(reserved) = reserved {
                let key = BotRunKey {
                    owner: candidate.owner,
                    bot: candidate.bot,
                    run: candidate.run,
                };
                if let Ok(Some(started)) =
                    self.orchestrator
                        .bot_dispatch_reserved(&key, self.device, &reserved)
                {
                    self.pump(started);
                }
            }
        }
        if self.commands.send(Command::BotDrain).is_err() {
            self.bot_drain_pending = false;
        }
    }
}

#[cfg(test)]
mod native_reply_tests {
    use super::*;
    #[test]
    fn purge_retention_failures_explain_retained_state_and_only_unavailability_retries() {
        for code in [
            "bot_retained_state",
            "bot_retention_inconsistent",
            "bot_retention_unavailable",
        ] {
            let error = koru_delegate_runtime::PortError::new(
                koru_delegate_runtime::Port::Store,
                code,
                "Retained state cannot be safely removed.",
            );
            let failure = failure_of(&error.into());
            assert_eq!(failure.code, code);
            assert_eq!(failure.message, "Retained state cannot be safely removed.");
            assert_eq!(failure.retryable, code == "bot_retention_unavailable");
        }
    }
    #[test]
    fn pending_native_stop_is_retryable_failure_never_applied() {
        let failure = bot_failure(
            koru_delegate_runtime::PortError::new(
                koru_delegate_runtime::Port::Store,
                "bot_native_effects_pending",
                "fixture",
            )
            .into(),
        );
        assert_eq!(failure.code, "bot_native_effects_pending");
        assert!(failure.retryable);
    }
}
