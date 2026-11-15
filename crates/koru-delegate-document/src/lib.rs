// SPDX-License-Identifier: MPL-2.0

//! The Run document — how a delegated run is written into and read from one
//! Loro document. It knows the shape, not the store, and never the window.
//!
//! A run is **one `LoroDoc`** registered in a workspace manifest under the
//! kind token [`RUN_KIND`] (`docs/architecture/DATAMODEL.md`, ADR-337). Two
//! root containers:
//!
//! ```text
//! run       LoroMap   last-writer-wins; written by the executing device (D15)
//!             schema         i64      ordinary/Interactive 3, Routine 4; reader maximum
//!                                     RUN_DOCUMENT_SCHEMA_VERSION — the run's OWN gate,
//!                                     never `meta.schema_version` (proposal F3)
//!             id             RunId as text
//!             created_at_ms  i64
//!             requested_by   ActorRef
//!             objective      text     content — lives here, never in an event
//!             agent          SessionProfileKey
//!             scope          WorkspaceScope
//!             context        list of pins (§7.2)
//!             grants         list of WorkspaceId text (D28)
//!             status         RunStatus { state, … }
//!             environment    ExecutionEnvironment { kind, device }  — the executing device (D16)
//!             results        list of ResultReference (D27)
//!             models         LoroMap  model_ref -> ModelIdentity (§17.3)
//!             economics      EconomicsSummary — the fold of the usage entries, cached
//!             rating         RunRating { rating, note, … } (D44; the note is content)
//! timeline  LoroList  of immutable entry values (D45), each a step or a usage fact
//! ```
//!
//! Every value under `run` and every entry in `timeline` is the vocabulary's
//! own serde shape (`koru-delegate-core`), so what a header means is decided
//! once, there, and this crate decides only where it goes. Two keys are
//! spelled as the proposal spells them rather than as the struct does —
//! `run.id` for `RunHeader::run` and `run.results` for `RunHeader::artifacts`
//! — and [`codec`] holds that table.
//!
//! **Stored names never change.** `koru-run`, `koru-schedule`, `run`,
//! `timeline`, `schema` and every key above are what is on disk on every
//! device; a rename is a migration, and the version that gates one is
//! [`RUN_DOCUMENT_SCHEMA_VERSION`].
//!
//! ## How a run is created (what P1.6 calls)
//!
//! ```text
//! let (id, _) = store.create_document(ws, title, RUN_KIND, None, None, at)?;   // manifest entry
//! let run = RunDocument::create(&header)?;                                     // the shape, in memory
//! store.apply_update(ws, id, run.export_updates_since(&VersionVector::default())?, at, None)?;
//! store.flush(ws, id, at)?;
//! ```
//!
//! and every step afterwards is `append_step` / `record_usage` / `set_status`
//! on the same `RunDocument`, then `export_updates_since(last sent)` —
//! 205–248 bytes a step, whatever the history — applied and flushed. A
//! reader adopts `store.fork_document(ws, id)` or `from_snapshot` of
//! `store.snapshot(ws, id)`. `crates/koru-store-sqlite/tests/run_document.rs`
//! proves the sequence through the real store.

mod codec;
mod document;
mod schedule;

pub use codec::{Part, RunDocumentError, TimelineItem};
pub use document::RunDocument;
pub use schedule::{
    RoutineDetailProjection, RoutineExpiryAuthority, RoutineExpiryPlan, RoutineOverview,
    ScheduleClaimAuthority, ScheduleClaimPlan, ScheduleDocument, ScheduleDocumentError,
    ScheduleRunClaim, ScheduleSkipClaim, ScheduleWrite,
};

/// The manifest kind token a run is registered under. A **stored name**: it
/// is the value in every workspace manifest's open `kind` string on every
/// device, so it never changes. `koru_core::explore::KIND_RUN` is the same
/// literal on the side that decides what a person browses; the two are
/// pinned equal by a test in each crate rather than shared, because the
/// store side cannot link the node model (ADR-80).
pub const RUN_KIND: &str = "koru-run";

/// The manifest kind token a schedule is registered under (M2, P2.2). Reserved
/// here so the hiding and the projection exclusion can name it before any
/// schedule exists; the same stored-name rule as [`RUN_KIND`].
pub const SCHEDULE_KIND: &str = "koru-schedule";

/// The Run document's own version, written to `run.schema`. Gated with
/// `koru_store_core::version::Compatibility::against(found, this)`: a run
/// written by a newer build opens read-only here, never refused, and never
/// through the node model's `meta.schema_version` (proposal F3).
pub const RUN_DOCUMENT_SCHEMA_VERSION: i64 = 4;
/// Ordinary and Interactive Bot creation keeps writing the existing format.
pub const RUN_INTERACTIVE_DOCUMENT_SCHEMA_VERSION: i64 = 3;
/// A Routine Run must retain its coherent scheduled Bot provenance.
pub const RUN_ROUTINE_DOCUMENT_SCHEMA_VERSION: i64 = 4;

/// The schedule document's own local gate, stored at `schedule.schema`.
pub const SCHEDULE_DOCUMENT_SCHEMA_VERSION: i64 = 2;
/// Ordinary schedules keep writing their original compatible format.
pub const ORDINARY_SCHEDULE_DOCUMENT_SCHEMA_VERSION: i64 = 1;
/// Bot association is mandatory in this format; ordinary execution refuses it.
pub const BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION: i64 = 2;

#[cfg(test)]
mod tests;
