// SPDX-License-Identifier: MPL-2.0

//! **One table of routes**, and the OpenAPI document is built from it — so a
//! route cannot be *described* without existing.
//!
//! This is the same idiom as `koru-workspace-host`'s `cli.rs` `FLAGS` table, for the
//! same reason: a specification kept beside an implementation drifts, and the
//! drift is invisible until an integrator hits it.
//!
//! ## Why a route cannot exist without a row
//!
//! [`router`] is **folded from [`ROUTES`]**, so this table is the only list of
//! paths in the crate. A route that is not a row is not served, and a row whose
//! `handler` is missing does not compile. Drift is not something a test hunts
//! for; it is a state that cannot be expressed.
//!
//! It was not always so, and what happened without it is the argument for it.
//! The router used to be seven hand-written `Router::new().route(..)` chains,
//! and only one direction was ever checked:
//! `apps/koru-store-server/tests/api.rs`'s `every_route_in_the_table_answers` proves
//! **table → router** and has never failed. **Router → table was unchecked,
//! because axum cannot be asked what it serves** — and eleven routes went out
//! through it, among them `POST /v1/import` and
//! `POST /v1/workspaces/{id}/export`: the whole of a feature `GET /v1/status`
//! advertises as `workspace-export`, `workspace-import` and `backup-restore`. A
//! client negotiating on capabilities, exactly as we tell it to, found nothing
//! to call.
//!
//! **The gate probe stays**, because the fold does not check everything. A
//! row's `method` is documentation while its `handler` is what is wired, and
//! `/v1/sync` is the one row where they differ on purpose — it is registered
//! with `any` so a client that upgrades with something other than `GET` is
//! answered rather than 405'd. Only a real request can catch the two drifting
//! apart.
//!
//! The remaining gaps — a public document separate from an internal one, and
//! getting the WebSocket described at all — are in
//! [`docs/contributing/techdebt/API_DOC.md`](../../../docs/contributing/techdebt/API_DOC.md).
//!
//! ## Why not `utoipa`
//!
//! The plan named `utoipa`, generating the document from annotated handlers.
//! It was dropped after trying it, and the reason is worth recording: the
//! derive needs `ToSchema` on every type it describes, and those types live in
//! `koru-store-core` — the crate that must stay free of HTTP so a mobile build
//! can embed it. Pulling an OpenAPI dependency into core to document an HTTP
//! API would invert the boundary this whole crate exists to hold.
//!
//! The table gives what actually drifts — paths, methods, scopes — with no
//! dependency and no attribute noise. Response *shapes* are described here in
//! prose rather than generated; that is the part this trades away, and it is
//! the part that changes least.

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{MethodRouter, any, delete, get, patch, post, put};
use koru_store_core::model::{Blob, DocumentEntry, Profile, Scope, Workspace};
use koru_store_core::reminders::{ReminderSettings, StoredReminder};
use koru_store_wire::attributes::{AttributeQuery, AttributeQueryResponse};
use koru_store_wire::internal::{
    CloudSyncStatus, DocumentReport, Metrics, StoreReport, WorkspaceReport,
};
use koru_store_wire::reminders::{
    ActionRequest, ClearedHistory, ReminderQuery, ReminderQueryResponse, ReminderSnapshot,
    RevisionRequest, RuleRequest, SettingsPatch,
};
use koru_store_wire::search::{Navigation, SearchRequest, SearchResponse};
use koru_store_wire::tasks::{TaskQuery, TaskQueryResponse};
use koru_store_wire::{
    ApplyPrivateChanges, AssociateBlob, BindProfileAccount, CaptureHistory, ClearProfileAccount,
    Created, DocumentLocation, DocumentOutline, EnsuredFileNote, HistoryEntry, HistoryPage,
    LocalProfileRemoval, LocalProfileRestoration, ManifestRef, MoveWorkspace, NewDocument,
    NewProfile, NewWorkspace, PauseBlobTransfer, PersonalAttributesResponse,
    PersonalDictionaryResponse, PrivateState, ProfileAccountBindingOutcome,
    ProfileAccountClearOutcome, ProfileAppearance, ProfileLifecycleInventory,
    ProfileTransportModeOutcome, RelabelHistory, RenameProfile, RenameWorkspace,
    SeedDocumentRequest, SeedDocumentWithCandidateRequest, SetDictionaryWord, SetPersonalAttribute,
    SetProfileTransportMode, Status, Stopping, StoreBackup, StoreBackupResult, TransferDocuments,
    Transferred, WelcomeProgress, WorkspaceAppearance, WorkspaceOrder, WorkspaceUsage,
};
use schemars::{Schema, SchemaGenerator};

use crate::routes;
use crate::state::AppState;

/// Imported document snapshots are JSON byte arrays, so their HTTP body is
/// several times larger than the opaque Loro bytes they carry. Keep the route
/// bounded while leaving ample room for a genuinely large imported page.
const DOCUMENT_IMPORT_BODY_LIMIT: usize = 512 * 1024 * 1024;
/// File and OneNote importers accept an individual file up to 2 GiB. The blob
/// route carries those bytes directly rather than inflating them through JSON.
const BLOB_UPLOAD_BODY_LIMIT: usize = koru_store_core::ports::BLOB_UPLOAD_BODY_LIMIT as usize;

/// How a row names the shape it carries.
///
/// A function rather than a type parameter, for the same reason
/// [`Route::handler`] is: a row is a `const` literal, and `subschema_for` is a
/// generic call that cannot run in one. Calling it **registers the type's
/// definition with the generator and returns a `$ref` to it**, which is what
/// keeps one schema per type however many routes carry it.
pub type SchemaFor = fn(&mut SchemaGenerator) -> Schema;

/// Who a route is for.
///
/// **Two audiences, because there are two kinds of route.** The ones an
/// application calls to function are a contract: a customer builds against
/// them and we do not get to move them quietly. The `/v1/debug/*` counters and
/// `/v1/metrics` are ours — they change whenever something new is worth seeing,
/// and publishing them invites a dependency nobody offered.
///
/// This decides what appears in which document. It is **not** an access
/// control: an internal route needs the same token and the same scope as any
/// other, and hiding it from a document does not hide it from the port.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Audience {
    /// In `GET /v1/openapi.json`. A promise.
    Public,
    /// In `GET /v1/debug/openapi.json` only. No compatibility promise.
    Internal,
}

