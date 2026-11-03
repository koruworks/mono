// SPDX-License-Identifier: MPL-2.0

//! **What crosses the Koru Store's HTTP boundary, defined once.**
//!
//! Every request body and every response body lives here, and the server and
//! the client both name these types. That is the whole purpose: a field renamed
//! on one side stops compiling on the other.
//!
//! ## What this replaced, and why it was worth a crate
//!
//! `koru-store-client` used to build every request body as a
//! `serde_json::json!` literal against a type declared in `koru-store-api` and
//! never named — two of which (`RenameWorkspace`, `WorkspaceAppearance`) were
//! not even `pub`, so the client *could not* have named them. Renaming
//! `NewProfile::purpose` to `intent` compiled clean on both sides: the client
//! went on sending `"purpose"`, `#[serde(default)]` filled `intent` with
//! `None`, and a profile was recorded with no purpose. Nothing failed.
//!
//! In TypeScript that gap is why `zod` exists — the interface is erased, so the
//! shape has to be re-materialized to be checked. Rust does not erase it:
//! `Json<NewProfile>` *is* the parse, from the same declaration the compiler
//! checked, and a handler returning `Json<Profile>` cannot return anything
//! else. What was missing was not validation but a **single declaration**, and
//! that is what this crate is.
//!
//! ## What it deliberately is not
//!
//! **Not `koru-contract`.** That is the protobuf domain-projection and command
//! contract, generated into Swift, Kotlin and Rust for a mobile shell. This is
//! the Rust-side HTTP wire, in serde. Whether the two should become one — and
//! what it costs, because proto3 JSON is a different dialect from this one — is
//! item 6 of [TECHDEBT_API_DOC](../../../docs/contributing/techdebt/API_DOC.md).
//!
//! **Not a validator.** Constraints are expressed by *parsing*: a
//! [`WorkspaceId`](koru_store_core::ids::WorkspaceId) deserializes through
//! `parse` and fails on anything malformed, so an invalid id cannot exist as a
//! value and no handler has to remember to check one.
//!
//! ## Two serde defaults that decide how loudly drift arrives
//!
//! **Unknown fields are ignored**, and that is left alone on purpose. It is
//! what lets a newer store add a field without breaking an older client, and it
//! is why the internal reports in [`internal`] can be typed at all. Reaching
//! for `#[serde(deny_unknown_fields)]` because it sounds stricter would make
//! every added field a breaking change.
//!
//! **A missing field is an error unless it says otherwise.** `#[serde(default)]`
//! belongs only where absence is *meaningful* — an absent `locale` means
//! undetermined, which is not the same as English — and never as a way to make
//! a required field quiet.

pub mod attributes;
pub mod bundle;
pub mod internal;
pub mod profile_lifecycle;
pub mod search;
pub mod tasks;

pub use profile_lifecycle::*;

use koru_store_core::ids::{DocumentId, DocumentVersionId, HistoryEntryId, ProfileId, WorkspaceId};
use koru_store_core::model::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod reminders;

// ------------------------------------------------------------------ profiles

/// `POST /v1/profiles`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewProfile {
    pub name: String,
    /// BCP 47. Absent means undetermined — **not** English: the absence of an
    /// answer is not an answer.
    ///
    /// It is the **person's** locale, not the language a client happens to be
    /// drawing in: a client with no Korean strings showing English to a `ko-KR`
    /// person still sends `ko-KR`, or the profile records something untrue and
    /// stays wrong on the day Korean arrives.
    #[serde(default)]
    pub locale: Option<String>,
    /// Free text — "work", "school". Deliberately not an enum: it is theirs.
    #[serde(default)]
    pub purpose: Option<String>,
    /// A token, `<kind>:<value>` — `color:purple` today, `blob:<hash>` when a
    /// person can upload a picture. Passed through unread.
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// `PATCH /v1/profiles/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameProfile {
    pub name: String,
}

