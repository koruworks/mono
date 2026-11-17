// SPDX-License-Identifier: MPL-2.0

//! A stream observation is staged before any ordinary event cache is changed.
//! Source publication owns its two frontiers; a physical pump epoch is only a
//! current-producer fence and never permission to reset either frontier.
//!
//! One source-bearing frame at a time: after its exact target is retained the
//! producer pauses (its subscription is dropped) and reconnects only from the
//! accepted frontier pair once the journal has drained. No-op frames advance
//! nothing durable; on reconnect they are replayed and ignored again.

mod fold;
mod snapshot;
#[cfg(test)]
mod tests;
mod usage;

use super::*;
use crate::orchestrator::state::{TurnState, UsageIdentity};
use crate::{
    DecisionUsageLookup, HandleOutcome, RecoveredDecisionUsage, RunSourceObservation, SessionUpdate,
};
use fold::SourceFold;
use koru_delegate_core::decisions::*;
use koru_delegate_core::{AgentTurnId, AttemptGeneration, ModelRef, RunAttemptId, SessionId};
use koru_session_protocol::{AgentEvent, SessionEvent, StreamSequence};
use snapshot::SourceMemory;
pub(crate) use usage::SourceUsageCache;
use usage::{UsageModel, UsageStage};

/// One unknown writer result in the existing per-Run inventory. No raw update
/// queue survives acceptance, and no later event can overwrite this identity.
pub(crate) struct DecisionSourceRetry {
    transition: DecisionTransitionId,
    wait: BotWaitId,
    base: DecisionPublicationBase,
    staged: SourceFold,
    /// The ordinary model-cache key for a model first minted by this target,
    /// installed beside the exact applied identity so later ordinary writes
    /// reuse it rather than mint a duplicate.
    ordinary_model: Option<((String, Option<String>, Option<String>), ModelRef)>,
    /// Positive native end. After Applied the producer stays disconnected;
    /// retirement and wait settlement belong to the cleanup owner.
    ends_process: bool,
}

/// The single reobserve-owed identity. While present the producer stays
/// disconnected and nothing past the accepted frontiers is marked consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceObligation {
    /// One exact cold key awaits the fixed worker's bounded lookup.
    Usage(DecisionUsageLookup),
    /// The cold lookup refused (conflicting, malformed or oversized evidence).
    /// It is not retried on a timer; the gap/cleanup owner decides.
    UsageUnavailable,
    /// A Session observation only a Control owner may project (handover).
    Control,
    /// Positive native end was accepted for the current consumer.
    ProcessEnded,
}

/// What the producer does after one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceProgress {
    /// Outside the exclusive window the ordinary owner handled it.
    Ordinary(HandleOutcome),
    /// A no-op or exact duplicate frame: grant the next delivery permit.
    Continue,
    /// A target was retained, or a gap/lookup/control obligation recorded, or
    /// the stream closed. Drop this subscription; reconnect only from the
    /// retained frontiers when the orchestrator offers a producer again.
    Paused,
}

/// Where a decision producer connects: the current consumer's session and
/// the exact accepted frontier pair for that consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceProducer {
    pub session: SessionId,
    pub frontiers: DecisionSourceFrontiers,
}

enum SourceReceipt {
    Pending,
    Applied,
    StaleProducer,
}

/// The kind of frame, decided before any ledger read or memory freeze.
#[derive(Clone, Copy)]
enum Frame<'a> {
    Agent(StreamSequence, &'a AgentEvent),
    ProcessEnded(StreamSequence),
}

/// The retained applied predecessor every new Source extends.
struct SourceBase {
    previous: DecisionProjectionRecord,
    bookkeeping: Option<DecisionSourceBookkeeping>,
    wait: BotWaitId,
    fence: DecisionFence,
    frontiers: DecisionSourceFrontiers,
}

