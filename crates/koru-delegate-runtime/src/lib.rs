// SPDX-License-Identifier: MPL-2.0

//! The orchestrator of a delegated run, host-agnostic.
//!
//! A person hands a piece of work to an agent, walks away, and comes back
//! to a result (ROADMAP §1). What happens in between is this crate: it
//! drives a run from request to terminal state through the **ports** it is
//! given, and it knows three things — the order of §7.1, the fences of §7.4
//! and the meaning of each provider event per §7.5 — and nothing about
//! processes, HTTP or windows. `koru-agent.exe --worker delegate` composes
//! it over `runs.db`, the Store reached through the gateway and the session
//! host over its IPC; a test composes it over fakes; D12's cloud worker is a
//! second host over the same ports.
//!
//! ## The shape
//!
//! [`Orchestrator`] owns the ports and a table of the runs open on this
//! device. Its verbs are the roadmap's:
//!
//! - [`Orchestrator::create`] is §7.1's four steps in order — the durable
//!   intent, the context capture, the Run document, the satisfied intent —
//!   with a capture failure abandoning the request and leaving no document.
//! - [`Orchestrator::start`] admits the scope (P1.5, D34), materializes the
//!   context, binds the session key **before** the host is asked to create
//!   (§7.6), creates the delegated session with the preamble as the first
//!   submission's preface, and hands the host the event stream to pump.
//! - [`Orchestrator::handle`] applies one session update through
//!   `koru_delegate_core::transition`, appending the lean steps §7.5 lists
//!   (coalescing tool completions, never a message delta), committing usage
//!   facts per §17.2 and publishing every fact in §17.1's order — the
//!   document first, then the queue, then the bus, then the acknowledgement.
//! - [`Orchestrator::cancel`] reaches the session with `CancelReason::Run`;
//!   a takeover boundary never terminalizes (D36), and a pending takeover
//!   fences both submission and completion (§7.4).
//! - [`Orchestrator::restart`] completes pending intents forward, rebuilds
//!   the scope claims, and classifies every live attempt with §7.6's table
//!   and the host's non-creating lookup — reattach, wait and re-ask,
//!   reconcile, or `Interrupted`, never `Failed` (D10).
//! - [`Orchestrator::resume`] continues an interrupted run as a fresh
//!   attempt (§7.6): D48's admission over the original pins, the old key
//!   retired at the host **before** the next generation opens, and the
//!   provider started through the adapter's resume seam or the
//!   recorded-context fallback — the timeline says which.
//!
//! **Nothing here blocks on the world.** The host pumps a run's
//! [`SessionEvents`] on a thread of its own and calls `handle` with what
//! arrives; the one wait the orchestrator itself performs — re-asking a
//! reservation in flight — goes through the [`Sleep`] seam, so a test
//! counts it rather than waits for it.
//!
//! **It can be asked what it is doing.** [`counters`] holds the worker's
//! totals — requests recorded, attempts opened, contexts captured,
//! publications queued, acknowledged and retried, results registered,
//! restarts by verdict, schedule wakes/evaluations/documents/failures, and
//! newly appended firing outcomes — a handful of integers, always live, content-free,
//! and read through the worker's hello. What the ledger holds is read
//! separately and **without asking this crate at all**, through
//! `koru_delegate_sqlite::report`, because a worker that will not answer
//! is when the question is asked.
//!
//! **Privacy is structural.** Every fact this crate emits ([`RunFact`]) is
//! ids, counts, times and closed vocabularies; the objective, the preamble
//! and every path reach only the provider, and the routine trace behind the
//! host's `KORU_DELEGATE_DEBUG` names a run by id and a step by kind.

pub mod counters;
mod facts;
mod ledger;
mod orchestrator;
mod ports;
mod scheduler;

pub use counters::{Counters, CountingCapture};
pub use facts::{RunFact, UsageReport};
pub use orchestrator::{
    CreateError, CreateIntentStatus, Created, HandleOutcome, Options, Orchestrator, Purged,
    RestartReport, RunError, RunPlace, RunView, ScheduledScopeReservation,
    ScopeReservationResolution, Started, TOOL_STEP_BOUND, TimelineWindow, WindowItem,
};
pub use ports::{
    ArtifactRegistrations, CaptureFailure, CaptureStage, CapturedContext, Clock, ContextCapture,
    ContextPin, CreatedSession, ListedRun, ListedSchedule, MaterializeFailure, MaterializeStage,
    MaterializedContext, McpEndpoints, McpLaunch, NoScheduleEvents, Port, PortError, PortResult,
    Ports, Preamble, PreparedScheduleEvent, PublishFailure, RunDocumentHandle, RunDocuments,
    RunEvents, RunStore, ScheduleCatalog, ScheduleChangeWatch, ScheduleClock,
    ScheduleDefinitionEvent, ScheduleDocumentHandle, ScheduleDocuments, ScheduleEventMaintenance,
    ScheduleEventRef, ScheduleEvents, ScheduleLedger, ScheduleOccurrenceEvent, SchedulePreparation,
    ScheduleWake, ScheduleWatchTargets, SessionEvents, SessionLaunch, SessionUpdate, Sessions,
    Sleep, SourceRefusal, SourceRefusalReason, schedule_definition_event,
};
pub use scheduler::{
    PlannedAction, ScheduleAdmission, ScheduleCoordinator, ScheduleEffects, ScheduleEvaluation,
    ScheduleList, ScheduleListing, ScheduleNext, SchedulePlan, plan_schedule,
};

// The vocabulary a host names when it composes this crate, re-exported so
// a host names one crate for the orchestrator and its ports.
pub use koru_delegate_scope::{ResolvedScope, RootLookup, ScopeRoots, StartOrigin};
pub use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, IntentRecord, LedgerError, LookupAnswer, OpenPublication,
    PendingIntent, Projection, WriteOutcome,
};

#[cfg(test)]
mod tests;
