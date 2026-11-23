// SPDX-License-Identifier: MPL-2.0

//! **The run's closed vocabulary, said in a person's own language.**
//!
//! A run's history holds no words at all — an entry is an id, a step kind,
//! references and a time (D45) — so every sentence a person reads about a
//! run is made here, from `koru-i18n`, in eight languages. That is what
//! makes UC3 possible: understanding a run you never watched, from Activity
//! alone, without opening the terminal.
//!
//! Two rules this module exists to keep:
//!
//! - **Unavailable is unavailable, never zero** (§17.2). A missing token
//!   count and a genuine zero are different facts, and [`tokens`] renders
//!   them differently: *not reported*, *at least 1,200 (2 steps not
//!   reported)*, or the number.
//! - **A step this build cannot decode is a step, not a silence.** An
//!   unrecognized entry gets its own sentence and keeps its place.

use koru_delegate_api::wire::TimelineItem;
use koru_delegate_core::{
    ApprovalOutcome, Attention, BlobKind, CancelReason, ContextState, ControlState, FailureStage,
    PriorAttempt, Rating, ResumeSeam, RunContextItem, RunStatus, StepKind, TokenTotal, ToolFailure,
    ToolFailures, ToolKind, TurnOriginator, UsageFact, WorkspaceScope,
};
use koru_i18n::number::decimal;
use koru_i18n::{Key, Lang, fill, t};

/// Where the run is, in one phrase.
pub(crate) fn status(lang: Lang, status: &RunStatus) -> String {
    let key = match status {
        RunStatus::Created => Key::RunStatusCreated,
        RunStatus::Starting => Key::RunStatusStarting,
        RunStatus::Running { .. } => Key::RunStatusRunning,
        RunStatus::Interrupted { .. } => Key::RunStatusInterrupted,
        RunStatus::Completed => Key::RunStatusCompleted,
        RunStatus::Failed { .. } => Key::RunStatusFailed,
        RunStatus::Cancelled => Key::RunStatusCancelled,
        RunStatus::AwaitingReview => Key::RunStatusAwaitingReview,
        RunStatus::WaitingForParticipant => Key::RunStatusWaitingForParticipant,
    };
    match status {
        // A failure names the stage it failed at; a stage is a closed
        // token, so it costs nothing to keep and answers "why" at a glance.
        RunStatus::Failed { stage } => format!("{} — {}", t(lang, key), t(lang, stage_key(*stage))),
        _ => t(lang, key).to_owned(),
    }
}

/// Every marker on a running run, one sentence each (§7.5).
pub(crate) fn attention(lang: Lang, status: &RunStatus) -> Vec<String> {
    let Some(set) = status.attention() else {
        return Vec::new();
    };
    set.iter()
        .map(|marker| {
            t(
                lang,
                match marker {
                    Attention::ApprovalRequired => Key::RunNeedsApproval,
                    Attention::WaitingForInput => Key::RunNeedsInput,
                    Attention::ChildSessionDetected => Key::RunNeedsChildStopped,
                    Attention::FinishedNeedingAttention => Key::RunNeedsAttentionNoResult,
                    // M6: the Bot asked another Bot and its Run is held
                    // until the result comes back (contract 4.1).
                    Attention::WaitingForBot => Key::RunNeedsOtherBot,
                },
            )
            .to_owned()
        })
        .collect()
}

/// Who holds the session (§7.4).
pub(crate) fn control(lang: Lang, control: ControlState) -> &'static str {
    t(
        lang,
        match control {
            ControlState::Orchestrator => Key::RunControlAgent,
            ControlState::HandoverPending { .. } => Key::RunControlWaiting,
            ControlState::Human { .. } => Key::RunControlYours,
        },
    )
}

pub(crate) fn scope(lang: Lang, scope: &WorkspaceScope) -> &'static str {
    t(
        lang,
        match scope {
            WorkspaceScope::None => Key::RunScopeNone,
            WorkspaceScope::ProjectRoot { .. } => Key::RunScopeProjectRoot,
            WorkspaceScope::ProjectFolder { .. } => Key::RunScopeProjectFolder,
            WorkspaceScope::Temporary => Key::RunScopeTemporary,
            WorkspaceScope::BotFolder { .. } => Key::RunScopeBotFolder,
        },
    )
}

