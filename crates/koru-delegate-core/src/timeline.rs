// SPDX-License-Identifier: MPL-2.0

//! A run's history, one lean step at a time (D45).
//!
//! An entry carries an id, a step kind, references to what it touched, a
//! time and bounded counts — **never a copy of anything**. This is the event
//! bus's own rule applied to the run's history, and it is what makes D32
//! (keep everything, everywhere, forever) affordable and safe at once: a
//! step records *that* a document was read, not what it said, so retaining
//! it costs tens of bytes and leaks nothing.
//!
//! The rule is enforced by the compiler. Every type an entry may contain
//! implements [`ContentFree`], a sealed trait `String` never implements, and
//! `entry_is_content_free` below destructures every field and every variant
//! payload so that adding a text field anywhere in an entry fails to build.

use serde::{Deserialize, Serialize};

use crate::{
    ActorRef, AgentTurnId, ApprovalId, ArtifactVersionId, AttachmentId, BlobHash, CancelReason,
    DeviceId, DocumentId, DocumentVersionId, FailureStage, ModelRef, PriorAttempt, Rating,
    RunAttemptId, ScheduleId, SessionId, TimelineEntryId, ToolFailure, ToolFailureTally,
    ToolFailures, ToolKind, TurnOriginator,
};

/// The most references one step may carry. A step touches a handful of
/// things; a step that touches hundreds is a copy wearing a costume.
pub const MAX_TIMELINE_REFS: usize = 8;

/// Marker for the types a timeline entry may carry: ids, counts, times and
/// closed vocabularies. Sealed, so the set is this crate's to extend, and
/// **never implemented for `String`** or anything that holds one.
///
/// ```compile_fail
/// fn check<T: koru_delegate_core::ContentFree>() {}
/// check::<String>();
/// ```
pub trait ContentFree: sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

macro_rules! content_free {
    ($($ty:ty),* $(,)?) => {
        $(
            impl sealed::Sealed for $ty {}
            impl ContentFree for $ty {}
        )*
    };
}

content_free!(
    bool,
    u8,
    u16,
    u32,
    u64,
    i64,
    TimelineEntryId,
    RunAttemptId,
    AgentTurnId,
    ApprovalId,
    AttachmentId,
    SessionId,
    ArtifactVersionId,
    DeviceId,
    ScheduleId,
    DocumentId,
    DocumentVersionId,
    ModelRef,
    ActorRef,
    TurnOriginator,
    CancelReason,
    FailureStage,
    crate::RunFailureReason,
    PriorAttempt,
    Rating,
    ApprovalOutcome,
    ResumeSeam,
    // Two closed vocabularies and a bounded tally of them: a classification
    // an adapter made, never a word a provider wrote.
    ToolKind,
    ToolFailure,
    ToolFailureTally,
    ToolFailures,
    StepKind,
    TimelineRef,
    TimelineRefs,
    TimelineEntry,
);

// A blob's identity is its content *hash* — `b3:<64 hex>` — which names the
// bytes without containing any of them. The type wraps a `String` for the
// algorithm prefix's sake; the value is never text a person wrote.
content_free!(BlobHash);

impl<T: ContentFree> sealed::Sealed for Option<T> {}
impl<T: ContentFree> ContentFree for Option<T> {}

/// How an approval ended.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
    Expired,
}

/// Which seam a resume went through (P1.9: the timeline says which).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResumeSeam {
    /// The adapter's own resume token continued the provider session.
    Native,
    /// The objective was re-submitted with the original pins and the
    /// artifacts so far — a fresh attempt, not continuity.
    RecordedContext,
}