/// One endpoint.
pub struct Route {
    pub audience: Audience,
    /// **Documentation, not wiring.** [`Self::handler`] decides what is
    /// actually served; this is what the document says about it. They agree on
    /// every row but `/v1/sync`, deliberately — see the module note.
    pub method: &'static str,
    /// As axum spells it, with `{}` placeholders.
    pub path: &'static str,
    /// What a caller must hold. `None` means "any authenticated caller" —
    /// which is never no authentication at all.
    pub scope: Option<Scope>,
    pub summary: &'static str,
    /// What comes back, in a sentence, for a person reading the document.
    ///
    /// **Kept beside the generated schema rather than replaced by it.** A
    /// schema says what the fields are; this says why you would call the route
    /// and what the answer means. They are different questions and a reader
    /// needs both.
    pub returns: &'static str,
    /// What answers it.
    ///
    /// A function rather than a value because a `MethodRouter` is built by a
    /// generic call that cannot run in a `const`. A non-capturing closure
    /// coerces to this `fn` pointer, so a row stays a literal.
    pub handler: fn() -> MethodRouter<Arc<AppState>>,
    /// The body this accepts. `None` for a route with no body, and for the two
    /// that take raw bytes — a blob and a `.koru` bundle cross as
    /// `application/octet-stream`, which has no schema to give.
    pub request: Option<SchemaFor>,
    /// The body this answers with, under the same rule. Also `None` for
    /// `/v1/sync`, which answers with a protocol rather than a body, and for
    /// the two diagnostic routes whose payload is whatever the daemon last
    /// published.
    pub response: Option<SchemaFor>,
}

