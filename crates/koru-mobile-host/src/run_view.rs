// SPDX-License-Identifier: MPL-2.0

//! A delegated run as a phone reads it (P1.18, D15, D16, D27).
//!
//! The Run document is opened through the same path as a note — `locate`,
//! `documents`, `fork_document` — but read with the run model
//! (`koru-delegate-document`) rather than a node-tree walk. This module is
//! the projection: a [`RunHeader`] becomes a [`RunView`] a phone can draw,
//! and a window of [`TimelineItem`]s becomes rows of ids, tokens, times and
//! counts that the phone formats through its own localization.
//!
//! **Tokens, never prose.** Every status, marker, stage and step crosses as
//! the run vocabulary's own serde spelling (`running`, `approval_required`,
//! `tools_completed`), taken from the vocabulary's `Serialize` rather than
//! re-spelled here, so the phone translates and the two sides cannot drift.
//! The only free text is what the person wrote — the objective, a result's
//! title, a rating note — shown to the person who owns it and never logged.
//!
//! **Devices are named at display time** (PRINCIPLES, *a stable identifier
//! never derives from a name*): the header carries a `DeviceId`; the phone's
//! own trust catalog says what it is called today, and says whether it is
//! this phone (D16 names the executor only elsewhere).

use koru_delegate_core::{
    ResultReference, RunHeader, RunStatus, StepKind, TimelineEntry, UsageFact, WorkspaceScope,
};
use koru_delegate_document::{RunDocument, TimelineItem};
use koru_device::DeviceId;
use koru_store_core::{DocumentId, WorkspaceId};
use serde::Serialize;

/// The pill role a run reference is stored under (P1.0 §3.1). A **stored
/// name**: the value in every carrying document's open `Attr.role`, on
/// every device. P1.15 writes it; `koru-core`'s older-reader harness pins
/// it; this is the reading side.
pub(crate) const RUN_PILL_ROLE: &str = "koru.run";

/// The first window a phone draws, and the size it pages by (proposal §9).
pub(crate) const TIMELINE_WINDOW: usize = 50;

/// The most rows one window may carry: a bound on what one answer costs,
/// not a page size the phone has to know.
pub(crate) const MAX_TIMELINE_WINDOW: usize = 500;

/// How a phone names devices: its own id, and its trust catalog.
pub(crate) struct DeviceNames<'a> {
    pub local: Option<DeviceId>,
    pub name_of: &'a dyn Fn(DeviceId) -> Option<String>,
}

impl DeviceNames<'_> {
    fn here(&self, device: DeviceId) -> bool {
        self.local == Some(device)
    }
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
pub(crate) struct RunResultView {
    pub artifact_id: String,
    pub held_by_device_id: String,
    pub held_by_device_name: Option<String>,
    pub held_here: bool,
    pub title: String,
    pub size_bytes: u64,
    pub registered_at_millis: i64,
}

