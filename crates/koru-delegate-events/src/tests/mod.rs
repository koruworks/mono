// SPDX-License-Identifier: MPL-2.0

//! Pure unit tests. Nothing here touches disk (the vectors are the one
//! exception, and only under `KORU_UPDATE_EVENTS=1`), reaches a store or a
//! journal, or sends anything; the dependency test shells out to `cargo
//! tree` and nothing else.
//!
//! `every_row` is the fixture the crate's promises are checked against: one
//! envelope per registry row, built through the builders with deterministic
//! ids and clocks, so the validity, privacy and vector tests all speak about
//! the same 33 envelopes and a row added to the registry without a builder —
//! or a builder without a row — fails `rows::every_delegate_row_has_exactly_one_builder`.

use std::collections::BTreeSet;

use koru_artifact_core::ArtifactFamilyId;
use koru_delegate_core::{
    ActorRef, AgentTurnId, AmendmentRevision, ApprovalId, ApprovalOutcome, ArtifactVersionId,
    AttachmentId, AttemptGeneration, CancelReason, Completeness, DocumentId, EconomicsSummary,
    FailureStage, IncompleteReason, ModelRef, PriorAttempt, ProfileId, Rating, RatingNote,
    ResumeSeam, RunAttemptId, RunId, RunRating, ScheduleId, SessionId, TokenTotal, TurnIdentity,
    TurnOriginator, UsageFact, UsageFactId, WorkspaceId,
};
use koru_events::{Actor, DeviceId, EmitContext, Envelope, EventId, SessionId as BusSessionId};
use koru_session_protocol::CachedInputRelationship;
use uuid::Uuid;

use crate::*;

mod actors;
mod dependency;
mod identity;
mod privacy;
mod private;
mod rows;
mod tokens;
mod vectors;

/// A deterministic v4 UUID — version nibble 4, variant 8 — so strict ids
/// accept it and a fixture is the same on every run.
pub(crate) fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

/// An event id from a fixture number, in its own range so it can never
/// collide with a `v4(n)` domain id.
pub(crate) fn event_id(n: u8) -> EventId {
    EventId(
        format!("00000000-0000-4000-8000-00000000ee{n:02x}")
            .parse()
            .unwrap(),
    )
}

pub(crate) const T0: i64 = 1_756_000_000_000;

/// A stamp at a fixed clock, with a fixed id: `n` is the row's own number,
/// so no two rows of `every_row` share an id.
pub(crate) fn stamp(n: u8) -> Stamp {
    Stamp {
        event_id: event_id(n),
        occurred_at_ms: T0 + i64::from(n) * 1_000,
    }
}

pub(crate) fn ctx() -> EmitContext {
    EmitContext {
        session_id: BusSessionId(v4(0xa1)),
        device_id: Some(DeviceId(v4(0xa2))),
        profile_id: Some(koru_events::ProfileId(v4(0xa3))),
        actor: Actor::system("delegate-worker"),
    }
}

pub(crate) fn run_ref() -> RunRef {
    RunRef {
        run: RunId::from_uuid(v4(1)),
        document: DocumentId::from_uuid(v4(2)),
        workspace: WorkspaceId::from_uuid(v4(3)),
    }
}

/// A second run, in the same workspace, for isolation tests.
pub(crate) fn other_run_ref() -> RunRef {
    RunRef {
        run: RunId::from_uuid(v4(0x11)),
        document: DocumentId::from_uuid(v4(0x12)),
        workspace: WorkspaceId::from_uuid(v4(3)),
    }
}

pub(crate) fn schedule_ref() -> ScheduleRef {
    ScheduleRef {
        schedule: ScheduleId::from_uuid(v4(0x0f)),
        document: DocumentId::from_uuid(v4(0x16)),
        workspace: WorkspaceId::from_uuid(v4(3)),
    }
}

pub(crate) fn scheduled_run() -> RunId {
    RunId::from_uuid(v4(0x17))
}

