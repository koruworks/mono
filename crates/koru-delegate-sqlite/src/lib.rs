// SPDX-License-Identifier: MPL-2.0

//! The device-local execution ledger of delegated runs.
//!
//! One SQLite file, `<channel-root>/delegate/runs.db`, holding how far *this
//! machine* got with each run: the creation intent with the whole delegation
//! payload a restart completes the run from, one row per execution attempt
//! with the fence a fresh attempt must pass, the publications not yet
//! acknowledged, where each projection of the run's facts has got to, and the
//! schedule slots this device has claimed. It holds **no canonical run
//! status** — the Run document is the run (ROADMAP §7.1, §15) — and it
//! **never syncs** (D3).
//!
//! Three rules shape the crate:
//!
//! - **Durability is the meaning of every write.** The file runs
//!   `synchronous = FULL`: these are facts a person is entitled to see
//!   (§7.1's authority table), not observations. Every write is one
//!   transaction, and the [`CrashPoint`] seam lets a test abort it before the
//!   write, after the write but before its commit, and after the commit but
//!   before the caller hears of it — so the ordering §7.1 promises is proved
//!   by crashing it, not described.
//! - **Every domain is closed twice.** The database `CHECK`s each state
//!   token and the Rust side converts at the boundary ([`AttemptPhase`],
//!   [`IntentState`], [`Projection`], [`SlotState`]); a value outside the
//!   domain is refused by both. There is no `failed` attempt state (D10).
//! - **Content never leaves the file.** The intent payload and a
//!   publication bodies are the content columns. They are
//!   read back by the worker that retries, and they appear in no error, no
//!   log, no `Debug` rendering and no diagnostic count.
//!
//! [`classify_at_restart`] is §7.6's table as a pure function over an
//! attempt row and the session host's non-creating lookup answer, and
//! [`inspect`], [`report`] and [`run_report`] are the read-only views
//! diagnostics read **with no worker running** — the counts, the whole
//! ledger as rows, and one run in as much detail as this file holds.

mod attempts;
mod bots;
mod claims;
mod clock;
mod crash;
mod cursors;
mod db;
mod decisions;
mod deletions;
mod domains;
mod error;
mod inspect;
mod intents;
mod ledger;
mod native_results;
mod publications;
mod restart;
mod schedule_publications;

pub use attempts::AttemptRow;
pub use bots::{
    BOT_ACTIVE_BUDGET_MS, BOT_HOST_EXECUTION_LIMIT, BOT_PENDING_LIMIT, BotAdmissionRecord,
    BotExecutionCapacity, BotExecutionInventory, BotExecutionRecord, BotOwnedExecution,
    BotInitialSend, BotLedger, BotLedgerError, BotReservation, BotSettlement,
    QueuedBotAdmission, RoutineHeldCursor, RoutineOccurrenceFact, RoutinePage, RoutinePolicyRecord,
    RoutineQueuedCursor, RoutineRecoveryEntry, RoutineSlotCursor,
};
pub use claims::{ClaimedSlot, SkippedSlot, SlotClaim};
pub use clock::{Clock, SystemClock};
pub use crash::WriteOp;
#[cfg(any(test, feature = "crash-points"))]
pub use crash::{CrashPoint, Moment};
pub use db::{DATABASE_NAME, OpenError, RUNS_SCHEMA_VERSION, runs_db_path};
pub use deletions::DeletionRecord;
pub use decisions::{DecisionRecord, DecisionWaitExecution, DecisionWaitRecord};
pub use domains::{AttemptPhase, IntentState, Projection, SlotOutcome, SlotState};
pub use error::LedgerError;
pub use inspect::{
    AttemptSummary, CursorSummary, InspectError, IntentSummary, PUBLICATION_ROWS,
    PublicationSummary, Report, RunReport, SCHEDULE_SKIP_ROWS, ScheduleSkipSummary, ScheduleTotals,
    SlotSummary, Summary, inspect, report, run_report,
};
pub use intents::{IntentFacts, IntentRecord, PendingIntent};
pub use ledger::{RunsDb, WriteOutcome};
pub use native_results::{
    NativeEffectLease, NativeIntentRecord, NativeLeaseOutcome, NativeLeaseState,
    NativePolicyRecord, NativeRecoveryCursor, NativeResultLedger, PendingNativeResult,
};
pub use publications::{
    OpenPublication, PendingPublicationRunCursor, PendingPublicationRunSeek,
    PublicationBody, PublicationRecord,
};
pub use restart::{LookupAnswer, RestartVerdict, ResumePolicy, classify_at_restart};
pub use schedule_publications::{
    SchedulePublicationBody, SchedulePublicationCursor, SchedulePublicationFact,
    SchedulePublicationKind, SchedulePublicationPreparation, SchedulePublicationRecord,
    SchedulePublicationRef,
};

#[cfg(test)]
pub(crate) mod fixtures;
