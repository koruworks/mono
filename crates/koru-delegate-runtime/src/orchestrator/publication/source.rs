// SPDX-License-Identifier: MPL-2.0

//! A stream observation is staged before any ordinary event cache is changed.
//! Source publication owns its two frontiers; a physical pump epoch is only a
//! current-producer fence and never permission to reset either frontier.

mod fold;
mod snapshot;

use super::*;
use crate::orchestrator::state::TurnState;
use crate::{HandleOutcome, SessionUpdate};
use fold::SourceFold;
use koru_delegate_core::decisions::*;
use snapshot::SourceMemory;

/// One unknown writer result in the existing per-Run inventory. No raw update
/// queue survives acceptance, and no later event can overwrite this identity.
pub(crate) struct DecisionSourceRetry {
    transition: DecisionTransitionId,
    wait: BotWaitId,
    base: DecisionPublicationBase,
    staged: SourceFold,
}

enum SourceReceipt {
    Pending,
    Applied,
    StaleProducer,
}

impl Orchestrator {
    /// A physical reconnect rotates only this eligibility token. It cannot
    /// erase accepted frontiers or an exact unknown SQL receipt obligation.
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

    pub fn handle_from_producer(
        &mut self,
        run: RunId,
        epoch: uuid::Uuid,
        update: SessionUpdate,
    ) -> Result<HandleOutcome, RunError> {
        self.restore_publication_owner(run)?;
        if self
            .runs
            .get(&run)
            .is_none_or(|state| state.source_epoch != Some(epoch))
        {
            return Err(publication_pending().into());
        }
        if !self.run_publication_pending(run) {
            return self.handle(run, update);
        }
        self.accept_publication_source(run, epoch, &update)?;
        Ok(HandleOutcome::Continue)
    }

    fn accept_publication_source(
        &mut self,
        run: RunId,
        epoch: uuid::Uuid,
        update: &SessionUpdate,
    ) -> Result<(), RunError> {
        // Receipt recovery is independent of current producer eligibility.
        // A positive missing row, however, never authorizes stale insertion.
        self.retry_decision_source(run)?;
        if self
            .runs
            .get(&run)
            .is_some_and(|state| state.decision_source.is_some())
        {
            return Err(publication_pending().into());
        }
        let owner = self.publication_owner(run)?;
        let execution = self
            .bot_ledger()?
            .decision_wait_execution(owner, run)?
            .ok_or_else(publication_pending)?;
        if self
            .bot_ledger()?
            .next_decision_projection(owner, run)?
            .is_some()
        {
            return Err(publication_pending().into());
        }
        let previous = self
            .bot_ledger()?
            .latest_applied_decision_projection(owner, run)?
            .filter(|record| record.identity.wait == execution.wait.wait)
            .ok_or_else(publication_pending)?;
        let bookkeeping = previous
            .target
            .resulting_bookkeeping()
            .map_err(|_| publication_pending())?;
        let state = self.runs.get(&run).ok_or_else(publication_pending)?;
        if state.source_epoch != Some(epoch) || state.source_gap {
            return Err(publication_pending().into());
        }
        let memory = SourceMemory::freeze(state.attempt.as_ref().ok_or_else(publication_pending)?)
            .map_err(|_| publication_pending())?;
        let fence = source_fence(&execution)?;
        if memory.attempt != fence.attempt
            || memory.generation != fence.generation
            || memory.session != Some(fence.session)
        {
            return Err(publication_pending().into());
        }
        let identity = (fence.attempt, fence.generation, fence.session);
        let mut resulting = bookkeeping
            .as_ref()
            .filter(|b| {
                (
                    b.boundary.fence.attempt,
                    b.boundary.fence.generation,
                    b.boundary.fence.session,
                ) == identity
            })
            .map(|b| b.boundary.resulting.clone())
            .or_else(|| {
                (state.source_identity == Some(identity)).then(|| state.source_frontiers.clone())
            })
            .unwrap_or(DecisionSourceFrontiers {
                agent_after: None,
                session_after: None,
            });
        let (stream, sequence, event) = match update {
            SessionUpdate::Agent(event) => {
                (DecisionSourceStream::Agent, event.sequence, &event.event)
            }
            // Session/gap/usage staging has its own owner below; never advance
            // an unsupported observation as if it had been retained.
            _ => return Err(publication_pending().into()),
        };
        if resulting.agent_after.is_some_and(|old| sequence <= old) {
            return Err(publication_pending().into());
        }
        resulting.agent_after = Some(sequence);
        let predecessor = DecisionProjectionPredecessor {
            observed_created_at_ms: previous.target.predecessor().observed_created_at_ms,
            timeline_start: previous
                .target
                .timeline_end()
                .map_err(|_| publication_pending())?,
            projection: Some(DecisionProjectionPrior {
                transition: previous.identity.transition,
                sequence: previous.identity.sequence,
                target_digest: previous.identity.target_digest,
            }),
            status: previous.target.effects().status.clone(),
            failure_reason: previous.target.effects().failure_reason,
        };
        let base = DecisionPublicationBase {
            workspace: previous.identity.workspace,
            document: previous.identity.document,
            source_schema: 5u8.try_into().map_err(|_| publication_pending())?,
            observed_created_at_ms: predecessor.observed_created_at_ms,
            timeline_len: predecessor.timeline_start,
            subject_digest: previous.identity.subject_digest,
            predecessor: predecessor.clone(),
            bookkeeping,
        };
        let staged = SourceFold::agent(
            memory,
            DecisionSourceBoundary {
                fence,
                producer_epoch: epoch,
                stream,
                sequence,
                resulting,
            },
            predecessor,
            event,
            self.ports.clock.now_ms(),
        )
        .map_err(|_| publication_pending())?;
        self.runs
            .get_mut(&run)
            .ok_or_else(publication_pending)?
            .decision_source = Some(DecisionSourceRetry {
            transition: DecisionTransitionId::mint(),
            wait: execution.wait.wait,
            base,
            staged,
        });
        self.retry_decision_source(run)
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
                let state = self.runs.get_mut(&run).ok_or_else(publication_pending)?;
                let memory = retry.staged.memory;
                if let Some(attempt) = state.attempt.as_mut().filter(|attempt| {
                    attempt.id == memory.attempt
                        && attempt.generation == memory.generation
                        && attempt.session == memory.session
                }) {
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
                    attempt.pending_approvals =
                        memory.approvals.as_slice().iter().copied().collect();
                    attempt.pending_questions =
                        memory.questions.as_slice().iter().copied().collect();
                }
                let frontiers = &retry
                    .staged
                    .target
                    .source_bookkeeping()
                    .ok_or_else(publication_pending)?
                    .boundary
                    .resulting;
                let identity = (
                    memory.attempt,
                    memory.generation,
                    memory.session.ok_or_else(publication_pending)?,
                );
                // An old accepted target can finish after a successor became
                // current. Its receipt remains true; its cursor is not the new
                // session's replay position.
                if state.source_identity == Some(identity) {
                    state.source_frontiers.agent_after = state
                        .source_frontiers
                        .agent_after
                        .max(frontiers.agent_after);
                    state.source_frontiers.session_after = state
                        .source_frontiers
                        .session_after
                        .max(frontiers.session_after);
                }
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
