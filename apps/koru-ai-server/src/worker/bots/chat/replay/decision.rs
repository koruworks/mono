// SPDX-License-Identifier: MPL-2.0
//! A Bot task enrolled in a Koru decision wait: the receipt-qualified origin
//! terminal interceptor, the held wait, the caller of the atomic
//! consumer-fence/cursor rollover and the final Needed-you trigger.
//!
//! Enrollment is permission to interpret one internal cause, never proof that
//! the stop happened: the delegate's matching cleanup outcome is read on the
//! bounded Run-read lane before anything is interpreted. A UI card never
//! qualifies any of this. Ordinary Stop and unrelated terminals keep their
//! existing mapping. Nothing here fabricates a provider event.
use super::*;
use crate::worker::bots::decision_wait::apply::{
    ApplyJob, ApplyOutcome, ApplyProgress, ApplyResult, ApplyWork, approved_receipt,
};
use crate::worker::bots::decision_wait::{DecisionWaitJob, DecisionWaitPortError};
use koru_ai_sqlite::BotDecisionClosure;
use koru_delegate_core::decisions::{
    BotDecisionWaitObservation, BotWaitCleanupCause, BotWaitEnrollmentKey, DecisionClosureReason,
    DecisionDisposition, DecisionFence, DecisionResume, DecisionResumeState, DecisionState,
    DecisionValidate, FreshWaitStopEligibility, OriginWaitStop,
};

#[cfg(test)]
mod tests;

/// 250 ms doubling to 32 s, the same cap as a finishing chat's backoff.
const MAX_ROUND: u32 = 7;

type WaitRead = Result<Option<BotDecisionWaitObservation>, DecisionWaitPortError>;
type ResumeRead = Result<Option<DecisionResume>, DecisionWaitPortError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum Stage {
    /// A terminal of the enrolled origin is staged in `chat.pending` while the
    /// delegate's matching cleanup outcome is read. Nothing is committed.
    OriginProof,
    /// The proof says this staged terminal keeps its ordinary mapping.
    Ordinary,
    /// The origin terminal was suppressed; the AI task stays live, waiting
    /// for its successor's acknowledged input or its final closure.
    Held,
    /// The acknowledged consumer is installed and observed normally; only
    /// the final Needed-you closure is still watched for.
    Consumer,
}

pub(in crate::worker) struct DecisionWait {
    pub(super) key: BotWaitEnrollmentKey,
    pub(super) stage: Stage,
    busy: bool,
    due: Instant,
    rounds: u32,
    /// Consumer stage only: Koru's own save of the approved edit (O133).
    apply: Option<ApplyProgress>,
}
impl DecisionWait {
    fn new(key: BotWaitEnrollmentKey, stage: Stage) -> Self {
        Self {
            key,
            stage,
            busy: false,
            due: Instant::now(),
            rounds: 0,
            apply: None,
        }
    }
    /// The acknowledged consumer, observed normally, with its approved-edit
    /// trigger armed.
    fn consumer(key: BotWaitEnrollmentKey, consumer: BotExecutionFence) -> Self {
        let mut wait = Self::new(key, Stage::Consumer);
        wait.apply = Some(ApplyProgress::new(consumer));
        wait
    }
    /// Only these stages let the ordinary observation step run.
    pub(in crate::worker) fn observes(&self) -> bool {
        matches!(self.stage, Stage::Ordinary | Stage::Consumer)
    }
    fn reads(&self) -> bool {
        !matches!(self.stage, Stage::Ordinary)
    }
    fn defer(&mut self) {
        self.rounds = (self.rounds + 1).min(MAX_ROUND);
        self.due = Instant::now() + Duration::from_millis(250) * (1u32 << self.rounds);
    }
}

/// Why an approved-edit attempt could not be built on the writer.
enum DecisionApplyGap {
    /// Never in this process: no receipt (O146), no executor, another subject.
    Absent,
    /// Not on this read (native readiness, a repository or barrier read).
    NotNow,
}

