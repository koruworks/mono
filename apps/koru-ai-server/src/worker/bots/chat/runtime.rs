// SPDX-License-Identifier: MPL-2.0
//! Finite owner calls. No provider launch, repository handle, or idle wait in a job.
use super::*;
use koru_delegate_core::{BotInputRefusal, BotRunKey, BotSubmission};
#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Phase {
    Admission,
    Ready,
    /// Native pictures, after the pre-turn Observe and before Begin.
    Stage,
    Begin,
    Observe,
    Delivery,
    Stop,
    Interrupt,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    Stop,
    Interrupt,
}
/// The highest doubling of a finishing chat's retry delay: 250 ms << 7 is
/// 32 s, the owner's own cleanup backoff cap.
const FINISH_BACKOFF_MAX_ROUND: u32 = 7;
/// A finishing chat whose owner reply made no progress (pending, refused or
/// a failed reconcile) waits longer each time instead of polling at 4 Hz for
/// a whole takeover. Any applied finishing command resets it, and so does a
/// person's Stop.
fn finishing_backoff(round: u32) -> Duration {
    Duration::from_millis(250) * (1u32 << round.min(FINISH_BACKOFF_MAX_ROUND))
}
/// Only a person's Stop records Cancelled with the local owner-unavailable
/// stop reason (`cancel_bot_chat`). A native turn cancellation, such as a
/// takeover "now" or Esc in the borrowed agent, replays Cancelled and leaves
/// the stop reason empty: it is an automatic outcome, not owner intent.
fn person_stopped(replay: &koru_ai_core::bots::replay::BotReplayState) -> bool {
    replay.stop_reason == Some(koru_ai_core::bots::replay::BotReplayStop::OwnerUnavailable)
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
    /// The in-flight non-control job runs on the normal administration pool
    /// (automatic reconciliation), not in the provider Bot slot. Recorded at
    /// dispatch: a person's Stop may change `finishing` while it is in flight.
    automatic: bool,
    /// Consecutive finishing replies that made no progress.
    retries: u32,
    finishing: Option<Finish>,
    pub(super) staging: super::attachments::Staging,
}
impl LiveChat {
    pub(super) fn control_fence(&self) -> Option<BotExecutionFence> {
        if self.finishing.is_some()
            || self.pending.is_some()
            || !matches!(self.phase, Phase::Observe)
        {
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
            automatic: false,
            retries: 0,
            finishing: None,
            staging: Default::default(),
        }
    }
}
pub(in crate::worker) struct StepJob {
    attempt: InferenceAttemptId,
    phase: Phase,
    command: DelegateCommand,
    /// A staged picture's bytes, read here on the pool thread.
    ingest: Option<super::attachments::IngestRead>,
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
        let (port, ingest, command) = (self.port, self.ingest, self.command);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let command = match ingest {
                Some(read) => read.load(command)?,
                None => command,
            };
            port.execute(command)
        }))
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
        // Local file and folder references go as the ordinary route sends
        // them, frozen with the rest of the first input (O30). They are never
        // staged or read: only pictures with a stored copy are.
        input.references = koru_ai_provider_agent::path_references(&request);
        koru_ai_provider_agent::check_path_references(&request, &input.references)?;
        if input.text.len() > MAX_REPLAY_BYTES || self.bot_chats.len() >= PROVIDER_QUEUE_CAPACITY {
            return Err(context_too_large());
        }
        let as_paths = self.retries_refused_pictures(id);
        let staging = super::attachments::plan(&request, &self.attachments, as_paths);
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
                automatic: false,
                retries: 0,
                finishing: None,
                staging,
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
        let control = self.bot_chats.values().any(|chat| {
            chat.finishing == Some(Finish::Stop) && !chat.control_busy && chat.due <= now
        });
        let automatic = !control
            && self.bot_chats.values().any(|chat| {
                chat.finishing == Some(Finish::Interrupt) && !chat.busy && chat.due <= now
            });
        // Automatic reconciliation runs on the normal administration pool, so
        // an in-flight automatic job never occupies the provider Bot slot,
        // even after a person's Stop changed that chat's disposition.
        if !control
            && !automatic
            && self
                .bot_chats
                .values()
                .any(|chat| chat.busy && !chat.automatic)
        {
            return;
        }
        let Some(id) = self
            .bot_chats
            .iter()
            .filter(|(_, chat)| {
                chat.due <= now
                    && if control {
                        chat.finishing == Some(Finish::Stop) && !chat.control_busy
                    } else if automatic {
                        chat.finishing == Some(Finish::Interrupt) && !chat.busy
                    } else {
                        chat.finishing.is_none() && !chat.busy
                    }
            })
            .min_by_key(|(_, chat)| chat.due)
            .map(|(id, _)| *id)
        else {
            return;
        };
        let chat = self.bot_chats.get_mut(&id).expect("selected chat");
        let mut ingest = None;
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
            Phase::Stage => {
                if chat.finishing.is_some() {
                    return;
                }
                let Some(step) = chat.staging.next(chat.fence.as_ref()) else {
                    return;
                };
                ingest = step.read;
                step.command
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
            Phase::Interrupt => DelegateCommand::Interrupt {
                key: run_key(&chat.submission),
                fence: chat.fence.clone(),
            },
        };
        let job = StepJob {
            attempt: id,
            phase: chat.phase.clone(),
            command,
            ingest,
            port,
            administrative: control,
        };
        let sent = if control {
            self.provider_admin.as_ref().is_some_and(|pool| {
                pool.try_submit(ProviderAdminJob::BotChatControl(job))
                    .is_ok()
            })
        } else if automatic {
            self.provider_admin.as_ref().is_some_and(|pool| {
                pool.try_submit(ProviderAdminJob::BotChatRecovery(job))
                    .is_ok()
            })
        } else {
            self.provider_pool.try_submit_bot(job)
        };
        if sent {
            if matches!(chat.phase, Phase::Stage) {
                chat.staging.mark_sent(chat.fence.as_ref());
            }
            if control {
                chat.control_busy = true;
            } else {
                chat.busy = true;
                chat.automatic = automatic;
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
            chat.automatic = false;
        }
        chat.due = Instant::now() + Duration::from_millis(250);
        if chat.finishing == Some(Finish::Stop) && !step.administrative {
            self.bot_chats.insert(step.attempt, chat);
            return;
        }
        // A finishing command's only success is Applied. A pending, refused or
        // failed reply (including a refused reconcile) backs off; a reconcile
        // that only leads back to the same command neither resets nor grows it.
        let finishing = chat.finishing.is_some();
        let progressed = finishing
            && matches!(
                (&step.phase, &step.result),
                (
                    Phase::Stop | Phase::Interrupt | Phase::Delivery,
                    Ok(BotReply::Applied)
                )
            );
        let stalled = finishing
            && match (&step.phase, &step.result) {
                // A late picture-staging reply is neither finishing progress
                // nor a stalled finishing command.
                (Phase::Stage, _) => false,
                (_, Err(_)) => true,
                (Phase::Stop | Phase::Interrupt | Phase::Delivery, Ok(reply)) => {
                    !matches!(reply, BotReply::Applied)
                }
                _ => false,
            };
        // The identical recovery scope was already retained by the first
        // stalled reply; a repeat would only churn the shared gate.
        let repeat = stalled && chat.retries > 0;
        // A retryable Begin is re-issued with the same exact input, which the
        // owner and session answer idempotently. It backs off the same way
        // rather than asking four times a second until the active budget
        // fences the attempt.
        let begin_retry = !finishing
            && matches!(step.phase, Phase::Begin)
            && matches!(&step.result, Err(error) if error.retryable);
        if stalled || begin_retry {
            chat.retries = (chat.retries + 1).min(FINISH_BACKOFF_MAX_ROUND);
            chat.due = Instant::now() + finishing_backoff(chat.retries);
        } else if progressed || (!finishing && matches!(step.phase, Phase::Begin)) {
            chat.retries = 0;
        }
        // A definite refusal of the task's first input. The owner names it;
        // the task ends now instead of asking again for the same answer.
        let refusal = match (&step.phase, &step.result) {
            (Phase::Begin, Err(error)) if !finishing && !error.retryable => {
                BotInputRefusal::from_code(&error.code)
            }
            _ => None,
        };
        let recovery = match &step.phase {
            _ if repeat => None,
            Phase::Stop => Some(koru_delegate_core::NativeDrainReason::OwnerStop),
            Phase::Interrupt => Some(koru_delegate_core::NativeDrainReason::RunRecovery),
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
        if !repeat && matches!(&step.phase, Phase::Delivery) {
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
            && matches!(&step.phase, Phase::Delivery | Phase::Interrupt)
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
                // An automatic failure never selects Stop, and it never
                // replaces a person's Stop that is already in progress.
                match refusal {
                    Some(refusal) => self.refuse_bot_chat(step.attempt, refusal),
                    None => self.interrupt_bot_chat(step.attempt),
                }
                chat.phase = Phase::Admission;
                if chat.finishing.is_none() {
                    // Retries counted before finishing are not finishing stalls.
                    chat.retries = 0;
                    chat.finishing = Some(Finish::Interrupt);
                }
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
        if chat.finishing.is_some()
            && !matches!(
                phase,
                Phase::Stop | Phase::Interrupt | Phase::Admission | Phase::Delivery
            )
        {
            // A result may arrive after the person's Stop. It is never a new
            // permission to begin/advance the old AI task.
            if let Ok(BotReply::Turn { turn }) = &result {
                if let Some(fence) = &mut chat.fence {
                    fence.turn = Some(*turn);
                }
            }
            chat.phase = Phase::Admission;
            return Ok(true);
        }
        if matches!(phase, Phase::Stage) {
            // Before the generic retryable arm below: retrying a staging step
            // would resend an ingest whose reply was lost. Staging lists it.
            if self.apply_bot_stage(id, chat, result)? {
                chat.phase = Phase::Begin;
                chat.due = Instant::now();
            }
            return Ok(true);
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) if error.retryable || error.code == "bot_delivery_pending" => {
                return Ok(true);
            }
            Err(_) if matches!(phase, Phase::Stop | Phase::Interrupt) => {
                chat.phase = Phase::Admission;
                return Ok(true);
            }
            // A finished Run's refused history acknowledgment is not retried in
            // a loop; its durable outbox stays pending for the next recovery.
            Err(_) if chat.finishing.is_some() && matches!(phase, Phase::Delivery) => {
                return Ok(false);
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
                    if chat.finishing.is_some() {
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
                    if chat.finishing.is_some() {
                        // Real delivery history still reaches the owner; none
                        // is synthesized for an interruption.
                        if self.repository.bot_delivery_pending(id)? {
                            chat.phase = Phase::Delivery;
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                    return Err(bot_route_error());
                }
                if let Some(finish) = chat.finishing {
                    // Reconcile may refresh this attempt's turn after an
                    // uncertain Begin. A different attempt, generation or
                    // session is never substituted: the owner no longer runs
                    // the attempt this chat was finishing, so nothing is sent.
                    if let (Some(old), Some(current)) = (&chat.fence, &admission.fence) {
                        if old.attempt != current.attempt
                            || old.generation != current.generation
                            || old.session != current.session
                        {
                            return Ok(false);
                        }
                    }
                    chat.fence = admission.fence;
                    chat.phase = match finish {
                        Finish::Stop => Phase::Stop,
                        Finish::Interrupt => Phase::Interrupt,
                    };
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
                // Native pictures are staged against this Session first.
                chat.phase = if self.bot_stage_settled(id, chat) {
                    Phase::Begin
                } else {
                    Phase::Stage
                };
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
            (Phase::Stop | Phase::Interrupt, BotReply::Applied) => {
                if !self.repository.bot_delivery_pending(id)? {
                    return Ok(false);
                }
                chat.phase = Phase::Delivery;
            }
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
    /// Lifecycle (Archive, later Trash) ends a live conversation task as an
    /// automatic interruption, never as the person's Stop: the AI attempt
    /// ends interrupted, and the chat's finishing lane sends the owner's
    /// `Interrupt`, whose owned cleanup cannot end a session a person holds.
    /// A chat already finishing (a person's Stop, an earlier interruption)
    /// is left as it is. `true` when this changed the chat.
    pub(in crate::worker) fn interrupt_bot_chat_for_lifecycle(
        &mut self,
        id: InferenceAttemptId,
    ) -> bool {
        if self
            .bot_chats
            .get(&id)
            .is_none_or(|chat| chat.finishing.is_some())
        {
            return false;
        }
        self.interrupt_bot_chat(id);
        let Some(chat) = self.bot_chats.get_mut(&id) else {
            return false;
        };
        chat.finishing = Some(Finish::Interrupt);
        chat.phase = Phase::Admission;
        chat.retries = 0;
        chat.due = Instant::now();
        true
    }
    /// The owner definitely refused the task's first input. The same
    /// automatic end as [`Self::interrupt_bot_chat`], except that the attempt
    /// fails with the reason in the existing failure vocabulary, and keeps
    /// the owner's code for a later Retry.
    pub(in crate::worker) fn refuse_bot_chat(
        &mut self,
        id: InferenceAttemptId,
        refusal: BotInputRefusal,
    ) {
        if matches!(self.repository.attempt(id),Ok(Some(attempt)) if attempt.status.is_terminal()) {
            return;
        }
        let error = super::refused_input_error(refusal);
        tracing::info!(
            target: "koru_ai::bots",
            attempt = %id,
            refusal = refusal.code(),
            "Bot task's first input was refused; the task ends"
        );
        if self
            .repository
            .refuse_bot_input(id, error.class, refusal.code(), Timestamp::now())
            .is_ok()
        {
            self.publish(AiEvent::Failed {
                attempt_id: id,
                error,
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
        self.request_bot_stop(id, &receipt.intent.submission, state.fence);
        self.publish(AiEvent::Cancelled {
            attempt_id: id,
            at: Timestamp::now(),
        });
        self.streams.remove(&id);
        self.work_terminal(id);
        Ok(AttemptStatus::Cancelled)
    }
    /// A person's Stop after an automatic interruption, failure or native turn
    /// cancellation already ended the AI attempt. The stronger Stop reaches the owner on the reserved lane
    /// at once, without waiting for an in-flight automatic reply. The attempt's
    /// durable status is left as it ended: recording Cancelled over it needs a
    /// schema change (its terminal status is absorbing), so nothing claims it.
    pub(in crate::worker) fn stop_interrupted_bot_chat(
        &mut self,
        id: InferenceAttemptId,
        status: AttemptStatus,
    ) -> ApiResult<AttemptStatus> {
        let receipt = self
            .repository
            .bot_submission_for_attempt(id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let state = self.repository.bot_replay_state(id).map_err(ai_failure)?;
        self.request_bot_stop(id, &receipt.intent.submission, state.fence);
        Ok(status)
    }
    /// Whether a Bot attempt already ended without the person's Stop, so a
    /// Stop pressed now must still reach the owner: an automatic Interrupted
    /// or Failed outcome, or a natively cancelled turn (takeover "now", Esc).
    pub(in crate::worker) fn bot_ended_without_stop(
        &self,
        id: InferenceAttemptId,
        status: AttemptStatus,
    ) -> Result<bool, AiError> {
        Ok(match status {
            AttemptStatus::Interrupted | AttemptStatus::Failed => true,
            AttemptStatus::Cancelled => !person_stopped(&self.repository.bot_replay_state(id)?),
            _ => false,
        })
    }
    fn request_bot_stop(
        &mut self,
        id: InferenceAttemptId,
        submission: &BotSubmission,
        fence: Option<BotExecutionFence>,
    ) {
        let chat = self.bot_chats.entry(id).or_insert_with(|| LiveChat {
            submission: submission.clone(),
            input: submission.input.clone(),
            fence,
            mapper: None,
            live: Vec::new(),
            pending: None,
            phase: Phase::Admission,
            due: Instant::now(),
            busy: false,
            control_busy: false,
            automatic: false,
            retries: 0,
            finishing: Some(Finish::Stop),
            staging: Default::default(),
        });
        // A person's Stop is never delayed by an automatic backoff.
        chat.finishing = Some(Finish::Stop);
        chat.phase = Phase::Admission;
        chat.retries = 0;
        chat.due = Instant::now();
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
            // Durable Cancelled alone is not a person's Stop: a takeover "now"
            // also cancels the native turn. Only the recorded owner Stop
            // selects the stronger route after a restart.
            let finish = match self.repository.attempt(id) {
                Ok(Some(attempt)) if attempt.status == AttemptStatus::Cancelled => {
                    Some(if person_stopped(&replay) {
                        Finish::Stop
                    } else {
                        Finish::Interrupt
                    })
                }
                Ok(Some(attempt))
                    if matches!(
                        attempt.status,
                        AttemptStatus::Interrupted | AttemptStatus::Failed
                    ) =>
                {
                    Some(Finish::Interrupt)
                }
                Ok(Some(_)) => None,
                // Never guess a disposition, and never resume the old task.
                _ => continue,
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
                    phase: if delivery && finish.is_none() {
                        Phase::Delivery
                    } else {
                        Phase::Admission
                    },
                    due: Instant::now(),
                    busy: false,
                    control_busy: false,
                    automatic: false,
                    retries: 0,
                    finishing: finish,
                    staging: Default::default(),
                },
            );
        }
        let _ = self.output.try_send(Envelope::BotChatRecover);
    }
}