/// `PATCH /v1/profiles/{id}/appearance`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProfileAppearance {
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// `DELETE /v1/profiles/{id}/local`.
///
/// Both outcomes mean the catalog marker is durable: retries do not turn an
/// acknowledged write into a not-found error. Settings cleanup is outside this
/// Store response, while clients can still distinguish the first marker write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocalProfileRemoval {
    Removed,
    AlreadyRemoved,
}

/// `PUT /v1/profiles/{id}/local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocalProfileRestoration {
    Restored,
    AlreadyActive,
}

/// Atomic compare-and-set request for the existing Profile `account` field.
/// Both values are canonical Koru Identity UUIDs without the storage prefix;
/// emails, names, and provider subjects have no representation here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BindProfileAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_identity_id: Option<String>,
    pub new_identity_id: String,
}

/// Trusted enrollment adapter's initial local Profile projection. Existing
/// Profile metadata is never replaced by these hosted discovery snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvisionAccountProfile {
    pub identity_id: String,
    pub name: String,
    pub locale: Option<String>,
    pub created_at_millis: i64,
}

/// Compare-and-clear request used when a hosted-deleted Profile is explicitly
/// kept as an offline-only local Profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClearProfileAccount {
    pub expected_identity_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAccountBindingOutcome {
    Bound,
    AlreadyBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAccountClearOutcome {
    Cleared,
    AlreadyOffline,
}

// ------------------------------------------------------- personal attributes

// The typed value is declared once, in `koru-store-core`, exactly as the ids
// are — the server validates and stores the same shape this client sends.
pub use koru_store_core::personal_attributes::PersonalAttributeValue;

/// `GET /v1/profiles/{id}/personal-attributes`.
///
/// `entity` is the stable entity reference the attributes attach to:
/// `<kind>:<workspace>:<document>:<node>`, a lowercase ASCII token and three
/// UUIDs. It travels as a query parameter because it holds only ids — nothing
/// a person wrote ever appears in a URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PersonalAttributesQuery {
    #[serde(default)]
    pub entity: Option<String>,
}

/// What both personal-attribute routes answer: the entity in its canonical
/// spelling, and every attribute it carries for the requesting profile. An
/// unset attribute is an absent key, never a null.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PersonalAttributesResponse {
    pub entity: String,
    pub attributes: std::collections::BTreeMap<String, PersonalAttributeValue>,
}

/// `PUT /v1/profiles/{id}/personal-attributes`.
///
/// One write: set `key` on `entity` to `value`, or clear it when `value` is
/// null or absent. A cleared attribute is deleted, so reading it back finds
/// no key at all.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetPersonalAttribute {
    pub entity: String,
    pub key: String,
    #[serde(default)]
    pub value: Option<PersonalAttributeValue>,
}

// ------------------------------------------------- document-private state

// Declared once, in `koru-store-core`, like the personal value above.
pub use koru_store_core::document_private::{PrivateChange, PrivateState};

/// `PATCH /v1/profiles/{id}/documents/{document}/private-state`.
///
/// A bounded batch of explicit values for one reader's private view of one
/// document. Each change names the value it wants — never "invert" — so a
/// retried request is a no-op. All or nothing: one invalid change rejects the
/// batch and applies none of it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyPrivateChanges {
    pub changes: Vec<PrivateChange>,
}

/// `GET /v1/profiles/{id}/dictionary`, and every write's echo.
///
/// The personal spelling dictionary: language tag → sorted words. Personal
/// state — it replicates between this person's devices and nobody else's.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PersonalDictionaryResponse {
    pub words: std::collections::BTreeMap<String, Vec<String>>,
}

/// `PUT /v1/profiles/{id}/dictionary`.
///
/// One write: add `word` to `language`'s set when `present`, remove it
/// otherwise. A removed word is a deleted key, so reading back finds no word
/// at all.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetDictionaryWord {
    pub language: String,
    pub word: String,
    pub present: bool,
}

// ---------------------------------------------------------------- workspaces

/// `POST /v1/workspaces`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkspace {
    pub owner: ProfileId,
    pub name: String,
}

