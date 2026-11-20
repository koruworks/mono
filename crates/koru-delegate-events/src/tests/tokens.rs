// SPDX-License-Identifier: MPL-2.0

//! Every variant of every closed enum maps to a token inside its row's
//! vocabulary — and, the other way, every vocabulary entry is reachable
//! from some variant, so the registry cannot promise a token no builder
//! can produce.

use koru_events::registry;
use koru_session_protocol::{ErrorCode, ProviderKind};

use super::*;

/// The vocabulary of one attribute on one row.
fn vocabulary(name: &str, key: &str) -> Vec<&'static str> {
    registry::spec(name)
        .unwrap_or_else(|| panic!("{name} is not registered"))
        .attr(key)
        .unwrap_or_else(|| panic!("{name} declares no `{key}`"))
        .vocabulary
        .to_vec()
}

/// Both directions: every variant lands in the vocabulary, and the
/// vocabulary is exactly the variants' tokens.
fn covers<T: RowToken>(variants: &[T], name: &str, key: &str) {
    let tokens: BTreeSet<&str> = variants.iter().map(|variant| variant.token()).collect();
    let declared: BTreeSet<&str> = vocabulary(name, key).into_iter().collect();
    assert_eq!(
        tokens, declared,
        "{name}.{key}: the enum's tokens and the registry's vocabulary differ"
    );
}

#[test]
fn turn_originator() {
    covers(
        &[TurnOriginator::Orchestrator, TurnOriginator::Human],
        "delegate.turn.started",
        "originator",
    );
}

#[test]
fn cancel_reason() {
    covers(
        &[CancelReason::Run, CancelReason::TakeoverBoundary],
        "delegate.run.cancelled",
        "reason",
    );
    covers(
        &[CancelReason::Run, CancelReason::TakeoverBoundary],
        "delegate.turn.cancelled",
        "reason",
    );
}

#[test]
fn failure_stage() {
    let all = [
        FailureStage::ScopeAdmission,
        FailureStage::ContextCapture,
        FailureStage::ContextMaterialization,
        FailureStage::SessionCreation,
        FailureStage::ProviderStart,
        FailureStage::Authority,
        FailureStage::Turn,
    ];
    covers(&all, "delegate.run.failed", "stage");
    covers(&all, "delegate.turn.failed", "stage");
}

#[test]
fn prior_attempt_and_resume_seam() {
    covers(
        &[PriorAttempt::Ended, PriorAttempt::Unknown],
        "delegate.run.interrupted",
        "prior_attempt",
    );
    covers(
        &[ResumeSeam::Native, ResumeSeam::RecordedContext],
        "delegate.run.resumed",
        "via",
    );
}

#[test]
fn approval_outcome() {
    covers(
        &[
            ApprovalOutcome::Approved,
            ApprovalOutcome::Rejected,
            ApprovalOutcome::Expired,
        ],
        "delegate.approval.resolved",
        "outcome",
    );
}

#[test]
fn rating() {
    covers(
        &[Rating::Good, Rating::NotGood],
        "delegate.run.rated",
        "rating",
    );
}

#[test]
fn cached_relationship_and_completeness() {
    covers(
        &[
            CachedInputRelationship::Included,
            CachedInputRelationship::Separate,
            CachedInputRelationship::Undeclared,
        ],
        "delegate.turn.usage",
        "cached_relationship",
    );
    covers(&every_completeness(), "delegate.turn.usage", "completeness");
}

#[test]
fn the_crates_own_enums() {
    covers(DelegatedAgent::ALL, "delegate.run.created", "agent");
    covers(ScopeKind::ALL, "delegate.run.created", "scope_kind");
    covers(CompletionOutcome::ALL, "delegate.run.completed", "outcome");
    covers(ToolOutcome::ALL, "delegate.tool.completed", "outcome");
    covers(
        AttachmentRejection::ALL,
        "delegate.attachment.rejected",
        "reason",
    );
    covers(RunOutcome::ALL, "delegate.run.usage", "outcome");
    covers(ScopeOverlap::ALL, "delegate.scope.busy", "overlap");
    covers(ScopeOverlap::ALL, "delegate.scope.skipped", "overlap");
}

#[test]
fn schedule_pause_and_skip_reasons() {
    covers(
        &[
            SchedulePauseReason::Manual,
            SchedulePauseReason::ProviderNotDelegatable,
            SchedulePauseReason::ScopeUnavailable,
            SchedulePauseReason::ContextUnavailable {
                item: 0,
                stage: ScheduleContextFailureStage::Resolve,
            },
        ],
        "delegate.schedule.paused",
        "reason",
    );
    covers(
        &[
            ScheduleContextFailureStage::Resolve,
            ScheduleContextFailureStage::Commit,
            ScheduleContextFailureStage::Retention,
            ScheduleContextFailureStage::Materialize,
        ],
        "delegate.schedule.paused",
        "stage",
    );
    covers(
        &[
            ScheduleSkipReason::missed(),
            ScheduleSkipReason::busy(run_ref().run),
        ],
        "delegate.schedule.skipped",
        "reason",
    );
}