/// How one staged origin terminal is interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Interception {
    /// Matching internal origin-stop receipt: keep the cursor, suppress the
    /// terminal effect, `work_terminal`, stream removal and terminal events.
    Suppress,
    /// Existing mapping (no internal stop for this wait, ordinary Stop, or a
    /// natural completion).
    Ordinary,
    /// Unavailable or contradictory proof: keep the staged candidate and the
    /// accepted cursor; interpret nothing.
    Uncertain,
}

pub(super) fn origin_of(key: &BotWaitEnrollmentKey) -> DecisionFence {
    DecisionFence {
        owner: key.owner,
        bot: key.bot,
        run: key.run,
        attempt: key.origin_attempt,
        generation: key.origin_generation,
        session: key.origin_session,
        turn: key.origin_turn,
    }
}
fn same_origin(key: &BotWaitEnrollmentKey, fence: &BotExecutionFence) -> bool {
    let origin = origin_of(key);
    fence.owner == origin.owner
        && fence.bot == origin.bot
        && fence.run == origin.run
        && fence.attempt == origin.attempt
        && fence.generation == origin.generation
        && fence.session == origin.session
        && fence.turn == Some(origin.turn)
}
fn ai_owner(key: &BotWaitEnrollmentKey) -> koru_ai_core::bots::BotOwner {
    koru_ai_core::bots::BotOwner {
        user_scope: key.owner.user_scope,
        device: key.owner.device,
    }
}
/// The authenticated read, but only for this exact wait.
fn matched<'a>(
    key: &BotWaitEnrollmentKey,
    read: &'a WaitRead,
) -> Option<&'a BotDecisionWaitObservation> {
    match read {
        Ok(Some(observed)) if observed.validate().is_ok() && observed.key == *key => Some(observed),
        _ => None,
    }
}

/// Pure verdict. `ordinary_stop` covers a person's Stop in progress and any
/// replay stop fence; `completed` a natural completion.
pub(super) fn classify_origin_terminal(
    key: &BotWaitEnrollmentKey,
    fence: &BotExecutionFence,
    ordinary_stop: bool,
    completed: bool,
    read: &WaitRead,
) -> Interception {
    if ordinary_stop || completed || !same_origin(key, fence) {
        return Interception::Ordinary;
    }
    let Some(observed) = matched(key, read) else {
        // Unknown, refused, Missing or another wait: nothing is interpreted.
        return Interception::Uncertain;
    };
    match &observed.origin_stop {
        OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            cause,
            ..
        } if *origin_fence == origin_of(key)
            && *wait_id == key.wait_id
            && *intent_digest == key.receipt_digest
            && *enrollment_digest == key.receipt_digest
            && *cause == BotWaitCleanupCause::DecisionWaitCleanup =>
        {
            Interception::Suppress
        }
        OriginWaitStop::Confirmed { .. } => Interception::Uncertain,
        OriginWaitStop::Pending {} => {
            match (&observed.disposition, observed.fresh_stop_eligibility) {
                // The internal stop is still owed: wait for its receipt.
                (
                    DecisionDisposition::Open { .. },
                    FreshWaitStopEligibility::EligibleOriginWaitStop,
                ) => Interception::Uncertain,
                // No internal stop for this wait: the terminal is ordinary.
                _ => Interception::Ordinary,
            }
        }
    }
}

/// What a held wait does next.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum HeldNext {
    Rollover(DecisionResume),
    Close,
    Wait,
}
pub(super) fn classify_held(
    key: &BotWaitEnrollmentKey,
    read: &WaitRead,
    resume: Option<&ResumeRead>,
) -> HeldNext {
    let Some(observed) = matched(key, read) else {
        return HeldNext::Wait;
    };
    if let Some(Ok(Some(resume))) = resume
        && resume.validate().is_ok()
        && resume.decision == key.decision_id
        && resume.origin == origin_of(key)
        && matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
    {
        return HeldNext::Rollover(resume.clone());
    }
    match &observed.disposition {
        DecisionDisposition::Settled { reason, .. }
            if *reason != DecisionClosureReason::Completed =>
        {
            HeldNext::Close
        }
        _ => HeldNext::Wait,
    }
}

