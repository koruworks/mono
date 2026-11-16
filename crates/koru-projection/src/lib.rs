// SPDX-License-Identifier: MPL-2.0

//! A read model of every document the store holds.
//!
//! **Search is its first consumer, not its owner.** AI context retrieval and
//! specialized views — a calendar, a kanban, a task roll-up — want the same
//! thing: *what is in this document, semantically, without opening a
//! `LoroDoc`*. Built once as a read model, they all read it; built as "what the
//! indexer needs", every later consumer either bends the schema or reopens
//! documents behind its back.
//!
//! ```text
//! koru-store-core     authoritative. Opaque bytes. No node model.
//!       │  Committed { workspace, document, revision } — a trigger, never truth
//!       ▼
//! koru-projection     forks the LoroDoc, walks the node model,
//!       │             writes a schema-versioned read model per document
//!       │
//!       ├──► the search index
//!       ├──► retrieval
//!       └──► views
//! ```
//!
//! ## Why it is not inside the store
//!
//! `koru-store-core` persists documents as **opaque bytes** and names nothing
//! from `koru-core`, deliberately: a mobile build embeds core directly, and a
//! core that had learned the node model would carry it there too
//! ([ADR-80](../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents)).
//! So the walk lives here, above core, wired in by the daemon. **This crate is
//! the only thing above the store that links `koru-core`** — and nothing
//! downstream of it does, because everything it produces is strings and
//! primitives.
//!
//! ## The rules it is built on
//!
//! - **Events are triggers, never truth.** A commit says *look at this
//!   document*; this layer then reads the authoritative state. A lost event
//!   costs a stale projection, never a wrong one, and is repaired by comparing
//!   revisions.
//! - **The unit of work is one document, always.** A workspace may hold many
//!   gigabytes; one edit re-projects one document and reads no other.
//! - **Consumers own their cursors.** Each records what it consumed, in its own
//!   store, and reconciles by comparison. This layer tracks nobody — which is
//!   also what lets a consumer that was stopped, crashed or re-versioned catch
//!   up without a stream it must not miss.
//! - **Derived and disposable.** Deleting `projection.db` must be safe, and a
//!   schema bump is a rebuild rather than a migration.

pub mod consumer;
pub mod events;
pub mod fixtures;
pub mod model;
pub mod reminders;
pub mod store;
pub mod walk;
pub mod worker;

pub use consumer::Consumer;
pub use events::{AttributeChange, AttributeEvent, AttributeEventKind, AttributeEventValue};
pub use model::{
    DocumentProjection, PROJECTION_SCHEMA_VERSION, ProjectedAttr, ProjectedNode, ProjectedRun,
    ProjectionSourceStatus, UnfiledDocument,
};
pub use store::{ProjectionError, Projections, ReadableReference, ReadableScope, Staleness};
pub use worker::{Projector, ProjectorStats};
