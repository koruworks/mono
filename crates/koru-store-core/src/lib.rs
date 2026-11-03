// SPDX-License-Identifier: MPL-2.0

//! **Koru Store, without a process around it.**
//!
//! The store is the only component allowed to persist Koru's CRDT state. This
//! crate is the part of it that is not a daemon: the model, the manifest, the
//! Loro lifecycle, the recovery policy, and the traits a persistence layer
//! implements. `apps/koru-store-server` hosts it behind a socket; a mobile build
//! embeds it directly. Neither is privileged, which is the point — a store that
//! only worked behind a socket would need a fork to run on a phone.
//!
//! ## What is deliberately not here
//!
//! - **No tokio, no axum, no HTTP.** This crate is synchronous. Asynchrony is a
//!   property of a host, not of a store.
//! - **No filesystem policy.** Where a database lives is the daemon's business.
//! - **No SQL.** Persistence enters through the traits in [`ports`].
//! - **No `koru-core` or `koru-docstore`.** Not because those are UI crates —
//!   they are not — but because they *are* the one-giant-`LoroDoc` model this
//!   replaces, and depending on them would import the shape being redone.
//!
//! ## The shape
//!
//! ```text
//! Workspace W
//!   manifest LoroDoc      one per workspace; the store defines its shape
//!     meta      LoroMap    name, created_at, schema_version
//!     documents LoroTree   an entry per document
//!   document LoroDoc       one per entry — opaque to the store
//!   blobs                  content-addressed, outside Loro entirely
//! ```
//!
//! The store is **content-agnostic below the manifest**. A document is
//! checkpoint bytes plus update bytes; what those mean is the application's.
//! The manifest is the one exception, because a workspace has to be listable
//! without opening everything in it.
//!
//! ## Reading order
//!
//! [`ids`] → [`model`] → [`manifest`] → [`ports`] → [`lifecycle`].
//! [`errors`], [`text`] and [`version`] are referenced from all of them, and
//! [`fixtures`] is the text every suite in the store is tested against.

pub mod bundle;
pub mod commit_signal;
pub mod document_private;
pub mod errors;
pub mod fixtures;
pub mod ids;
pub mod lifecycle;
pub mod manifest;
pub mod model;
pub mod personal_attributes;
pub mod ports;
pub mod profile_account;
pub mod profile_manifest;
pub mod profile_units;
pub mod recovery;
pub mod reminders;
pub mod service;
pub mod stats;
pub mod temporal;
pub mod text;
pub mod transport;
pub mod version;

pub use bundle::{Bundle, ImportMode};
pub use errors::{Class, Code, Error, Result};
pub use ids::{ActionId, ClientId, DocumentId, ProfileId, ReminderId, RuleRevisionId, WorkspaceId};
pub use lifecycle::{CheckpointPolicy, CommitWindow, LiveDocument, checksum};
pub use manifest::Manifest;
pub use model::{
    ApiClient, Blob, BlobHash, DocumentEntry, DocumentKind, Lineage, LineageKind, Profile, Scope,
    Timestamp, Workspace,
};
pub use personal_attributes::{EntityRef, PersonalAttributeValue};
pub use ports::{
    BackupId, BackupInfo, Backups, BlobStore, Catalog, Checkpoint, CheckpointHeader, DocumentIndex,
    DocumentState, LocalProfileRemoval, LocalProfileRestoration, Opened, ProfileCatalogState,
    Recovery, SeedDocument, SeededDocument, StoredUpdate, UpdateBatch, WorkspaceRepo,
};
pub use profile_account::{
    ProfileAccountBinding, ProfileAccountBindingOutcome, ProfileAccountClearOutcome,
};
pub use reminders::{
    ActionKind, DueRelation, EffectiveState, LocalTime, OccurrenceAction, OccurrenceKey,
    OccurrenceMoment, OccurrenceSource, OneTimeSchedule, PauseReason, ReducedReminder,
    ReminderActions, ReminderChange, ReminderReductionInput, ReminderRule, ReminderSettings,
    RuleStatus, SemanticOffset, StoredReminder, TargetState, ToneId, WeekStartPreference,
};
pub use service::TransportModeChoice;
pub mod sync_digest;
pub use service::{
    Backend, Committed, DictionaryCommitted, IdentityChange, IdentityCommitted, ReminderCommitted,
    SeededCreation, Store,
};
pub use temporal::{CalendarDate, FloatingDateTime, TemporalValue, ZonedDateTime};
pub use text::Locale;
pub use transport::{
    EffectiveTransportMode, InitializeTransportMode, SetTransportMode, SharingEvidence,
    TransportMode, TransportModeCommit, TransportModeRequest, TransportModeState, TransportPath,
    TransportSubject,
};
pub use version::{
    API_VERSION, BUNDLE_FORMAT_VERSION, CAPABILITIES, CATALOG_SCHEMA_VERSION, CRDT_SCHEMA_VERSION,
    Compatibility, WORKSPACE_SCHEMA_VERSION,
};