/// A copy of the staged batch with only the terminal Event effect removed.
/// Every frame and sequence stays, so the accepted cursor still advances
/// exactly past the native terminal; non-terminal effects keep applying.
fn suppressed_batch(batch: &BotReplayBatch) -> BotReplayBatch {
    let mut batch = batch.clone();
    for event in &mut batch.events {
        event.effects.retain(|effect| {
            !matches!(
                effect,
                BotReplayEffect::Event {
                    terminal_cleanup: Some(_),
                    ..
                }
            )
        });
    }
    batch
}

/// The public closing event of a Bot turn the closure just ended as
/// "Needed you" (O128): exactly one, only when this call wrote the closure,
/// never for an exact replay or any other outcome. Not a failure.
pub(super) fn needed_you_event(
    attempt: InferenceAttemptId,
    closure: BotDecisionClosure,
    at: Timestamp,
) -> Option<PublicAiEvent> {
    match closure {
        BotDecisionClosure::Closed { reason } => Some(PublicAiEvent::NeededYou {
            attempt_id: attempt,
            reason,
            at,
        }),
        BotDecisionClosure::ClosedBefore
        | BotDecisionClosure::AlreadyEnded
        | BotDecisionClosure::Owed => None,
    }
}

fn decision_proof_unavailable() -> AiError {
    // Never shown: a Storage-class result keeps the chat and its staged
    // candidate for the next finite observation (`complete_bot_chat_step`).
    AiError::new(AiErrorClass::Storage, "", true)
}

impl WorkerState {
    /// `None` keeps the candidate staged in `chat.pending` for the proof read.
    pub(super) fn intercept_origin_terminal(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
        staged: Staged,
    ) -> Result<Option<Staged>, AiError> {
        if !staged.terminal || chat.is_finishing() {
            return Ok(Some(staged));
        }
        match chat.decision.as_ref().map(|wait| wait.stage) {
            Some(Stage::Ordinary) => {
                chat.decision = None;
                return Ok(Some(staged));
            }
            // A consumer never borrows the origin enrollment.
            Some(Stage::Consumer) => return Ok(Some(staged)),
            _ => {}
        }
        let provenance = &chat.submission.provenance;
        let owner = koru_ai_core::bots::BotOwner {
            user_scope: provenance.owner.user_scope,
            device: provenance.owner.device,
        };
        match self
            .repository
            .bot_wait_enrollment_for_origin(owner, id, &staged.batch.fence)
        {
            Ok(None) => Ok(Some(staged)),
            Ok(Some(key)) => {
                chat.decision = Some(DecisionWait::new(key, Stage::OriginProof));
                chat.pending = Some(staged);
                Ok(None)
            }
            Err(_) => {
                chat.pending = Some(staged);
                Err(decision_proof_unavailable())
            }
        }
    }

    /// At most one chat decision read is in flight; it shares the Run-read
    /// lane's bounded queue and never occupies the provider Bot slot.
    pub(in crate::worker) fn schedule_bot_decision_reads(&mut self) {
        let Some(delegate) = self.decision_waits.delegate() else {
            return;
        };
        if self
            .bot_chats
            .values()
            .any(|chat| chat.decision.as_ref().is_some_and(|wait| wait.busy))
        {
            return;
        }
        let now = Instant::now();
        let Some((attempt, key, resume)) = self
            .bot_chats
            .iter()
            .filter_map(|(id, chat)| {
                let wait = chat.decision.as_ref()?;
                (wait.reads() && wait.due <= now && !chat.is_finishing())
                    .then(|| (*id, wait.key.clone(), wait.stage == Stage::Held, wait.due))
            })
            .min_by_key(|(.., due)| *due)
            .map(|(id, key, resume, _)| (id, key, resume))
        else {
            return;
        };
        let job = DecisionWaitJob::for_chat(attempt, key, resume, delegate);
        let sent = self.provider_admin.as_ref().is_some_and(|pool| {
            pool.try_submit(ProviderAdminJob::DecisionWaitRead(job))
                .is_ok()
        });
        if let Some(wait) = self
            .bot_chats
            .get_mut(&attempt)
            .and_then(|chat| chat.decision.as_mut())
        {
            if sent {
                wait.busy = true;
            } else {
                wait.defer();
            }
        }
    }

