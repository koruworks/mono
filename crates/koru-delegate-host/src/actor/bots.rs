// SPDX-License-Identifier: MPL-2.0

//! Credential checks happen at the private server boundary; this owner repeats
//! the trusted device binding and owns all effects and queue transitions.
use super::*;
use koru_delegate_api::bots::{BotCommand, BotObservation, BotReply};
use koru_delegate_core::{BotRunKey, WorkspaceScope};

fn unavailable(code: &'static str) -> Failure {
    Failure::new(code, "The Bot operation is not available.", false)
}
fn bot_failure(error: RunError) -> Failure {
    match error {
        RunError::Port(error)
            if matches!(
                error.code.as_str(),
                "bot_native_effects_pending" | "bot_delivery_pending" | "bot_stop_unconfirmed"
            ) =>
        {
            Failure::new(
                error.code,
                "The Bot operation is still being reconciled.",
                true,
            )
        }
        RunError::Port(error) if error.code.starts_with("bot_") => {
            Failure::new(error.code, "The Bot operation was refused.", false)
        }
        error => failure_of(&error),
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
        let device = self.device;
        let reply = match command {
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
            BotCommand::PrepareNativeResult { request } => BotReply::NativeResult {
                intent: self
                    .orchestrator
                    .prepare_native_effect(&request, device)
                    .map_err(bot_failure)?,
            },
            BotCommand::ReconcileNativeResult { key } => BotReply::NativeResult {
                intent: self
                    .orchestrator
                    .reconcile_native_result(&key, device)
                    .map_err(bot_failure)?,
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
                approval,
                decision,
                choice,
            } => {
                self.orchestrator
                    .bot_answer_approval(&fence, device, approval, decision, choice)
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
            BotCommand::Stop { key, fence } => {
                self.orchestrator
                    .bot_stop(&key, device, fence.as_ref())
                    .map_err(bot_failure)?;
                self.notify(key.run);
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
