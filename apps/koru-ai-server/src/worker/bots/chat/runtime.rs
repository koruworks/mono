// SPDX-License-Identifier: MPL-2.0
//! Finite owner calls. No provider launch, repository handle, or idle wait in a job.
use super::*;
use koru_delegate_core::{BotInputRefusal, BotRunKey, BotSubmission};
#[cfg(test)]
mod tests;

#[derive(Clone)]
enum Phase {
    RoutineAdmission,
    RoutineBegin,
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
    /// A Koru decision wait this task is enrolled in (origin terminal proof,
    /// held wait or acknowledged consumer). `None` for every ordinary task.
    pub(super) decision: Option<super::replay::decision::DecisionWait>,
    /// A child wait this task's origin is enrolled in (M6): the origin
    /// terminal's proof or the held wait. `None` for every other task.
    pub(super) child: Option<super::replay::child::ChildWait>,
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
    /// This worker run saw the task's first input refused (or the task
    /// otherwise end automatically) while it drove it, so its automatic
    /// re-send may be owed once it settles (`chat/resend.rs`). A task rebuilt
    /// by a restart's recovery or by a person's Stop starts without it, and
    /// is rebuilt already finishing, so the one place that sets it never
    /// sees it; the exception is a task rebuilt only to deliver its terminal
    /// receipt (`Phase::Delivery`, not finishing), which can be set here if
    /// that delivery fails for good, but such a Run was admitted and ran, so
    /// neither kind of re-send can be owed for it (`bot_resend_plan`): a
    /// re-send is owed only in the run that saw the refusal.
    resend: bool,
    pub(super) staging: super::attachments::Staging,
}
impl LiveChat {
    pub(super) fn control_fence(&self) -> Option<BotExecutionFence> {
        if self.finishing.is_some()
            || self.pending.is_some()
            || self.decision.is_some()
            || self.child.is_some()
            || !matches!(self.phase, Phase::Observe)
        {
            return None;
        }
        self.fence
            .as_ref()
            .filter(|fence| fence.turn.is_some())
            .cloned()
    }
    /// A person's Stop or an automatic interruption is in progress.
    pub(super) fn is_finishing(&self) -> bool {
        self.finishing.is_some()
    }
    /// Observe the current fence at once (after a decision verdict or the
    /// consumer rollover); never used to leave a finishing disposition.
    pub(super) fn observe_now(&mut self) {
        if self.finishing.is_none() {
            self.phase = Phase::Observe;
            self.due = Instant::now();
        }
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
            decision: None,
            child: None,
            phase: Phase::Observe,
            due: Instant::now(),
            busy: false,
            control_busy: false,
            automatic: false,
            retries: 0,
            finishing: None,
            resend: false,
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
    routine: Option<(policy::dispatch::RoutineDispatch, Arc<dyn BotAuthority>)>,
}
pub(in crate::worker) struct StepResult {
    attempt: InferenceAttemptId,
    phase: Phase,
    administrative: bool,
    result: Result<BotReply, koru_delegate_api::Failure>,
    routine: Option<policy::dispatch::RoutineDispatch>,
}
fn routine_step_failure(error: koru_ai_wire::Failure) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure {
        code: error.code,
        message: error.message,
        retryable: error.retryable,
        details: error.details,
    }
}
impl StepJob {
    pub(in crate::worker) fn run(self) -> StepResult {
        let (port, ingest, command) = (self.port, self.ingest, self.command);
        let result = catch_unwind(AssertUnwindSafe(|| {
            if let Some((routine, authority)) = &self.routine {
                routine
                    .synchronize(
                        authority.as_ref(),
                        port.as_ref(),
                        Instant::now() + Duration::from_secs(5),
                    )
                    .map_err(routine_step_failure)?;
                return Ok(BotReply::Applied);
            }
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
            routine: self.routine.map(|(routine, _)| routine),
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
        memory: koru_ai_core::bots::VerifiedMemoryIndex,
    ) -> Result<(), AiError> {
        let id = request.attempt_id;
        let receipt = self
            .repository
            .bot_submission_for_attempt(id)?
            .ok_or_else(bot_route_error)?;
        let routine = receipt.intent.submission.provenance.routine.is_some();
        if routine {
            // Local full identity first; authenticated owner/Store checks run
            // on the existing finite step lane before advancing Admission.
            policy::dispatch::RoutineDispatch::snapshot(
                &self.repository,
                &receipt.intent.submission,
            )
            .map_err(|_| bot_route_error())?;
        }
        let mut input = receipt.intent.submission.input.clone();
        input.text = koru_ai_provider_agent::compose_delegated_prompt(&request);
        if !memory.bytes.is_empty() {
            let block =
                std::str::from_utf8(&memory.bytes).map_err(|_| context_port_unavailable())?;
            input.text.push_str("\n\n");
            input.text.push_str(block);
        }
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
                decision: None,
                child: None,
                phase: if routine {
                    Phase::RoutineAdmission
                } else {
                    Phase::Admission
                },
                due: Instant::now(),
                busy: false,
                control_busy: false,
                automatic: false,
                retries: 0,
                finishing: None,
                resend: false,
                staging,
            },
        );
        let bot = self
            .repository
            .bot(receipt.intent.submission.provenance.bot)
            .map_err(|_| bot_route_error())?
            .ok_or_else(bot_route_error)?;
        if !routine {
            self.queue_bot_receipt(&bot, BotAdmissionReceipt::Submission(receipt), None);
        }
        Ok(())
    }
    pub(in crate::worker) fn schedule_bot_chat_step(&mut self) {
        self.prune_bot_steers();
        // Decision reads use the bounded Run-read lane, never this Bot slot.
        self.schedule_bot_decision_reads();
        // Child wait reads (M6) use the administration lane, never this slot.
        self.schedule_bot_child_reads();
        // At most one finite Bot IO job occupies this pool at once. Native
        // waiting happens in the host, not on either AI provider thread.
        let Some(port) = self.bot_delegate.clone() else {
            return;
        };
        let now = Instant::now();
        let control = self.bot_chats.values().any(|chat| {
            chat.finishing == Some(Finish::Stop) && !chat.control_busy && chat.due <= now
        });
        // At most one automatic job is outstanding. It runs on the shared
        // administration lane, whose queue holds eight and which the other
        // background passes share: a restart's walk rebuilds up to eight
        // tasks, and one job each, with the owner silent, filled that queue
        // and refused the next background job `ai_worker_saturated` (before
        // preparation had a lane of its own, a person's Bot request too).
        // The lane runs one job at a time anyway, so the walk loses nothing
        // but a turn of the writer.
        let automatic = !control
            && !self
                .bot_chats
                .values()
                .any(|chat| chat.busy && chat.automatic)
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
                        // A staged origin terminal awaiting proof, or a held
                        // wait, is not observed again until its verdict.
                        chat.finishing.is_none()
                            && !chat.busy
                            && chat.decision.as_ref().is_none_or(|wait| wait.observes())
                            && chat.child.as_ref().is_none_or(|wait| wait.observes())
                    }
            })
            .min_by_key(|(_, chat)| chat.due)
            .map(|(id, _)| *id)
        else {
            return;
        };
        // A task's first input starts it: never while lifecycle holds its
        // conversation or its Bot (P1.7). The person's Stop ends it instead,
        // as the operation itself would; a hold that cannot be read waits.
        if let Some(chat) = self.bot_chats.get(&id)
            && matches!(chat.phase, Phase::Begin | Phase::RoutineBegin)
        {
            let hold = ChatId::parse(&chat.submission.provenance.chat_id.to_string())
                .map(|chat| self.repository.bot_chat_hold(chat));
            match hold {
                Some(Ok(None)) => {}
                Some(Err(_)) => {
                    if let Some(chat) = self.bot_chats.get_mut(&id) {
                        chat.due = now + Duration::from_millis(250);
                    }
                    return;
                }
                Some(Ok(Some(_))) | None => {
                    // Stopped, it moves to the Stop lane; a Stop that failed
                    // waits its turn like any retry, so it never stands in
                    // front of the other chats every pass.
                    let _ = self.stop_attempt(id);
                    if let Some(chat) = self.bot_chats.get_mut(&id)
                        && chat.finishing.is_none()
                    {
                        chat.due = now + Duration::from_millis(250);
                    }
                    return;
                }
            }
        }
        let routine = {
            let chat = self.bot_chats.get(&id).expect("selected chat");
            if matches!(chat.phase, Phase::RoutineAdmission | Phase::RoutineBegin) {
                let prepared = self
                    .bot_authority
                    .clone()
                    .ok_or_else(unavailable_failure)
                    .and_then(|authority| {
                        policy::dispatch::RoutineDispatch::snapshot(
                            &self.repository,
                            &chat.submission,
                        )
                        .map(|proof| (proof, authority))
                    });
                match prepared {
                    Ok(prepared) => Some(prepared),
                    Err(_) => {
                        self.bot_chats.get_mut(&id).expect("selected").due =
                            now + Duration::from_millis(250);
                        return;
                    }
                }
            } else {
                None
            }
        };
        let chat = self.bot_chats.get_mut(&id).expect("selected chat");
        let mut ingest = None;
        let command = match chat.phase {
            Phase::RoutineAdmission | Phase::RoutineBegin | Phase::Admission => {
                DelegateCommand::Reconcile {
                    key: BotSubmissionKey {
                        owner: chat.submission.provenance.owner,
                        bot: chat.submission.provenance.bot,
                        request: chat.submission.provenance.submission,
                    },
                }
            }
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
            routine,
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
    pub(in crate::worker) fn complete_bot_chat_step(&mut self, mut step: StepResult) {
        if step.result.is_ok() {
            if let Some(routine) = &step.routine {
                if let Err(error) = routine.recheck(&self.repository) {
                    step.result = Err(routine_step_failure(error));
                }
            }
        }
        if step.result.is_ok() {
            self.bot_recovery_retry.answered(Instant::now());
        }
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
                // A finishing task its owner never admitted: Missing until
                // its receipt is refused, which can take as long as its
                // refusal fence cannot land. It waits like any stall.
                (Phase::Admission, Ok(BotReply::Missing)) => true,
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
            // The task's live state ends here. A refusal this run saw may be
            // owed its automatic re-send; the person's Stop declines it; a
            // task a restart rebuilt owes none.
            Ok(false) => match chat.finishing {
                Some(Finish::Stop) => self.decline_bot_resend(step.attempt),
                Some(Finish::Interrupt) if chat.resend => self.bot_chat_refused(step.attempt),
                _ => {}
            },
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
                    // Seen here, in this run: the one place a re-send can
                    // become owed.
                    chat.resend = true;
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
                chat.phase = if chat.submission.provenance.routine.is_some() {
                    Phase::RoutineBegin
                } else {
                    Phase::Begin
                };
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
            (Phase::RoutineAdmission, BotReply::Applied) => {
                chat.phase = Phase::Admission;
                chat.due = Instant::now();
            }
            (Phase::RoutineBegin, BotReply::Applied) => {
                chat.phase = Phase::Begin;
                chat.due = Instant::now();
            }
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
                if r.intent.submission.provenance.routine.is_none() {
                    self.queue_bot_receipt(&bot, BotAdmissionReceipt::Submission(r), None);
                }
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
                    if chat.submission.provenance.routine.is_some() {
                        Phase::RoutineBegin
                    } else {
                        Phase::Begin
                    }
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
    /// A context job returned for a Bot task it can no longer hand on: its
    /// receipt is unreadable (the Bot was archived or trashed meanwhile) or
    /// its association could not be read. It ends here, as an automatic
    /// interruption, and never reaches the ordinary provider route. When even
    /// that cannot be written, the attempt fails through the ordinary
    /// failure event, which also starts nothing.
    pub(in crate::worker) fn end_bot_context(&mut self, id: InferenceAttemptId) {
        self.interrupt_bot_chat(id);
        if matches!(self.repository.attempt(id), Ok(Some(attempt)) if attempt.status.is_terminal())
        {
            return;
        }
        self.handle_provider_event(AiEvent::Failed {
            attempt_id: id,
            error: bot_route_error(),
            at: Timestamp::now(),
        });
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
    /// Lifecycle (Archive, later Trash) is the person's explicit action, so
    /// it stops a live conversation task exactly as the person's Stop does
    /// (O27, O37): a running attempt ends Cancelled with the person's stop
    /// recorded, and one that already ended Interrupted, Failed or natively
    /// Cancelled forwards the stronger Stop to the owner, which ends the
    /// session even while a person holds it. A chat whose Stop lane is
    /// already running is left to it, so its own backoff is not reset each
    /// round. `true` when this sent a Stop.
    pub(in crate::worker) fn stop_bot_chat_for_lifecycle(
        &mut self,
        id: InferenceAttemptId,
    ) -> bool {
        if self
            .bot_chats
            .get(&id)
            .is_none_or(|chat| chat.finishing == Some(Finish::Stop))
        {
            return false;
        }
        self.stop_attempt(id).is_ok()
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
        // A refused task the person stops is not sent again by itself.
        self.decline_bot_resend(id);
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
            decision: None,
            child: None,
            phase: Phase::Admission,
            due: Instant::now(),
            busy: false,
            control_busy: false,
            automatic: false,
            retries: 0,
            finishing: Some(Finish::Stop),
            resend: false,
            staging: Default::default(),
        });
        // A person's Stop is never delayed by an automatic backoff, and it
        // supersedes any decision wait: ordinary Stop keeps its own mapping.
        chat.decision = None;
        chat.child = None;
        chat.finishing = Some(Finish::Stop);
        chat.phase = Phase::Admission;
        chat.retries = 0;
        chat.due = Instant::now();
    }
    /// M6: an admitted child request's opening reply. Its turn was prepared
    /// with the ask (the task as the request, the pending reply); its task
    /// now observes the delegate from admission on, exactly as a Bot reply
    /// whose context job finished does, with the submission's own first
    /// input (the task and the pages it names). `false` when the task cannot
    /// be started now: already live, ended, or the worker is full.
    pub(in crate::worker) fn start_child_bot_chat(&mut self, id: InferenceAttemptId) -> bool {
        if self.bot_chats.contains_key(&id) || self.bot_chats.len() >= PROVIDER_QUEUE_CAPACITY {
            return false;
        }
        let Ok(Some(receipt)) = self.repository.bot_submission_for_attempt(id) else {
            return false;
        };
        if receipt.intent.submission.provenance.parent.is_none()
            || !matches!(self.repository.attempt(id), Ok(Some(attempt)) if !attempt.status.is_terminal())
        {
            return false;
        }
        let submission = receipt.intent.submission;
        self.work_started(id);
        self.bot_chats.insert(
            id,
            LiveChat {
                input: submission.input.clone(),
                submission,
                fence: None,
                mapper: None,
                live: Vec::new(),
                pending: None,
                decision: None,
                child: None,
                phase: Phase::Admission,
                due: Instant::now(),
                busy: false,
                control_busy: false,
                automatic: false,
                retries: 0,
                finishing: None,
                resend: false,
                staging: Default::default(),
            },
        );
        true
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
                    decision: None,
                    child: None,
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
                    // A restart never makes a re-send owed (`chat/resend.rs`).
                    resend: false,
                    staging: Default::default(),
                },
            );
        }
        let _ = self.output.try_send(Envelope::BotChatRecover);
    }
}