    /// A finished chat decision read. A result for another wait is stale.
    pub(in crate::worker) fn complete_chat_decision_read(
        &mut self,
        attempt: InferenceAttemptId,
        key: BotWaitEnrollmentKey,
        read: WaitRead,
        resume: Option<ResumeRead>,
    ) {
        let Some(mut chat) = self.bot_chats.remove(&attempt) else {
            return;
        };
        if !chat.decision.as_ref().is_some_and(|wait| wait.key == key) {
            self.bot_chats.insert(attempt, chat);
            return;
        }
        let Some(wait) = chat.decision.as_mut() else {
            return;
        };
        wait.busy = false;
        let stage = wait.stage;
        let keep = if chat.is_finishing() {
            // A person's Stop supersedes the wait; its own mapping applies.
            chat.decision = None;
            true
        } else {
            match stage {
                Stage::OriginProof => self.decide_origin_terminal(attempt, &mut chat, &key, &read),
                Stage::Held => self.advance_held(attempt, &mut chat, &key, &read, resume.as_ref()),
                Stage::Consumer => self.advance_consumer(attempt, &mut chat, &key, &read),
                Stage::Ordinary => true,
            }
        };
        if keep {
            self.bot_chats.insert(attempt, chat);
        }
    }

    fn defer_decision(chat: &mut LiveChat) {
        if let Some(wait) = chat.decision.as_mut() {
            wait.defer();
        }
    }

    fn decide_origin_terminal(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        read: &WaitRead,
    ) -> bool {
        let Some(staged) = chat.pending.as_ref() else {
            chat.decision = None;
            chat.observe_now();
            return true;
        };
        let ordinary_stop = !matches!(
            self.repository.bot_replay_state(attempt),
            Ok(replay) if replay.stop_reason.is_none()
        );
        match classify_origin_terminal(
            key,
            &staged.batch.fence,
            ordinary_stop,
            staged.completed,
            read,
        ) {
            Interception::Uncertain => Self::defer_decision(chat),
            Interception::Ordinary => {
                if let Some(wait) = chat.decision.as_mut() {
                    wait.stage = Stage::Ordinary;
                }
                chat.observe_now();
            }
            Interception::Suppress => {
                let batch = suppressed_batch(&staged.batch);
                match commit_staged(&mut self.repository, &batch) {
                    Ok(true) => {
                        let mut staged = chat.pending.take().expect("staged candidate");
                        staged.batch = batch;
                        staged.published.retain(|event| !event.is_terminal());
                        staged.terminal = false;
                        staged.completed = false;
                        // The non-terminal part is durable; a failed question
                        // read only delays its live presentation.
                        let _ = self.install_staged(attempt, chat, staged);
                        chat.decision = Some(DecisionWait::new(key.clone(), Stage::Held));
                        Self::defer_decision(chat);
                    }
                    // Not proven durable: keep the original candidate.
                    Ok(false) | Err(_) => Self::defer_decision(chat),
                }
            }
        }
        true
    }

    fn advance_held(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        read: &WaitRead,
        resume: Option<&ResumeRead>,
    ) -> bool {
        match classify_held(key, read, resume) {
            HeldNext::Rollover(resume) => self.rollover_consumer(attempt, chat, key, &resume),
            HeldNext::Close => match matched(key, read) {
                Some(observed) => self.close_needed_you(attempt, chat, key, observed),
                None => {
                    Self::defer_decision(chat);
                    true
                }
            },
            HeldNext::Wait => {
                Self::defer_decision(chat);
                true
            }
        }
    }

    fn advance_consumer(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        read: &WaitRead,
    ) -> bool {
        let Some(observed) = matched(key, read) else {
            Self::defer_decision(chat);
            return true;
        };
        match &observed.disposition {
            // The consumer continues; its own terminal arrives through replay.
            DecisionDisposition::Settled { reason, .. }
                if *reason == DecisionClosureReason::Completed =>
            {
                chat.decision = None;
                true
            }
            DecisionDisposition::Settled { .. } => {
                self.close_needed_you(attempt, chat, key, observed)
            }
            DecisionDisposition::Open { state } => {
                Self::defer_decision(chat);
                self.schedule_decision_apply(attempt, chat, key, state);
                true
            }
        }
    }