/// A public query parameter tied back to one catalog route. Query parameters
/// are catalog data just like JSON bodies; keeping them here prevents a
/// required extractor field from disappearing from generated OpenAPI.
pub struct QueryParameter {
    pub method: &'static str,
    pub path: &'static str,
    pub name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

pub const QUERY_PARAMETERS: &[QueryParameter] = &[
    QueryParameter {
        method: "GET",
        path: "/v1/profiles/{id}/personal-attributes",
        name: "entity",
        required: true,
        description: "Canonical <kind>:<workspace>:<document>:<node> entity reference",
    },
    QueryParameter {
        method: "GET",
        path: "/v1/profiles/{id}/reminders",
        name: "entity",
        required: true,
        description: "Canonical <kind>:<workspace>:<document>:<node> entity reference",
    },
    QueryParameter {
        method: "DELETE",
        path: "/v1/profiles/{id}/reminder-history",
        name: "entity",
        required: false,
        description: "Optional canonical entity reference; omission clears the profile",
    },
];

/// Every route the store serves.
///
/// **Adding a route means adding a row here**, and the tests will say so: one
/// asserts the router and this table agree, another that the OpenAPI document
/// covers every row.
pub const ROUTES: &[Route] = &[
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/status",
        handler: || get(routes::status::status),
        request: None,
        response: Some(|g| g.subschema_for::<Status>()),
        scope: None,
        summary: "What this store is and what it can do",
        returns: "api_version, capabilities, channel, started_at",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/openapi.json",
        handler: || get(routes::meta::openapi),
        request: None,
        response: None,
        scope: None,
        summary: "This document",
        returns: "an OpenAPI 3.1 document",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/shutdown",
        handler: || post(routes::admin::shutdown),
        request: None,
        response: Some(|g| g.subschema_for::<Stopping>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Ask the store to stop; ?only_if_idle=true stops only when nothing is connected",
        returns: "stopping: whether it is going down",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/backup",
        handler: || post(routes::admin::backup),
        request: Some(|g| g.subschema_for::<StoreBackup>()),
        response: Some(|g| g.subschema_for::<StoreBackupResult>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Write a consistent whole-Store backup",
        returns: "the destination after the backup is complete",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles",
        handler: || get(routes::workspaces::list_profiles),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<Profile>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Every profile",
        returns: "an array of profiles",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/internal/profiles/lifecycle-inventory",
        handler: || get(routes::workspaces::profile_lifecycle_inventory),
        request: None,
        response: Some(|g| g.subschema_for::<ProfileLifecycleInventory>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "List exact account-bound local Profile presence for lifecycle reconciliation",
        returns: "active and locally removed Profile ids, bindings, and removal timestamps; no names or content",
    },
    Route {
        method: "PUT",
        audience: Audience::Internal,
        path: "/v1/internal/profiles/{id}/account-provisioning",
        handler: || put(routes::workspaces::provision_account_profile),
        request: Some(|g| g.subschema_for::<koru_store_wire::ProvisionAccountProfile>()),
        response: Some(|g| g.subschema_for::<Profile>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Materialize an authorized enrolled Profile before content arrives",
        returns: "the durable Profile; existing metadata is preserved and conflicting bindings or removed Profiles are refused",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/internal/welcome-progress",
        handler: || get(routes::welcome::progress),
        request: None,
        response: Some(|g| g.subschema_for::<WelcomeProgress>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Which Welcome page rows are ticked on this installation; ?rows=a,b,c names at most 32",
        returns: "done: the asked-about rows that are ticked, in the order asked; shared by every Profile here and never replicated",
    },
    Route {
        method: "PUT",
        audience: Audience::Internal,
        path: "/v1/internal/welcome-progress/{row}",
        handler: || put(routes::welcome::mark_done),
        request: None,
        response: Some(|g| g.subschema_for::<WelcomeProgress>()),
        scope: Some(Scope::EditDocuments),
        summary: "Tick one Welcome page row for every Profile on this installation",
        returns: "done: the row just ticked; a row id is 1 to 40 characters of a-z, 0-9 and -",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles",
        handler: || post(routes::workspaces::create_profile),
        request: Some(|g| g.subschema_for::<NewProfile>()),
        response: Some(|g| g.subschema_for::<Profile>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Create a profile. Requires no account and no network.",
        returns: "the profile, with its new id",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/account",
        handler: || put(routes::workspaces::bind_profile_account),
        request: Some(|g| g.subschema_for::<BindProfileAccount>()),
        response: Some(|g| g.subschema_for::<ProfileAccountBindingOutcome>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Atomically bind a Profile to an exact hosted Koru Identity",
        returns: "bound or already_bound; a compare-and-set mismatch never overwrites",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/account",
        handler: || delete(routes::workspaces::clear_profile_account),
        request: Some(|g| g.subschema_for::<ClearProfileAccount>()),
        response: Some(|g| g.subschema_for::<ProfileAccountClearOutcome>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Atomically detach a hosted-deleted Profile for offline-only use",
        returns: "cleared or already_offline; local Profile content is retained",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/local",
        handler: || delete(routes::workspaces::remove_profile_local),
        request: None,
        response: Some(|g| g.subschema_for::<LocalProfileRemoval>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Mark an empty profile removed in this installation's Store catalog",
        returns: "removed or already_removed; profile and deleted workspace data remain",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/local",
        handler: || put(routes::workspaces::restore_profile_local),
        request: None,
        response: Some(|g| g.subschema_for::<LocalProfileRestoration>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Restore a locally removed Profile from retained Store data",
        returns: "restored or already_active; workspace tombstones remain unchanged",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}",
        handler: || get(routes::workspaces::get_profile),
        request: None,
        response: Some(|g| g.subschema_for::<Profile>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One profile",
        returns: "a profile",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/profiles/{id}",
        handler: || patch(routes::workspaces::rename_profile),
        request: Some(|g| g.subschema_for::<RenameProfile>()),
        response: Some(|g| g.subschema_for::<Profile>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Rename a profile in its replicated manifest",
        returns: "the profile, renamed",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/appearance",
        handler: || patch(routes::workspaces::set_profile_appearance),
        request: Some(|g| g.subschema_for::<ProfileAppearance>()),
        response: Some(|g| g.subschema_for::<Profile>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Set a profile's switcher icon and named colour",
        returns: "the profile with both appearance fields",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/personal-attributes",
        handler: || get(routes::workspaces::personal_attributes),
        request: None,
        response: Some(|g| g.subschema_for::<PersonalAttributesResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One profile's personal attributes for one entity, named by ?entity=<kind>:<workspace>:<document>:<node>",
        returns: "the entity reference and its attributes for this profile only. An unset \
                  attribute is an absent key, never a null. The reference travels as a \
                  query parameter because it holds only ids",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/personal-attributes",
        handler: || put(routes::workspaces::set_personal_attribute),
        request: Some(|g| g.subschema_for::<SetPersonalAttribute>()),
        response: Some(|g| g.subschema_for::<PersonalAttributesResponse>()),
        scope: Some(Scope::EditDocuments),
        summary: "Set one personal attribute on one entity, or clear it with a null value",
        returns: "the entity's attributes for this profile after the write. Personal state \
                  belongs to the profile in the path and never overwrites another \
                  profile's answer to the same question",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/documents/{document}/private-state",
        handler: || get(routes::workspaces::document_private_state),
        request: None,
        response: Some(|g| g.subschema_for::<PrivateState>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One reader's device-local private view state for one document",
        returns: "the reader's private attributes by node id — today `koru.collapsed` for a \
                  closed toggle — and a revision that grows with each change. A reader \
                  who never changed anything gets an empty map at revision 0, and the \
                  read writes nothing. Never replicated or exported",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/documents/{document}/private-state",
        handler: || patch(routes::workspaces::apply_document_private_changes),
        request: Some(|g| g.subschema_for::<ApplyPrivateChanges>()),
        response: Some(|g| g.subschema_for::<PrivateState>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Apply up to 512 explicit private view-state changes for one reader and document",
        returns: "the reader's private state after the one atomic save. Each change names \
                  the value it wants, so a repeat is a no-op; one invalid change rejects \
                  the batch. Needs only read access: it changes this reader's view, never \
                  the document, and it never wakes sync",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders",
        handler: || get(routes::reminders::list_for_entity),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<StoredReminder>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Every reminder for ?entity=<canonical entity reference>",
        returns: "the entity's synchronized reminder rules and action history",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders",
        handler: || post(routes::reminders::create),
        request: Some(|g| g.subschema_for::<RuleRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Create a reminder at a client-minted id",
        returns: "the authoritative stored reminder; an identical retry is idempotent",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}",
        handler: || get(routes::reminders::get_one),
        request: None,
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Fetch one reminder",
        returns: "the synchronized rule, deletion fact and scoped actions",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
        handler: || get(routes::reminders::scoped::get_one),
        request: None,
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Fetch one reminder",
        returns: "the synchronized rule, deletion fact and scoped actions",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
        handler: || put(routes::reminders::scoped::update),
        request: Some(|g| g.subschema_for::<RuleRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Replace one whole reminder rule revision",
        returns: "the authoritative stored reminder; path and body ids must match",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
        handler: || delete(routes::reminders::scoped::delete),
        request: None,
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Monotonically delete one reminder",
        returns: "the reminder with its irreversible deletion fact set",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/pause",
        handler: || post(routes::reminders::scoped::pause),
        request: Some(|g| g.subschema_for::<RevisionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Pause one reminder as a new atomic revision",
        returns: "the authoritative paused reminder",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/resume",
        handler: || post(routes::reminders::scoped::resume),
        request: Some(|g| g.subschema_for::<RevisionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Resume one reminder as a new atomic revision",
        returns: "the authoritative enabled reminder",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/snooze",
        handler: || post(routes::reminders::scoped::snooze),
        request: Some(|g| g.subschema_for::<ActionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Append a snooze action",
        returns: "the authoritative reminder; the body action must be snoozed_until",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/handle",
        handler: || post(routes::reminders::scoped::handle),
        request: Some(|g| g.subschema_for::<ActionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Append a handled action",
        returns: "the authoritative reminder; the body action must be handled",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/dictionary",
        handler: || get(routes::workspaces::personal_dictionary),
        request: None,
        response: Some(|g| g.subschema_for::<PersonalDictionaryResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One profile's personal spelling dictionary, language tag to words",
        returns: "every language's words for this profile only. An empty dictionary is an \
                  empty map, and it replicates between this person's devices and nobody \
                  else's",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}",
        handler: || put(routes::reminders::update),
        request: Some(|g| g.subschema_for::<RuleRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Replace one whole reminder rule revision",
        returns: "the authoritative stored reminder; path and body ids must match",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}",
        handler: || delete(routes::reminders::delete),
        request: None,
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Monotonically delete one reminder",
        returns: "the reminder with its irreversible deletion fact set",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}/pause",
        handler: || post(routes::reminders::pause),
        request: Some(|g| g.subschema_for::<RevisionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Pause one reminder as a new atomic revision",
        returns: "the authoritative paused reminder",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}/resume",
        handler: || post(routes::reminders::resume),
        request: Some(|g| g.subschema_for::<RevisionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Resume one reminder as a new atomic revision",
        returns: "the authoritative enabled reminder",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}/snooze",
        handler: || post(routes::reminders::snooze),
        request: Some(|g| g.subschema_for::<ActionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Append a snooze action",
        returns: "the authoritative reminder; the body action must be snoozed_until",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/{reminder}/handle",
        handler: || post(routes::reminders::handle),
        request: Some(|g| g.subschema_for::<ActionRequest>()),
        response: Some(|g| g.subschema_for::<StoredReminder>()),
        scope: Some(Scope::EditDocuments),
        summary: "Append a handled action",
        returns: "the authoritative reminder; the body action must be handled",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminder-history",
        handler: || delete(routes::reminders::clear_history),
        request: None,
        response: Some(|g| g.subschema_for::<ClearedHistory>()),
        scope: Some(Scope::EditDocuments),
        summary: "Clear handled reminder history, optionally for one entity",
        returns: "the number of handled action ids monotonically hidden in the requested scope; active and snoozed actions remain",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminder-settings",
        handler: || get(routes::reminders::settings),
        request: None,
        response: Some(|g| g.subschema_for::<ReminderSettings>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Fetch reminder settings",
        returns: "end-of-day time and default tone",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminder-settings",
        handler: || patch(routes::reminders::update_settings),
        request: Some(|g| g.subschema_for::<SettingsPatch>()),
        response: Some(|g| g.subschema_for::<ReminderSettings>()),
        scope: Some(Scope::EditDocuments),
        summary: "Update reminder settings independently",
        returns: "the authoritative settings; absent fields are unchanged and null clears end-of-day",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/query",
        handler: || post(routes::reminders::query),
        request: Some(|g| g.subschema_for::<ReminderQuery>()),
        response: Some(|g| g.subschema_for::<ReminderQueryResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Query active, waiting, missed, upcoming, or handled reminders",
        returns: "canonical joined rows; missed/upcoming require explicit caller now and canonical IANA zone",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/reminders/reconciliation",
        handler: || get(routes::reminders::reconciliation),
        request: None,
        response: Some(|g| g.subschema_for::<ReminderSnapshot>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Fetch the Agent's batched reminder reconciliation snapshot",
        returns: "device-neutral canonical rows joined to target existence, title, due, and bin state",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/dictionary",
        handler: || put(routes::workspaces::set_dictionary_word),
        request: Some(|g| g.subschema_for::<SetDictionaryWord>()),
        response: Some(|g| g.subschema_for::<PersonalDictionaryResponse>()),
        scope: Some(Scope::EditDocuments),
        summary: "Add or remove one personal-dictionary word for one language",
        returns: "the whole dictionary after the write. A removed word is a deleted key, \
                  never a stored false",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/dictionary",
        handler: || delete(routes::workspaces::clear_personal_dictionary),
        request: None,
        response: Some(|g| g.subschema_for::<PersonalDictionaryResponse>()),
        scope: Some(Scope::EditDocuments),
        summary: "Clear one profile's personal spelling dictionary",
        returns: "the empty dictionary after every word visible to this device is deleted",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces",
        handler: || get(routes::workspaces::list_workspaces),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<Workspace>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Every workspace",
        returns: "an array of workspaces",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/internal/workspaces/catalog-watch",
        handler: || get(routes::catalog::watch),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "Subscribe to content-free workspace-catalog invalidations",
        returns: "a WebSocket carrying only a process-local generation; each frame means the workspace catalog should be listed again",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspace-usage",
        handler: || get(routes::workspaces::workspace_usage),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<WorkspaceUsage>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Live entry counts by kind and referenced blob bytes for every workspace in one profile",
        returns: "one usage row per workspace, including deduplicated live file/blob bytes",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/profiles/{id}/workspace-order",
        handler: || put(routes::workspaces::reorder_workspaces),
        request: Some(|g| g.subschema_for::<WorkspaceOrder>()),
        response: Some(|g| g.subschema_for::<Vec<Workspace>>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Replace a profile's complete workspace order atomically",
        returns: "that profile's workspaces in their saved order",
    },
    Route {
        method: "DELETE",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}",
        handler: || delete(routes::workspaces::delete_workspace),
        request: None,
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::DestroyWorkspaces),
        summary: "Soft-delete a workspace; its database and blobs remain recoverable",
        returns: "the workspace with deleted_at set. Takes destroy_workspaces, which no ordinary token holds: removing a workspace closes its terminals, and the Gateway's lifecycle operation is what does that first",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/restore",
        handler: || post(routes::workspaces::restore_workspace),
        request: None,
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::DestroyWorkspaces),
        summary: "Restore a soft-deleted workspace",
        returns: "the workspace with deleted_at cleared. The same scope the delete takes: whoever decides what the trash contains decides both ways",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/move",
        handler: || post(routes::workspaces::move_workspace),
        request: Some(|g| g.subschema_for::<MoveWorkspace>()),
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::MigrateWorkspaces),
        summary: "Give a workspace to another profile",
        returns: "the workspace with its new owner and its position at the end of that profile's order. Its id, its documents and its history are untouched. Takes migrate_workspaces, which no ordinary token holds: a move closes every terminal the workspace owns, and the Gateway's migration operation is what does that first",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces",
        handler: || post(routes::workspaces::create_workspace),
        request: Some(|g| g.subschema_for::<NewWorkspace>()),
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Create a workspace and its manifest",
        returns: "the workspace, with its new immutable id",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}",
        handler: || get(routes::workspaces::get_workspace),
        request: None,
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One workspace",
        returns: "a workspace",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}",
        handler: || patch(routes::workspaces::rename_workspace),
        request: Some(|g| g.subschema_for::<RenameWorkspace>()),
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Rename a workspace, in the catalog and in its manifest",
        returns: "the workspace, renamed",
    },
    Route {
        method: "PATCH",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/appearance",
        handler: || patch(routes::workspaces::set_appearance),
        request: Some(|g| g.subschema_for::<WorkspaceAppearance>()),
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "How a workspace appears in a switcher: icon, colour, pinned, archived",
        returns: "the workspace. All four are sent every time — there is no absent-versus-null. Changing `archived` additionally takes destroy_workspaces: archiving closes the workspace's terminals and hides its projects, and it only travels here because these four fields are edited together",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/documents",
        handler: || get(routes::workspaces::list_documents),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<DocumentEntry>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "The workspace's documents, without opening any of them",
        returns: "an array of document entries in tree order",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/documents/{id}/location",
        handler: || get(routes::workspaces::locate_document),
        request: None,
        response: Some(|g| g.subschema_for::<DocumentLocation>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Resolve a stable document id to its owning profile and workspace",
        returns: "the document's current location",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/documents/{id}/outline",
        handler: || get(routes::workspaces::document_outline),
        request: None,
        response: Some(|g| g.subschema_for::<DocumentOutline>()),
        scope: Some(Scope::ReadDocuments),
        summary: "The document's headings, from the read model, without opening it",
        returns: "headings in reading order, and whether the document is indexed yet",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/documents",
        handler: || {
            post(routes::workspaces::create_document)
                .layer(DefaultBodyLimit::max(DOCUMENT_IMPORT_BODY_LIMIT))
        },
        request: Some(|g| g.subschema_for::<NewDocument>()),
        response: Some(|g| g.subschema_for::<Created>()),
        scope: Some(Scope::EditDocuments),
        summary: "Register a new document in the workspace",
        returns: "the new document id",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/documents/{document}/note",
        handler: || post(routes::workspaces::ensure_file_note),
        request: None,
        response: Some(|g| g.subschema_for::<EnsuredFileNote>()),
        scope: Some(Scope::EditDocuments),
        summary: "The one note a file carries, creating it when it has none",
        returns: "the note's document id — the existing one when the file already has a note, so the call is the answer to both Add note and Open note — and whether this call created it",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/transfer",
        handler: || post(routes::workspaces::transfer_documents),
        request: Some(|g| g.subschema_for::<TransferDocuments>()),
        response: Some(|g| g.subschema_for::<Transferred>()),
        scope: Some(Scope::EditDocuments),
        summary: "Move a document and its explore subtree into another workspace",
        returns: "the document ids that moved, root first. Ids are preserved.",
    },
    Route {
        method: "POST",
        audience: Audience::Internal,
        path: "/v1/internal/workspaces/{id}/documents/seeded",
        handler: || post(routes::workspaces::seed_document),
        request: Some(|g| g.subschema_for::<SeedDocumentRequest>()),
        response: None,
        scope: Some(Scope::EditDocuments),
        summary: "Plant opaque document content before publishing its manifest entry",
        returns: "the original application/octet-stream update and idempotency headers",
    },
    Route {
        method: "POST",
        audience: Audience::Internal,
        path: "/v2/internal/workspaces/{id}/documents/seeded",
        handler: || post(routes::workspaces::seed_document_with_candidate),
        request: Some(|g| g.subschema_for::<SeedDocumentWithCandidateRequest>()),
        response: None,
        scope: Some(Scope::EditDocuments),
        summary: "Plant opaque document content at a caller-minted document identity",
        returns: "the original application/octet-stream update and idempotency headers",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/manifest",
        handler: || get(routes::workspaces::get_manifest),
        request: None,
        response: Some(|g| g.subschema_for::<ManifestRef>()),
        scope: Some(Scope::ReadDocuments),
        summary: "The document id a client joins to see the workspace's contents",
        returns: "a document id",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/sync",
        handler: || any(routes::sync::upgrade),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "WebSocket carrying the standard Loro Protocol",
        returns: "a WebSocket; the token travels on the upgrade, never in the protocol",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/snapshot/{document}",
        handler: || post(routes::workspaces::snapshot),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "A document's whole state, for a client that has none",
        returns: "application/octet-stream — a Loro snapshot, ready to import",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/documents/{document}/history",
        handler: || get(routes::workspaces::list_history),
        request: None,
        response: Some(|g| g.subschema_for::<HistoryPage>()),
        scope: Some(Scope::ReadDocuments),
        summary: "A document's durable history markers, newest first",
        returns: "a cursor-paginated history page",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/documents/{document}/history",
        handler: || post(routes::workspaces::capture_history),
        request: Some(|g| g.subschema_for::<CaptureHistory>()),
        response: Some(|g| g.subschema_for::<HistoryEntry>()),
        scope: Some(Scope::EditDocuments),
        summary: "Capture a durable document version and append a history marker",
        returns: "the new history marker",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/workspaces/{id}/documents/{document}/cloud-sync",
        handler: || get(routes::workspaces::cloud_sync_status),
        request: None,
        response: Some(|g| g.subschema_for::<CloudSyncStatus>()),
        scope: Some(Scope::ReadDocuments),
        summary: "What Cloud sync has done to one document, and where its last local change came from",
        returns: "times, counts and stable codes — never a title or a byte of content",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/workspaces/{id}/documents/{document}/cloud-sync/coverage",
        handler: || get(routes::workspaces::cloud_sync_coverage),
        request: None,
        response: Some(|g| g.subschema_for::<koru_store_wire::internal::CloudDocumentCoverage>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Whether Koru Cloud holds one document, and how many of the images and files it references",
        returns: "whether the document is synced, and synced-of-total counts for its images and other files — never an address, a filename or a byte of content",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/workspaces/{id}/cloud-sync/waiting",
        handler: || get(routes::workspaces::cloud_sync_waiting),
        request: None,
        response: Some(|g| g.subschema_for::<koru_store_wire::CloudWaitingDocuments>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Which documents are waiting on a file Koru Cloud does not hold yet, and for which device",
        returns: "per document its id, title, how many files it is missing and the names of the devices known to have held them — never a byte of content or a file's address",
    },
    Route {
        method: "POST",
        audience: Audience::Internal,
        path: "/v1/workspaces/{id}/blobs/pause",
        handler: || post(routes::workspaces::pause_blob_transfer),
        request: Some(|g| g.subschema_for::<PauseBlobTransfer>()),
        response: Some(|g| g.subschema_for::<koru_store_wire::internal::BlobTransferReport>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Hold this workspace's picture transfers on this device, or let them go again",
        returns: "the blob transfer report as it stands afterwards; documents are untouched",
    },
    Route {
        method: "POST",
        audience: Audience::Internal,
        path: "/v1/workspaces/{id}/blobs/release",
        handler: || post(routes::workspaces::release_local_blobs),
        request: None,
        response: Some(|g| g.subschema_for::<koru_store_wire::internal::BlobReleaseReport>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Drop this device's copies of every file Koru Cloud holds, keeping every reference",
        returns: "how many copies left, how many were kept because Cloud has not confirmed them, and the bytes freed; documents are untouched",
    },
    Route {
        method: "PUT",
        audience: Audience::Internal,
        path: "/v1/profiles/{id}/transport-mode",
        handler: || put(routes::workspaces::set_profile_transport_mode),
        request: Some(|g| g.subschema_for::<SetProfileTransportMode>()),
        response: Some(|g| g.subschema_for::<ProfileTransportModeOutcome>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Move a Profile, and optionally its Workspaces, to a device-local transport mode",
        returns: "what was written; a Workspace somebody already chose for is never in the group",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/history/{entry}/label",
        handler: || post(routes::workspaces::relabel_history),
        request: Some(|g| g.subschema_for::<RelabelHistory>()),
        response: Some(|g| g.subschema_for::<HistoryEntry>()),
        scope: Some(Scope::EditDocuments),
        summary: "Rename a history marker; its immutable state is untouched",
        returns: "the marker with its new label",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/history/{entry}/snapshot",
        handler: || post(routes::workspaces::history_snapshot),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "The checksum-verified CRDT snapshot named by a history marker",
        returns: "application/octet-stream — an immutable Loro snapshot",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/versions/{version}/snapshot",
        handler: || get(routes::workspaces::version_snapshot),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "The checksum-verified CRDT snapshot a document version id names",
        returns: "application/octet-stream — an immutable Loro snapshot, with its document id, checksum and capture time on X-Koru-Document-Id, X-Koru-Checksum and X-Koru-Captured-At; `document_version_not_found` when the workspace holds no such version",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/blobs",
        handler: || get(routes::workspaces::list_blobs),
        request: None,
        response: Some(|g| g.subschema_for::<Vec<Blob>>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Every blob this workspace holds",
        returns: "an array of blobs — hash, size, mime, filename, created_at",
    },
    Route {
        method: "PUT",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/blobs",
        handler: || {
            put(routes::workspaces::put_blob).layer(DefaultBodyLimit::max(BLOB_UPLOAD_BODY_LIMIT))
        },
        request: None,
        response: Some(|g| g.subschema_for::<Blob>()),
        scope: Some(Scope::EditDocuments),
        summary: "Store bytes and get back the address they are filed under",
        returns: "a blob. The caller does not choose the hash: the store hashes what it \
                  receives, so storing the same bytes twice makes no second copy. The \
                  filename travels percent-encoded in X-Koru-Filename, because a filename \
                  is arbitrary Unicode and a header value is not",
    },
    Route {
        method: "GET",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/blobs/{hash}",
        handler: || get(routes::workspaces::get_blob),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "The local or on-demand hydrated bytes behind a hash",
        returns: "application/octet-stream, fetched from an authorized connected peer when absent locally, then verified and persisted against the address",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/blobs/{hash}",
        handler: || post(routes::workspaces::associate_blob),
        request: Some(|g| g.subschema_for::<AssociateBlob>()),
        response: Some(|g| g.subschema_for::<Blob>()),
        scope: Some(Scope::EditDocuments),
        summary: "Associate another workspace of the same Profile with one blob",
        returns: "the target workspace's durable blob record. No bytes are copied: one \
                  object serves every associated workspace, and the record is written \
                  before the caller publishes a CRDT reference to it",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/workspaces/{id}/export",
        handler: || post(routes::bundles::export),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "A whole workspace as one .koru bundle",
        returns: "application/octet-stream — manifest, a snapshot per document, then the \
                  blobs. A POST because it is work; it changes nothing",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/import",
        // A bundle is the Store's own export format: manifest, every document
        // snapshot, then every blob. Axum's 2 MiB extractor default is not a
        // format limit, and applying it here makes a valid Store export
        // impossible to import as soon as the aggregate crosses that size.
        handler: || post(routes::bundles::import).layer(DefaultBodyLimit::disable()),
        request: None,
        response: Some(|g| g.subschema_for::<Workspace>()),
        scope: Some(Scope::ManageWorkspaces),
        summary: "Take a .koru bundle in. ?mode=restore or ?mode=duplicate, and ?owner=<profile>",
        returns: "the workspace. The complete bundle is buffered in memory, with no HTTP \
                  size ceiling beyond the bundle format itself. There is no default mode, \
                  deliberately: `restore` keeps the ids so the result can converge with its \
                  origin, `duplicate` mints new ones so it never will, and guessing wrong is \
                  quiet either way",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/attributes/query",
        handler: || post(routes::attributes::query),
        request: Some(|g| g.subschema_for::<AttributeQuery>()),
        response: Some(|g| g.subschema_for::<AttributeQueryResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Which nodes carry these attributes, across workspaces, from the read model",
        returns: "a page of rows — each carrying workspace, document and node together, every \
                  attribute the node holds, an excerpt, and the source document's title and \
                  inherited trashed flag — plus the attribute definitions and tag tables in \
                  scope, so a filter UI builds from types. A POST because the query travels \
                  in the body: a filter can name an attribute key or a tag, and a URL is \
                  written to the access log. `group_by: \"tag\"` adds `groups` — a count per \
                  tag over the **whole** matching corpus rather than the page, with merged \
                  tags folded into the tag they were merged into and binned rows counted and \
                  flagged. `updating_workspaces` names any workspace the \
                  answer may be behind on rather than waiting for it",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/tasks/query",
        handler: || post(routes::tasks::query),
        request: Some(|g| g.subschema_for::<TaskQuery>()),
        response: Some(|g| g.subschema_for::<TaskQueryResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "What is outstanding, across workspaces, from the read model",
        returns: "a page of tasks — each carrying workspace, document and node together, the \
                  `task:` reference that is both its identity and the page cursor, its \
                  lifecycle, its title, the breadcrumb that says where it lives, both trash \
                  answers under their own names, and every attribute the node holds — plus \
                  the attribute definitions and tag tables in scope, so a row draws without a \
                  second request. A POST for the same reason the attribute query is one: the \
                  filter travels in the body rather than in a URL that is written to the \
                  access log. Paging is a keyset on the task reference, so `next_after` \
                  continues and `restarted` says the cursor named a row the filter no longer \
                  matches. `updating_workspaces` names any workspace the answer may be \
                  behind on rather than waiting for it",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/search",
        handler: || post(routes::search::search),
        request: Some(|g| g.subschema_for::<SearchRequest>()),
        response: Some(|g| g.subschema_for::<SearchResponse>()),
        scope: Some(Scope::ReadDocuments),
        summary: "Search every workspace this store holds",
        returns: "a page of hits, and optionally the workspace list and recent queries. \
                  A POST because the query travels in the body: a URL is written to the \
                  access log, and a search is not something to leave in a file that \
                  rotates for a week. `updating_workspaces` names any workspace the \
                  answer may be behind on rather than waiting for it",
    },
    Route {
        method: "POST",
        audience: Audience::Public,
        path: "/v1/search/navigated",
        handler: || post(routes::search::navigated),
        request: Some(|g| g.subschema_for::<Navigation>()),
        response: None,
        scope: Some(Scope::EditDocuments),
        summary: "Record that a search led somewhere",
        returns: "nothing. Machine-local and derived: it feeds recent queries and a \
                  workspace's recency, and reaches no document and no shared profile. \
                  A query earns a place in the history only by leading somewhere, which \
                  is why the text arrives here rather than on the search",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/openapi.json",
        handler: || get(routes::meta::openapi_internal),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "Every route this store serves, diagnostics included",
        returns: "an OpenAPI 3.1 document. Build against /v1/openapi.json instead: \
                  the routes only this one describes carry no compatibility promise",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/store",
        handler: || get(routes::debug::store),
        request: None,
        response: Some(|g| g.subschema_for::<StoreReport>()),
        scope: Some(Scope::ReadDocuments),
        summary: "What this store is doing, for a bug report",
        returns: "channel, pid, schema versions, what is open, rooms, recoveries. \
                  The data directory appears here as one opaque string and nowhere else",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/workspaces/{id}",
        handler: || get(routes::debug::workspace),
        request: None,
        response: Some(|g| g.subschema_for::<WorkspaceReport>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One workspace's physical footprint, and what blob transfer is doing",
        returns: "database bytes, document count, blob count and bytes, lineage; and, on a build with a Cloud blob engine, the referenced/present/durable counts, the stable pending code, which path and which object host answered, and how many walks and passes have run. Ids, counts and codes only",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/documents/{id}",
        handler: || get(routes::debug::document),
        request: None,
        response: Some(|g| g.subschema_for::<DocumentReport>()),
        scope: Some(Scope::ReadDocuments),
        summary: "One document's CRDT state",
        returns: "version vector and frontiers as hex, checkpoints, the updates the newest \
                  checkpoint does not cover, subscribers",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/projection",
        handler: || get(routes::debug::projection),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "What the projection layer is doing",
        returns: "pending, projected, failed, sweep progress — or null if none runs",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/search",
        handler: || get(routes::debug::search),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "What the search index is doing",
        returns: "queue depth, indexed, removed, failed, counts — or null if none runs",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/attributes",
        handler: || get(routes::debug::attributes),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "What the attribute query service is doing",
        returns: "queries served, rows answered, query millis — or null if none runs. \
                  Counters only, never a key, a value or a tag name",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/tasks",
        handler: || get(routes::debug::tasks),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "What the tasks query service is doing",
        returns: "queries served, rows answered, sweeps requested, cursor restarts, query \
                  millis — or null if none runs. Counters only, never a title, a breadcrumb \
                  or a task reference",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/debug/sync",
        handler: || get(routes::debug::sync),
        request: None,
        response: None,
        scope: Some(Scope::ReadDocuments),
        summary: "What peer replication control is doing",
        returns: "readiness, authorization generation and peer/workspace counts",
    },
    Route {
        method: "GET",
        audience: Audience::Internal,
        path: "/v1/metrics",
        handler: || get(routes::debug::metrics),
        request: None,
        response: Some(|g| g.subschema_for::<Metrics>()),
        scope: Some(Scope::ReadDocuments),
        summary: "The few numbers worth graphing",
        returns: "uptime, workspaces known and open, documents open, rooms, subscribers, \
                  recoveries",
    },
];

/// Every route, wired.
///
/// **Grouped by path**, because axum panics on two `.route()` calls for one
/// path: `/v1/profiles` is two rows and has to become one `MethodRouter`
/// carrying both. `MethodRouter::merge` does that — and panics if *both* sides
/// carry a fallback, which `any()` sets. `/v1/sync` is the only `any` row and
/// the only row on its path, so nothing merges into it; a second row there
/// would panic at start-up rather than fail to compile, which is worth knowing
/// before adding one.
///
/// The caller applies authentication and logging over the result. Nothing here
/// layers anything, so this function says only *what is served*.
pub fn routes() -> Router<Arc<AppState>> {
    let mut by_path: Vec<(&'static str, MethodRouter<Arc<AppState>>)> = Vec::new();
    for route in ROUTES {
        let built = (route.handler)();
        match by_path.iter_mut().find(|(path, _)| *path == route.path) {
            Some((_, existing)) => {
                // `take` because `merge` consumes both halves and we are
                // holding one through a `&mut`.
                let merged = std::mem::replace(existing, MethodRouter::new()).merge(built);
                *existing = merged;
            }
            None => by_path.push((route.path, built)),
        }
    }
    by_path
        .into_iter()
        .fold(Router::new(), |router, (path, methods)| {
            router.route(path, methods)
        })
}

/// The OpenAPI document, built from [`ROUTES`].
///
/// Deliberately small. It describes what a client needs in order to call the
/// API — the paths, the methods, the scopes, and the shape every error takes —
/// and does not attempt to be a schema registry.
///
/// **The audience decides what is in it.** [`Audience::Public`] emits the
/// routes a customer may build against. [`Audience::Internal`] emits
/// *everything*, public rows included: our own tooling should read one document
/// rather than stitching two, and a diagnostic is only useful next to the
/// operation it explains.
///
/// **The schemas are derived, never written.** Each row's `request` and
/// `response` register their type with one `SchemaGenerator` and come back as
/// `$ref`s, so `components/schemas` is generated from the same declarations —
/// and the same serde attributes — that produce the bytes. The document cannot
/// describe a shape the server does not send.
pub fn openapi(audience: Audience, api_version: u32) -> serde_json::Value {
    use serde_json::{Map, Value, json};

    // `openapi3` puts definitions under `/components/schemas`, which is where a
    // `$ref` in an OpenAPI document has to point.
    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    // Every failure carries this, so it is registered whether or not a route
    // happens to name it.
    let failure = schemas.subschema_for::<koru_store_wire::Failure>();

    let mut paths = Map::new();
    for route in ROUTES {
        if audience == Audience::Public && route.audience == Audience::Internal {
            continue;
        }
        let scope = match route.scope {
            Some(s) => json!([s.as_str()]),
            None => json!([]),
        };
        let mut operation = json!({
            "summary": route.summary,
            "description": route.returns,
            "security": [{ "bearerAuth": scope }],
            "responses": {
                "200": { "description": route.returns },
                "401": { "description": "no bearer token, or one that is not recognised",
                         "content": { "application/json": { "schema": failure } } },
                "403": { "description": "the credential does not hold the required scope",
                         "content": { "application/json": { "schema": failure } } },
            }
        });
        if let Some(request) = route.request {
            operation["requestBody"] = json!({
                "required": true,
                "content": { "application/json": { "schema": request(&mut schemas) } },
            });
        }
        let mut parameters: Vec<_> = route
            .path
            .split('/')
            .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
            .map(|name| {
                json!({
                    "name": name,
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" },
                })
            })
            .collect();
        parameters.extend(
            QUERY_PARAMETERS
                .iter()
                .filter(|parameter| {
                    parameter.method == route.method && parameter.path == route.path
                })
                .map(|parameter| {
                    json!({
                        "name": parameter.name,
                        "in": "query",
                        "required": parameter.required,
                        "description": parameter.description,
                        "schema": { "type": "string" },
                    })
                }),
        );
        if !parameters.is_empty() {
            operation["parameters"] = Value::Array(parameters);
        }
        if let Some(response) = route.response {
            operation["responses"]["200"]["content"] =
                json!({ "application/json": { "schema": response(&mut schemas) } });
        }
        if is_reminder_route(route) {
            for (status, description) in reminder_error_responses(route) {
                operation["responses"][status] = json!({
                    "description": description,
                    "content": { "application/json": { "schema": failure.clone() } },
                });
            }
        }
        let entry = paths
            .entry(route.path.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(object) = entry.as_object_mut() {
            object.insert(route.method.to_lowercase(), operation);
        }
    }
    // After every route, so nothing a row named is missing.
    let definitions = schemas.take_definitions(true);

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": match audience {
                Audience::Public => "Koru Store",
                Audience::Internal => "Koru Store (internal)",
            },
            "version": api_version.to_string(),
            "description": match audience {
                Audience::Public =>
                    "The only component that persists Koru's CRDT state. Every persistent \
                     mutation available to the first-party Koru UI is available here or \
                     through the standard Loro Protocol on the WebSocket endpoint. \
                     Errors are { code, message }, where `code` is a stable machine-readable \
                     token and `message` is untranslated developer text that must not be \
                     shown to a person.",
                Audience::Internal =>
                    "Every route this store serves, including the diagnostic ones. \
                     The diagnostic routes are counters for looking at a running store \
                     and carry no compatibility promise: fields appear and disappear with \
                     whatever is worth seeing. Build against the public document instead, \
                     at /v1/openapi.json. Errors are { code, message }, where `code` is a \
                     stable machine-readable token and `message` is untranslated developer \
                     text that must not be shown to a person.",
            },
        },
        "servers": [{ "url": "http://127.0.0.1", "description": "loopback only" }],
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer" }
            },
            // Generated from the wire types. There is no hand-written schema
            // here any more — one used to be, for `Failure`, and a hand-written
            // schema beside a derived one is the drift this whole table exists
            // to prevent.
            "schemas": Value::Object(definitions),
        },
        "security": [{ "bearerAuth": [] }],
        "paths": Value::Object(paths),
    })
}

fn is_reminder_route(route: &Route) -> bool {
    route.audience == Audience::Public && route.path.contains("/reminder")
}

fn reminder_error_responses(route: &Route) -> Vec<(&'static str, &'static str)> {
    let mut errors = vec![
        ("400", "invalid_request or invalid_id"),
        ("404", "profile_not_found or reminder_not_found"),
    ];
    let conflict = matches!(
        (route.method, route.path),
        ("POST", "/v1/profiles/{id}/reminders")
            | ("PUT", "/v1/profiles/{id}/reminders/{reminder}")
            | ("POST", "/v1/profiles/{id}/reminders/{reminder}/pause")
            | ("POST", "/v1/profiles/{id}/reminders/{reminder}/resume")
            | ("POST", "/v1/profiles/{id}/reminders/{reminder}/snooze")
            | ("POST", "/v1/profiles/{id}/reminders/{reminder}/handle")
    );
    if conflict || route.path.contains("/reminders/{reminder}") {
        errors.push(("409", "idempotency_conflict or profile_unit_required"));
    }
    if matches!(
        route.path,
        "/v1/profiles/{id}/reminders/query" | "/v1/profiles/{id}/reminders/reconciliation"
    ) {
        errors.push((
            "503",
            "projection_unavailable, reminder_index_updating or profile_private_unavailable",
        ));
    } else if route.path.ends_with("/reminder-settings") {
        errors.push(("503", "profile_private_unavailable"));
    }
    errors
}

#[cfg(test)]
mod tests;
