// SPDX-License-Identifier: MPL-2.0

//! Independent versions for each persisted format and API boundary.
//!
//! A single "store version" would force a bump of everything whenever any one
//! surface moved, and — worse — would leave a reader unable to say *which*
//! surface it could not understand. These are separate so a build can be exact
//! about what it does and does not know.
//!
//! | version | changes when |
//! | --- | --- |
//! | [`API_VERSION`] | a route's request or response shape changes incompatibly |
//! | [`CATALOG_SCHEMA_VERSION`] | `catalog.db` gains a table or column |
//! | [`WORKSPACE_SCHEMA_VERSION`] | `workspace.db` gains a table or column |
//! | [`CRDT_SCHEMA_VERSION`] | the manifest's Loro shape changes |
//! | [`BUNDLE_FORMAT_VERSION`] | the `.koru` bundle layout changes |
//!
//! **The rule the whole module exists for:** a newer release must never
//! silently write state an older release cannot understand. Both SQLite
//! versions are enforced by forward-only migrations, which an older binary
//! simply refuses. The CRDT version is the interesting one, because a document
//! reaches an older build through *sync* rather than through a file — so
//! [`Compatibility`] gates writing, and a too-new document is opened read-only
//! and **visibly**, never written to in a shape its author would not recognise.

use std::fmt;

/// The `/v1` in every route. Present in `GET /v1/status` so a client can
/// negotiate rather than guess.
pub const API_VERSION: u32 = 1;

/// `catalog.db`, held in SQLite's `PRAGMA user_version`.
///
/// `2` added a workspace's switcher appearance — icon, colour, pinned,
/// archived. `3` added nullable `deleted_at`, independently of archive. `4`
/// added the persisted workspace position used by drag reorder. `5` adds the
/// Store-owned profile CRDT snapshots. `6` adds disposable document digest
/// leaves used by hierarchical anti-entropy. The leaves retain their source
/// revisions so they can be repaired after a crash; neither table replaces its
/// authoritative profile/document state. `7` adds the profile switcher's
/// explicit emoji and color fields, backfilling legacy `color:*` avatars. `8`
/// adds the seven-day, local-only history of remote state changes. `9` adds a
/// device-local Profile removal marker; Profile and Workspace rows remain intact.
/// OAuth and dev independently used `9`: one added the device-local removal
/// marker and the other `profiles.kind`, which distinguishes the system-owned
/// workspace holder from personas. `10` conditionally reconciles either stamped
/// shape so both columns exist without repeating an `ALTER TABLE`.
/// `11` adds separate identity/private/personal Profile snapshots and the
/// rebuildable reminder due index (migration 018). `12` adds
/// `local_transport_modes`, the device-local Local/P2P/Cloud choice per Profile
/// and Workspace with the compare-and-set revision that orders it (D230, D249,
/// migration 024). Seeded Local-only, and deliberately never replicated: two
/// devices are *supposed* to disagree about this one.
pub const CATALOG_SCHEMA_VERSION: i64 = 12;

/// `workspace.db`, held in SQLite's `PRAGMA user_version`.
///
/// `2` added `documents.trashed_at` and `3` added `documents.trashed_by`. The
/// `documents` table is a **rebuildable projection** of the manifest, so both
/// migrations add a column and nothing else: the values arrive on the next
/// `reindex`, and a database that never gets one is a database with nothing in
/// the bin.
///
/// `4` added immutable `document_versions` and their user-visible
/// `document_history` entries. `5` adds `document_creations`, the seven-day
/// idempotency ledger for content-first document creation. Unlike `documents`,
/// those tables are durable protocol state: history retains exact versions,
/// while the creation ledger remembers the original document and initial
/// update so a timed-out create can replay its original success instead of
/// making a duplicate.
///
/// **`trashed_by` is `3` rather than part of `2` because `2` had already
/// shipped.** It was appended to that migration's SQL, which no database
/// stamped `2` ever re-runs — so the column was there on every workspace
/// created afterwards and missing on every one created before, and the failure
/// arrived as *Couldn't create that document*. A column added to a migration
/// that has run is a column that reaches nobody.
///
/// `6` added `loro_updates.origin_json` and `7` added the derived
/// `documents.mime`. Two development branches briefly assigned those numbers
/// differently, so a database stamped `7` can contain either column without
/// the other. `8` idempotently ensures both columns and is the first version
/// whose number unambiguously names the combined schema. `9` adds
/// `blobs.attributes`, the typed key/value map (JSON) that carries a blob's
/// origin device and every fact about it added later without a new column.
/// `10` adds `blob_origins`, the same provenance fact learned from a peer's
/// advertisement, for a device that never downloaded the bytes. `11` adds
/// `documents.note_doc`, the derived projection of a file's attached note.
/// `12` admits Cloud provenance in the existing journal JSON. Its empty
/// structural migration fences older writers that would silently lose it.
/// `13` adds `document_private_crdts`, a reader's device-local private view
/// state per document (toggle folds), which cascades with `loro_documents`.
pub const WORKSPACE_SCHEMA_VERSION: i64 = 13;