/// `PATCH /v1/workspaces/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameWorkspace {
    pub name: String,
}

/// `POST /v1/workspaces/{id}/move`.
///
/// **The destination only.** The profile the workspace is leaving is not sent:
/// the Store already knows it, and a caller that had to name it could name the
/// wrong one — which would either be ignored, making the field a lie, or
/// refused, making a move fail because a listing was a moment stale. The field
/// is spelled `owner` because that is what a workspace's profile is called
/// everywhere else on this wire, [`NewWorkspace`] included.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveWorkspace {
    pub owner: ProfileId,
}

/// `PATCH /v1/workspaces/{id}/appearance`.
///
/// **All four, every time.** They are edited together and the caller knows all
/// of them, so sending them whole sidesteps the *absent versus null* question a
/// partial update has to answer in JSON — and which nobody answers the same way
/// twice.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceAppearance {
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
}

/// `GET /v1/workspaces`. Deleted rows are opt-in so an older client cannot
/// accidentally present or open them as ordinary workspaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceListQuery {
    #[serde(default)]
    pub include_deleted: bool,
}

/// `GET /v1/profiles/{id}/workspace-usage`.
///
/// Live file storage is one number on purpose. A caller needs to answer how
/// much content the visible file set represents, not expose retained recovery
/// data or the persistence engine's page allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceUsage {
    pub workspace: WorkspaceId,
    /// Every user-visible manifest entry, across every application kind.
    pub entries: u64,
    /// Counts by application-defined kind token. The Store does not decide
    /// whether `page`, `file`, or a future token is called a document in a UI.
    pub kinds: std::collections::BTreeMap<String, u64>,
    /// Deduplicated bytes referenced by live first-class file entries,
    /// excluding embedded note media, Trash, retained recovery data, database
    /// allocation and backups.
    pub stored_bytes: u64,
}

/// `GET /v1/documents/{id}/location`.
///
/// A document id is globally unique, but opening it still needs the profile and
/// workspace which own it. Legacy Koru pills predate routed references and
/// carry only this id; this response lets a client upgrade that old reference
/// at the boundary where the authoritative catalog lives.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DocumentLocation {
    pub profile: ProfileId,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

/// `GET /v1/documents/{id}/outline`.
///
/// The headings of one document, for a picker offering jump targets. Answered
/// from the derived read model rather than the document itself, because a
/// picker must not pay a CRDT decode per keystroke.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DocumentOutline {
    pub document: DocumentId,
    /// In reading order.
    pub headings: Vec<OutlineHeading>,
    /// Whether the read model has seen this document at all.
    ///
    /// **An empty list means two different things and a caller must be able to
    /// tell them apart.** The projector is debounced, so a heading typed a
    /// moment ago is not here yet; `projected: false` is "still indexing" and
    /// `projected: true` with no headings is "this document has none".
    pub projected: bool,
}

/// One heading inside a document.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlineHeading {
    /// Meaningful only inside the document that minted it, which is why this
    /// response names that document.
    pub node: String,
    /// 1–6.
    pub level: u8,
    pub text: String,
}

/// `PUT /v1/profiles/{owner}/workspace-order`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceOrder {
    /// The complete ordered set, including archived and softly deleted rows.
    pub workspaces: Vec<WorkspaceId>,
}

/// `DELETE /v1/workspaces/{id}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DeleteWorkspaceQuery {
    #[serde(default)]
    pub force: bool,
}

// ----------------------------------------------------------------- documents