/// The summary as plain numbers (D39). A total is absent when any turn
/// failed to report it — unavailable is never zero (§17.2).
#[derive(Clone, Serialize, Debug, PartialEq, Eq, Default)]
pub(crate) struct RunEconomicsView {
    pub orchestrator_turns: u32,
    pub human_turns: u32,
    pub turns_with_usage: u32,
    pub input_tokens: Option<u64>,
    pub input_unavailable_turns: u32,
    pub output_tokens: Option<u64>,
    pub output_unavailable_turns: u32,
    pub cached_input_tokens: Option<u64>,
    pub cached_input_unavailable_turns: u32,
    pub tool_calls: u32,
    pub interventions: u32,
    pub elapsed_ms: Option<u64>,
    pub models: u32,
    pub complete: bool,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
pub(crate) struct RunView {
    pub document_id: String,
    pub workspace_id: String,
    pub run_id: String,
    pub revision: u64,
    pub status: String,
    pub attention: Vec<String>,
    pub control: Option<String>,
    pub failure_stage: Option<String>,
    pub prior_attempt: Option<String>,
    pub agent: String,
    pub objective: String,
    pub scope_kind: String,
    pub context_count: u32,
    pub executing_device_id: String,
    pub executing_device_name: Option<String>,
    pub executing_here: bool,
    pub created_at_millis: i64,
    pub results: Vec<RunResultView>,
    pub economics: RunEconomicsView,
    pub rating: Option<String>,
    pub rating_note: Option<String>,
    pub rated_at_millis: Option<i64>,
    pub timeline_len: u32,
    pub needs_newer_koru: bool,
}

/// One row of the activity. `kind` is `step`, `usage` or `gap`; the
/// optional fields are the payload the step or fact carries and nothing
/// else — a row is ids, a token, a time and bounded counts (D45).
#[derive(Clone, Serialize, Debug, PartialEq, Eq, Default)]
pub(crate) struct TimelineRow {
    pub kind: &'static str,
    pub id: Option<String>,
    pub at_millis: Option<i64>,
    pub step: Option<String>,
    pub originator: Option<String>,
    pub calls: Option<u32>,
    pub failed: Option<u32>,
    pub outcome: Option<String>,
    pub artifacts_registered: Option<u32>,
    pub reason: Option<String>,
    pub stage: Option<String>,
    pub prior_attempt: Option<String>,
    pub via: Option<String>,
    pub discovered: Option<bool>,
    pub rating: Option<String>,
    pub events_lost: Option<u32>,
    pub refs: u32,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub revision: Option<u32>,
    pub completeness: Option<String>,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
pub(crate) struct TimelineWindow {
    pub document_id: String,
    pub newest_first_from: u32,
    pub rows: Vec<TimelineRow>,
    pub timeline_len: u32,
    pub revision: u64,
}

/// The vocabulary's own spelling of a closed value. A plain enum serializes
/// to its token; an internally tagged one (`{"step": "…", …}`,
/// `{"kind": "…"}`, `{"holder": "…"}`) to a map whose tag is the token.
fn token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(token)) => token,
        Ok(serde_json::Value::Object(fields)) => fields
            .into_iter()
            .find_map(|(key, value)| match value {
                serde_json::Value::String(token)
                    if matches!(key.as_str(), "step" | "kind" | "holder" | "state" | "ref") =>
                {
                    Some(token)
                }
                _ => None,
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The agent as text: a built-in profile's key (`codex`) or a custom
/// profile's id — the `value` beside the key's `kind` tag.
fn agent_text(header: &RunHeader) -> String {
    match serde_json::to_value(&header.agent) {
        Ok(serde_json::Value::Object(mut fields)) => match fields.remove("value") {
            Some(serde_json::Value::String(value)) => value,
            Some(other) => other.to_string(),
            None => String::new(),
        },
        Ok(serde_json::Value::String(value)) => value,
        _ => String::new(),
    }
}

fn result_view(reference: &ResultReference, names: &DeviceNames<'_>) -> RunResultView {
    RunResultView {
        artifact_id: reference.artifact.to_storage(),
        held_by_device_id: reference.held_by.to_storage(),
        held_by_device_name: (names.name_of)(reference.held_by),
        held_here: names.here(reference.held_by),
        title: reference.title.as_str().to_owned(),
        size_bytes: reference.size_bytes,
        registered_at_millis: reference.registered_at_ms,
    }
}

fn economics_view(header: &RunHeader) -> RunEconomicsView {
    let economics = &header.economics;
    RunEconomicsView {
        orchestrator_turns: economics.orchestrator_turns,
        human_turns: economics.human_turns,
        turns_with_usage: economics.turns_with_usage,
        input_tokens: economics.input.complete(),
        input_unavailable_turns: economics.input.unavailable_turns,
        output_tokens: economics.output.complete(),
        output_unavailable_turns: economics.output.unavailable_turns,
        cached_input_tokens: economics.cached_input.complete(),
        cached_input_unavailable_turns: economics.cached_input.unavailable_turns,
        tool_calls: economics.tool_calls,
        interventions: economics.interventions,
        elapsed_ms: economics.elapsed_ms,
        models: economics.models.len().min(u32::MAX as usize) as u32,
        complete: economics.complete,
    }
}

/// The header as the phone shows it. `revision` is the host's number for
/// the document version the run was forked at, so the phone can tell a new
/// version from a repeated answer without decoding a version vector.
pub(crate) fn run_view(
    document: DocumentId,
    workspace: WorkspaceId,
    revision: u64,
    run: &RunDocument,
    header: &RunHeader,
    names: &DeviceNames<'_>,
) -> RunView {
    // A cloud worker (D12, later) is an environment with no device: it
    // names nothing here rather than pretending to be one.
    let executing_device = header.executing_device();
    let (attention, control) = match &header.status {
        RunStatus::Running { attention, control } => (
            attention.iter().map(|marker| token(&marker)).collect(),
            Some(token(control)),
        ),
        _ => (Vec::new(), None),
    };
    let failure_stage = match &header.status {
        RunStatus::Failed { stage } => Some(token(stage)),
        _ => None,
    };
    let prior_attempt = match &header.status {
        RunStatus::Interrupted { prior_attempt } => Some(token(prior_attempt)),
        _ => None,
    };
    let scope_kind = match &header.scope {
        WorkspaceScope::None => "none",
        WorkspaceScope::ProjectRoot { .. } => "project_root",
        WorkspaceScope::ProjectFolder { .. } => "project_folder",
        WorkspaceScope::Temporary => "temporary",
        WorkspaceScope::BotFolder { .. } => "bot_folder",
    };
    RunView {
        document_id: document.to_storage(),
        workspace_id: workspace.to_storage(),
        run_id: header.run.to_storage(),
        revision,
        status: header.status.name().to_owned(),
        attention,
        control,
        failure_stage,
        prior_attempt,
        agent: agent_text(header),
        objective: header.objective.as_str().to_owned(),
        scope_kind: scope_kind.to_owned(),
        context_count: header.context.len().min(u32::MAX as usize) as u32,
        executing_device_id: executing_device
            .map(DeviceId::to_storage)
            .unwrap_or_default(),
        executing_device_name: executing_device.and_then(|device| (names.name_of)(device)),
        executing_here: executing_device.is_some_and(|device| names.here(device)),
        created_at_millis: header.created_at_ms,
        results: header
            .artifacts
            .iter()
            .map(|reference| result_view(reference, names))
            .collect(),
        economics: economics_view(header),
        rating: header.rating.as_ref().map(|rating| token(&rating.rating)),
        rating_note: header
            .rating
            .as_ref()
            .and_then(|rating| rating.note.as_ref())
            .map(|note| note.as_str().to_owned()),
        rated_at_millis: header.rating.as_ref().map(|rating| rating.rated_at_ms),
        timeline_len: run.timeline_len().min(u32::MAX as usize) as u32,
        needs_newer_koru: !run.compatibility().writable(),
    }
}

fn step_row(entry: &TimelineEntry) -> TimelineRow {
    let mut row = TimelineRow {
        kind: "step",
        id: Some(entry.id.to_storage()),
        at_millis: Some(entry.at_ms),
        step: Some(token(&entry.kind)),
        refs: entry.refs.len().min(u32::MAX as usize) as u32,
        ..TimelineRow::default()
    };
    match entry.kind {
        StepKind::RunCreated
        | StepKind::Started
        | StepKind::ApprovalRequired
        | StepKind::WaitingForInput
        | StepKind::AttachmentAccepted
        | StepKind::AttachmentRejected
        | StepKind::ChildSessionDetected
        | StepKind::ControlReturned
        // M6 sweep: a field-free step; the phone shows its token as any other.
        | StepKind::WaitingForBot
        | StepKind::ArtifactRegistered
        // The holder count stays on the desktop; the phone shows the step.
        | StepKind::ScopeBusy { .. }
        | StepKind::Completed => {}
        StepKind::TurnStarted { originator } => row.originator = Some(token(&originator)),
        // The failure breakdown stays on the desktop for now: the phone's
        // row is a protobuf contract with generated Kotlin and Swift, and
        // widening it is its own change on both platforms (ADR-362). The
        // phone shows the counts, as it always has.
        StepKind::ToolsCompleted { calls, failed, .. } => {
            row.calls = Some(calls);
            row.failed = Some(failed);
        }
        StepKind::ApprovalResolved { outcome } => row.outcome = Some(token(&outcome)),
        StepKind::TurnCompleted {
            originator,
            artifacts_registered,
        } => {
            row.originator = Some(token(&originator));
            row.artifacts_registered = Some(artifacts_registered);
        }
        StepKind::TurnCancelled { originator, reason } => {
            row.originator = Some(token(&originator));
            row.reason = Some(token(&reason));
        }
        StepKind::TurnFailed { originator, stage } => {
            row.originator = Some(token(&originator));
            row.stage = Some(token(&stage));
        }
        StepKind::Interrupted { prior_attempt } => {
            row.prior_attempt = Some(token(&prior_attempt));
        }
        StepKind::Resumed { via } => row.via = Some(token(&via)),
        StepKind::ControlTaken { discovered } => row.discovered = Some(discovered),
        StepKind::Failed { stage, .. } => row.stage = Some(token(&stage)),
        StepKind::Cancelled { reason } => row.reason = Some(token(&reason)),
        StepKind::Rated { rating } => row.rating = Some(token(&rating)),
        StepKind::Gap { events_lost } => row.events_lost = events_lost,
    }
    row
}

fn usage_row(fact: &UsageFact) -> TimelineRow {
    TimelineRow {
        kind: "usage",
        id: Some(fact.identity.to_storage()),
        input_tokens: fact.input,
        output_tokens: fact.output,
        cached_input_tokens: fact.cached_input,
        revision: Some(fact.revision.0),
        completeness: Some(token(&fact.completeness)),
        ..TimelineRow::default()
    }
}

/// A window of the activity, newest first, from the run the host already
/// holds: O(window), and no store bytes (proposal §9, criterion 2 and 3).
/// An entry this build cannot decode is a `gap` row in its place, so the
/// phone shows a step it does not understand rather than one step fewer.
pub(crate) fn timeline_rows(
    run: &RunDocument,
    newest_first_from: usize,
    len: usize,
) -> Vec<TimelineRow> {
    run.timeline_window(newest_first_from, len)
        .iter()
        .map(|item| match item {
            TimelineItem::Step(entry) => step_row(entry),
            TimelineItem::Usage(fact) => usage_row(fact),
            TimelineItem::Unrecognized => TimelineRow {
                kind: "gap",
                ..TimelineRow::default()
            },
        })
        .collect()
}

#[cfg(test)]
mod tests;