pub(crate) fn schedule_chain() -> ScheduleChain {
    ScheduleChain::created(
        &ctx(),
        schedule_ref(),
        stamp(27),
        ScheduleCreated {
            requested_by: person(),
            revision: 1,
        },
    )
    .1
}

pub(crate) fn attempt() -> RunAttemptId {
    RunAttemptId::from_uuid(v4(4))
}

/// A session-protocol id from a fixture number. Those ids are minted or
/// parsed, never wrapped, so the fixture goes through their strict parser.
pub(crate) fn protocol_id<T: std::str::FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    v4(n).hyphenated().to_string().parse().unwrap()
}

pub(crate) fn turn_id() -> AgentTurnId {
    protocol_id(5)
}

pub(crate) fn session() -> SessionId {
    protocol_id(6)
}

pub(crate) fn approval() -> ApprovalId {
    protocol_id(0x0a)
}

pub(crate) fn person() -> ActorRef {
    ActorRef::Person {
        profile: ProfileId::from_uuid(v4(7)),
    }
}

pub(crate) fn orchestrator_turn() -> TurnIdentity {
    TurnIdentity {
        attempt: attempt(),
        turn: turn_id(),
        originator: TurnOriginator::Orchestrator,
    }
}

pub(crate) fn human_turn() -> TurnIdentity {
    TurnIdentity {
        attempt: attempt(),
        turn: turn_id(),
        originator: TurnOriginator::Human,
    }
}

pub(crate) fn created_fact() -> RunCreated {
    RunCreated {
        requested_by: person(),
        agent: DelegatedAgent::Codex,
        scope: ScopeKind::ProjectFolder,
        context_items: 3,
    }
}

/// The chain of `run_ref`, rooted at `stamp(0)`.
pub(crate) fn chain() -> RunChain {
    RunChain::created(&ctx(), run_ref(), stamp(0), created_fact()).1
}

pub(crate) fn usage_fact(input: Option<u64>, revision: u32) -> UsageFact {
    UsageFact {
        identity: UsageFactId::from_uuid(v4(8)),
        attempt: attempt(),
        turn: turn_id(),
        revision: AmendmentRevision(revision),
        input,
        output: Some(140),
        cached_input: None,
        model: ModelRef::from_uuid(v4(9)),
        completeness: Completeness::Complete,
    }
}

pub(crate) fn summary() -> EconomicsSummary {
    EconomicsSummary {
        orchestrator_turns: 3,
        human_turns: 1,
        turns_with_usage: 3,
        input: TokenTotal {
            known: 1_200,
            unavailable_turns: 1,
        },
        output: TokenTotal {
            known: 400,
            unavailable_turns: 0,
        },
        cached_input: TokenTotal {
            known: 0,
            unavailable_turns: 3,
        },
        tool_calls: 12,
        interventions: 2,
        elapsed_ms: Some(65_000),
        models: BTreeSet::from([ModelRef::from_uuid(v4(9))]),
        complete: false,
    }
}

pub(crate) const NOTE_SENTINEL: &str = "NOTE-SENTINEL it was fine";

pub(crate) fn rating_with_note() -> RunRating {
    RunRating {
        rating: Rating::Good,
        note: Some(RatingNote::new(NOTE_SENTINEL).unwrap()),
        rated_by: person(),
        rated_at_ms: T0,
    }
}

