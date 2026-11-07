// SPDX-License-Identifier: MPL-2.0

//! Errors clients can act on.
//!
//! Two rules, both of them internationalization invariants rather than style:
//!
//! - **The API returns codes, not prose.** [`Code`] is a closed set of stable
//!   ASCII tokens. A client branches on the code and renders its own message in
//!   its own language; `message` is an untranslated developer string that no
//!   client may surface to a person.
//! - **No error carries customer content.** `detail` describes the *shape* of
//!   the failure — which id, which route, which limit — never a document title,
//!   a filename or a fragment of text. An error is a thing that gets logged and
//!   put in a bug report, and neither of those is a place for someone's writing.
//!
//! [`Class`] is how the HTTP layer picks a status without this crate knowing
//! that HTTP exists. That indirection is the whole reason a second transport
//! can be added later without touching a single error site.

use std::fmt;

/// The stable, machine-readable code. **Append-only**: a client in the wild
//  branches on these, so a token is never renamed and never reused.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Code {
    // ---- not found
    ProfileNotFound,
    WorkspaceNotFound,
    DocumentNotFound,
    BlobNotFound,
    BackupNotFound,
    HistoryEntryNotFound,
    /// No `document_versions` row by that id in this workspace: never
    /// captured here, or gone with a physical purge. A *pin* (a delegated
    /// run's captured version, ROADMAP §7.2) is addressed by version id,
    /// not by the history marker that recorded it.
    DocumentVersionNotFound,
    ReminderNotFound,
    // ---- the caller asked for something impossible
    InvalidRequest,
    InvalidId,
    InvalidLocale,
    UnsupportedMediaType,
    PayloadTooLarge,
    // ---- versioning
    SchemaTooNew,
    ApiVersionUnsupported,
    VersionUnavailable,
    BundleFormatUnsupported,
    // ---- authorization
    Unauthenticated,
    Forbidden,
    // ---- conflict
    WorkspaceExists,
    ProfileExists,
    DocumentExists,
    IdempotencyConflict,
    WorkspaceInUse,
    /// A local Profile cannot be removed while it still owns an active Workspace.
    /// Deleted Workspace tombstones do not make it non-empty.
    ProfileNotEmpty,
    /// The Profile exists but was removed from this installation's catalog.
    ProfileRemoved,
    ProfileBeingRemoved,
    RemovalInProgress,
    RemovalAttemptStale,
    RemovalNotBegun,
    RemovalKindHosted,
    /// The persisted binding differed from the caller's compare-and-set expectation.
    ProfileAccountConflict,
    ProfileUnitRequired,
    ProfileUnitScopeMismatch,
    /// **Two Profile identity lineages disagree about a field that cannot
    /// change.** The document names the right unit — `profile_id` agrees and
    /// every scope check passed — but merging it would rewrite `created_at` or
    /// `kind`, which `docs/cloud-platform/contracts/profile-units-v1.md` calls
    /// immutable. That only happens when `IdentityManifest::create` ran twice
    /// for one Profile id, on two installations, and both lineages met.
    ///
    /// Its own code because it is **not** a scope mismatch and reading it as
    /// one sends you to the wrong end: the unit reference, the export mode and
    /// the advertised version are all correct, and no transfer can fix it. See
    /// PLAN19 round five.
    ProfileIdentityConflict,
    ProfilePrivateUnavailable,
    ReminderIndexUpdating,
    /// **The system-owned holder is not an editable persona.** Renaming,
    /// recolouring and removing all refuse on it, and they refuse with one code
    /// because they refuse for one reason: it is not a persona, so none of the
    /// things one does to a persona apply. A caller that reads this has asked
    /// something that will never become possible for this record.
    ProfileReserved,
    /// **A Workspace was selected above its Profile's transport mode.** The
    /// ladder is Local < P2P < Cloud with the Profile as the ceiling (D230), and
    /// a child never silently raises its parent — so the answer is "choose the
    /// Profile first", which is advice a caller must be able to give without
    /// reading the message.
    TransportModeCeiling,
    /// **A Profile downgrade left higher-mode Workspaces unresolved.** Its own
    /// code because the remedy is different from every other conflict: the
    /// caller resolves the named children and resubmits them in one group, which
    /// then commits atomically.
    TransportModeChildren,
    ReadOnly,
    /// **The bytes exist somewhere, just not here and not on any connected
    /// device.** Answered by a blob read whose hydration parked waiting for a
    /// peer — the holder is offline, or nothing is linked. Its own code
    /// because a caller has to tell "come back when the device is online"
    /// from every failure that retrying cannot help.
    BlobWaitingForPeer,
    /// A blob read parked with **nothing linked at all**: no device exists
    /// that could ever bring the bytes, so "waiting" would be a lie. Its own
    /// code because the advice is different — link a device — and a caller
    /// must not have to read the message to give it.
    NoLinkedDevices,
    // ---- the store could not do it
    StorageFailed,
    /// **There is no store to talk to** — nothing listening, or the process
    /// answering is not the one this client handshook with. Minted by a client
    /// and never sent by a store, which could not report it about itself.
    ///
    /// Its own code for the same reason [`SearchUnavailable`](Self::SearchUnavailable)
    /// has one: a caller has to tell "the store is gone" from "the store's disk
    /// failed" without reading the message, because only the first is worth
    /// re-dialling for. A store's session credential is minted per connection
    /// and its port is whatever it bound, so a cached connection is valid only
    /// for as long as that process lives.
    StoreUnreachable,
    /// This build serves no search index — `search.db` would not open, or none
    /// runs. **Not a storage failure**: the store is intact and serving
    /// documents, and a client should be able to tell "search is not here" from
    /// "the disk is broken" without reading the message.
    SearchUnavailable,
    /// This build moves no blob bytes — no transfer engine was composed, so
    /// there is nothing to ask about a picture and nothing to pause.
    ///
    /// **Its own code, for [`SearchUnavailable`](Self::SearchUnavailable)'s
    /// reason exactly.** A surface has to tell "this store has no blob engine"
    /// from "the engine refused" without reading the message: the first
    /// disables a button and the second explains one.
    BlobTransferUnavailable,
    ProjectionUnavailable,
    CorruptState,
    BlobChecksumMismatch,
    CrdtRejected,
    Internal,
}