/// One context item and what it points at *now* (D46). The pin never moves;
/// the state beside it does, and an unknown state says nothing rather than
/// saying *gone*.
pub(crate) fn context(lang: Lang, item: &RunContextItem, state: Option<ContextState>) -> String {
    let what = t(
        lang,
        match item {
            RunContextItem::Document { .. } => Key::RunContextDocument,
            RunContextItem::Block { .. } => Key::RunContextBlock,
            RunContextItem::Blob { blob_kind, .. } => match blob_kind {
                BlobKind::File => Key::RunContextFile,
                BlobKind::Image => Key::RunContextImage,
                BlobKind::Pdf => Key::RunContextPdf,
            },
        },
    );
    match state {
        None => what.to_owned(),
        Some(state) => format!(
            "{what} — {}",
            t(
                lang,
                match state {
                    ContextState::Live => Key::RunContextStateLive,
                    ContextState::InTrash => Key::RunContextStateInTrash,
                    ContextState::Unavailable => Key::RunContextStateUnavailable,
                }
            )
        ),
    }
}

fn stage_key(stage: FailureStage) -> Key {
    match stage {
        FailureStage::ScopeAdmission => Key::RunStageScopeAdmission,
        FailureStage::ContextCapture => Key::RunStageContextCapture,
        FailureStage::ContextMaterialization => Key::RunStageContextMaterialization,
        FailureStage::SessionCreation => Key::RunStageSessionCreation,
        FailureStage::ProviderStart => Key::RunStageProviderStart,
        FailureStage::Authority => Key::RunStageAuthority,
        FailureStage::Turn => Key::RunStageTurn,
    }
}