/// The manifest `LoroDoc`'s shape, stored as `meta.schema_version`.
///
/// `2` replaces the single LWW trash flag with observed-remove deletion
/// tombstones. The old display fields remain as compatibility hints, but a v2
/// reader decides visibility from the tombstone set.
pub const CRDT_SCHEMA_VERSION: i64 = 2;

/// Schema understood by the profile metadata CRDT. Kept separate from the
/// workspace manifest because the two documents evolve independently. `2`
/// adds optional `icon` and `color` metadata for profile switchers. `3` adds
/// `kind`, so the holder is a holder on every device that syncs it rather than
/// a persona wearing a name that means nothing to a machine.
pub const PROFILE_CRDT_SCHEMA_VERSION: i64 = 3;

/// Independent fresh Profile identity, personal and private unit formats (D170).
pub const PROFILE_IDENTITY_CRDT_SCHEMA_VERSION: i64 = 1;
pub const PERSONAL_CRDT_SCHEMA_VERSION: i64 = 1;
pub const PROFILE_PRIVATE_CRDT_SCHEMA_VERSION: i64 = 1;
/// A reader's device-local private view state for one document
/// (`document_private_crdts`), stored as its own Loro snapshot.
pub const DOCUMENT_PRIVATE_CRDT_SCHEMA_VERSION: i64 = 1;

/// The `.koru` bundle, stored as `format_version` in its `manifest.json`.
pub const BUNDLE_FORMAT_VERSION: u32 = 2;

/// What this build can name about itself, for `GET /v1/status`.
///
/// A capability is a coarse feature flag, not a version — a client asks "can
/// this store do X" rather than deducing it from a number. Adding a capability
/// is additive and needs no version bump; removing one does.
pub const CAPABILITIES: &[&str] = &[
    "loro-protocol",
    "workspace-export",
    "workspace-import",
    "workspace-trash",
    "workspace-order",
    "blobs-blake3",
    "backup-restore",
    "document-history-v1",
    "document-create-trashed-at",
    "seeded-document-create-v1",
    "profile-editing",
    "profile-units-v1",
    // **`search`, not `search-fts5`.** The document is tested never to name
    // SQLite, and FTS5 is SQLite's — but the real reason is that the token
    // would buy a client nothing. Query text is normalised into terms before it
    // reaches `MATCH`, so no operator, quotation mark or column filter a caller
    // types is ever interpreted: there is no query syntax to negotiate.
    "search",
    "reminders",
];

/// Whether this build may *write* a document it has just opened.
///
/// Deliberately the same shape `koru-core` proves out: an enum rather than a
/// bool, so the reason travels with the answer and a caller cannot drop it by
/// accident.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Compatibility {
    Writable,
    /// Written by a newer build. Read-only, and the caller must say so.
    TooNew {
        found: i64,
        understood: i64,
    },
}

impl Compatibility {
    /// Takes `understood` rather than reading a constant, because there are
    /// four things to compare and each has its own constant.
    pub fn against(found: Option<i64>, understood: i64) -> Self {
        match found {
            Some(v) if v > understood => Self::TooNew {
                found: v,
                understood,
            },
            _ => Self::Writable,
        }
    }

    /// The manifest's CRDT schema version against this build's.
    ///
    /// `None` means a document written before the key existed, which is
    /// writable — the absence of a version is not a claim to a newer one.
    pub fn of_crdt(found: Option<i64>) -> Self {
        Self::against(found, CRDT_SCHEMA_VERSION)
    }

    pub fn writable(self) -> bool {
        matches!(self, Self::Writable)
    }

    /// The stable error code a refused write reports. `None` when writable.
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::Writable => None,
            Self::TooNew { .. } => Some("schema_too_new"),
        }
    }
}

impl fmt::Display for Compatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Writable => write!(f, "writable"),
            Self::TooNew { found, understood } => write!(
                f,
                "written by a newer version of Koru (schema {found}; this build understands {understood}), so it is open for reading only"
            ),
        }
    }
}

#[cfg(test)]
mod tests;
