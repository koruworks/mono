// SPDX-License-Identifier: MPL-2.0

//! The facts a run publishes — what P1.8 turns into `delegate.*` rows.
//!
//! A closed enum mirroring §7.5's timeline rows plus the lifecycle, control
//! and economics facts of §7.4 and §17. Every variant is ids, counts, times
//! and closed vocabularies, and serializes as the body a pending
//! publication stores (§7.1's queue) — so a fact is the same bytes on the
//! first send and on every retry, and the journal dedupes on the
//! [`crate::EventId`] the orchestrator minted for it.
//!
//! **A fact carries what its row needs.** The body in the queue is what a
//! re-publication after a restart is rebuilt from, so anything the event
//! row spells has to be *in* the fact: the attempt generation, the
//! overlap a busy start proceeded onto, the code an attachment was refused
//! with. Re-deriving those from the ledger at retry time would read
//! whatever is true *then* — a resumed run has a later generation than the
//! interruption that is still queued — and publish a row that was never
//! true. What a fact does not carry is what never changes: a run's asker,
//! agent and scope kind come from the creation intent, which is written
//! once and never rewritten.
//!
//! **Nothing here can carry text.** No variant has a `String`, an
//! objective, a title, a path or a note; the one closed-vocabulary field a
//! fact carries beyond ids and counts is a stage, a reason, an outcome or
//! an originator from the run vocabulary.
//! `crates/koru-delegate-runtime/src/tests/privacy.rs` feeds a sentinel
//! objective through a whole run and asserts it is in no fact.

use crate::{
    ActorRef, AgentTurnId, AmendmentRevision, ApprovalId, ApprovalOutcome, ArtifactVersionId,
    AttachmentId, AttemptGeneration, CancelReason, Completeness, DocumentId, FailureStage,
    ModelRef, PriorAttempt, Rating, ResumeSeam, RunAttemptId, RunId, ScopeOverlap, SessionId,
    TurnOriginator, UsageFactId, WorkspaceId,
};
use koru_session_protocol::{CachedInputRelationship, ErrorCode};
use serde::{Deserialize, Serialize};

/// One turn's usage, as a fact (§17.2): the stable identity, the revision
/// a retry repeats and a correction raises, the counts, and the model
/// reference. `None` is unavailable, never zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    pub usage: UsageFactId,
    pub revision: AmendmentRevision,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached_input: Option<u64>,
    pub model: ModelRef,
    /// Complete, or the one reason it is not (§17.2) — the row's own word,
    /// so a retry does not have to guess why a number was a guess.
    pub completeness: Completeness,
    /// What the adapter declared about `cached_input`'s relationship to
    /// `input`; the reason a fact can be incomplete with every count
    /// present.
    pub cached_relationship: CachedInputRelationship,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum RunFact {
    // -------------------------------------------------------- lifecycle
    /// The Run document was committed (§7.1 step 3).
    Created {
        run: RunId,
        workspace: WorkspaceId,
        document: DocumentId,
        context_items: u32,
    },
    /// An attempt's provider is up.
    Started {
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        session: SessionId,
    },
    /// A restart, or the provider's end, ended the attempt (D10).
    Interrupted {
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        prior_attempt: PriorAttempt,
    },
    Resumed {
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        via: ResumeSeam,
    },
    Completed {
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        artifacts: u32,
    },
    Failed {
        run: RunId,
        attempt: Option<RunAttemptId>,
        generation: AttemptGeneration,
        stage: FailureStage,
    },
    Cancelled {
        run: RunId,
        attempt: Option<RunAttemptId>,
        generation: AttemptGeneration,
        reason: CancelReason,
    },
    /// A manual start proceeded onto a held folder (D34). The holder is the
    /// earliest overlapping claim — the one the person was shown — and
    /// `others` counts the rest, so the row names who was there without
    /// naming the folder.
    ScopeBusy {
        run: RunId,
        attempt: RunAttemptId,
        holder: RunId,
        holder_attempt: RunAttemptId,
        overlap: ScopeOverlap,
        others: u32,
    },
    /// Facts Koru never received (§7.1).
    Gap {
        run: RunId,
        attempt: RunAttemptId,
        events_lost: Option<u32>,
    },

    // -------------------------------------------------------- execution
    TurnStarted {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        originator: TurnOriginator,
    },
    /// `ToolCompleted` events, coalesced per turn (§7.5).
    ToolsCompleted {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        calls: u32,
        failed: u32,
    },
    ApprovalRequired {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        approval: ApprovalId,
    },
    ApprovalResolved {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        approval: ApprovalId,
        outcome: ApprovalOutcome,
    },
    WaitingForInput {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
    },
    AttachmentAccepted {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
    },
    AttachmentRejected {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        attachment: AttachmentId,
        /// The session protocol's own closed code. A token, never its text.
        code: ErrorCode,
    },
    /// A child session appeared (§7.3). `terminated` says whether the
    /// termination this orchestrator asked for was accepted.
    ChildSessionDetected {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        child: SessionId,
        terminated: bool,
    },
    TurnCompleted {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        originator: TurnOriginator,
        artifacts_registered: u32,
    },
    TurnCancelled {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        originator: TurnOriginator,
        reason: CancelReason,
    },
    TurnFailed {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        originator: TurnOriginator,
        stage: FailureStage,
    },

    // ---------------------------------------------------------- control
    /// The human lease was granted (§7.4). `discovered` marks a takeover
    /// found already in place on recovery.
    ControlTaken {
        run: RunId,
        attempt: RunAttemptId,
        discovered: bool,
    },
    ControlReturned {
        run: RunId,
        attempt: RunAttemptId,
    },

    // -------------------------------------------------------- artifacts
    ArtifactRegistered {
        run: RunId,
        attempt: RunAttemptId,
        artifact: ArtifactVersionId,
    },

    // -------------------------------------------------------- economics
    Usage {
        run: RunId,
        attempt: RunAttemptId,
        turn: AgentTurnId,
        report: UsageReport,
    },
    Rated {
        run: RunId,
        rated_by: ActorRef,
        rating: Rating,
        /// Whether a note was written. Never the note (D44).
        has_note: bool,
    },
}