impl Code {
    /// The token that crosses the wire. Stable forever.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProfileNotFound => "profile_not_found",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::DocumentNotFound => "document_not_found",
            Self::BlobNotFound => "blob_not_found",
            Self::BackupNotFound => "backup_not_found",
            Self::HistoryEntryNotFound => "history_entry_not_found",
            Self::DocumentVersionNotFound => "document_version_not_found",
            Self::ReminderNotFound => "reminder_not_found",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidId => "invalid_id",
            Self::InvalidLocale => "invalid_locale",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::PayloadTooLarge => "payload_too_large",
            Self::SchemaTooNew => "schema_too_new",
            Self::ApiVersionUnsupported => "api_version_unsupported",
            Self::VersionUnavailable => "version_unavailable",
            Self::BundleFormatUnsupported => "bundle_format_unsupported",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::WorkspaceExists => "workspace_exists",
            Self::ProfileExists => "profile_exists",
            Self::DocumentExists => "document_exists",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::WorkspaceInUse => "workspace_in_use",
            Self::ProfileNotEmpty => "profile_not_empty",
            Self::ProfileRemoved => "profile_removed",
            Self::ProfileBeingRemoved => "profile_being_removed",
            Self::RemovalInProgress => "removal_in_progress",
            Self::RemovalAttemptStale => "removal_attempt_stale",
            Self::RemovalNotBegun => "removal_not_begun",
            Self::RemovalKindHosted => "removal_kind_hosted",
            Self::ProfileAccountConflict => "profile_account_conflict",
            Self::ProfileUnitRequired => "profile_unit_required",
            Self::ProfileUnitScopeMismatch => "profile_unit_scope_mismatch",
            Self::ProfileIdentityConflict => "profile_identity_conflict",
            Self::ProfilePrivateUnavailable => "profile_private_unavailable",
            Self::ReminderIndexUpdating => "reminder_index_updating",
            Self::ProfileReserved => "profile_reserved",
            Self::TransportModeCeiling => "transport_mode_ceiling",
            Self::TransportModeChildren => "transport_mode_children",
            Self::ReadOnly => "read_only",
            Self::BlobWaitingForPeer => "blob_waiting_for_peer",
            Self::NoLinkedDevices => "no_linked_devices",
            Self::StorageFailed => "storage_failed",
            Self::StoreUnreachable => "store_unreachable",
            Self::SearchUnavailable => "search_unavailable",
            Self::BlobTransferUnavailable => "blob_transfer_unavailable",
            Self::ProjectionUnavailable => "projection_unavailable",
            Self::CorruptState => "corrupt_state",
            Self::BlobChecksumMismatch => "blob_checksum_mismatch",
            Self::CrdtRejected => "crdt_rejected",
            Self::Internal => "internal",
        }
    }

    /// Every code, so a test can prove the token set is complete and unique and
    /// the docs can be generated from it rather than kept in step by hand.
    pub const ALL: &'static [Self] = &[
        Self::ProfileNotFound,
        Self::WorkspaceNotFound,
        Self::DocumentNotFound,
        Self::BlobNotFound,
        Self::BackupNotFound,
        Self::HistoryEntryNotFound,
        Self::DocumentVersionNotFound,
        Self::ReminderNotFound,
        Self::InvalidRequest,
        Self::InvalidId,
        Self::InvalidLocale,
        Self::UnsupportedMediaType,
        Self::PayloadTooLarge,
        Self::SchemaTooNew,
        Self::ApiVersionUnsupported,
        Self::VersionUnavailable,
        Self::BundleFormatUnsupported,
        Self::Unauthenticated,
        Self::Forbidden,
        Self::WorkspaceExists,
        Self::ProfileExists,
        Self::DocumentExists,
        Self::IdempotencyConflict,
        Self::WorkspaceInUse,
        Self::ProfileNotEmpty,
        Self::ProfileRemoved,
        Self::ProfileBeingRemoved,
        Self::RemovalInProgress,
        Self::RemovalAttemptStale,
        Self::RemovalNotBegun,
        Self::RemovalKindHosted,
        Self::ProfileAccountConflict,
        Self::ProfileUnitRequired,
        Self::ProfileUnitScopeMismatch,
        Self::ProfileIdentityConflict,
        Self::ProfilePrivateUnavailable,
        Self::ReminderIndexUpdating,
        Self::ProfileReserved,
        Self::TransportModeCeiling,
        Self::TransportModeChildren,
        Self::ReadOnly,
        Self::BlobWaitingForPeer,
        Self::NoLinkedDevices,
        Self::StorageFailed,
        Self::StoreUnreachable,
        Self::SearchUnavailable,
        Self::BlobTransferUnavailable,
        Self::ProjectionUnavailable,
        Self::CorruptState,
        Self::BlobChecksumMismatch,
        Self::CrdtRejected,
        Self::Internal,
    ];

    /// What kind of failure this is, in transport-neutral terms.
    pub fn class(self) -> Class {
        match self {
            Self::ProfileNotFound
            | Self::WorkspaceNotFound
            | Self::DocumentNotFound
            | Self::BlobNotFound
            | Self::BackupNotFound => Class::NotFound,
            Self::HistoryEntryNotFound | Self::DocumentVersionNotFound | Self::ReminderNotFound => {
                Class::NotFound
            }

            Self::InvalidRequest
            | Self::InvalidId
            | Self::InvalidLocale
            | Self::UnsupportedMediaType
            | Self::PayloadTooLarge
            | Self::ApiVersionUnsupported
            | Self::VersionUnavailable
            | Self::BundleFormatUnsupported
            | Self::CrdtRejected => Class::Invalid,

            Self::ProfileUnitScopeMismatch => Class::Invalid,

            Self::Unauthenticated => Class::Unauthenticated,
            Self::Forbidden => Class::Forbidden,

            Self::WorkspaceExists
            | Self::ProfileExists
            | Self::DocumentExists
            | Self::IdempotencyConflict
            | Self::WorkspaceInUse
            | Self::ProfileNotEmpty
            | Self::ProfileRemoved
            | Self::ProfileBeingRemoved
            | Self::RemovalInProgress
            | Self::RemovalAttemptStale
            | Self::RemovalNotBegun
            | Self::RemovalKindHosted
            | Self::ProfileAccountConflict
            | Self::ProfileIdentityConflict
            | Self::ProfileUnitRequired
            | Self::ProfileReserved
            | Self::TransportModeCeiling
            | Self::TransportModeChildren
            | Self::ReadOnly
            | Self::SchemaTooNew => Class::Conflict,

            Self::StorageFailed
            | Self::StoreUnreachable
            | Self::CorruptState
            | Self::BlobChecksumMismatch
            | Self::BlobWaitingForPeer
            | Self::NoLinkedDevices
            | Self::SearchUnavailable
            | Self::BlobTransferUnavailable
            | Self::ProjectionUnavailable => Class::Unavailable,

            Self::ProfilePrivateUnavailable | Self::ReminderIndexUpdating => Class::Unavailable,

            Self::Internal => Class::Internal,
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The shape of a failure, without naming a protocol.
///
/// The HTTP layer maps these to statuses; a future IPC transport maps them to
/// whatever it uses. Nothing in this crate mentions 404.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Class {
    NotFound,
    Invalid,
    Unauthenticated,
    Forbidden,
    Conflict,
    Unavailable,
    Internal,
}

/// A failure, as a client sees it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Error {
    pub code: Code,
    /// **Developer text, never shown to a person, never customer content.**
    detail: String,
    /// Display names of the devices a parked blob hydration is waiting on —
    /// display data for the client's own sentence, empty for every other
    /// failure. Structured here so a client does not have to parse it out of
    /// `detail`; `Display` still renders the `waiting_on=` suffix for the
    /// consumers that cross a stringly boundary.
    waiting_on: Vec<String>,
    /// **Why blob transfer is holding**, when that is what refused this.
    ///
    /// A `TransferHold` code from the blob engine's own registry —
    /// `mode_refuses_transport`, `network_metered`, `network_low_data`,
    /// `network_offline`, `paused_by_person`, `cloud_access_unavailable`.
    /// A different vocabulary from [`Code`] and deliberately not folded into
    /// it: *this device is not using Cloud for this Workspace* is a policy
    /// answer rather than a Store failure, and a person reads a different
    /// sentence for each. Absent on everything that is not a hold.
    hold: Option<String>,
}