impl Orchestrator {
    /// A physical reconnect rotates only this eligibility token. It cannot
    /// erase accepted frontiers or an exact unknown SQL receipt obligation.
    /// A new consumer (attempt/generation/session) starts its own cursor.
    pub fn register_source_producer(&mut self, run: RunId, epoch: uuid::Uuid) {
        if let Some(state) = self.runs.get_mut(&run) {
            state.source_epoch = Some(epoch);
            if let Some(identity) = state.attempt.as_ref().and_then(|attempt| {
                attempt
                    .session
                    .map(|session| (attempt.id, attempt.generation, session))
            }) && state.source_identity != Some(identity)
            {
                state.source_identity = Some(identity);
                state.source_frontiers.agent_after = None;
                state.source_frontiers.session_after = None;
                state.source_gap = false;
            }
        }
    }

    /// The producer to connect now, if any. Offered only while the exclusive
    /// handle is attached here, the journal is empty, no exact retry or
    /// obligation is retained, and the current resume names this consumer.
    pub fn source_producer(&mut self, run: RunId) -> Result<Option<SourceProducer>, RunError> {
        let Some(state) = self.runs.get(&run) else {
            return Ok(None);
        };
        if !matches!(
            state.doc,
            RunDocumentOwner::Attached {
                exclusive: true,
                ..
            }
        ) || state.decision_source.is_some()
            || state.source_obligation.is_some()
            || state.source_gap
        {
            return Ok(None);
        }
        let Some((attempt, generation, session)) = state.attempt.as_ref().and_then(|attempt| {
            attempt
                .session
                .map(|session| (attempt.id, attempt.generation, session))
        }) else {
            return Ok(None);
        };
        // The origin Koru stopped for its wait has ended; nothing connects
        // to it again. The successor is another consumer.
        if state.source_origin_ended == Some((attempt, generation, session)) {
            return Ok(None);
        }
        let owner = self.publication_owner(run)?;
        if self
            .bot_ledger()?
            .next_decision_projection(owner, run)?
            .is_some()
        {
            return Ok(None);
        }
        let Some(base) = self.source_base(run, owner)? else {
            return Ok(None);
        };
        // The current resume determines the fence. Until working memory names
        // that same consumer nothing can be accepted, so nothing connects.
        if (
            base.fence.attempt,
            base.fence.generation,
            base.fence.session,
        ) != (attempt, generation, session)
        {
            return Ok(None);
        }
        Ok(Some(SourceProducer {
            session,
            frontiers: base.frontiers,
        }))
    }

    /// Connect the offered producer: rotate the epoch first (revoking any
    /// queued old-epoch frame), then subscribe both streams from their own
    /// cursors. A failed subscription leaves nothing consumed.
    pub fn subscribe_source_producer(
        &mut self,
        run: RunId,
    ) -> Result<Option<(uuid::Uuid, Box<dyn crate::SessionEvents>)>, RunError> {
        let Some(producer) = self.source_producer(run)? else {
            return Ok(None);
        };
        let epoch = uuid::Uuid::new_v4();
        self.register_source_producer(run, epoch);
        let events = self
            .ports
            .sessions
            .subscribe_decision(producer.session, &producer.frontiers)?;
        Ok(Some((epoch, events)))
    }

    pub fn handle_from_producer(
        &mut self,
        run: RunId,
        epoch: uuid::Uuid,
        update: SessionUpdate,
    ) -> Result<SourceProgress, RunError> {
        self.restore_publication_owner(run)?;
        if self
            .runs
            .get(&run)
            .is_none_or(|state| state.source_epoch != Some(epoch))
        {
            return Err(publication_pending().into());
        }
        if !self.run_publication_pending(run) {
            return self.handle(run, update).map(SourceProgress::Ordinary);
        }
        self.accept_publication_source(run, epoch, &update)
    }

