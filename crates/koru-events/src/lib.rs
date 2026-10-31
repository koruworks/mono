// SPDX-License-Identifier: MPL-2.0

//! **What a Koru event is — and nothing that stores, transmits, or reacts to
//! one.**
//!
//! Events are facts distributed after commit, never commands: the store stays
//! the truth, an envelope carries references and typed attributes, and a
//! consumer that missed one reconciles rather than mourns (ADR-101 applied a
//! layer up). This crate is the contract every side names — the events
//! worker's journal, the SDK in `koru-api-client`, the store's commit feed,
//! and later the metrics ingest service and the flow-rule engine — so a
//! renamed field breaks every build instead of arriving as a silent `None`.
//!
//! The map:
//! - [`Envelope`] and its parts — the record ([`envelope`]).
//! - [`REGISTRY`] — every published name, its class, telemetry policy and
//!   typed attrs ([`registry`]); `reference/EVENTS.md` is generated from it.
//! - [`Draft`], [`EmitContext`] and the typed payloads ([`catalog`]).
//! - [`validate`] — the one boundary check, used by both sides.
//! - [`telemetry`] — the export wire shapes and the identity-stripping
//!   [`telemetry::project`].
//! - [`flow`] — what Milestone 3 shares today ([`flow::ExecutionScope`]).
//!
//! Architecture, the model and the decisions: `docs/architecture/eventbus/README.md`.

pub mod catalog;
pub mod docs;
pub mod envelope;
pub mod flow;
pub mod registry;
pub mod telemetry;
mod validate;
pub mod wire;

pub use catalog::{
    AccountClient, AccountFailure, AccountOperationCompleted, AccountOutcome, AccountPlatform,
    AccountProvider, AccountStage, AppEnvironment, AppInstalled, AppSessionEnded,
    AppSessionStarted, AppUpdateAccepted, AppUpdateAvailable, AppUpdateDownloaded, AppUpdated,
    BUCKET_LADDER, DictationOutcome, DictationSurface, DocumentPrivateChanged, Draft, EmitContext,
    EventPayload, ImportCompleted, ImportOutcome, ImportSource, OnboardingEntryChoice,
    OnboardingEntrySelected, OnboardingProfilePurposeSelected, PreviousSession,
    ProfileAppearanceChanged, ProfileCreated, ProfileDictionaryChanged, ProfilePurpose,
    ProfileRenamed, SearchOpened, SearchSurface, SettingsAiChanged, SettingsGlobalChanged,
    SettingsSection, SettingsTerminalChanged, TaskAttributeChange, TaskChildAdded, TaskChildClass,
    TaskChildCompleted, TaskChildMoved, TaskChildRemoved, TaskChildReopened, TaskCompleted,
    TaskCompletedReason, TaskContentChanged, TaskCreated, TaskCreatedReason, TaskDueChanged,
    TaskNodeKind, TaskPriority, TaskPriorityChanged, TaskRemoved, TaskReopened, TaskReopenedReason,
    TaskSubject, TaskTemporalKind, TerminalAttachmentDelivered, TerminalDeliveryMode,
    TerminalDeliveryOutcome, TerminalExperienceSummary, TerminalInputKind, TerminalInputSubmitted,
    TerminalMode, TerminalProfileKind, TerminalProjectCreated, TerminalProjectDeleted,
    TerminalProjectRoot, TerminalProjectScope, TerminalProviderKind, TerminalSessionEnded,
    TerminalSessionOutcome, TerminalSessionReattached, TerminalSessionStarted, TerminalSurface,
    TerminalSurfaceOpened, TerminalTabKind, TerminalTabOpened, VoiceDictationCompleted,
    VoiceEngine, VoiceModelInstalled, VoiceTier, WorkspaceCatalogAppearanceChanged,
    WorkspaceCatalogArchived, WorkspaceCatalogCreated, WorkspaceCatalogDeleted,
    WorkspaceCatalogImported, WorkspaceCatalogMoved, WorkspaceCatalogRenamed,
    WorkspaceCatalogReordered, WorkspaceCatalogRestored, WorkspaceCatalogUnarchived,
    WorkspaceCreated, WorkspaceDeleted, WorkspaceImportMode, WorkspaceSource, bucket,
};
pub use envelope::{
    Actor, ActorId, ActorKind, AttrKey, AttrValue, DeviceId, Envelope, EventId, EventName, Origin,
    ProfileId, Scope, SessionId, Subject, SubjectKind, WorkspaceId,
};
pub use registry::{
    AttrKind, AttrSpec, EventClass, EventSpec, FieldClass, REGISTRY, TelemetryPolicy,
};
pub use validate::{Violation, validate};

#[cfg(test)]
mod tests;
