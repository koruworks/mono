// SPDX-License-Identifier: MPL-2.0
//! A Bot task held in a child wait (M6, contract 4.1 steps 7 and 11): the
//! receipt-qualified origin terminal interceptor, the held wait and the
//! caller of the atomic replay rollover to the successor.
//!
//! The same shape as the decision wait's interceptor (`decision.rs`), keyed
//! by the wait. The enrollment is permission to ask, never proof: the
//! delegate's matching confirmed origin stop (`ReadBotChildWait`) is read
//! before a terminal is suppressed. A natural completion and a person's Stop
//! keep their ordinary mapping (O127). An uncertain read interprets nothing:
//! the staged candidate and the accepted cursor wait for the next read.
use super::decision::Interception;
use super::*;
use koru_delegate_core::children::{
    BotChildWaitEnrollmentKey, BotChildWaitObservation, ChildResume, ChildWaitDisposition,
};
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, DecisionFence, DecisionResumeState, DecisionValidate,
    FreshWaitStopEligibility, OriginWaitStop,
};

#[cfg(test)]
mod tests;

/// 250 ms doubling to 32 s, as the decision wait's reads.
const MAX_ROUND: u32 = 7;

/// Why a child wait read gave no observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum ChildReadError {
    /// No answer: the delegate is away, slow or answered retryably.
    Unavailable,
    /// `bot_child_wait_unknown`: the delegate holds no such wait.
    Unknown,
    /// Any other definite refusal, or an answer about another wait.
    Refused,
}
pub(in crate::worker) type ChildWaitRead = Result<BotChildWaitObservation, ChildReadError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum ChildStage {
    /// A terminal of the enrolled origin is staged in `chat.pending` while
    /// the delegate's confirmed origin stop is read. Nothing is committed.
    OriginProof,
    /// The proof says this staged terminal keeps its ordinary mapping.
    Ordinary,
    /// The origin terminal was suppressed; the task stays live, waiting for
    /// its successor's acknowledged input or the wait's closure.
    Held,
}

pub(in crate::worker) struct ChildWait {
    pub(super) key: BotChildWaitEnrollmentKey,
    pub(super) stage: ChildStage,
    busy: bool,
    due: Instant,
    rounds: u32,
}
impl ChildWait {
    fn new(key: BotChildWaitEnrollmentKey, stage: ChildStage) -> Self {
        Self {
            key,
            stage,
            busy: false,
            due: Instant::now(),
            rounds: 0,
        }
    }
    /// Only the ordinary verdict lets the observation step run again.
    pub(in crate::worker) fn observes(&self) -> bool {
        matches!(self.stage, ChildStage::Ordinary)
    }
    fn reads(&self) -> bool {
        !matches!(self.stage, ChildStage::Ordinary)
    }
    fn defer(&mut self) {
        self.rounds = (self.rounds + 1).min(MAX_ROUND);
        self.due = Instant::now() + Duration::from_millis(250) * (1u32 << self.rounds);
    }
}

fn origin_of(key: &BotChildWaitEnrollmentKey) -> DecisionFence {
    key.origin()
}
fn same_origin(key: &BotChildWaitEnrollmentKey, fence: &BotExecutionFence) -> bool {
    let origin = origin_of(key);
    fence.owner == origin.owner
        && fence.bot == origin.bot
        && fence.run == origin.run
        && fence.attempt == origin.attempt
        && fence.generation == origin.generation
        && fence.session == origin.session
        && fence.turn == Some(origin.turn)
}
fn ai_owner(key: &BotChildWaitEnrollmentKey) -> koru_ai_core::bots::BotOwner {
    koru_ai_core::bots::BotOwner {
        user_scope: key.owner.user_scope,
        device: key.owner.device,
    }
}
/// The authenticated read, but only for this exact wait.
fn matched<'a>(
    key: &BotChildWaitEnrollmentKey,
    read: &'a ChildWaitRead,
) -> Option<&'a BotChildWaitObservation> {
    match read {
        Ok(observed) if observed.validate().is_ok() && observed.key == *key => Some(observed),
        _ => None,
    }
}

