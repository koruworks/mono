// SPDX-License-Identifier: MPL-2.0

//! Every `RunFact` becomes a decision.
//!
//! The point of this file is the first test: it walks the whole `RunFact`
//! vocabulary and asserts each variant either names a `delegate.*` row or
//! is one of the two the roadmap has not given a row yet. `row` has no
//! wildcard arm, so a variant added later fails to compile here; this
//! catches the other half — a variant quietly mapped to nothing.

use koru_delegate_core::{
    ActorRef, AgentTurnId, AmendmentRevision, ApprovalId, ApprovalOutcome, ArtifactVersionId,
    AttachmentId, AttemptGeneration, CancelReason, Completeness, DocumentId, EventId, FailureStage,
    ModelRef, PriorAttempt, ProfileId, Rating, ResumeSeam, RunAttemptId, RunId, ScopeOverlap,
    SessionId, TurnOriginator, UsageFactId, WorkspaceId,
};
use koru_delegate_events::{EmitContext, RunChain, RunRef, Stamp};
use koru_delegate_runtime::{RunFact, UsageReport};
use koru_session_protocol::{CachedInputRelationship, ErrorCode};

use super::{NoRow, Row, row};

fn chain() -> (RunChain, RunId) {
    let run = RunId::mint();
    let ctx = EmitContext::system("delegate-test");
    let chain = RunChain::rejoin(
        &ctx,
        RunRef {
            run,
            document: DocumentId::new(),
            workspace: WorkspaceId::new(),
        },
        EventId::mint(),
    );
    (chain, run)
}

fn stamp() -> Stamp {
    Stamp {
        event_id: EventId::mint(),
        occurred_at_ms: 1_770_000_000_000,
    }
}

/// One of every variant, so the walk below is a walk over the vocabulary
/// and not over the ones somebody remembered.
fn every_fact(run: RunId) -> Vec<RunFact> {
    let attempt = RunAttemptId::mint();
    let generation = AttemptGeneration::FIRST;
    let turn = AgentTurnId::mint();
    let session = SessionId::mint();
    vec![
        RunFact::Created {
            run,
            workspace: WorkspaceId::new(),
            document: DocumentId::new(),
            context_items: 2,
        },
        RunFact::Started {
            run,
            attempt,
            generation,
            session,
        },
        RunFact::Interrupted {
            run,
            attempt,
            generation,
            prior_attempt: PriorAttempt::Ended,
        },
        RunFact::Resumed {
            run,
            attempt,
            generation,
            via: ResumeSeam::RecordedContext,
        },
        RunFact::Completed {
            run,
            attempt,
            generation,
            artifacts: 1,
        },
        RunFact::Failed {
            run,
            attempt: Some(attempt),
            generation,
            stage: FailureStage::ProviderStart,
        },
        RunFact::Cancelled {
            run,
            attempt: Some(attempt),
            generation,
            reason: CancelReason::Run,
        },
        RunFact::ScopeBusy {
            run,
            attempt,
            holder: RunId::mint(),
            holder_attempt: RunAttemptId::mint(),
            overlap: ScopeOverlap::Inside,
            others: 0,
        },
        RunFact::Gap {
            run,
            attempt,
            events_lost: Some(3),
        },
        RunFact::TurnStarted {
            run,
            attempt,
            turn,
            originator: TurnOriginator::Orchestrator,
        },
        RunFact::ToolsCompleted {
            run,
            attempt,
            turn,
            calls: 4,
            failed: 1,
        },
        RunFact::ApprovalRequired {
            run,
            attempt,
            turn,
            approval: ApprovalId::mint(),
        },
        RunFact::ApprovalResolved {
            run,
            attempt,
            turn,
            approval: ApprovalId::mint(),
            outcome: ApprovalOutcome::Approved,
        },
        RunFact::WaitingForInput { run, attempt, turn },
        RunFact::AttachmentAccepted { run, attempt, turn },
        RunFact::AttachmentRejected {
            run,
            attempt,
            turn,
            attachment: AttachmentId::mint(),
            code: ErrorCode::AttachmentLimit,
        },
        RunFact::ChildSessionDetected {
            run,
            attempt,
            turn,
            child: SessionId::mint(),
            terminated: true,
        },
        RunFact::TurnCompleted {
            run,
            attempt,
            turn,
            originator: TurnOriginator::Orchestrator,
            artifacts_registered: 0,
        },
        RunFact::TurnCancelled {
            run,
            attempt,
            turn,
            originator: TurnOriginator::Human,
            reason: CancelReason::TakeoverBoundary,
        },
        RunFact::TurnFailed {
            run,
            attempt,
            turn,
            originator: TurnOriginator::Orchestrator,
            stage: FailureStage::Turn,
        },
        RunFact::ControlTaken {
            run,
            attempt,
            discovered: false,
        },
        RunFact::ControlReturned { run, attempt },
        RunFact::ArtifactRegistered {
            run,
            attempt,
            artifact: ArtifactVersionId::mint(),
        },
        RunFact::Usage {
            run,
            attempt,
            turn,
            report: UsageReport {
                usage: UsageFactId::mint(),
                revision: AmendmentRevision::FIRST,
                input: Some(120),
                output: Some(40),
                cached_input: Some(20),
                model: ModelRef::mint(),
                completeness: Completeness::Complete,
                cached_relationship: CachedInputRelationship::Included,
            },
        },
        RunFact::Rated {
            run,
            rated_by: ActorRef::Person {
                profile: ProfileId::new(),
            },
            rating: Rating::Good,
            has_note: true,
        },
    ]
}