    fn accept_publication_source(
        &mut self,
        run: RunId,
        epoch: uuid::Uuid,
        update: &SessionUpdate,
    ) -> Result<SourceProgress, RunError> {
        // Receipt recovery is independent of current producer eligibility.
        // A positive missing row, however, never authorizes stale insertion.
        self.retry_decision_source(run)?;
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        if state.decision_source.is_some() || state.source_obligation.is_some() {
            return Err(publication_pending().into());
        }
        let frame = match update {
            SessionUpdate::Agent(event) => match &event.event {
                AgentEvent::MessageDelta { .. }
                | AgentEvent::ToolStarted { .. }
                | AgentEvent::ProviderState { .. }
                | AgentEvent::ModeChanged { .. }
                | AgentEvent::Notice { .. }
                | AgentEvent::CommandsUpdated { .. }
                | AgentEvent::ContextUsage { .. } => return Ok(SourceProgress::Continue),
                other => Frame::Agent(event.sequence, other),
            },
            SessionUpdate::Session(event) => match &event.event {
                SessionEvent::Exited { .. } | SessionEvent::Failed { .. } => {
                    Frame::ProcessEnded(event.sequence)
                }
                SessionEvent::HandoverChanged { .. } => {
                    // Handover maps to Control actions with lease follow-through;
                    // a Source cannot carry it. Nothing past the frontier is consumed.
                    state.source_obligation = Some(SourceObligation::Control);
                    return Ok(SourceProgress::Paused);
                }
                _ => return Ok(SourceProgress::Continue),
            },
            SessionUpdate::AgentGap { .. } | SessionUpdate::SessionGap { .. } => {
                return Ok(self.source_gap(run));
            }
            SessionUpdate::Closed { error } => {
                // Upstream overflow is Closed(StreamGap): loss, not process end.
                // A plain close ends only this subscription.
                if error
                    .as_ref()
                    .is_some_and(|e| e.code == koru_session_protocol::ErrorCode::StreamGap)
                {
                    return Ok(self.source_gap(run));
                }
                return Ok(SourceProgress::Paused);
            }
        };
        let owner = self.publication_owner(run)?;
        if self
            .bot_ledger()?
            .next_decision_projection(owner, run)?
            .is_some()
        {
            return Err(publication_pending().into());
        }
        let base = self
            .source_base(run, owner)?
            .ok_or_else(publication_pending)?;
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        if state.source_epoch != Some(epoch) || state.source_gap {
            return Err(publication_pending().into());
        }
        // An unprovable or oversized boundary refuses acceptance and keeps the
        // explicit gap/cleanup obligation; it is never truncated or emptied.
        let Ok(memory) =
            SourceMemory::freeze(state.attempt.as_ref().ok_or_else(publication_pending)?)
        else {
            return Ok(self.source_gap(run));
        };
        if memory.attempt != base.fence.attempt
            || memory.generation != base.fence.generation
            || memory.session != Some(base.fence.session)
        {
            return Err(publication_pending().into());
        }
        // The process Koru itself stopped for an enrolled wait ended. That is
        // the origin stop taking effect, not the Run ending: nothing is
        // staged (the Run is not interrupted) and no cleanup is owed. The
        // wait's driver proves the end on its own and records the stop.
        if matches!(frame, Frame::ProcessEnded(_))
            && self.origin_stop_owns_end(owner, run, &base.fence)?
        {
            let ended = (
                base.fence.attempt,
                base.fence.generation,
                base.fence.session,
            );
            self.runs
                .get_mut(&run)
                .ok_or_else(publication_pending)?
                .source_origin_ended = Some(ended);
            return Ok(SourceProgress::Paused);
        }
        let (stream, sequence) = match frame {
            Frame::Agent(sequence, _) => (DecisionSourceStream::Agent, sequence),
            Frame::ProcessEnded(sequence) => (DecisionSourceStream::Session, sequence),
        };
        let resulting =
            advance_frontier(&base.frontiers, stream, sequence).ok_or_else(publication_pending)?;
        let predecessor = DecisionProjectionPredecessor {
            observed_created_at_ms: base.previous.target.predecessor().observed_created_at_ms,
            timeline_start: base
                .previous
                .target
                .timeline_end()
                .map_err(|_| publication_pending())?,
            projection: Some(DecisionProjectionPrior {
                transition: base.previous.identity.transition,
                sequence: base.previous.identity.sequence,
                target_digest: base.previous.identity.target_digest,
            }),
            status: base.previous.target.effects().status.clone(),
            failure_reason: base.previous.target.effects().failure_reason,
        };
        let publication_base = DecisionPublicationBase {
            workspace: base.previous.identity.workspace,
            document: base.previous.identity.document,
            source_schema: 5u8.try_into().map_err(|_| publication_pending())?,
            observed_created_at_ms: predecessor.observed_created_at_ms,
            timeline_len: predecessor.timeline_start,
            subject_digest: base.previous.identity.subject_digest,
            predecessor: predecessor.clone(),
            bookkeeping: base.bookkeeping,
        };
        let boundary = DecisionSourceBoundary {
            fence: base.fence,
            producer_epoch: epoch,
            stream,
            sequence,
            resulting,
        };
        let now = self.ports.clock.now_ms();
        let ends_process = matches!(frame, Frame::ProcessEnded(_));
        let mut ordinary_model = None;
        let staged = match frame {
            Frame::ProcessEnded(_) => SourceFold::process_ended(memory, boundary, predecessor, now),
            Frame::Agent(_, AgentEvent::Usage { turn_id, usage }) => {
                match self.stage_source_usage(run, base.fence.attempt, *turn_id, usage)? {
                    None => return Ok(SourceProgress::Paused),
                    Some((UsageStage::Duplicate, _)) => return Ok(SourceProgress::Continue),
                    Some((UsageStage::Stage { snapshot, model }, key)) => {
                        ordinary_model = model.as_ref().map(|model| (key, model.id));
                        SourceFold::usage(memory, boundary, predecessor, snapshot, model, now)
                    }
                }
            }
            Frame::Agent(_, event) => {
                let child_terminated = match event {
                    AgentEvent::ChildSessionCreated {
                        child_session_id, ..
                    } if !predecessor.status.is_terminal() => {
                        // §7.3 follow-through: ask once, before staging. The
                        // answer is request acceptance, never process death.
                        Some(self.ports.sessions.terminate(*child_session_id).is_ok())
                    }
                    _ => None,
                };
                SourceFold::agent(memory, boundary, predecessor, event, child_terminated, now)
            }
        };
        // A frame one bounded target cannot represent (64 KiB, at most two
        // transitions, a closed transition table) is not split or skipped.
        let Ok(staged) = staged else {
            return Ok(self.source_gap(run));
        };
        self.runs
            .get_mut(&run)
            .ok_or_else(publication_pending)?
            .decision_source = Some(DecisionSourceRetry {
            transition: DecisionTransitionId::mint(),
            wait: base.wait,
            base: publication_base,
            staged,
            ordinary_model,
            ends_process,
        });
        self.retry_decision_source(run)?;
        Ok(SourceProgress::Paused)
    }

