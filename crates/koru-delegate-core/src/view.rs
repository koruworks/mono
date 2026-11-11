// SPDX-License-Identifier: MPL-2.0

//! What a device answers about one run: its header, what this device holds,
//! the state of each pinned source, and a window of its history.
//!
//! These are the bodies of `GET /v1/runs/{run}` and of the private Bot
//! `Read`, and the Bots API's `read_run` answers the same view (P1.8 B1). They
//! live in the vocabulary crate so that `koru-ai-core` can name a [`RunView`]
//! without depending on the transport crate; `koru-delegate-api`'s `wire`
//! re-exports them under their old path, byte for byte.

use serde::{Deserialize, Serialize};

use crate::{ContextState, DeviceId, DocumentId, RunHeader, SessionId, TimelineEntry, UsageFact};

/// One entry of the history as the wire carries it. `Unrecognized` is an
/// entry this build could not decode, in its place.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum TimelineItem {
    Step(TimelineEntry),
    Usage(UsageFact),
    Unrecognized,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TimelineWindow {
    pub total: u64,
    pub from: u64,
    pub items: Vec<TimelineItem>,
}

/// What the device that answered can do about the run, and what it holds
/// (D3, D15, §7.4).
///
/// Deliberately **not** a header field: the header is what a run means on
/// every device, and how far *this* machine got is execution state. A
/// surface compares [`Self::device`] with the header's executing device to
/// decide whether it may offer an action at all, and takes control through
/// [`Self::session`] when there is one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunExecution {
    /// The device whose worker answered.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub device: DeviceId,
    /// The provider session of the attempt this device is driving right
    /// now. `None` everywhere else — on another device, between attempts,
    /// and after the run ended — and a surface offers no handover without
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// **The folder the agent is working in**, as this device resolved it.
    /// A scope says what *kind* of folder was asked for — a project's root,
    /// a folder inside one, or one Koru makes — and only the device driving
    /// the run knows which one that turned out to be. A person judging a run
    /// they did not watch is entitled to be told where it happened.
    ///
    /// `None` on a device that is not driving it, and before an attempt has
    /// one. It is a path, so it is a person's own and stays on this wire: it
    /// reaches no log, no event attribute and no metric (§13.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_workspace: Option<String>,
}

/// `GET /v1/runs/{run}`: the header, what this device holds, the state of
/// each pinned source, and a window of the history.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunView {
    pub document: DocumentId,
    pub header: RunHeader,
    pub execution: RunExecution,
    /// What each of the header's context pins points at *now* (D46, D47),
    /// in the header's own order. Either empty — this device could not
    /// resolve the sources, and a surface says nothing rather than saying
    /// "gone" — or exactly as long as `header.context`.
    #[serde(default)]
    pub context: Vec<ContextState>,
    pub timeline: TimelineWindow,
}
