// SPDX-License-Identifier: MPL-2.0

//! What a run did, on the bus (§7.5, P1.8).
//!
//! The orchestrator speaks one closed `RunFact` enum; the journal takes
//! `delegate.*` envelopes that only `koru-delegate-events` may spell. This
//! is the one place the two meet, and it meets them by **exhaustive
//! match** — [`row`] has no wildcard arm, so a fact added to the runtime is
//! a build error here until someone says which registry row it becomes or
//! that it becomes none.
//!
//! **The chain.** Every row of a run correlates to the real `EventId` of
//! its `delegate.run.created`, so a publisher needs that id, the Run
//! document and the workspace before it can build anything else. A run
//! created in this process keeps its [`RunChain`] in memory. A run whose
//! `created` went out before a restart is **rejoined from the ledger**:
//! `run_intents` names the workspace and the document, and the run's own
//! publication list holds the row whose body is the `created` fact — the
//! id it was first sent under. No column was added for it, because the
//! queue that exists to remember publication identity already remembers
//! this one.
//!
//! **Retry identity.** The orchestrator mints the `EventId` and commits
//! the fact under it before calling `publish`, and re-delivers a queued row
//! under the same id; the journal dedupes. The moment comes from the queue
//! row too, so the envelope a retry sends is the envelope the first
//! delivery sent, byte for byte, rather than one carrying today's clock.
//!
//! Nothing here decides what an event says. Every attribute comes from the
//! fact or from an intent written once at step 1, and no builder in
//! `koru-delegate-events` has a parameter that can hold a person's text.

use std::collections::HashMap;

use koru_api_client::Client;
use koru_delegate_core::{EventId, RunId, TurnIdentity, UsageFact};
use koru_delegate_events::{
    ApprovalRequired, ApprovalResolved, AttachmentRejected, AttachmentRejection,
    ChildSessionDetected, ControlReturned, ControlTaken, DelegatedAgent, EmitContext, Envelope,
    InputWaiting, RunCancelled, RunChain, RunCompleted, RunCreated, RunFailed, RunInterrupted,
    RunRated, RunRef, RunResumed, RunStarted, ScopeBusy, ScopeKind, Stamp, TimelineGap,
    ToolsCompleted, TurnCancelled, TurnCompleted, TurnFailed, TurnStarted, TurnUsage,
};
use koru_delegate_runtime::{PublishFailure, RunEvents, RunFact, RunStore};

use crate::ledger::SharedLedger;

/// The component name the worker's envelopes are emitted under. Every
/// `delegate.*` row names its own actor, so this is the context's default
/// and never reaches a row.
const COMPONENT: &str = "delegate";

/// Why a fact has no `delegate.*` row of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoRow {
    /// §7.5 records an accepted attachment as a step; the bus has a row
    /// for a *refusal* only, because the refusal is the news.
    AttachmentAccepted,
    /// `delegate.artifact.registered` names the family, the ordinal and
    /// whether the version declares provenance. A `ResultReference` carries
    /// none of the three, and neither does `RunFact::ArtifactRegistered`
    /// which is built from one — so the registration is in the Run document
    /// and in the Result tab, and not on the bus. Publishing it needs the
    /// three carried from the registrations port through the fact, which is
    /// a change to both shapes and P1.10's to make.
    ArtifactRegistered,
}

impl NoRow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AttachmentAccepted => "attachment_accepted",
            Self::ArtifactRegistered => "artifact_registered",
        }
    }
}

/// What one fact becomes.
#[derive(Debug)]
pub enum Row {
    /// The envelope to publish.
    Built(Box<Envelope>),
    /// The chain's own root, which [`RunChain::created`] builds because it
    /// *makes* the chain rather than joining one.
    Root,
    /// No row today, for a reason that is a decision and not an oversight.
    None(NoRow),
}