    /// O133 design B: on this authenticated consumer read, start at most one
    /// attempt to save the approved edit. Never on the read lane, never
    /// blocking the writer; a lane that cannot take it counts as an attempt.
    fn schedule_decision_apply(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        state: &DecisionState,
    ) {
        let Some(apply) = chat.decision.as_mut().and_then(|wait| wait.apply.as_mut()) else {
            return;
        };
        if !apply.due(state) {
            return;
        }
        let work = match self.decision_apply_work(key, apply.consumer()) {
            Ok(work) => work,
            Err(DecisionApplyGap::Absent) => {
                apply.stop();
                return;
            }
            Err(DecisionApplyGap::NotNow) => {
                apply.start();
                apply.finish(ApplyOutcome::Unavailable);
                return;
            }
        };
        apply.start();
        let job = ApplyJob {
            attempt,
            key: key.clone(),
            work,
        };
        let sent = self.provider_admin.as_ref().is_some_and(|pool| {
            pool.try_submit(ProviderAdminJob::DecisionApply(Box::new(job)))
                .is_ok()
        });
        if !sent {
            apply.finish(ApplyOutcome::Unavailable);
        }
    }

    /// Everything one attempt needs, from the writer's own state: the
    /// retained receipt, the trusted Store executor, the delegate, the
    /// authority reader and the current global barrier (the way a card's
    /// answer builds its proof). Absent pieces stop the trigger; the model's
    /// exact retry stays the fallback.
    fn decision_apply_work(
        &self,
        key: &BotWaitEnrollmentKey,
        consumer: &BotExecutionFence,
    ) -> Result<ApplyWork, DecisionApplyGap> {
        let view = approved_receipt(
            self.decision_waits.receipt(key.decision_id),
            key,
            &origin_of(key),
        )
        .ok_or(DecisionApplyGap::Absent)?;
        let koru_delegate_core::decisions::DecisionBindingV1::KoruEdit(binding) =
            &view.receipt.binding
        else {
            return Err(DecisionApplyGap::Absent);
        };
        let recovery = self
            .native_recovery
            .as_ref()
            .ok_or(DecisionApplyGap::Absent)?;
        if !recovery.gate.ready() {
            return Err(DecisionApplyGap::NotNow);
        }
        let port = self.bot_delegate.clone().ok_or(DecisionApplyGap::Absent)?;
        let authority = self.bot_authority.clone().ok_or(DecisionApplyGap::Absent)?;
        let bot = match self.repository.bot(binding.bot) {
            Ok(Some(bot)) => bot,
            Ok(None) => return Err(DecisionApplyGap::Absent),
            Err(_) => return Err(DecisionApplyGap::NotNow),
        };
        if bot.user_scope != binding.owner.user_scope
            || bot.owner_device != binding.owner.device
            || bot.home_workspace != binding.workspace
        {
            return Err(DecisionApplyGap::Absent);
        }
        let global = match self.lifecycle_policy_barrier(&bot) {
            Ok(Some(barrier)) => barrier,
            Ok(None) => {
                crate::worker::bots::policy::barrier(&bot).map_err(|_| DecisionApplyGap::NotNow)?
            }
            Err(_) => return Err(DecisionApplyGap::NotNow),
        };
        Ok(ApplyWork {
            store: recovery.store.clone(),
            port,
            authority,
            view: view.clone(),
            consumer: consumer.clone(),
            proof: koru_delegate_core::FreshBotPolicyProof {
                global,
                routine: None,
            },
        })
    }

    /// One finished attempt. The writer records nothing durable: it owes the
    /// shared effect's two notifications, then updates the wait's transient
    /// trigger state if the task still waits on the same decision.
    pub(in crate::worker) fn complete_decision_apply(&mut self, result: ApplyResult) {
        let ApplyResult {
            attempt,
            key,
            report,
        } = result;
        if report.settled {
            self.recover_bot_policy(true);
        }
        if let Some((owner, bot, native)) = report.uncertain {
            let run = native.run;
            let _ = self.queue_native_recovery(
                owner,
                bot,
                Some(run),
                Some(native),
                koru_delegate_core::NativeDrainReason::UnconfirmedOperation,
            );
        }
        tracing::debug!(outcome = ?report.outcome, "approved edit attempt finished");
        if let Some(apply) = self
            .bot_chats
            .get_mut(&attempt)
            .and_then(|chat| chat.decision.as_mut())
            .filter(|wait| wait.key == key)
            .and_then(|wait| wait.apply.as_mut())
        {
            apply.finish(report.outcome);
        }
    }