/// `POST /v1/workspaces/{id}/documents`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewDocument {
    /// Optional client-minted identity. Importers use this so snapshots can
    /// contain routed references to other entries in the same batch.
    #[serde(default)]
    pub id: Option<DocumentId>,
    pub title: String,
    /// The application's own kind — `page`, `task-list`. A stable token, and
    /// the store never interprets it.
    pub kind: String,
    /// MIME type for file entries. Optional for ordinary documents and older
    /// clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Absent means the workspace root.
    #[serde(default)]
    pub parent: Option<DocumentId>,
    /// Where among `parent`'s children it goes. **Absent means the end**, which
    /// is what every caller with no opinion sends and what an older client's
    /// body decodes as.
    ///
    /// The same coordinate `MoveDocument` uses — an index among siblings,
    /// counting the trashed ones, because that is what the manifest tree holds.
    /// Past the end is clamped rather than refused: a new page below the last
    /// one is an ordinary request, not an arithmetic mistake.
    #[serde(default)]
    pub index: Option<usize>,
    /// Create the manifest row already in Trash at this source timestamp.
    /// Absent preserves the pre-capability behavior for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<Timestamp>,
    /// Optional complete initial state. Used by importers so registering the
    /// manifest row and committing content is one Store operation rather than
    /// a briefly visible empty page followed by a client update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Vec<u8>>,
}

/// Internal content-first document creation. This lives in the body so a
/// title never enters an access-log URL; the opaque update is a byte array
/// because this route is private to Koru adapters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeedDocumentRequest {
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub parent: Option<DocumentId>,
    #[serde(default)]
    pub index: Option<usize>,
    pub idempotency_key: String,
    /// Hash of the canonical semantic request. The Store compares it but never
    /// interprets the request whose identity it represents.
    pub request_hash: String,
    pub initial_update: Vec<u8>,
}

/// Versioned internal content-first creation with a caller-minted identity.
/// An older Store has no v2 route and must refuse before mutation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeedDocumentWithCandidateRequest {
    pub title: String,
    pub kind: String,
    pub candidate: DocumentId,
    #[serde(default)]
    pub parent: Option<DocumentId>,
    #[serde(default)]
    pub index: Option<usize>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub initial_update: Vec<u8>,
}

/// What a create answers with.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Created {
    pub id: DocumentId,
}

/// What `POST /v1/workspaces/{id}/documents/{document}/note` answers with.
///
/// [`Created`] would lose the one fact the caller cannot otherwise learn:
/// whether this call made the note or the file already had one. The store
/// reports it — `created` is its answer, not an inference from the delta —
/// so the answer carries it through unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnsuredFileNote {
    /// The note's document id — the existing one when the file already had a
    /// note.
    pub id: DocumentId,
    /// Whether this call created the note.
    pub created: bool,
}

/// `POST /v1/workspaces/{id}/transfer` — move a document (and its explore
/// subtree) into another workspace in the same store, preserving document ids.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransferDocuments {
    pub target: WorkspaceId,
    pub root: DocumentId,
    #[serde(default)]
    pub parent: Option<DocumentId>,
    /// Entry index among siblings under `parent`. Past the end means append.
    #[serde(default)]
    pub index: usize,
}

/// What a transfer answers with: every document id that moved, root first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Transferred {
    pub ids: Vec<DocumentId>,
}

/// `POST /v1/workspaces/{id}/blobs/{hash}` — associate another workspace owned
/// by the same Profile with one content-addressed blob.
///
/// A selection moved across workspaces carries blob references in its CRDT
/// cutting, and the target must be associated before those references are
/// published or it holds a reference it may not read.
///
/// **Named for the association rather than a copy**, because no bytes move:
/// one stored object serves every workspace associated with it, and the
/// association is the permission. The old name described a layout that filed
/// one picture once per workspace, and a caller that still believes it would
/// duplicate a picture library across workspaces.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssociateBlob {
    pub target: WorkspaceId,
}

/// `GET /v1/workspaces/{id}/manifest` — the document a client joins to see what
/// a workspace contains.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ManifestRef {
    pub document: DocumentId,
}

// ------------------------------------------------------------------- history

