// SPDX-License-Identifier: MPL-2.0

//! **The only place a `delegate.*` event name is spelled.**
//!
//! Facts about delegated runs and schedules become visible to the rest of Koru
//! through the event bus, and the registry row each one lands in is a promise about what
//! can never leave the machine (telemetry class `Never`, D41). This crate is
//! how that promise is kept at the call site: one builder per registry row,
//! taking typed ids and closed enums — never a string — and
//! returning an [`Envelope`] the boundary will accept. A call site cannot
//! spell a name (each builder ties itself to its row with `event_name!`),
//! cannot invent a token (every variant is mapped by exhaustive `match` in
//! [`tokens`]), and cannot be handed an objective, a title, a folder or a
//! rating's note (no builder has a parameter of a type that holds text).
//!
//! **Subject and correlation** (ROADMAP §7.5, D22). Runs and schedules are
//! manifest-registered documents, so their document ids are subjects while
//! `RunId` and `ScheduleId` ride as typed `Id` attributes. Each history has
//! its own real root EventId: `delegate.run.created` through [`RunChain`] and
//! `delegate.schedule.created` through [`ScheduleChain`].
//!
//! **Retry identity** (§7.1, §17.2). Every builder takes a [`Stamp`] — the
//! `EventId` and the wall clock — from its caller. A re-publication from
//! `runs.db`'s pending list passes the stamp it stored, so the journal
//! deduplicates it; a usage *amendment* passes a fresh one, so a correction
//! is never swallowed.
//!
//! **Actors.** An orchestrator fact carries `Actor::agent` with the run as
//! its durable identity and the `delegate` token; a human-originated fact — a
//! turn a person submitted, a takeover, a cancellation, a rating — carries
//! `Actor::user()`. The rule is [`chain::actor_for`] and the tests hold every
//! row to it.
//!
//! This crate publishes nothing: adapters map their closed facts onto these
//! builders, and surfaces read what the journal answers. See
//! `docs/architecture/eventbus/README.md` and ADR-346.

mod artifacts;
mod chain;
mod control;
mod economics;
mod execution;
mod lifecycle;
mod schedule;
mod scope;
mod tokens;

pub use artifacts::{ArtifactAttached, ArtifactRegistered};
pub use chain::{RunChain, RunRef, Stamp, actor_for};
pub use control::{ControlReturned, ControlTaken};
pub use economics::{RunRated, RunUsage, TurnUsage};
pub use execution::{
    ApprovalRequired, ApprovalResolved, AttachmentRejected, ChildSessionDetected, InputWaiting,
    TimelineGap, ToolsCompleted, TurnCancelled, TurnCompleted, TurnFailed, TurnStarted,
};
pub use lifecycle::{
    RunCancelled, RunCompleted, RunCreated, RunFailed, RunInterrupted, RunResumed, RunStarted,
};
pub use schedule::{
    ScheduleChain, ScheduleContextFailureStage, ScheduleCreated, ScheduleFired,
    ScheduleFiringOutcome, SchedulePauseReason, SchedulePaused, ScheduleRef, ScheduleSkipReason,
    ScheduleSkipped, ScheduleUpdated,
};
pub use scope::{ScopeBusy, ScopeSkipped, scope_skipped};
pub use tokens::{
    AttachmentRejection, CompletionOutcome, DelegatedAgent, RowToken, RunOutcome, ScopeKind,
    ScopeOverlap, ToolOutcome,
};

// The vocabulary a caller of this crate already speaks, re-exported so an
// adapter names one crate for the envelope side of a run.
pub use koru_events::{EmitContext, Envelope, EventId};
pub use koru_session_protocol::CachedInputRelationship;

#[cfg(test)]
mod tests;