impl RunFact {
    /// The closed vocabulary a fact is reported under.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Created { .. } => "run.created",
            Self::Started { .. } => "run.started",
            Self::Interrupted { .. } => "run.interrupted",
            Self::Resumed { .. } => "run.resumed",
            Self::Completed { .. } => "run.completed",
            Self::Failed { .. } => "run.failed",
            Self::Cancelled { .. } => "run.cancelled",
            Self::ScopeBusy { .. } => "run.scope_busy",
            Self::Gap { .. } => "run.gap",
            Self::TurnStarted { .. } => "turn.started",
            Self::ToolsCompleted { .. } => "turn.tools_completed",
            Self::ApprovalRequired { .. } => "turn.approval_required",
            Self::ApprovalResolved { .. } => "turn.approval_resolved",
            Self::WaitingForInput { .. } => "turn.waiting_for_input",
            Self::AttachmentAccepted { .. } => "turn.attachment_accepted",
            Self::AttachmentRejected { .. } => "turn.attachment_rejected",
            Self::ChildSessionDetected { .. } => "turn.child_session_detected",
            Self::TurnCompleted { .. } => "turn.completed",
            Self::TurnCancelled { .. } => "turn.cancelled",
            Self::TurnFailed { .. } => "turn.failed",
            Self::ControlTaken { .. } => "control.taken",
            Self::ControlReturned { .. } => "control.returned",
            Self::ArtifactRegistered { .. } => "artifact.registered",
            Self::Usage { .. } => "turn.usage",
            Self::Rated { .. } => "run.rated",
        }
    }

    pub fn run(&self) -> RunId {
        match self {
            Self::Created { run, .. }
            | Self::Started { run, .. }
            | Self::Interrupted { run, .. }
            | Self::Resumed { run, .. }
            | Self::Completed { run, .. }
            | Self::Failed { run, .. }
            | Self::Cancelled { run, .. }
            | Self::ScopeBusy { run, .. }
            | Self::Gap { run, .. }
            | Self::TurnStarted { run, .. }
            | Self::ToolsCompleted { run, .. }
            | Self::ApprovalRequired { run, .. }
            | Self::ApprovalResolved { run, .. }
            | Self::WaitingForInput { run, .. }
            | Self::AttachmentAccepted { run, .. }
            | Self::AttachmentRejected { run, .. }
            | Self::ChildSessionDetected { run, .. }
            | Self::TurnCompleted { run, .. }
            | Self::TurnCancelled { run, .. }
            | Self::TurnFailed { run, .. }
            | Self::ControlTaken { run, .. }
            | Self::ControlReturned { run, .. }
            | Self::ArtifactRegistered { run, .. }
            | Self::Usage { run, .. }
            | Self::Rated { run, .. } => *run,
        }
    }

    /// Which queue a fact waits in (§7.1's pending-publication list).
    pub fn publication_kind(&self) -> crate::PublicationKind {
        use crate::PublicationKind;
        match self {
            Self::Usage { .. } => PublicationKind::Usage,
            Self::ArtifactRegistered { .. } => PublicationKind::ArtifactRegistration,
            _ => PublicationKind::Lifecycle,
        }
    }
}

#[cfg(test)]
mod tests;