/// **One step of a run's history, as a sentence.**
///
/// Every arm of `StepKind` is here, and the match is exhaustive on purpose:
/// a step kind added later fails to build until somebody has decided what a
/// person reads when it happens.
pub(crate) fn step(lang: Lang, kind: StepKind) -> String {
    use TurnOriginator::{Human, Orchestrator};
    match kind {
        StepKind::RunCreated => t(lang, Key::RunStepCreated).to_owned(),
        StepKind::Started => t(lang, Key::RunStepStarted).to_owned(),
        StepKind::TurnStarted {
            originator: Orchestrator,
        } => t(lang, Key::RunStepTurnStartedAgent).to_owned(),
        StepKind::TurnStarted { originator: Human } => {
            t(lang, Key::RunStepTurnStartedHuman).to_owned()
        }
        StepKind::ToolsCompleted {
            calls, failed: 0, ..
        } => fill(
            t(lang, Key::RunStepTools),
            &[&decimal(lang, u64::from(calls))],
        ),
        StepKind::ToolsCompleted { calls, failed, .. } => fill(
            t(lang, Key::RunStepToolsWithFailures),
            &[
                &decimal(lang, u64::from(calls)),
                &decimal(lang, u64::from(failed)),
            ],
        ),
        StepKind::ApprovalRequired => t(lang, Key::RunStepApprovalRequired).to_owned(),
        StepKind::ApprovalResolved { outcome } => t(
            lang,
            match outcome {
                ApprovalOutcome::Approved => Key::RunStepApprovalApproved,
                ApprovalOutcome::Rejected => Key::RunStepApprovalRejected,
                ApprovalOutcome::Expired => Key::RunStepApprovalExpired,
            },
        )
        .to_owned(),
        StepKind::WaitingForInput => t(lang, Key::RunStepWaitingForInput).to_owned(),
        // M6: the step a `ChildWaiting` projection writes when the Bot asked
        // another Bot and waits for its result.
        StepKind::WaitingForBot => t(lang, Key::RunStepWaitingForBot).to_owned(),
        StepKind::AttachmentAccepted => t(lang, Key::RunStepAttachmentAccepted).to_owned(),
        StepKind::AttachmentRejected => t(lang, Key::RunStepAttachmentRejected).to_owned(),
        StepKind::ChildSessionDetected => t(lang, Key::RunStepChildSession).to_owned(),
        StepKind::TurnCompleted {
            originator: Orchestrator,
            artifacts_registered: 0,
        } => t(lang, Key::RunStepTurnCompletedAgent).to_owned(),
        StepKind::TurnCompleted {
            originator: Orchestrator,
            artifacts_registered,
        } => fill(
            t(lang, Key::RunStepTurnCompletedAgentWithResults),
            &[&decimal(lang, u64::from(artifacts_registered))],
        ),
        StepKind::TurnCompleted {
            originator: Human, ..
        } => t(lang, Key::RunStepTurnCompletedHuman).to_owned(),
        StepKind::TurnCancelled {
            reason: CancelReason::TakeoverBoundary,
            ..
        } => t(lang, Key::RunStepTurnCancelledTakeover).to_owned(),
        StepKind::TurnCancelled {
            reason: CancelReason::Run,
            ..
        } => t(lang, Key::RunStepTurnCancelledRun).to_owned(),
        StepKind::TurnFailed {
            originator: Orchestrator,
            stage,
        } => fill(
            t(lang, Key::RunStepTurnFailedAgent),
            &[t(lang, stage_key(stage))],
        ),
        StepKind::TurnFailed {
            originator: Human, ..
        } => t(lang, Key::RunStepTurnFailedHuman).to_owned(),
        StepKind::Interrupted {
            prior_attempt: PriorAttempt::Ended,
        } => t(lang, Key::RunStepInterrupted).to_owned(),
        StepKind::Interrupted {
            prior_attempt: PriorAttempt::Unknown,
        } => t(lang, Key::RunStepInterruptedUnknown).to_owned(),
        StepKind::Resumed {
            via: ResumeSeam::Native,
        } => t(lang, Key::RunStepResumedNative).to_owned(),
        StepKind::Resumed {
            via: ResumeSeam::RecordedContext,
        } => t(lang, Key::RunStepResumedRecorded).to_owned(),
        StepKind::ControlTaken { discovered: false } => {
            t(lang, Key::RunStepControlTaken).to_owned()
        }
        StepKind::ControlTaken { discovered: true } => {
            t(lang, Key::RunStepControlDiscovered).to_owned()
        }
        StepKind::ControlReturned => t(lang, Key::RunStepControlReturned).to_owned(),
        StepKind::ArtifactRegistered => t(lang, Key::RunStepArtifactRegistered).to_owned(),
        StepKind::Completed => t(lang, Key::RunStepCompleted).to_owned(),
        StepKind::Failed { stage, .. } => {
            fill(t(lang, Key::RunStepFailed), &[t(lang, stage_key(stage))])
        }
        StepKind::Cancelled { .. } => t(lang, Key::RunStepCancelled).to_owned(),
        StepKind::Rated {
            rating: Rating::Good,
        } => t(lang, Key::RunStepRatedGood).to_owned(),
        StepKind::Rated {
            rating: Rating::NotGood,
        } => t(lang, Key::RunStepRatedNotGood).to_owned(),
        StepKind::Gap { events_lost: None } => t(lang, Key::RunStepGap).to_owned(),
        StepKind::Gap {
            events_lost: Some(lost),
        } => fill(
            t(lang, Key::RunStepGapCount),
            &[&decimal(lang, u64::from(lost))],
        ),
        StepKind::ScopeBusy { holders } => fill(
            t(lang, Key::RunStepScopeBusy),
            &[&decimal(lang, u64::from(holders))],
        ),
    }
}

/// **What failed, under the count that says how many did.**
///
/// *Six tool calls, two of them failed* is a number a person can worry
/// about and cannot act on. These are the lines under it — one per
/// `(tool, reason)` pair the turn recorded, in the order they first failed
/// — and both halves of each come from a closed vocabulary the adapter
/// chose, never from anything a provider wrote.
///
/// **Empty is the honest answer, not a shrug.** A step written before this
/// build recorded nothing, so it gets no lines at all and the count above
/// stands exactly as it always has. A failure the adapter could not place
/// gets a line saying so, because *it failed and the agent did not say how*
/// is a fact worth reading and a guess never is.
pub(crate) fn tool_failures(lang: Lang, failures: &ToolFailures) -> Vec<String> {
    let mut lines: Vec<String> = failures
        .iter()
        .map(|tally| {
            let tool = t(lang, tool_key(tally.tool));
            let why = t(lang, failure_key(tally.failure));
            if tally.calls <= 1 {
                fill(t(lang, Key::RunStepToolFailure), &[tool, why])
            } else {
                fill(
                    t(lang, Key::RunStepToolFailureTimes),
                    &[tool, why, &decimal(lang, u64::from(tally.calls))],
                )
            }
        })
        .collect();
    // The tail past the entry's bound is counted, never dropped: the lines
    // and this one together account for every failure the step claims.
    if failures.elided() > 0 {
        lines.push(fill(
            t(lang, Key::RunStepToolFailuresMore),
            &[&decimal(lang, u64::from(failures.elided()))],
        ));
    }
    lines
}