/// `POST /v1/workspaces/{workspace}/documents/{document}/history`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CaptureHistory {
    /// One of the `HistoryReason` tokens. Public captures normally use
    /// `manual`; MCP and restore use their paired boundary reasons.
    pub reason: String,
    #[serde(default)]
    pub frontiers: Option<Vec<u8>>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub boundary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryEntry {
    pub id: HistoryEntryId,
    pub document: DocumentId,
    pub version: DocumentVersionId,
    pub reason: String,
    pub recorded_at: i64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub boundary: Option<String>,
    pub pinned: bool,
}

/// **Hold this workspace's pictures, or let them go again** (D256).
///
/// Device-local and this process only. Documents are untouched: what stops a
/// document is the transport mode, which is a durable choice and a different
/// operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PauseBlobTransfer {
    pub paused: bool,
}

/// `GET /v1/workspaces/{id}/cloud-sync/waiting` — **which documents are
/// waiting on a file Koru Cloud does not hold yet**, and for whom.
///
/// The answer to *"Waiting for the device that has these files" — which
/// files, and which device?*, grouped by document because a document is what
/// a person recognises: a page naming six missing pictures is one entry
/// saying six.
///
/// **Here rather than in [`internal`], on purpose.** Those reports promise
/// never to carry a title, because they are the ones pasted into bug reports.
/// This one exists to show a person their own document names on their own
/// device, so it carries titles and says so — and it still carries no
/// content, no filename and no blob address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudWaitingDocuments {
    pub workspace: Option<WorkspaceId>,
    /// Most files first, then by title. Empty when nothing in the workspace
    /// is waiting on another device — which includes a workspace held for
    /// another reason, such as a metered connection.
    pub documents: Vec<CloudWaitingDocument>,
}

/// One document in [`CloudWaitingDocuments`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CloudWaitingDocument {
    pub document: DocumentId,
    /// The manifest's title. Empty when the manifest has none for this id,
    /// which a surface says in its own word for *untitled*.
    pub title: String,
    /// Distinct files this document references that Cloud does not hold.
    pub missing_files: u32,
    /// Display names of the devices known to have held those files, other
    /// than this one. **Empty is a real answer**: no device this one knows
    /// of is recorded as holding them, and a surface must not guess one.
    #[serde(default)]
    pub holders: Vec<String>,
}

/// **What a person answered when they were asked** (D258).
///
/// Turning a Profile's Cloud sync on asks whether its Workspaces should move
/// too. The Profile moves either way; `include_workspaces` is the answer.
///
/// **Device-local.** D230: no hosted mode row, policy CRDT, peer message or
/// remote stop notice follows from this. A second device is told nothing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetProfileTransportMode {
    /// `local_only`, `p2p` or `cloud`. The stored spelling, because an
    /// unknown token must be refused rather than read as a default.
    pub mode: String,
    /// Whether the Workspaces under this Profile move with it. A Workspace
    /// somebody already chose explicitly is never in the group either way.
    #[serde(default)]
    pub include_workspaces: bool,
}

/// What that answer did.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProfileTransportModeOutcome {
    /// What the Profile row reads now — the stored spelling.
    pub mode: String,
    /// Whether the Profile row was written. `false` with a `mode` that
    /// matches is the ordinary "answered the same thing twice".
    pub profile_changed: bool,
    /// Whether the person's own explicit choice is what the row now carries.
    pub explicit_choice: bool,
    pub workspaces_submitted: u32,
    pub workspaces_moved: u32,
    /// Workspaces held out because somebody had already chosen for them.
    /// **Not a failure**: their choice is untouched, and that is the point.
    pub workspaces_left_alone: u32,
}