/// What a step is. A closed enum mirroring §7.5's rows plus control
/// handover (§7.4), artifact registration, recovery and the honest gap
/// (§7.1: replay never invents what it did not receive). Counts ride the
/// variant that needs them, bounded by type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepKind {
    /// The Run document was committed (§7.1 step 3).
    RunCreated,
    /// An attempt's provider is up.
    Started,
    TurnStarted {
        originator: TurnOriginator,
    },
    /// `ToolCompleted` events, coalesced (§7.5: bounded, never one row per
    /// call).
    ///
    /// **`failures` is what makes the count actionable** — which tools
    /// failed and how, in tokens, bounded by type. An entry written before
    /// this build carries none, and reads as the counts alone: the same
    /// sentence it has always drawn, with nothing invented under it.
    ToolsCompleted {
        calls: u32,
        failed: u32,
        #[serde(default, skip_serializing_if = "ToolFailures::is_empty")]
        failures: ToolFailures,
    },
    ApprovalRequired,
    ApprovalResolved {
        outcome: ApprovalOutcome,
    },
    WaitingForInput,
    AttachmentAccepted,
    /// Rejection is news (§7.5).
    AttachmentRejected,
    /// A child session appeared and was terminated (§7.3).
    ChildSessionDetected,
    TurnCompleted {
        originator: TurnOriginator,
        artifacts_registered: u32,
    },
    TurnCancelled {
        originator: TurnOriginator,
        reason: CancelReason,
    },
    TurnFailed {
        originator: TurnOriginator,
        stage: FailureStage,
    },
    /// A restart ended the attempt (§7.6).
    Interrupted {
        prior_attempt: PriorAttempt,
    },
    Resumed {
        via: ResumeSeam,
    },
    /// The human lease was granted. `discovered` marks a takeover recovery
    /// found already in place, timestamped at discovery (§7.4).
    ControlTaken {
        discovered: bool,
    },
    ControlReturned,
    /// A Bot Run asked other Bots and waits for them (M6). No fields.
    WaitingForBot,
    ArtifactRegistered,
    Completed,
    Failed {
        stage: FailureStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<crate::RunFailureReason>,
    },
    Cancelled {
        reason: CancelReason,
    },
    Rated {
        rating: Rating,
    },
    /// Facts Koru never received — a provider tail evicted during delegate
    /// downtime. `events_lost` when the eviction reported a count.
    Gap {
        events_lost: Option<u32>,
    },
    /// A manual start proceeded onto a folder another run holds (D34,
    /// §7.8): the warning the person saw and overrode, as a step. `holders`
    /// counts the runs that held it; their attempts ride the refs.
    ScopeBusy {
        holders: u32,
    },
}

impl StepKind {
    /// Whether this step ends the run.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Something a step touched, by id.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "ref", rename_all = "snake_case")]
pub enum TimelineRef {
    Attempt {
        attempt: RunAttemptId,
    },
    Turn {
        turn: AgentTurnId,
    },
    Actor {
        actor: ActorRef,
    },
    /// A document as the run received it — the pin, so a later deletion is
    /// rendered as the target's state (D46), never as a missing step.
    Document {
        document: DocumentId,
        version: DocumentVersionId,
    },
    Blob {
        blob: BlobHash,
    },
    Artifact {
        artifact: ArtifactVersionId,
    },
    Device {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        device: DeviceId,
    },
    Approval {
        approval: ApprovalId,
    },
    Attachment {
        attachment: AttachmentId,
    },
    Session {
        session: SessionId,
    },
    Model {
        model: ModelRef,
    },
    /// The schedule authority that created this run. The claimed revision and
    /// occurrence ride with it so a later template edit cannot rewrite history.
    Schedule {
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
    },
}

/// More than [`MAX_TIMELINE_REFS`] references were offered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TooManyRefs {
    pub offered: usize,
}

impl std::fmt::Display for TooManyRefs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a timeline entry carries at most {MAX_TIMELINE_REFS} references, {} offered",
            self.offered
        )
    }
}

impl std::error::Error for TooManyRefs {}

/// The references of one step, bounded by [`MAX_TIMELINE_REFS`].
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct TimelineRefs(Vec<TimelineRef>);

impl TimelineRefs {
    pub fn new(refs: impl IntoIterator<Item = TimelineRef>) -> Result<Self, TooManyRefs> {
        let refs: Vec<_> = refs.into_iter().collect();
        if refs.len() > MAX_TIMELINE_REFS {
            return Err(TooManyRefs {
                offered: refs.len(),
            });
        }
        Ok(Self(refs))
    }

