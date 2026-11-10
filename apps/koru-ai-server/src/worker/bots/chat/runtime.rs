// SPDX-License-Identifier: MPL-2.0
//! Finite owner calls. No provider launch, repository handle, or idle wait in a job.
use super::*;
use koru_delegate_core::{BotRunKey, BotSubmission};
#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Phase {
    Admission,
    Ready,
    Begin,
    Observe,
    Delivery,
    Stop,
}
pub(in crate::worker) struct LiveChat {
    pub(super) submission: BotSubmission,
    pub(super) input: PendingInput,
    pub(super) fence: Option<BotExecutionFence>,
    pub(super) mapper: Option<AgentEventMapper>,
    pub(super) live: Vec<AgentLiveAction>,
    pub(super) pending: Option<super::replay::Staged>,
    phase: Phase,
    due: Instant,
    busy: bool,
    control_busy: bool,
    stopping: bool,
}
impl LiveChat {
    pub(super) fn control_fence(&self) -> Option<BotExecutionFence> {
        if self.stopping || self.pending.is_some() || !matches!(self.phase, Phase::Observe) {
            return None;
        }
        self.fence
            .as_ref()
            .filter(|fence| fence.turn.is_some())
            .cloned()
    }
}
#[cfg(test)]
impl LiveChat {
    pub(super) fn fixture(submission: BotSubmission, fence: BotExecutionFence) -> Self {
        let id =
            InferenceAttemptId::parse(&submission.provenance.ai_attempt_id.unwrap().to_string())
                .unwrap();
        let turn = fence.turn.unwrap();
        Self {
            input: submission.input.clone(),
            submission,
            fence: Some(fence),
            mapper: Some(AgentEventMapper::new(
                id,
                turn,
                None,
                UsageDelivery::EachEvent,
            )),
            live: Vec::new(),
            pending: None,
            phase: Phase::Observe,
            due: Instant::now(),
            busy: false,
            control_busy: false,
            stopping: false,
        }
    }
}
pub(in crate::worker) struct StepJob {
    attempt: InferenceAttemptId,
    phase: Phase,
    command: DelegateCommand,
    administrative: bool,
    port: Arc<dyn BotDelegatePort>,
}
pub(in crate::worker) struct StepResult {
    attempt: InferenceAttemptId,
    phase: Phase,
    administrative: bool,
    result: Result<BotReply, koru_delegate_api::Failure>,
}
impl StepJob {
    pub(in crate::worker) fn run(self) -> StepResult {
        let result = catch_unwind(AssertUnwindSafe(|| self.port.execute(self.command)))
            .unwrap_or_else(|_| {
                Err(koru_delegate_api::Failure {
                    code: "bot_unavailable".into(),
                    message: "Bot owner unavailable.".into(),
                    retryable: true,
                    details: Default::default(),
                })
            });
        StepResult {
            attempt: self.attempt,
            phase: self.phase,
            administrative: self.administrative,
            result,
        }
    }
}
fn run_key(submission: &BotSubmission) -> BotRunKey {
    BotRunKey {
        owner: submission.provenance.owner,
        bot: submission.provenance.bot,
        run: submission.request.run,
    }
}
impl WorkerState {
    pub(in crate::worker) fn start_bot_chat_context(
        &mut self,
        request: AiRequest,
    ) -> Result<(), AiError> {
        let id = request.attempt_id;
        let receipt = self
            .repository
            .bot_submission_for_attempt(id)?
            .ok_or_else(bot_route_error)?;
        let mut input = receipt.intent.submission.input.clone();
        input.text = koru_ai_provider_agent::compose_delegated_prompt(&request);
        if input.text.len() > MAX_REPLAY_BYTES || self.bot_chats.len() >= PROVIDER_QUEUE_CAPACITY {
            return Err(context_too_large());
        }
        self.bot_chats.insert(
            id,
            LiveChat {
                submission: receipt.intent.submission.clone(),
                input,
                fence: None,
                mapper: None,
                live: Vec::new(),
                pending: None,
                phase: Phase::Admission,
                due: Instant::now(),
                busy: false,
                control_busy: false,
                stopping: false,
            },
        );
        let bot = self
            .repository
            .bot(receipt.intent.submission.provenance.bot)
            .map_err(|_| bot_route_error())?
            .ok_or_else(bot_route_error)?;
        self.queue_bot_receipt(&bot, BotAdmissionReceipt::Submission(receipt), None);
        Ok(())
    }
    pub(in crate::worker) fn schedule_bot_chat_step(&mut self) {
        self.prune_bot_steers();
        // At most one finite Bot IO job occupies this pool at once. Native
        // waiting happens in the host, not on either AI provider thread.
        let Some(port) = self.bot_delegate.clone() else {
            return;
        };
        let now = Instant::now();
        let control = self
            .bot_chats
            .values()
            .any(|chat| chat.stopping && !chat.control_busy && chat.due <= now);
        if !control && self.bot_chats.values().any(|chat| chat.busy) {
            return;
        }
        let Some(id) = self
            .bot_chats
            .iter()
            .filter(|(_, chat)| {
                chat.due <= now
                    && if control {
                        chat.stopping && !chat.control_busy
                    } else {
                        !chat.stopping && !chat.busy
                    }
            })
            .min_by_key(|(_, chat)| chat.due)
            .map(|(id, _)| *id)
        else {
            return;
        };
        let chat = self.bot_chats.get_mut(&id).expect("selected chat");
        let command = match chat.phase {
            Phase::Admission => DelegateCommand::Reconcile {
                key: BotSubmissionKey {
                    owner: chat.submission.provenance.owner,
                    bot: chat.submission.provenance.bot,
                    request: chat.submission.provenance.submission,
                },
            },
            Phase::Ready | Phase::Observe => {
                let Some(fence) = chat.fence.clone() else {
                    return;
                };
                let Ok(state) = self.repository.bot_replay_state(id) else {
                    return;
                };
                DelegateCommand::Observe {
                    fence,
                    after: state
                        .last_sequence
                        .and_then(|n| koru_session_protocol::StreamSequence::new(n).ok()),
                }
            }
            Phase::Begin => DelegateCommand::BeginTurn {
                fence: chat.fence.clone().expect("ready fence"),
                input: chat.input.clone(),
            },
            Phase::Delivery => {
                let Ok(Some(receipt)) = self.repository.bot_delivery_receipt(id) else {
                    return;
                };
                DelegateCommand::AcknowledgeDelivery {
                    key: run_key(&chat.submission),
                    receipt,
                }
            }
            Phase::Stop => DelegateCommand::Stop {
                key: run_key(&chat.submission),
                fence: chat.fence.clone(),
            },
        };
        let job = StepJob {
            attempt: id,
            phase: chat.phase.clone(),
            command,
            port,
            administrative: control,
        };
        let sent = if control {
            self.provider_admin.as_ref().is_some_and(|pool| {
                pool.try_submit(ProviderAdminJob::BotChatControl(job))
                    .is_ok()
            })
        } else {
            self.provider_pool.try_submit_bot(job)
        };
        if sent {
            if control {
                chat.control_busy = true;
            } else {
                chat.busy = true;
            }
        } else {
            chat.due = now + Duration::from_millis(250);
        }
    }
    pub(in crate::worker) fn complete_bot_chat_step(&mut self, step: StepResult) {
        let Some(mut chat) = self.bot_chats.remove(&step.attempt) else {
            return;
        };
        if step.administrative {
            chat.control_busy = false;
        } else {
            chat.busy = false;
        }
        chat.due = Instant::now() + Duration::from_millis(250);
        if chat.stopping && !step.administrative {
            self.bot_chats.insert(step.attempt, chat);
            return;
        }
        let recovery = match &step.phase {
            Phase::Stop => Some(koru_delegate_core::NativeDrainReason::OwnerStop),
            Phase::Delivery => Some(koru_delegate_core::NativeDrainReason::TerminalTurn),
            Phase::Admission => Some(koru_delegate_core::NativeDrainReason::RunRecovery),
            _ => None,
        };
        let mut recovery_admitted = true;
        if let Some(reason) = recovery {
            let provenance = &chat.submission.provenance;
            recovery_admitted = self.queue_native_recovery(
                provenance.owner,
                provenance.bot,
                Some(chat.submission.request.run),
                None,
                reason,
            );
        }
        if matches!(&step.phase, Phase::Delivery) {
            let provenance = &chat.submission.provenance;
            recovery_admitted &= self.queue_native_recovery(
                provenance.owner,
                provenance.bot,
                Some(chat.submission.request.run),
                None,
                koru_delegate_core::NativeDrainReason::RunRecovery,
            );
        }
        if self.native_recovery.is_some()
            && !recovery_admitted
            && matches!(&step.phase, Phase::Delivery)
            && matches!(&step.result, Ok(BotReply::Applied))
        {
            // An absorbing Run may acknowledge delivery history while Held
            // effects still need drain. Keep its durable AI delivery outbox
            // unacknowledged until the scoped recovery trigger is retained.
            self.bot_chats.insert(step.attempt, chat);
            return;
        }
        // Stop Applied is different: the actor's Bot retirement guard already
        // proved no Held/Verified effect and canonical durability before reply.
        let outcome = self.apply_bot_chat_step(step.attempt, &mut chat, step.phase, step.result);
        match outcome {
            Ok(true) => {
                self.bot_chats.insert(step.attempt, chat);
            }
            Ok(false) => {}
            Err(error) if error.class == AiErrorClass::Storage => {
                // A failed transaction leaves mapper/live state and cursor
                // unchanged. The next finite observation replays that frame.
                self.bot_chats.insert(step.attempt, chat);
            }
            Err(_) => {
                self.interrupt_bot_chat(step.attempt);
                chat.phase = Phase::Stop;
                chat.stopping = true;
                self.bot_chats.insert(step.attempt, chat);
            }
        }
        self.recover_bot_chats();
    }
    fn apply_bot_chat_step(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
        phase: Phase,
        result: Result<BotReply, koru_delegate_api::Failure>,
    ) -> Result<bool, AiError> {
        if chat.stopping && !matches!(phase, Phase::Stop | Phase::Admission) {
            // A result may arrive after the person's Stop. It is never a new
            // permission to begin/advance the old AI task.
            if let Ok(BotReply::Turn { turn }) = &result {
                if let Some(fence) = &mut chat.fence {
                    fence.turn = Some(*turn);
                }
            }
            chat.phase = Phase::Stop;
            return Ok(true);
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) if error.retryable || error.code == "bot_delivery_pending" => {
                return Ok(true);
            }
            Err(_) if matches!(phase, Phase::Stop) => {
                chat.phase = Phase::Admission;
                return Ok(true);
            }
            Err(_) => return Err(bot_route_error()),
        };
        match (phase, reply) {
            (Phase::Admission, BotReply::Missing) => {
                let r = self
                    .repository
                    .bot_submission_for_attempt(id)?
                    .ok_or_else(bot_route_error)?;
                if BotAdmissionReceipt::Submission(r.clone())
                    .operation()
                    .status
                    == BotOperationStatus::Rejected
                {
                    if chat.stopping {
                        return Ok(false);
                    }
                    return Err(bot_route_error());
                }
                let bot = self
                    .repository
                    .bot(r.intent.submission.provenance.bot)
                    .map_err(|_| bot_route_error())?
                    .ok_or_else(bot_route_error)?;
                self.queue_bot_receipt(&bot, BotAdmissionReceipt::Submission(r), None);
            }
            (Phase::Admission, BotReply::Admission { admission }) => {
                let p = &chat.submission.provenance;
                if admission.key
                    != (BotSubmissionKey {
                        owner: p.owner,
                        bot: p.bot,
                        request: p.submission,
                    })
                    || admission.run != chat.submission.request.run
                {
                    return Err(bot_route_error());
                }
                if matches!(
                    admission.state,
                    BotAdmissionState::Refused | BotAdmissionState::Settled
                ) {
                    if chat.stopping {
                        return Ok(false);
                    }
                    return Err(bot_route_error());
                }
                if chat.stopping {
                    chat.fence = admission.fence;
                    chat.phase = Phase::Stop;
                    return Ok(true);
                }
                if let Some(fence) = admission.fence {
                    self.repository
                        .bind_bot_execution(id, &fence, Timestamp::now())?;
                    chat.fence = Some(fence);
                    chat.phase = Phase::Ready;
                    chat.due = Instant::now();
                }
            }
            (Phase::Ready, BotReply::Observation { observation }) => {
                if Some(&observation.fence) != chat.fence.as_ref()
                    || observation.observation.earlier_truncated
                {
                    return Err(bot_route_error());
                }
                // Observation is attached before initial input. Retained pre-turn
                // frames remain unacknowledged until the turn identity is bound.
                chat.phase = Phase::Begin;
                chat.due = Instant::now();
            }
            (Phase::Begin, BotReply::Turn { turn }) => {
                let mut fence = chat.fence.clone().ok_or_else(bot_route_error)?;
                if fence.turn.is_some_and(|old| old != turn) {
                    return Err(bot_route_error());
                }
                fence.turn = Some(turn);
                self.repository
                    .bind_bot_execution(id, &fence, Timestamp::now())?;
                chat.fence = Some(fence);
                chat.mapper = Some(AgentEventMapper::new(
                    id,
                    turn,
                    chat.input.mode.as_ref().map(|m| m.as_str().to_owned()),
                    UsageDelivery::EachEvent,
                ));
                chat.phase = Phase::Observe;
                chat.due = Instant::now();
            }
            (Phase::Observe, BotReply::Observation { observation }) => {
                if self.commit_bot_observation(id, chat, observation)? {
                    chat.phase = Phase::Delivery;
                    chat.input.text.clear();
                    chat.due = Instant::now();
                }
            }
            (Phase::Delivery, BotReply::Applied) => {
                let receipt = self
                    .repository
                    .bot_delivery_receipt(id)?
                    .ok_or_else(bot_route_error)?;
                self.repository
                    .acknowledge_bot_delivery(id, &receipt, Timestamp::now())?;
                return Ok(false);
            }
            (Phase::Stop, BotReply::Applied) => return Ok(false),
            _ => return Err(bot_route_error()),
        }
        Ok(true)
    }
    pub(in crate::worker) fn interrupt_bot_chat(&mut self, id: InferenceAttemptId) {
        if matches!(self.repository.attempt(id),Ok(Some(attempt)) if attempt.status.is_terminal()) {
            return;
        }
        if self
            .repository
            .interrupt_bot_preparation(id, Timestamp::now())
            .is_ok()
        {
            self.publish(AiEvent::Failed {
                attempt_id: id,
                error: bot_route_error(),
                at: Timestamp::now(),
            });
            self.streams.remove(&id);
            self.cancellations.remove(&id);
            self.work_terminal(id);
        }
    }
    pub(in crate::worker) fn stop_bot_chat(
        &mut self,
        id: InferenceAttemptId,
    ) -> ApiResult<AttemptStatus> {
        let receipt = self
            .repository
            .bot_submission_for_attempt(id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        self.repository
            .cancel_bot_chat(id, Timestamp::now())
            .map_err(ai_failure)?;
        if let Some(cancellation) = self.cancellations.get(&id) {
            cancellation.cancel();
        }
        let state = self.repository.bot_replay_state(id).map_err(ai_failure)?;
        let chat = self.bot_chats.entry(id).or_insert_with(|| LiveChat {
            submission: receipt.intent.submission.clone(),
            input: receipt.intent.submission.input.clone(),
            fence: state.fence,
            mapper: None,
            live: Vec::new(),
            pending: None,
            phase: Phase::Admission,
            due: Instant::now(),
            busy: false,
            control_busy: false,
            stopping: true,
        });
        chat.stopping = true;
        chat.phase = Phase::Admission;
        chat.due = Instant::now();
        self.publish(AiEvent::Cancelled {
            attempt_id: id,
            at: Timestamp::now(),
        });
        self.streams.remove(&id);
        self.work_terminal(id);
        Ok(AttemptStatus::Cancelled)
    }
    pub(in crate::worker) fn recover_bot_chats(&mut self) {
        let Some(after) = self.bot_chat_recovery else {
            return;
        };
        if self.bot_delegate.is_none() {
            self.bot_chat_recovery = None;
            return;
        }
        if self.bot_chats.len() >= PROVIDER_QUEUE_CAPACITY {
            return;
        }
        let Ok(rows) = self.repository.bot_recovery_page(after, 16) else {
            return;
        };
        if rows.is_empty() {
            self.bot_chat_recovery = None;
            return;
        }
        for (id, stop, delivery) in rows {
            if self.bot_chats.len() >= PROVIDER_QUEUE_CAPACITY {
                return;
            }
            self.bot_chat_recovery = Some(Some(id));
            if (!stop && !delivery) || self.bot_chats.contains_key(&id) {
                continue;
            }
            let Ok(Some(receipt)) = self.repository.bot_submission_for_attempt(id) else {
                continue;
            };
            let Ok(replay) = self.repository.bot_replay_state(id) else {
                continue;
            };
            let mut input = receipt.intent.submission.input.clone();
            input.text.clear();
            self.bot_chats.insert(
                id,
                LiveChat {
                    submission: receipt.intent.submission,
                    input,
                    fence: replay.fence,
                    mapper: None,
                    live: Vec::new(),
                    pending: None,
                    phase: if delivery {
                        Phase::Delivery
                    } else {
                        Phase::Admission
                    },
                    due: Instant::now(),
                    busy: false,
                    control_busy: false,
                    stopping: !delivery,
                },
            );
        }
        let _ = self.output.try_send(Envelope::BotChatRecover);
    }
}