/// Rename a history marker. `label: None` clears the name; the marker then
/// falls back to its reason in every listing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RelabelHistory {
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct HistoryQuery {
    #[serde(default)]
    pub before_at: Option<i64>,
    #[serde(default)]
    pub before_id: Option<HistoryEntryId>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    #[serde(default)]
    pub next_before_at: Option<i64>,
    #[serde(default)]
    pub next_before_id: Option<HistoryEntryId>,
    /// Whether the document has changed since its newest marker — **the
    /// store's own comparison**, not something a client derives.
    ///
    /// A property of the *document*, so it is on the page rather than on each
    /// entry: "changed since the newest marker" is one question however many
    /// rows the page carries.
    ///
    /// **`Option`, not `bool`, and that is the whole compatibility story.** A
    /// bare `bool` with `#[serde(default)]` reads `false` — *not stale* — from
    /// every store built before this field existed, and a client that believed
    /// it would offer to rename a marker over a document that has moved on.
    /// `None` means *this store does not say*, and a caller must then fall back
    /// to whatever it did before it could ask.
    ///
    /// `Some(true)` when the document has no markers at all: there is nothing
    /// to be unchanged relative to.
    #[serde(default)]
    pub stale: Option<bool>,
}

// ------------------------------------------------------------------- bundles

/// The query string of `POST /v1/import`.
///
/// **`mode` has no default, deliberately.** `restore` keeps the workspace and
/// document ids, so the result is another replica and can converge with its
/// origin; `duplicate` mints new ones, so it never will. Guessing picks one of
/// two operations whose difference is invisible until it matters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportQuery {
    /// `restore` or `duplicate`. Required.
    pub mode: String,
    /// Who owns the result.
    pub owner: ProfileId,
}

// -------------------------------------------------------------------- meta

/// `GET /v1/status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    pub api_version: u32,
    /// A coarse feature list a client negotiates against, rather than deducing
    /// what a store can do from a version number.
    pub capabilities: Vec<String>,
    /// Which store this is. Channels never share a database.
    pub channel: String,
    /// Epoch milliseconds. A number, so no calendar or locale is baked in.
    pub started_at: i64,
}

/// `GET /v1/internal/welcome-progress` and `PUT /v1/internal/welcome-progress/{row}`.
///
/// The Welcome page's ticked rows on **this installation** — shared by every
/// Profile here and never replicated. `done` names only rows that are ticked,
/// in the order they were asked about; a row not listed is simply not done.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WelcomeProgress {
    pub done: Vec<String>,
}

/// `POST /v1/shutdown`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Stopping {
    pub stopping: bool,
}

/// `POST /v1/backup`.
///
/// A destination rather than bytes: a whole Store can be large, and the
/// daemon is the process that can take a consistent snapshot while it holds
/// the live Store lock. The route is authenticated and loopback-only.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoreBackup {
    pub destination: String,
}

/// A completed `POST /v1/backup`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoreBackupResult {
    pub destination: String,
}

/// The body of every failed request.
///
/// `code` is a stable machine-readable token a client branches on. `message` is
/// **untranslated developer text** that a client must never show to a person —
/// it is for logs and bug reports. A client renders its own words, in its own
/// language, from the code.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Failure {
    /// A stable token to branch on.
    pub code: String,
    /// Untranslated developer text; **never shown** to a person. Render your
    /// own words, in your own language, from the code.
    pub message: String,
    /// Display names of the devices a parked blob hydration is waiting on.
    /// Display data for the client's own sentence — never part of `code`, and
    /// absent on every failure that is not a parked hydration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
    /// **Why blob transfer is holding**, when that is what refused this — a
    /// `TransferHold` code from the blob engine's registry
    /// (`mode_refuses_transport`, `network_metered`, `network_low_data`,
    /// `network_offline`, `paused_by_person`, `cloud_access_unavailable`).
    ///
    /// Structured beside `code` rather than inside it, for the same reason
    /// `waiting_on` is: these are the *blob plane's* vocabulary and `code` is
    /// the Store's, and a client that had to recover one by parsing the other
    /// would be branching on developer text. Without it, every hold that is
    /// not one of the Store's three blob codes arrives as `storage_failed`
    /// and the placeholder has nothing true to say — which is D259's second
    /// defect exactly: a person told to wake another device for a Workspace
    /// this one was never switched on for.
    ///
    /// Absent on every failure that is not a blob transfer hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
}

/// A workspace id, for the one route that answers with nothing else.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRef {
    pub workspace: WorkspaceId,
}

#[cfg(test)]
mod tests;