/// One envelope per registry row, in the order the registry lists them,
/// each under its own stamp. The root is `stamp(0)`.
pub(crate) fn every_row() -> Vec<(&'static str, Envelope)> {
    let ctx = ctx();
    let (created, chain) = RunChain::created(&ctx, run_ref(), stamp(0), created_fact());
    let (schedule_created, schedule_chain) = ScheduleChain::created(
        &ctx,
        schedule_ref(),
        stamp(27),
        ScheduleCreated {
            requested_by: person(),
            revision: 1,
        },
    );
    let generation = AttemptGeneration::FIRST;
    vec![
        ("delegate.run.created", created),
        (
            "delegate.run.started",
            chain.started(
                stamp(1),
                RunStarted {
                    attempt: attempt(),
                    generation,
                    session: session(),
                },
            ),
        ),
        (
            "delegate.run.interrupted",
            chain.interrupted(
                stamp(2),
                RunInterrupted {
                    attempt: attempt(),
                    generation,
                    prior_attempt: PriorAttempt::Ended,
                },
            ),
        ),
        (
            "delegate.run.resumed",
            chain.resumed(
                stamp(3),
                RunResumed {
                    attempt: RunAttemptId::from_uuid(v4(0x14)),
                    generation: generation.next(),
                    via: ResumeSeam::RecordedContext,
                },
            ),
        ),
        (
            "delegate.run.completed",
            chain.completed(
                stamp(4),
                RunCompleted {
                    attempt: attempt(),
                    generation,
                    artifacts: 1,
                },
            ),
        ),
        (
            "delegate.run.failed",
            chain.failed(
                stamp(5),
                RunFailed {
                    attempt: None,
                    generation,
                    stage: FailureStage::ContextCapture,
                },
            ),
        ),
        (
            "delegate.run.cancelled",
            chain.cancelled(
                stamp(6),
                RunCancelled {
                    attempt: Some(attempt()),
                    generation,
                    reason: CancelReason::Run,
                },
            ),
        ),
        (
            "delegate.turn.started",
            chain.turn_started(
                stamp(7),
                TurnStarted {
                    turn: orchestrator_turn(),
                },
            ),
        ),
        (
            "delegate.turn.completed",
            chain.turn_completed(
                stamp(8),
                TurnCompleted {
                    turn: orchestrator_turn(),
                    artifacts: 1,
                },
            ),
        ),
        (
            "delegate.turn.failed",
            chain.turn_failed(
                stamp(9),
                TurnFailed {
                    turn: human_turn(),
                    stage: FailureStage::Turn,
                },
            ),
        ),
        (
            "delegate.turn.cancelled",
            chain.turn_cancelled(
                stamp(10),
                TurnCancelled {
                    turn: orchestrator_turn(),
                    reason: CancelReason::TakeoverBoundary,
                },
            ),
        ),
        (
            "delegate.tool.completed",
            chain.tools_completed(
                stamp(11),
                ToolsCompleted {
                    attempt: attempt(),
                    turn: turn_id(),
                    calls: 7,
                    failed: 2,
                },
            ),
        ),
        (
            "delegate.approval.required",
            chain.approval_required(
                stamp(12),
                ApprovalRequired {
                    attempt: attempt(),
                    turn: turn_id(),
                    approval: approval(),
                },
            ),
        ),
        (
            "delegate.approval.resolved",
            chain.approval_resolved(
                stamp(13),
                ApprovalResolved {
                    attempt: attempt(),
                    turn: turn_id(),
                    approval: approval(),
                    outcome: ApprovalOutcome::Approved,
                },
            ),
        ),
        (
            "delegate.input.waiting",
            chain.input_waiting(
                stamp(14),
                InputWaiting {
                    attempt: attempt(),
                    turn: turn_id(),
                },
            ),
        ),
        (
            "delegate.child_session.detected",
            chain.child_session_detected(
                stamp(15),
                ChildSessionDetected {
                    attempt: attempt(),
                    child_session: protocol_id(0x0b),
                    terminated: true,
                },
            ),
        ),
        (
            "delegate.attachment.rejected",
            chain.attachment_rejected(
                stamp(16),
                AttachmentRejected {
                    attempt: attempt(),
                    turn: turn_id(),
                    attachment: protocol_id::<AttachmentId>(0x0c),
                    reason: AttachmentRejection::Limit,
                },
            ),
        ),
        (
            "delegate.timeline.gap",
            chain.timeline_gap(
                stamp(17),
                TimelineGap {
                    attempt: attempt(),
                    events_lost: None,
                },
            ),
        ),
        (
            "delegate.control.taken",
            chain.control_taken(
                stamp(18),
                ControlTaken {
                    attempt: attempt(),
                    discovered: false,
                },
            ),
        ),
        (
            "delegate.control.returned",
            chain.control_returned(stamp(19), ControlReturned { attempt: attempt() }),
        ),
        (
            "delegate.artifact.registered",
            chain.artifact_registered(
                stamp(20),
                ArtifactRegistered {
                    attempt: attempt(),
                    turn: Some(turn_id()),
                    artifact: ArtifactVersionId::from_uuid(v4(0x0d)),
                    family: ArtifactFamilyId::from_uuid(v4(0x0e)),
                    ordinal: 1,
                    derived: false,
                },
            ),
        ),
        (
            "delegate.artifact.attached",
            chain.artifact_attached(
                stamp(21),
                ArtifactAttached {
                    artifact: ArtifactVersionId::from_uuid(v4(0x0d)),
                },
            ),
        ),
        (
            "delegate.turn.usage",
            chain.turn_usage(
                stamp(22),
                &TurnUsage {
                    fact: usage_fact(Some(100), 0),
                    cached_relationship: CachedInputRelationship::Separate,
                },
            ),
        ),
        (
            "delegate.run.usage",
            chain.run_usage(
                stamp(23),
                &RunUsage {
                    summary: summary(),
                    outcome: RunOutcome::Completed,
                },
            ),
        ),
        (
            "delegate.run.rated",
            chain.run_rated(stamp(24), RunRated::from(&rating_with_note())),
        ),
        (
            "delegate.scope.busy",
            chain.scope_busy(
                stamp(25),
                ScopeBusy {
                    holder: RunId::from_uuid(v4(0x11)),
                    holder_attempt: RunAttemptId::from_uuid(v4(0x15)),
                    overlap: ScopeOverlap::Inside,
                    others: 1,
                },
            ),
        ),
        (
            "delegate.scope.skipped",
            scope_skipped(
                &ctx,
                stamp(26),
                ScopeSkipped {
                    workspace: WorkspaceId::from_uuid(v4(3)),
                    schedule: ScheduleId::from_uuid(v4(0x0f)),
                    schedule_document: None,
                    holder: RunId::from_uuid(v4(0x11)),
                    holder_attempt: RunAttemptId::from_uuid(v4(0x15)),
                    overlap: ScopeOverlap::Same,
                    others: 0,
                },
            ),
        ),
        ("delegate.schedule.created", schedule_created),
        (
            "delegate.schedule.updated",
            schedule_chain.updated(stamp(28), ScheduleUpdated { revision: 2 }),
        ),
        (
            "delegate.schedule.paused",
            schedule_chain.paused(
                stamp(29),
                SchedulePaused {
                    revision: 3,
                    reason: SchedulePauseReason::ContextUnavailable {
                        item: 2,
                        stage: ScheduleContextFailureStage::Materialize,
                    },
                },
            ),
        ),
        (
            "delegate.schedule.fired",
            schedule_chain.fired(
                stamp(30),
                ScheduleFired {
                    revision: 2,
                    occurrence_ms: T0 + 86_400_000,
                    run: scheduled_run(),
                },
            ),
        ),
        (
            "delegate.schedule.skipped",
            schedule_chain.skipped(
                stamp(31),
                ScheduleSkipped {
                    revision: 2,
                    occurrence_ms: T0 + 172_800_000,
                    reason: ScheduleSkipReason::busy(run_ref().run),
                },
            ),
        ),
        (
            "delegate.private_operation.settled",
            chain.private_operation_settled(
                stamp(32),
                private::private_fact(PrivateOutcome::Confirmed {
                    outcome_bytes: 2048,
                    withheld: false,
                }),
            ),
        ),
    ]
}

/// The `Incomplete` variants, for the tests that walk a vocabulary.
pub(crate) fn every_completeness() -> Vec<Completeness> {
    vec![
        Completeness::Complete,
        Completeness::Incomplete {
            reason: IncompleteReason::CachedInputRelationshipUndeclared,
        },
        Completeness::Incomplete {
            reason: IncompleteReason::MultipleModels,
        },
        Completeness::Incomplete {
            reason: IncompleteReason::NoUsageReported,
        },
    ]
}