impl Error {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            waiting_on: Vec::new(),
            hold: None,
        }
    }

    /// A code with nothing more to say. Common enough to be worth its own name.
    pub fn bare(code: Code) -> Self {
        Self {
            code,
            detail: String::new(),
            waiting_on: Vec::new(),
            hold: None,
        }
    }

    /// The devices a parked hydration is waiting on. Only meaningful on
    /// `Code::BlobWaitingForPeer`.
    pub fn with_waiting_on(mut self, waiting_on: Vec<String>) -> Self {
        self.waiting_on = waiting_on;
        self
    }

    pub fn waiting_on(&self) -> &[String] {
        &self.waiting_on
    }

    /// The blob transfer hold that refused this, when one did.
    pub fn with_hold(mut self, hold: Option<String>) -> Self {
        self.hold = hold;
        self
    }

    /// Why blob transfer is holding. See the field.
    pub fn hold(&self) -> Option<&str> {
        self.hold.as_deref()
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn class(&self) -> Class {
        self.code.class()
    }

    /// The untranslated developer string. A client must branch on
    /// [`Error::code`] and write its own message; this is for logs.
    pub fn message(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            write!(f, "{}", self.code)?;
        } else {
            write!(f, "{}: {}", self.code, self.detail)?;
        }
        // The suffix keeps the stringly boundary lossless: a consumer that only
        // has the rendered message can still recover the names.
        if !self.waiting_on.is_empty() {
            write!(f, " waiting_on={}", self.waiting_on.join("\u{1f}"))?;
        }
        if let Some(hold) = &self.hold {
            write!(f, " hold={hold}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