    /// The one caller of the unit-qualified atomic rollover. Afterwards only
    /// the old fence's transient staging is retired and observation starts on
    /// the acknowledged consumer with its reset cursor.
    fn rollover_consumer(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        resume: &DecisionResume,
    ) -> bool {
        let Ok(replay) = self.repository.bot_replay_state(attempt) else {
            Self::defer_decision(chat);
            return true;
        };
        let Some(old) = replay.fence.clone() else {
            Self::defer_decision(chat);
            return true;
        };
        // The rollover requires the decision's card (O129): write or replay
        // it first, in case the write after enrollment did not land.
        let _ = self.ensure_bot_decision_card(key);
        let rolled = match self.repository.rollover_bot_decision_replay(
            ai_owner(key),
            key,
            resume,
            &old,
            replay.last_sequence,
            Timestamp::now(),
        ) {
            Ok(rolled) => rolled,
            Err(_) => {
                Self::defer_decision(chat);
                return true;
            }
        };
        let Some(turn) = rolled.consumer_fence.turn else {
            Self::defer_decision(chat);
            return true;
        };
        if chat
            .pending
            .as_ref()
            .is_some_and(|staged| staged.batch.fence == rolled.old_fence)
        {
            chat.pending = None;
        }
        for action in std::mem::take(&mut chat.live) {
            if let AgentLiveAction::OfferApproval { invocation, .. }
            | AgentLiveAction::OfferQuestions { invocation, .. } = action
                && !self.bot_next_message_ids.contains(&invocation)
            {
                self.agent_live.remove(&invocation);
            }
        }
        chat.fence = Some(rolled.consumer_fence.clone());
        chat.mapper = Some(AgentEventMapper::new(
            attempt,
            turn,
            chat.input.mode.as_ref().map(|m| m.as_str().to_owned()),
            UsageDelivery::EachEvent,
        ));
        chat.decision = Some(DecisionWait::consumer(
            key.clone(),
            rolled.consumer_fence.clone(),
        ));
        chat.observe_now();
        true
    }

    /// `false` ends this task's live state: the writer closed it.
    fn close_needed_you(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotWaitEnrollmentKey,
        observed: &BotDecisionWaitObservation,
    ) -> bool {
        match self.repository.close_bot_decision_needed_you(
            ai_owner(key),
            key,
            observed,
            Timestamp::now(),
        ) {
            Ok(
                closure @ (BotDecisionClosure::Closed { .. } | BotDecisionClosure::ClosedBefore),
            ) => {
                // Only after the writer: the one closing event (O128) when
                // this call wrote the closure, then the transient task goes.
                if let Some(event) = needed_you_event(attempt, closure, Timestamp::now()) {
                    self.publish_public(event);
                }
                // Only after the writer: remove the matching transient task.
                for action in &chat.live {
                    if let AgentLiveAction::OfferApproval { invocation, .. }
                    | AgentLiveAction::OfferQuestions { invocation, .. } = action
                        && !self.bot_next_message_ids.contains(invocation)
                    {
                        self.agent_live.remove(invocation);
                    }
                }
                self.streams.remove(&attempt);
                self.cancellations.remove(&attempt);
                self.work_terminal(attempt);
                self.decision_waits.forget(key.decision_id);
                false
            }
            // Another cause ended it; that cause's owner drives this task.
            Ok(BotDecisionClosure::AlreadyEnded) => {
                chat.decision = None;
                self.decision_waits.forget(key.decision_id);
                true
            }
            Ok(BotDecisionClosure::Owed) | Err(_) => {
                Self::defer_decision(chat);
                true
            }
        }
    }
}