fn tool_key(tool: ToolKind) -> Key {
    match tool {
        ToolKind::CommandExecution => Key::RunToolCommandExecution,
        ToolKind::FileChange => Key::RunToolFileChange,
        ToolKind::McpTool => Key::RunToolMcp,
        ToolKind::DynamicTool => Key::RunToolDynamic,
        ToolKind::WebSearch => Key::RunToolWebSearch,
        ToolKind::ImageGeneration => Key::RunToolImageGeneration,
        ToolKind::Other => Key::RunToolOther,
    }
}

fn failure_key(failure: ToolFailure) -> Key {
    match failure {
        ToolFailure::Denied => Key::RunToolFailedDenied,
        ToolFailure::Cancelled => Key::RunToolFailedCancelled,
        ToolFailure::TimedOut => Key::RunToolFailedTimedOut,
        ToolFailure::Errored => Key::RunToolFailedErrored,
        ToolFailure::Unclassified => Key::RunToolFailedUnclassified,
    }
}

/// One usage fact, as the row it earns beside the step it belongs to. A
/// count nobody reported is *not reported*, never a zero.
pub(crate) fn usage(lang: Lang, fact: &UsageFact) -> String {
    fill(
        t(lang, Key::RunStepUsage),
        &[&count(lang, fact.input), &count(lang, fact.output)],
    )
}

fn count(lang: Lang, value: Option<u64>) -> String {
    match value {
        Some(value) => decimal(lang, value),
        None => t(lang, Key::RunUnavailable).to_owned(),
    }
}

/// One row of the history, whatever it is.
pub(crate) fn item(lang: Lang, item: &TimelineItem) -> String {
    match item {
        TimelineItem::Step(entry) => step(lang, entry.kind),
        TimelineItem::Usage(fact) => usage(lang, fact),
        TimelineItem::Unrecognized => t(lang, Key::RunStepUnknown).to_owned(),
    }
}

/// **§17.2's rule, rendered.** A total every step reported is the number;
/// a total some steps did not is a floor that says how many are missing;
/// a total nobody reported is *not reported*. None of the three is a zero
/// unless the run genuinely used zero.
pub(crate) fn tokens(lang: Lang, total: TokenTotal) -> String {
    match total.complete() {
        Some(value) => decimal(lang, value),
        None if total.known == 0 => t(lang, Key::RunUnavailable).to_owned(),
        None => fill(
            t(lang, Key::RunPartialTotal),
            &[
                &decimal(lang, total.known),
                &decimal(lang, u64::from(total.unavailable_turns)),
            ],
        ),
    }
}

/// How long the run actually worked, excluding the time it was interrupted
/// or in a person's hands (§17.2). `None` is a run that has not started.
pub(crate) fn duration(lang: Lang, elapsed_ms: Option<u64>) -> String {
    let Some(ms) = elapsed_ms else {
        return t(lang, Key::RunUnavailable).to_owned();
    };
    koru_i18n::time::elapsed(lang, ms)
}

/// Actionable copy from a closed, content-free retained cause.
pub(crate) fn failure_reason(lang: Lang, reason: koru_delegate_core::RunFailureReason) -> String {
    use koru_delegate_core::RunFailureReason as R;
    let key = match reason {
        R::CapabilityNotNegotiated => Key::RunFailureCapability,
        R::ProviderModelUnavailable => Key::DelegateModelUnavailable,
        R::ProviderIncompatible => Key::TerminalManagedProviderIncompatible,
        R::ProviderUnavailable => Key::TerminalManagedProviderUnavailable,
        R::ProviderFailed => Key::TerminalManagedProviderFailed,
        R::AuthenticationRequired => Key::RunFailureAuthentication,
        R::TimedOut => Key::RunFailureTimedOut,
        R::Disconnected => Key::RunFailureDisconnected,
        R::Backpressure => Key::RunFailureBusy,
        R::InvalidRequest => Key::RunFailureInvalidRequest,
        R::Unknown => Key::RunFailureUnknown,
    };
    t(lang, key).to_owned()
}