/// One fact as a row of its run's chain. **Exhaustive on purpose**: there
/// is no wildcard, so a new `RunFact` variant does not silently vanish.
pub fn row(chain: &RunChain, stamp: Stamp, fact: &RunFact) -> Row {
    let built = |envelope| Row::Built(Box::new(envelope));
    match fact {
        RunFact::Created { .. } => Row::Root,
        RunFact::Started {
            attempt,
            generation,
            session,
            ..
        } => built(chain.started(
            stamp,
            RunStarted {
                attempt: *attempt,
                generation: *generation,
                session: *session,
            },
        )),
        RunFact::Interrupted {
            attempt,
            generation,
            prior_attempt,
            ..
        } => built(chain.interrupted(
            stamp,
            RunInterrupted {
                attempt: *attempt,
                generation: *generation,
                prior_attempt: *prior_attempt,
            },
        )),
        RunFact::Resumed {
            attempt,
            generation,
            via,
            ..
        } => built(chain.resumed(
            stamp,
            RunResumed {
                attempt: *attempt,
                generation: *generation,
                via: *via,
            },
        )),
        RunFact::Completed {
            attempt,
            generation,
            artifacts,
            ..
        } => built(chain.completed(
            stamp,
            RunCompleted {
                attempt: *attempt,
                generation: *generation,
                artifacts: *artifacts,
            },
        )),
        RunFact::Failed {
            attempt,
            generation,
            stage,
            ..
        } => built(chain.failed(
            stamp,
            RunFailed {
                attempt: *attempt,
                generation: *generation,
                stage: *stage,
            },
        )),
        RunFact::Cancelled {
            attempt,
            generation,
            reason,
            ..
        } => built(chain.cancelled(
            stamp,
            RunCancelled {
                attempt: *attempt,
                generation: *generation,
                reason: *reason,
            },
        )),
        RunFact::ScopeBusy {
            holder,
            holder_attempt,
            overlap,
            others,
            ..
        } => built(chain.scope_busy(
            stamp,
            ScopeBusy {
                holder: *holder,
                holder_attempt: *holder_attempt,
                overlap: *overlap,
                others: *others,
            },
        )),
        RunFact::Gap {
            attempt,
            events_lost,
            ..
        } => built(chain.timeline_gap(
            stamp,
            TimelineGap {
                attempt: *attempt,
                events_lost: *events_lost,
            },
        )),
        RunFact::TurnStarted {
            attempt,
            turn,
            originator,
            ..
        } => built(chain.turn_started(
            stamp,
            TurnStarted {
                turn: TurnIdentity {
                    attempt: *attempt,
                    turn: *turn,
                    originator: *originator,
                },
            },
        )),
        RunFact::ToolsCompleted {
            attempt,
            turn,
            calls,
            failed,
            ..
        } => built(chain.tools_completed(
            stamp,
            ToolsCompleted {
                attempt: *attempt,
                turn: *turn,
                calls: *calls,
                failed: *failed,
            },
        )),
        RunFact::ApprovalRequired {
            attempt,
            turn,
            approval,
            ..
        } => built(chain.approval_required(
            stamp,
            ApprovalRequired {
                attempt: *attempt,
                turn: *turn,
                approval: *approval,
            },
        )),
        RunFact::ApprovalResolved {
            attempt,
            turn,
            approval,
            outcome,
            ..
        } => built(chain.approval_resolved(
            stamp,
            ApprovalResolved {
                attempt: *attempt,
                turn: *turn,
                approval: *approval,
                outcome: *outcome,
            },
        )),
        RunFact::WaitingForInput { attempt, turn, .. } => built(chain.input_waiting(
            stamp,
            InputWaiting {
                attempt: *attempt,
                turn: *turn,
            },
        )),
        RunFact::AttachmentAccepted { .. } => Row::None(NoRow::AttachmentAccepted),
        RunFact::AttachmentRejected {
            attempt,
            turn,
            attachment,
            code,
            ..
        } => built(chain.attachment_rejected(
            stamp,
            AttachmentRejected {
                attempt: *attempt,
                turn: *turn,
                attachment: *attachment,
                reason: AttachmentRejection::of(code.clone()),
            },
        )),
        RunFact::ChildSessionDetected {
            attempt,
            child,
            terminated,
            ..
        } => built(chain.child_session_detected(
            stamp,
            ChildSessionDetected {
                attempt: *attempt,
                child_session: *child,
                terminated: *terminated,
            },
        )),
        RunFact::TurnCompleted {
            attempt,
            turn,
            originator,
            artifacts_registered,
            ..
        } => built(chain.turn_completed(
            stamp,
            TurnCompleted {
                turn: TurnIdentity {
                    attempt: *attempt,
                    turn: *turn,
                    originator: *originator,
                },
                artifacts: *artifacts_registered,
            },
        )),
        RunFact::TurnCancelled {
            attempt,
            turn,
            originator,
            reason,
            ..
        } => built(chain.turn_cancelled(
            stamp,
            TurnCancelled {
                turn: TurnIdentity {
                    attempt: *attempt,
                    turn: *turn,
                    originator: *originator,
                },
                reason: *reason,
            },
        )),
        RunFact::TurnFailed {
            attempt,
            turn,
            originator,
            stage,
            ..
        } => built(chain.turn_failed(
            stamp,
            TurnFailed {
                turn: TurnIdentity {
                    attempt: *attempt,
                    turn: *turn,
                    originator: *originator,
                },
                stage: *stage,
            },
        )),
        RunFact::ControlTaken {
            attempt,
            discovered,
            ..
        } => built(chain.control_taken(
            stamp,
            ControlTaken {
                attempt: *attempt,
                discovered: *discovered,
            },
        )),
        RunFact::ControlReturned { attempt, .. } => {
            built(chain.control_returned(stamp, ControlReturned { attempt: *attempt }))
        }
        RunFact::ArtifactRegistered { .. } => Row::None(NoRow::ArtifactRegistered),
        RunFact::Usage {
            attempt,
            turn,
            report,
            ..
        } => built(chain.turn_usage(
            stamp,
            &TurnUsage {
                fact: UsageFact {
                    identity: report.usage,
                    attempt: *attempt,
                    turn: *turn,
                    revision: report.revision,
                    input: report.input,
                    output: report.output,
                    cached_input: report.cached_input,
                    model: report.model,
                    completeness: report.completeness,
                },
                cached_relationship: report.cached_relationship,
            },
        )),
        RunFact::Rated {
            rated_by,
            rating,
            has_note,
            ..
        } => built(chain.run_rated(
            stamp,
            RunRated {
                rated_by: *rated_by,
                rating: *rating,
                has_note: *has_note,
            },
        )),
    }
}