    pub fn iter(&self) -> impl Iterator<Item = &TimelineRef> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One step of a run's history (D45).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TimelineEntry {
    pub id: TimelineEntryId,
    pub kind: StepKind,
    #[serde(default)]
    pub refs: TimelineRefs,
    /// Epoch milliseconds UTC, for display and for the §17.2 definitions.
    /// Order is the timeline's own — append order — never this clock.
    pub at_ms: i64,
}

impl TimelineEntry {
    pub fn new(kind: StepKind, refs: TimelineRefs, at_ms: i64) -> Self {
        Self {
            id: TimelineEntryId::mint(),
            kind,
            refs,
            at_ms,
        }
    }
}

/// The compile-time proof. Every field of an entry, every payload of every
/// step kind and every reference is checked against [`ContentFree`] by
/// destructuring — a new field of a type that could carry text, or a new
/// variant not listed here, is a build failure rather than a review comment.
#[allow(dead_code)]
fn entry_is_content_free(entry: &TimelineEntry) {
    fn check<T: ContentFree>(_: &T) {}

    let TimelineEntry {
        id,
        kind,
        refs,
        at_ms,
    } = entry;
    check(id);
    check(refs);
    check(at_ms);
    for reference in refs.iter() {
        match reference {
            TimelineRef::Attempt { attempt } => check(attempt),
            TimelineRef::Turn { turn } => check(turn),
            TimelineRef::Actor { actor } => check(actor),
            TimelineRef::Document { document, version } => {
                check(document);
                check(version);
            }
            TimelineRef::Blob { blob } => check(blob),
            TimelineRef::Artifact { artifact } => check(artifact),
            TimelineRef::Device { device } => check(device),
            TimelineRef::Approval { approval } => check(approval),
            TimelineRef::Attachment { attachment } => check(attachment),
            TimelineRef::Session { session } => check(session),
            TimelineRef::Model { model } => check(model),
            TimelineRef::Schedule {
                schedule,
                occurrence_ms,
                revision,
            } => {
                check(schedule);
                check(occurrence_ms);
                check(revision);
            }
        }
    }
    match kind {
        StepKind::RunCreated
        | StepKind::Started
        | StepKind::ApprovalRequired
        | StepKind::WaitingForInput
        | StepKind::AttachmentAccepted
        | StepKind::AttachmentRejected
        | StepKind::ChildSessionDetected
        | StepKind::ControlReturned
        | StepKind::WaitingForBot
        | StepKind::ArtifactRegistered
        | StepKind::Completed => {}
        StepKind::TurnStarted { originator } => check(originator),
        StepKind::ToolsCompleted {
            calls,
            failed,
            failures,
        } => {
            check(calls);
            check(failed);
            check(failures);
            for tally in failures.iter() {
                let ToolFailureTally {
                    tool,
                    failure,
                    calls,
                } = tally;
                check(&tool);
                check(&failure);
                check(&calls);
            }
        }
        StepKind::ApprovalResolved { outcome } => check(outcome),
        StepKind::TurnCompleted {
            originator,
            artifacts_registered,
        } => {
            check(originator);
            check(artifacts_registered);
        }
        StepKind::TurnCancelled { originator, reason } => {
            check(originator);
            check(reason);
        }
        StepKind::TurnFailed { originator, stage } => {
            check(originator);
            check(stage);
        }
        StepKind::Interrupted { prior_attempt } => check(prior_attempt),
        StepKind::Resumed { via } => check(via),
        StepKind::ControlTaken { discovered } => check(discovered),
        StepKind::Failed { stage, reason } => {
            check(stage);
            check(reason);
        }
        StepKind::Cancelled { reason } => check(reason),
        StepKind::Rated { rating } => check(rating),
        StepKind::Gap { events_lost } => check(events_lost),
        StepKind::ScopeBusy { holders } => check(holders),
    }
}