/// The registry row each fact names — the whole vocabulary, decided.
fn expected(fact: &RunFact) -> Result<&'static str, NoRow> {
    match fact {
        // The root is built by `BusRunEvents::root`, which needs the
        // creation intent as well as the fact.
        RunFact::Created { .. } => Ok("delegate.run.created"),
        RunFact::Started { .. } => Ok("delegate.run.started"),
        RunFact::Interrupted { .. } => Ok("delegate.run.interrupted"),
        RunFact::Resumed { .. } => Ok("delegate.run.resumed"),
        RunFact::Completed { .. } => Ok("delegate.run.completed"),
        RunFact::Failed { .. } => Ok("delegate.run.failed"),
        RunFact::Cancelled { .. } => Ok("delegate.run.cancelled"),
        RunFact::ScopeBusy { .. } => Ok("delegate.scope.busy"),
        RunFact::Gap { .. } => Ok("delegate.timeline.gap"),
        RunFact::TurnStarted { .. } => Ok("delegate.turn.started"),
        RunFact::ToolsCompleted { .. } => Ok("delegate.tool.completed"),
        RunFact::ApprovalRequired { .. } => Ok("delegate.approval.required"),
        RunFact::ApprovalResolved { .. } => Ok("delegate.approval.resolved"),
        RunFact::WaitingForInput { .. } => Ok("delegate.input.waiting"),
        RunFact::AttachmentAccepted { .. } => Err(NoRow::AttachmentAccepted),
        RunFact::AttachmentRejected { .. } => Ok("delegate.attachment.rejected"),
        RunFact::ChildSessionDetected { .. } => Ok("delegate.child_session.detected"),
        RunFact::TurnCompleted { .. } => Ok("delegate.turn.completed"),
        RunFact::TurnCancelled { .. } => Ok("delegate.turn.cancelled"),
        RunFact::TurnFailed { .. } => Ok("delegate.turn.failed"),
        RunFact::ControlTaken { .. } => Ok("delegate.control.taken"),
        RunFact::ControlReturned { .. } => Ok("delegate.control.returned"),
        RunFact::ArtifactRegistered { .. } => Err(NoRow::ArtifactRegistered),
        RunFact::Usage { .. } => Ok("delegate.turn.usage"),
        RunFact::Rated { .. } => Ok("delegate.run.rated"),
    }
}

/// UC1 at the seam: every fact a run can publish is either a named
/// `delegate.*` row or a decision that it has none. Nothing falls through.
#[test]
fn every_run_fact_names_its_row_or_says_why_it_has_none() {
    let (chain, run) = chain();
    let facts = every_fact(run);
    // The vocabulary walked here is the whole vocabulary: `RunFact::name`
    // is exhaustive, so two facts sharing a name would mean one is missing.
    let names: std::collections::BTreeSet<&str> = facts.iter().map(|fact| fact.name()).collect();
    assert_eq!(names.len(), facts.len(), "a variant is listed twice");

    for fact in &facts {
        match (row(&chain, stamp(), fact), expected(fact)) {
            (Row::Built(envelope), Ok(name)) => {
                assert_eq!(envelope.name.as_str(), name, "{}", fact.name());
                assert_eq!(
                    envelope.correlation_id,
                    Some(chain.root()),
                    "{} left its chain",
                    fact.name()
                );
            }
            (Row::Root, Ok("delegate.run.created")) => {}
            (Row::None(reason), Err(expected)) => assert_eq!(reason, expected),
            (built, expected) => {
                panic!("{} mapped to {built:?}, expected {expected:?}", fact.name())
            }
        }
    }
}

/// The stamp is the caller's: the row carries the id the orchestrator
/// committed the fact under, so a re-publication is the same event.
#[test]
fn a_rows_identity_and_moment_are_the_stamp_it_was_given() {
    let (chain, run) = chain();
    let stamp = stamp();
    let fact = RunFact::Started {
        run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
    };
    let Row::Built(envelope) = row(&chain, stamp, &fact) else {
        panic!("a started fact is a row");
    };
    assert_eq!(envelope.event_id, stamp.event_id);
    assert_eq!(envelope.occurred_at_ms, stamp.occurred_at_ms);
}