/// The orchestrator's `RunEvents`, over the gateway's `/v1/events`.
pub struct BusRunEvents {
    client: Client,
    ctx: EmitContext,
    ledger: SharedLedger,
    chains: HashMap<RunId, RunChain>,
}

impl BusRunEvents {
    pub fn new(client: Client, ledger: SharedLedger) -> Self {
        Self {
            client,
            ctx: EmitContext::system(COMPONENT),
            ledger,
            chains: HashMap::new(),
        }
    }

    /// The moment the fact was queued, which is the moment it happened
    /// (§17.1 commits first, then queues). A retry reads the same number,
    /// so the envelope it sends is the one the first delivery sent.
    fn queued_at(&self, event: EventId) -> i64 {
        self.ledger
            .publication(event)
            .ok()
            .flatten()
            .map(|record| record.queued_at_ms)
            .unwrap_or_else(now_ms)
    }

    /// This run's chain — created in this process, or rejoined from the
    /// ledger after a restart. `None` when the ledger cannot name the run's
    /// document or the id its `created` went out under, which is a run this
    /// device has no record of.
    fn chain(&mut self, run: RunId) -> Option<&RunChain> {
        if !self.chains.contains_key(&run) {
            let chain = self.rejoin(run)?;
            self.chains.insert(run, chain);
        }
        self.chains.get(&run)
    }

    fn rejoin(&self, run: RunId) -> Option<RunChain> {
        let record = self.ledger.intent(run).ok().flatten()?;
        let document = record.run_document?;
        // The `created` fact's own publication row: the identity it first
        // travelled under, which every later row correlates to. Found by
        // reading the bodies rather than by trusting an order, because two
        // rows can share a millisecond.
        let created = self
            .ledger
            .publications(run)
            .ok()?
            .into_iter()
            .find(|publication| {
                matches!(
                    serde_json::from_str::<RunFact>(publication.body.as_str()),
                    Ok(RunFact::Created { .. })
                )
            })?
            .event;
        Some(RunChain::rejoin(
            &self.ctx,
            RunRef {
                run,
                document,
                workspace: record.workspace,
            },
            created,
        ))
    }