    /// Whether the process end of the consumer named by `fence` is Koru's own
    /// origin stop taking effect (M6 contract 4.1 steps 5 to 7; the decision
    /// wait's stop-and-successor chain): the Run has an open wait whose
    /// enrollment the AI worker stored, its origin is exactly this consumer,
    /// no successor exists yet and nobody has asked for the wait's cleanup.
    ///
    /// Enrollment is what authorizes the stop, so from then on the origin's
    /// end is the expected outcome whatever ended it. Everything else is as
    /// before: an end before the enrollment, an end of a successor, and any
    /// end once a person's Stop, the budget, a pause's cleanup or a failure
    /// has moved the wait to cleanup still interrupts the Run and hands the
    /// wait to its cleanup owner.
    fn origin_stop_owns_end(
        &mut self,
        owner: koru_delegate_core::BotOwner,
        run: RunId,
        fence: &DecisionFence,
    ) -> Result<bool, RunError> {
        use koru_delegate_core::children::BotChildWaitEnrollmentReply;
        let stops = |phase: BotWaitPhase| {
            matches!(phase, BotWaitPhase::HoldRequested | BotWaitPhase::Retired)
        };
        let same = |origin: &DecisionFence| {
            origin.attempt == fence.attempt
                && origin.generation == fence.generation
                && origin.session == fence.session
        };
        if let Some(child) = self.bot_ledger()?.child_wait_execution(owner, run)? {
            return Ok(child.resume.is_none()
                && child.wait.cleanup.is_none()
                && stops(child.wait.phase)
                && matches!(
                    child.wait.enrollment,
                    Some(BotChildWaitEnrollmentReply::Enrolled { .. })
                )
                && same(&child.wait.origin));
        }
        let Some(decision) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(false);
        };
        Ok(
            matches!(decision.wait.reason, BotWaitReason::Decision { .. })
                && decision.resume.is_none()
                && decision.wait.cleanup.is_none()
                && stops(decision.wait.phase)
                && matches!(
                    decision.wait.enrollment,
                    Some(BotWaitEnrollmentReply::Enrolled { .. })
                )
                && same(&decision.wait.origin),
        )
    }

    /// Explicit loss on either stream. It is never proof that the process
    /// ended; acceptance stops until the gap/cleanup owner settles it.
    fn source_gap(&mut self, run: RunId) -> SourceProgress {
        if let Some(state) = self.runs.get_mut(&run) {
            state.source_gap = true;
            if let Some(attempt) = state.attempt.as_mut() {
                attempt.approval_evidence_complete = false;
            }
        }
        SourceProgress::Paused
    }

    /// The applied predecessor, current fence and accepted frontier pair. The
    /// current resume determines the fence; retained bookkeeping supplies the
    /// frontiers only for that same consumer, otherwise the in-memory pair of
    /// an equal identity, otherwise an empty cursor for a new consumer.
    fn source_base(
        &mut self,
        run: RunId,
        owner: koru_delegate_core::BotOwner,
    ) -> Result<Option<SourceBase>, RunError> {
        let Some(execution) = self.bot_ledger()?.decision_wait_execution(owner, run)? else {
            return Ok(None);
        };
        let Some(previous) = self
            .bot_ledger()?
            .latest_applied_decision_projection(owner, run)?
            .filter(|record| record.identity.wait == execution.wait.wait)
        else {
            return Ok(None);
        };
        let bookkeeping = previous
            .target
            .resulting_bookkeeping()
            .map_err(|_| publication_pending())?;
        let fence = source_fence(&execution)?;
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        let frontiers = resume_frontiers(
            bookkeeping.as_ref(),
            (fence.attempt, fence.generation, fence.session),
            state.source_identity,
            &state.source_frontiers,
        );
        Ok(Some(SourceBase {
            previous,
            bookkeeping,
            wait: execution.wait.wait,
            fence,
            frontiers,
        }))
    }

    /// O111 staging for one usage report. `None` records the one exact cold
    /// key owed to the fixed worker; the producer pauses without consuming.
    #[allow(clippy::type_complexity)]
    fn stage_source_usage(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        report: &koru_session_protocol::AgentUsage,
    ) -> Result<Option<(UsageStage, (String, Option<String>, Option<String>))>, RunError> {
        use crate::orchestrator::usage::{model_key, model_report};
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        let provider = state
            .provider
            .clone()
            .unwrap_or_else(|| "unknown".parse().expect("a valid provider token"));
        let requested = state.requested_model.clone();
        let reported = report.model.as_ref().map(|name| name.as_str().to_owned());
        let ordinary_key = model_key(provider.as_str(), requested.as_deref(), reported.as_deref());
        let lookup = DecisionUsageLookup {
            attempt,
            turn,
            provider: provider.clone(),
            requested: model_report(requested.as_deref()),
            reported: model_report(reported.as_deref()),
        };
        // The ordinary cache holds only identities already committed to this
        // document; the decision cache holds exact worker/applied evidence.
        let model = match state.models.get(&ordinary_key) {
            Some(model) => Some(UsageModel::Known(*model)),
            None => match state.source_usage.model(&(
                lookup.provider.clone(),
                lookup.requested.clone(),
                lookup.reported.clone(),
            )) {
                Some(Some(model)) => Some(UsageModel::Known(model)),
                Some(None) => Some(UsageModel::Mint {
                    provider: lookup.provider.clone(),
                    requested: lookup.requested.clone(),
                    reported: lookup.reported.clone(),
                }),
                None => None,
            },
        };
        let recovered: Option<RecoveredDecisionUsage> =
            state.source_usage.usage(&(attempt, turn)).cloned();
        let (Some(recovered), Some(model)) = (recovered, model) else {
            state.source_obligation = Some(SourceObligation::Usage(lookup));
            return Ok(None);
        };
        let Ok(stage) = usage::stage_usage(&recovered, model, attempt, turn, report) else {
            // Inconsistent evidence or an exhausted revision: refuse, keep the
            // gap obligation, never guess an amendment.
            state.source_gap = true;
            return Ok(None);
        };
        Ok(Some((stage, ordinary_key)))
    }

    /// The worker's bounded answer for the one owed key, after the actual
    /// handle returned. A refused lookup is retained, never retried in a loop.
    pub fn accept_source_observation(
        &mut self,
        ticket: RunPublicationTicket,
        handle: Box<dyn RunDocumentHandle>,
        observed: PortResult<crate::DecisionUsageObservation>,
    ) -> Result<(), Box<dyn RunDocumentHandle>> {
        self.return_run_publication(ticket, handle)?;
        let Some(state) = self.runs.get_mut(&ticket.run) else {
            return Ok(());
        };
        let Some(SourceObligation::Usage(lookup)) = state.source_obligation.as_ref() else {
            return Ok(());
        };
        match observed {
            Ok(observation) if observation.lookup == *lookup => {
                state.source_usage.record_observation(&observation);
                state.source_obligation = None;
            }
            _ => state.source_obligation = Some(SourceObligation::UsageUnavailable),
        }
        Ok(())
    }

    /// The read-only worker operation owed for this Run, if any. Only while the
    /// journal is empty, so the latest applied marker is the observed base.
    pub(super) fn next_source_observation(
        &mut self,
        run: RunId,
        owner: koru_delegate_core::BotOwner,
    ) -> Result<Option<RunPublicationOperation>, RunError> {
        let Some(SourceObligation::Usage(lookup)) = self
            .runs
            .get(&run)
            .and_then(|state| state.source_obligation.clone())
        else {
            return Ok(None);
        };
        let projection = self
            .bot_ledger()?
            .latest_applied_decision_projection(owner, run)?
            .ok_or_else(publication_pending)?
            .identity;
        Ok(Some(RunPublicationOperation::ObserveSource(
            RunSourceObservation { projection, lookup },
        )))
    }

    pub(crate) fn retry_decision_source(&mut self, run: RunId) -> Result<(), RunError> {
        let Some(retry) = self
            .runs
            .get_mut(&run)
            .and_then(|state| state.decision_source.take())
        else {
            return Ok(());
        };
        let result = self.confirm_decision_source(run, &retry);
        match result {
            Ok(SourceReceipt::Applied) => {
                self.install_decision_source(run, retry)?;
                Ok(())
            }
            Ok(SourceReceipt::Pending) => {
                self.runs
                    .get_mut(&run)
                    .ok_or_else(publication_pending)?
                    .decision_source = Some(retry);
                Ok(())
            }
            Ok(SourceReceipt::StaleProducer) => Err(publication_pending().into()),
            Err(error) => {
                self.runs
                    .get_mut(&run)
                    .ok_or_else(publication_pending)?
                    .decision_source = Some(retry);
                Err(error)
            }
        }
    }

    /// Exact Applied plus the actually returned durable handle: only now does
    /// producer memory, the frontier pair and usage evidence advance.
    fn install_decision_source(
        &mut self,
        run: RunId,
        retry: DecisionSourceRetry,
    ) -> Result<(), RunError> {
        let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
        let memory = retry.staged.memory;
        let target = &retry.staged.target;
        let identity = (
            memory.attempt,
            memory.generation,
            memory.session.ok_or_else(publication_pending)?,
        );
        let mut installed = false;
        if let Some(attempt) = state.attempt.as_mut().filter(|attempt| {
            attempt.id == memory.attempt
                && attempt.generation == memory.generation
                && attempt.session == memory.session
        }) {
            installed = true;
            attempt.orchestrator_turn = memory.orchestrator_turn;
            attempt.bot_terminal_turn = memory.terminal_turn;
            attempt.turns = memory
                .turns
                .as_slice()
                .iter()
                .map(|tally| {
                    (
                        tally.turn,
                        TurnState {
                            originator: tally.originator,
                            tool_calls: tally.tool_calls,
                            tool_failures: tally.tool_failures,
                            failure_breakdown: tally.failure_breakdown,
                        },
                    )
                })
                .collect();
            attempt.pending_approvals = memory.approvals.as_slice().iter().copied().collect();
            // Prompts resolved by this observation lose their retained choices.
            let pending = &attempt.pending_approvals;
            attempt
                .native_prompts
                .retain(|approval, _| pending.contains_key(approval));
            attempt.pending_questions = memory.questions.as_slice().iter().copied().collect();
        }
        let bookkeeping = target
            .source_bookkeeping()
            .ok_or_else(publication_pending)?;
        // An old accepted target can finish after a successor became current.
        // Its receipt remains true; its cursor is not the new session's replay
        // position, and its process end is not the successor's.
        if state.source_identity == Some(identity) {
            let frontiers = &bookkeeping.boundary.resulting;
            state.source_frontiers.agent_after = state
                .source_frontiers
                .agent_after
                .max(frontiers.agent_after);
            state.source_frontiers.session_after = state
                .source_frontiers
                .session_after
                .max(frontiers.session_after);
            if retry.ends_process {
                state.source_obligation = Some(SourceObligation::ProcessEnded);
                // Retiring the ended consumer needs the C cleanup owner first.
                // The existing cleanup tick requests it (AuthorityChanged, or
                // adopts an earlier owner), observes and confirms positively.
                state.decision_cleanup.get_or_insert(
                    crate::orchestrator::state::DecisionCleanupRetry {
                        request: None,
                        confirm: None,
                        due: std::time::Instant::now(),
                        round: 0,
                    },
                );
            }
        }
        // Applied usage is a document fact whatever consumer is current now.
        if let Some(snapshot) = &bookkeeping.usage {
            state
                .source_usage
                .install(snapshot, target.effects().models.as_slice());
            state.usage.insert(
                (snapshot.attempt, snapshot.turn),
                UsageIdentity {
                    id: snapshot.id,
                    revision: snapshot.revision,
                    event: snapshot.event,
                    last: (
                        snapshot.input,
                        snapshot.output,
                        snapshot.cached_input,
                        snapshot.model,
                    ),
                },
            );
        }
        if let Some((key, model)) = retry.ordinary_model {
            state.models.entry(key).or_insert(model);
        }
        // The durable active-budget interval follows installed working memory
        // exactly as the ordinary owner does after an approval or turn change.
        // A failed refresh is retried by the next installed observation.
        if installed && let Err(error) = self.bot_refresh_active_clock(run) {
            self.trace(run, "active_clock_refresh_failed", format_args!("{error}"));
        }
        Ok(())
    }

    fn confirm_decision_source(
        &mut self,
        run: RunId,
        retry: &DecisionSourceRetry,
    ) -> Result<SourceReceipt, RunError> {
        let owner = retry.staged.target.fence().owner;
        let retained = self
            .bot_ledger()?
            .decision_projection(owner, run, retry.transition)?;
        let record = match retained {
            Some(record) => record,
            None => {
                let epoch = retry
                    .staged
                    .target
                    .source_bookkeeping()
                    .ok_or_else(publication_pending)?
                    .boundary
                    .producer_epoch;
                if self
                    .runs
                    .get(&run)
                    .is_none_or(|state| state.source_epoch != Some(epoch))
                {
                    // Positive absence plus a replaced producer ends only the
                    // unaccepted volatile candidate, never retained SQL work.
                    return Ok(SourceReceipt::StaleProducer);
                }
                match self.bot_ledger()?.record_decision_source(
                    owner,
                    retry.transition,
                    retry.wait,
                    &retry.base,
                    &retry.staged.target,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        // A lost commit reply is resolved by exact identity,
                        // before any recomputation or second mutation is made.
                        match self.bot_ledger()?.decision_projection(
                            owner,
                            run,
                            retry.transition,
                        )? {
                            Some(record) => record,
                            None => return Err(error.into()),
                        }
                    }
                }
            }
        };
        if record.target != retry.staged.target
            || record.identity.wait != retry.wait
            || record.identity.transition != retry.transition
            || record.identity.workspace != retry.base.workspace
            || record.identity.document != retry.base.document
            || record.identity.subject_digest != retry.base.subject_digest
            || matches!(record.state, DecisionProjectionState::Superseded { .. })
        {
            return Err(publication_pending().into());
        }
        // SQL acceptance reserves the one source slot; it does not advance
        // working memory. Applied plus the physically returned exclusive
        // handle is the producer's publication boundary.
        let returned = self.runs.get(&run).is_some_and(|state| {
            let Some(returned) = state.publication_returned.as_ref() else {
                return false;
            };
            let Some(prepared) = record.prepared() else {
                return false;
            };
            matches!(&state.doc, RunDocumentOwner::Attached { handle, exclusive: true }
                if handle.durable() && handle.document()
                    .check_returned_projection(prepared, &returned.proof).is_ok())
        });
        if matches!(record.state, DecisionProjectionState::Applied { .. }) && returned {
            Ok(SourceReceipt::Applied)
        } else {
            Ok(SourceReceipt::Pending)
        }
    }

    /// The exact transition of a retained, not yet installed Source, if any.
    pub(super) fn retained_source_transition(&self, run: RunId) -> Option<DecisionTransitionId> {
        self.runs
            .get(&run)
            .and_then(|state| state.decision_source.as_ref())
            .map(|retry| retry.transition)
    }
}