/// Pure verdict on one staged terminal. `ordinary_stop` covers a person's
/// Stop in progress and any replay stop fence; `completed` a natural
/// completion, which is never the outcome of an internal stop (O127).
pub(super) fn classify_child_origin_terminal(
    key: &BotChildWaitEnrollmentKey,
    fence: &BotExecutionFence,
    ordinary_stop: bool,
    completed: bool,
    read: &ChildWaitRead,
) -> Interception {
    if ordinary_stop || completed || !same_origin(key, fence) {
        return Interception::Ordinary;
    }
    let Some(observed) = matched(key, read) else {
        // Unknown, refused, unavailable or another wait: nothing is read
        // into the terminal.
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
            && *cause == BotWaitCleanupCause::ChildWaitCleanup =>
        {
            Interception::Suppress
        }
        OriginWaitStop::Confirmed { .. } => Interception::Uncertain,
        OriginWaitStop::Pending {} => {
            match (&observed.disposition, observed.fresh_stop_eligibility) {
                // The internal stop is still owed: wait for its receipt.
                (
                    ChildWaitDisposition::Open {},
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
pub(super) enum ChildHeldNext {
    /// The successor's continuation input is acknowledged: install it.
    Rollover(ChildResume),
    /// The wait closed without a resume (a Stop, the budget, an unqualified
    /// wait): the task ends.
    Close,
    Wait,
}
pub(super) fn classify_child_held(
    key: &BotChildWaitEnrollmentKey,
    read: &ChildWaitRead,
) -> ChildHeldNext {
    let Some(observed) = matched(key, read) else {
        return ChildHeldNext::Wait;
    };
    if let Some(resume) = &observed.resume
        && resume.validate().is_ok()
        && resume.wait == key.wait_id
        && resume.origin == origin_of(key)
        && matches!(resume.state, DecisionResumeState::InputAcknowledged { .. })
    {
        return ChildHeldNext::Rollover(resume.clone());
    }
    match observed.disposition {
        ChildWaitDisposition::Closed { .. } if observed.resume.is_none() => ChildHeldNext::Close,
        _ => ChildHeldNext::Wait,
    }
}

/// A copy of the staged batch with only the terminal Event effect removed:
/// every frame and sequence stays, so the accepted cursor still advances
/// exactly past the native terminal.
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

fn proof_unavailable() -> AiError {
    // Never shown: a Storage-class result keeps the chat and its staged
    // candidate for the next finite observation.
    AiError::new(AiErrorClass::Storage, "", true)
}

/// One finite `ReadBotChildWait`, on the administration lane. No writer
/// access; its result goes back to the writer.
pub(in crate::worker) struct ChildWaitReadJob {
    attempt: InferenceAttemptId,
    key: BotChildWaitEnrollmentKey,
    delegate: std::sync::Arc<dyn BotDelegatePort>,
    deadline: Instant,
}
impl ChildWaitReadJob {
    pub(in crate::worker) fn run(self) -> crate::worker::bots::children::Completion {
        let read = catch_unwind(AssertUnwindSafe(|| self.read()))
            .unwrap_or(Err(ChildReadError::Unavailable));
        crate::worker::bots::children::Completion::WaitRead {
            attempt: self.attempt,
            key: self.key,
            read,
        }
    }
    fn read(&self) -> ChildWaitRead {
        let within = self.deadline.saturating_duration_since(Instant::now());
        if within.is_zero() {
            return Err(ChildReadError::Unavailable);
        }
        match self.delegate.execute_within(
            DelegateCommand::ReadBotChildWait {
                owner: self.key.owner,
                bot: self.key.bot,
                run: self.key.run,
                wait_id: self.key.wait_id,
            },
            within,
        ) {
            Ok(BotReply::ChildWait { observation })
                if observation.validate().is_ok() && observation.key == self.key =>
            {
                Ok(observation)
            }
            Ok(_) => Err(ChildReadError::Refused),
            Err(error) if error.code == koru_delegate_api::bots::CHILD_WAIT_UNKNOWN => {
                Err(ChildReadError::Unknown)
            }
            Err(error) if error.retryable => Err(ChildReadError::Unavailable),
            Err(_) => Err(ChildReadError::Refused),
        }
    }
}

impl WorkerState {
    /// `None` keeps the candidate staged in `chat.pending` for the proof
    /// read. A task with no child enrollment for this exact origin passes
    /// through unchanged (the decision interceptor runs next).
    pub(super) fn intercept_child_origin_terminal(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
        staged: Staged,
    ) -> Result<Option<Staged>, AiError> {
        if !staged.terminal || chat.is_finishing() || chat.decision.is_some() {
            return Ok(Some(staged));
        }
        if let Some(wait) = &chat.child
            && wait.stage == ChildStage::Ordinary
        {
            chat.child = None;
            return Ok(Some(staged));
        }
        let provenance = &chat.submission.provenance;
        let owner = koru_ai_core::bots::BotOwner {
            user_scope: provenance.owner.user_scope,
            device: provenance.owner.device,
        };
        match self
            .repository
            .bot_child_wait_enrollment_for_origin(owner, id, &staged.batch.fence)
        {
            Ok(None) => Ok(Some(staged)),
            // O127: a natural completion is an ordinary completion, whatever
            // the wait's phase; the delegate closes the wait from its
            // delivery. Nothing needs to be read to know it.
            Ok(Some(_)) if staged.completed => Ok(Some(staged)),
            Ok(Some(key)) => {
                chat.child = Some(ChildWait::new(key, ChildStage::OriginProof));
                chat.pending = Some(staged);
                Ok(None)
            }
            Err(_) => {
                chat.pending = Some(staged);
                Err(proof_unavailable())
            }
        }
    }

    /// At most one child wait read is in flight. It runs on the
    /// administration lane and never occupies the provider Bot slot.
    pub(in crate::worker) fn schedule_bot_child_reads(&mut self) {
        let Some(delegate) = self.bot_delegate.clone() else {
            return;
        };
        if self
            .bot_chats
            .values()
            .any(|chat| chat.child.as_ref().is_some_and(|wait| wait.busy))
        {
            return;
        }
        let now = Instant::now();
        let Some((attempt, key)) = self
            .bot_chats
            .iter()
            .filter_map(|(id, chat)| {
                let wait = chat.child.as_ref()?;
                (wait.reads() && wait.due <= now && !chat.is_finishing())
                    .then(|| (*id, wait.key.clone(), wait.due))
            })
            .min_by_key(|(.., due)| *due)
            .map(|(id, key, _)| (id, key))
        else {
            return;
        };
        let job = ChildWaitReadJob {
            attempt,
            key,
            delegate,
            deadline: now + koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE,
        };
        let sent = self.provider_admin.as_ref().is_some_and(|pool| {
            pool.try_submit(ProviderAdminJob::BotChildWaitRead(job))
                .is_ok()
        });
        if let Some(wait) = self
            .bot_chats
            .get_mut(&attempt)
            .and_then(|chat| chat.child.as_mut())
        {
            if sent {
                wait.busy = true;
            } else {
                wait.defer();
            }
        }
    }

    /// A finished child wait read. A result for another wait is stale.
    pub(in crate::worker) fn complete_chat_child_read(
        &mut self,
        attempt: InferenceAttemptId,
        key: BotChildWaitEnrollmentKey,
        read: ChildWaitRead,
    ) {
        let Some(mut chat) = self.bot_chats.remove(&attempt) else {
            return;
        };
        let Some(wait) = chat.child.as_mut().filter(|wait| wait.key == key) else {
            self.bot_chats.insert(attempt, chat);
            return;
        };
        wait.busy = false;
        let stage = wait.stage;
        if chat.is_finishing() {
            // A person's Stop supersedes the wait; its own mapping applies.
            chat.child = None;
        } else {
            match stage {
                ChildStage::OriginProof => {
                    self.decide_child_origin_terminal(attempt, &mut chat, &key, &read)
                }
                ChildStage::Held => self.advance_child_held(attempt, &mut chat, &key, &read),
                ChildStage::Ordinary => {}
            }
        }
        self.bot_chats.insert(attempt, chat);
    }

    fn defer_child(chat: &mut LiveChat) {
        if let Some(wait) = chat.child.as_mut() {
            wait.defer();
        }
    }

    fn decide_child_origin_terminal(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotChildWaitEnrollmentKey,
        read: &ChildWaitRead,
    ) {
        let Some(staged) = chat.pending.as_ref() else {
            chat.child = None;
            chat.observe_now();
            return;
        };
        let ordinary_stop = !matches!(
            self.repository.bot_replay_state(attempt),
            Ok(replay) if replay.stop_reason.is_none()
        );
        match classify_child_origin_terminal(
            key,
            &staged.batch.fence,
            ordinary_stop,
            staged.completed,
            read,
        ) {
            Interception::Uncertain => Self::defer_child(chat),
            Interception::Ordinary => {
                if let Some(wait) = chat.child.as_mut() {
                    wait.stage = ChildStage::Ordinary;
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
                        chat.child = Some(ChildWait::new(key.clone(), ChildStage::Held));
                        Self::defer_child(chat);
                    }
                    // Not proven durable: keep the original candidate.
                    Ok(false) | Err(_) => Self::defer_child(chat),
                }
            }
        }
    }

    fn advance_child_held(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotChildWaitEnrollmentKey,
        read: &ChildWaitRead,
    ) {
        match classify_child_held(key, read) {
            ChildHeldNext::Rollover(resume) => {
                self.rollover_child_consumer(attempt, chat, key, &resume)
            }
            ChildHeldNext::Close => {
                // The wait was cleaned up while the origin was held: the Run
                // ends as stopped work, and so does this reply. Observing the
                // retired origin again leads it through the ordinary
                // interruption to its owner's settled admission.
                chat.child = None;
                self.interrupt_bot_chat(attempt);
                chat.observe_now();
            }
            ChildHeldNext::Wait => Self::defer_child(chat),
        }
    }

    /// The one caller of the atomic rollover. Afterwards only the old
    /// fence's transient staging is retired and observation starts on the
    /// acknowledged successor with its reset cursor.
    fn rollover_child_consumer(
        &mut self,
        attempt: InferenceAttemptId,
        chat: &mut LiveChat,
        key: &BotChildWaitEnrollmentKey,
        resume: &ChildResume,
    ) {
        let Ok(replay) = self.repository.bot_replay_state(attempt) else {
            Self::defer_child(chat);
            return;
        };
        let Some(old) = replay.fence.clone() else {
            Self::defer_child(chat);
            return;
        };
        let rolled = match self.repository.rollover_bot_child_replay(
            ai_owner(key),
            key,
            resume,
            &old,
            replay.last_sequence,
            Timestamp::now(),
        ) {
            Ok(rolled) => rolled,
            Err(_) => {
                Self::defer_child(chat);
                return;
            }
        };
        let Some(turn) = rolled.consumer_fence.turn else {
            Self::defer_child(chat);
            return;
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
        // The successor is an ordinary execution of this task from here on:
        // its own terminal is its own (a second wait enrolls again).
        chat.child = None;
        chat.observe_now();
    }
}