    /// `delegate.run.created`: the root, which also makes the chain. Built
    /// from the fact and from the creation intent, whose asker, launch
    /// profile and scope kind were settled at step 1 and are never
    /// rewritten — so a re-publication after a restart reads the same three.
    fn root(&mut self, stamp: Stamp, fact: &RunFact) -> Option<Envelope> {
        let RunFact::Created {
            run,
            workspace,
            document,
            context_items,
        } = fact
        else {
            return None;
        };
        let facts = self.ledger.intent_facts(*run).ok().flatten()?;
        let agent = koru_session_profiles::resolve(&facts.agent)
            .ok()
            .and_then(|profile| profile.provider)
            .map(|kind| DelegatedAgent::from_provider_kind(&kind))
            .unwrap_or(DelegatedAgent::Other);
        let (envelope, chain) = RunChain::created(
            &self.ctx,
            RunRef {
                run: *run,
                document: *document,
                workspace: *workspace,
            },
            stamp,
            RunCreated {
                requested_by: facts.requested_by,
                agent,
                scope: ScopeKind::of(&facts.scope),
                context_items: *context_items,
            },
        );
        self.chains.insert(*run, chain);
        Some(envelope)
    }

    fn send(&self, envelope: &Envelope) -> Result<(), PublishFailure> {
        if !self.client.events().available() {
            // The gateway opened no events worker on this machine. Retrying
            // would hold a queue row open for the life of the process; the
            // fact is in the Run document, which is the authority (§17.1).
            tracing::debug!(
                target: "koru::delegate",
                "the gateway serves no event journal; the fact stays in the Run document"
            );
            return Err(PublishFailure { retryable: false });
        }
        match self.client.events().send_envelope(envelope) {
            Ok(_) => Ok(()),
            Err(error) => {
                // A refused row is refused forever: the envelope is the
                // same bytes on every attempt, so trying it again would
                // hold a queue row open for good. Everything else — the
                // gateway restarting, the worker not up yet — is worth
                // another attempt under the same id.
                let retryable = error.code != koru_store_core::errors::Code::InvalidRequest;
                tracing::info!(
                    target: "koru::delegate",
                    code = error.code.as_str(),
                    retryable,
                    "the bus did not take a run fact"
                );
                Err(PublishFailure { retryable })
            }
        }
    }
}

impl RunEvents for BusRunEvents {
    fn publish(&mut self, event: EventId, fact: &RunFact) -> Result<(), PublishFailure> {
        let stamp = Stamp {
            event_id: event,
            occurred_at_ms: self.queued_at(event),
        };
        if matches!(fact, RunFact::Created { .. }) {
            let Some(envelope) = self.root(stamp, fact) else {
                tracing::warn!(
                    target: "koru::delegate",
                    fact = fact.name(),
                    "the creation intent could not be read; the run's chain has no root"
                );
                return Err(PublishFailure { retryable: true });
            };
            return self.send(&envelope);
        }
        let built = match self.chain(fact.run()) {
            Some(chain) => row(chain, stamp, fact),
            None => {
                tracing::warn!(
                    target: "koru::delegate",
                    fact = fact.name(),
                    "this device holds no chain for the run; the fact stays in its document"
                );
                return Err(PublishFailure { retryable: false });
            }
        };
        match built {
            Row::Built(envelope) => self.send(&envelope),
            // The root reaches the arm above; a fact naming itself created
            // twice would be a second root, which the chain refuses to be.
            Row::Root => Ok(()),
            Row::None(reason) => {
                tracing::debug!(
                    target: "koru::delegate",
                    fact = fact.name(),
                    reason = reason.as_str(),
                    "the fact has no bus row; it stays in the Run document"
                );
                Ok(())
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