type ConsumerIdentity = (RunAttemptId, AttemptGeneration, SessionId);

/// The accepted frontier pair for the current consumer. Retained bookkeeping
/// counts only when it names that same consumer; otherwise the in-memory pair
/// of an equal identity; otherwise a new consumer's empty cursor. An old
/// consumer's high cursor therefore never refuses a successor's low one.
fn resume_frontiers(
    bookkeeping: Option<&DecisionSourceBookkeeping>,
    current: ConsumerIdentity,
    memory_identity: Option<ConsumerIdentity>,
    memory: &DecisionSourceFrontiers,
) -> DecisionSourceFrontiers {
    bookkeeping
        .filter(|b| {
            (
                b.boundary.fence.attempt,
                b.boundary.fence.generation,
                b.boundary.fence.session,
            ) == current
        })
        .map(|b| b.boundary.resulting.clone())
        .or_else(|| (memory_identity == Some(current)).then(|| memory.clone()))
        .unwrap_or(DecisionSourceFrontiers {
            agent_after: None,
            session_after: None,
        })
}

/// Advance exactly one stream. A known earlier (or equal) boundary never
/// overwrites a later one, and the other stream's frontier is unchanged.
/// Noncontiguous sequences are legal; only an explicit gap proves loss.
fn advance_frontier(
    frontiers: &DecisionSourceFrontiers,
    stream: DecisionSourceStream,
    sequence: StreamSequence,
) -> Option<DecisionSourceFrontiers> {
    let mut resulting = frontiers.clone();
    let frontier = match stream {
        DecisionSourceStream::Agent => &mut resulting.agent_after,
        DecisionSourceStream::Session => &mut resulting.session_after,
    };
    if frontier.is_some_and(|old| sequence <= old) {
        return None;
    }
    *frontier = Some(sequence);
    Some(resulting)
}

fn source_fence(
    execution: &koru_delegate_sqlite::DecisionWaitExecution,
) -> Result<DecisionFence, RunError> {
    let mut fence = execution.wait.origin;
    if let Some(resume) = &execution.resume {
        let acknowledgment = match &resume.state {
            DecisionResumeState::InputAcknowledged { acknowledgment }
            | DecisionResumeState::Closed {
                acknowledgment: Some(acknowledgment),
            } => acknowledgment,
            _ => return Err(publication_pending().into()),
        };
        let (attempt, generation, session, _) = resume.consumer_identity();
        fence.attempt = attempt;
        fence.generation = generation;
        fence.session = session.ok_or_else(publication_pending)?;
        fence.turn = acknowledgment.turn;
    }
    if execution.current_attempt.attempt != fence.attempt
        || execution.current_attempt.generation != fence.generation
        || execution
            .current_attempt
            .provider_session
            .as_ref()
            .and_then(|provider| provider.session)
            != Some(fence.session)
    {
        return Err(publication_pending().into());
    }
    Ok(fence)
}