#[test]
fn schedule_skip_boundary_round_trips_the_canonical_outcome() {
    let holder = run_ref().run;
    let missed = ScheduleSkipReason::from_outcome(ScheduleFiringOutcome::SkippedMissed, None)
        .expect("missed without a holder");
    let busy = ScheduleSkipReason::from_outcome(ScheduleFiringOutcome::SkippedBusy, Some(holder))
        .expect("busy with a holder");
    assert_eq!(missed.outcome(), ScheduleFiringOutcome::SkippedMissed);
    assert_eq!(missed.holder(), None);
    assert_eq!(busy.outcome(), ScheduleFiringOutcome::SkippedBusy);
    assert_eq!(busy.holder(), Some(holder));

    assert_eq!(
        ScheduleSkipReason::from_outcome(ScheduleFiringOutcome::Fired, None),
        None
    );
    assert_eq!(
        ScheduleSkipReason::from_outcome(ScheduleFiringOutcome::SkippedMissed, Some(holder)),
        None
    );
    assert_eq!(
        ScheduleSkipReason::from_outcome(ScheduleFiringOutcome::SkippedBusy, None),
        None
    );
}

/// The provider category collapses locally: the four evaluated providers
/// keep their token, anything else is `other`.
#[test]
fn an_unknown_provider_collapses_to_other_on_this_machine() {
    let kind =
        |token: &str| DelegatedAgent::from_provider_kind(&token.parse::<ProviderKind>().unwrap());
    assert_eq!(kind("codex"), DelegatedAgent::Codex);
    assert_eq!(kind("claude"), DelegatedAgent::Claude);
    assert_eq!(kind("gemini"), DelegatedAgent::Gemini);
    assert_eq!(kind("opencode"), DelegatedAgent::Opencode);
    assert_eq!(kind("grok"), DelegatedAgent::Other);
    assert_eq!(kind("some-new-thing"), DelegatedAgent::Other);
}

#[test]
fn a_scope_kind_drops_the_folder() {
    use koru_delegate_core::{RelativeFolder, WorkspaceScope};
    let project = v4(0x20).hyphenated().to_string().parse().unwrap();
    assert_eq!(ScopeKind::of(&WorkspaceScope::None), ScopeKind::None);
    assert_eq!(
        ScopeKind::of(&WorkspaceScope::ProjectRoot { project }),
        ScopeKind::ProjectRoot
    );
    assert_eq!(
        ScopeKind::of(&WorkspaceScope::ProjectFolder {
            project,
            folder: RelativeFolder::new("FOLDER-SENTINEL/src").unwrap(),
        }),
        ScopeKind::ProjectFolder
    );
    assert_eq!(
        ScopeKind::of(&WorkspaceScope::Temporary),
        ScopeKind::Temporary
    );
}

#[test]
fn a_tool_outcome_is_derived_from_the_counts() {
    assert_eq!(ToolOutcome::of(0, 0), ToolOutcome::Succeeded);
    assert_eq!(ToolOutcome::of(5, 0), ToolOutcome::Succeeded);
    assert_eq!(ToolOutcome::of(5, 2), ToolOutcome::Partial);
    assert_eq!(ToolOutcome::of(5, 5), ToolOutcome::Failed);
}

#[test]
fn an_attachment_rejection_keeps_two_classes_and_folds_the_rest() {
    assert_eq!(
        AttachmentRejection::of(ErrorCode::AttachmentNotFound),
        AttachmentRejection::NotFound
    );
    assert_eq!(
        AttachmentRejection::of(ErrorCode::AttachmentLimit),
        AttachmentRejection::Limit
    );
    assert_eq!(
        AttachmentRejection::of(ErrorCode::Internal),
        AttachmentRejection::Other
    );
}

/// The fold is published under a terminal status only; a run in flight has
/// no outcome yet, and the match is exhaustive so a new status is decided
/// rather than defaulted.
#[test]
fn a_run_outcome_exists_only_for_a_terminal_status() {
    use koru_delegate_core::RunStatus;
    assert_eq!(
        RunOutcome::of(&RunStatus::Completed),
        Some(RunOutcome::Completed)
    );
    assert_eq!(
        RunOutcome::of(&RunStatus::Failed {
            stage: FailureStage::Turn
        }),
        Some(RunOutcome::Failed)
    );
    assert_eq!(
        RunOutcome::of(&RunStatus::Cancelled),
        Some(RunOutcome::Cancelled)
    );
    assert_eq!(RunOutcome::of(&RunStatus::Created), None);
    assert_eq!(RunOutcome::of(&RunStatus::Starting), None);
}
