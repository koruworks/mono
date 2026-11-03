// SPDX-License-Identifier: MPL-2.0

//! Public gateway composition. Owns public sessions and no domain database.

mod ai;
mod artifacts;
mod run_mcp;
mod runs;
mod schedules;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use koru_api_wire::{
    DomainReadiness, FederatedSearchHit, FederatedSearchPage, FederatedSearchRequest,
    FederatedSearchResponse, GatewayStatus, SearchSource, SearchSourceState, SearchSourceStatus,
};
use koru_store_server::{StoreWorkerInfo, WorkerError, WorkerHandle};
use koru_store_wire::search::{MAX_PAGE_SIZE, SearchRequest, SearchResponse};

mod docs_viewer;
mod session_bridge;
mod session_routes;
pub mod settings;
pub mod workspaces;

pub use session_bridge::{BridgeError, BridgeSubscription, SessionBridge};

pub const SESSION_TTL_MILLIS: i64 = 60 * 60 * 1_000;
/// Spelled again in `koru-api-client`, which does not link this crate;
/// `the_control_scope_literals_agree` in the gateway's tests pins the copies.
pub const CONTROL_SCOPE: &str = "gateway.control";
/// **Reading the event journal and its stream.** Ordinary, and in the default
/// set — but naming any scope *filters* rather than adds, so a caller that asks
/// for `settings.write` and still wants to hear a change fact has to name this
/// one too. It was a bare literal here and at every such call site until SP-6b
/// needed a third. Spelled again in `koru-api-client`, which does not link this
/// crate; `the_events_read_scope_literals_agree` pins the copies.
pub const EVENTS_READ_SCOPE: &str = "events.read";
/// **The non-destructive half of workspace management.**
///
/// It was six bare string literals, unlike [`CONTROL_SCOPE`] and the five
/// `sessions.*` scopes, which are constants — and a scope spelled six times
/// cannot be split safely. SP-6b named it; SP-8a split it. This half now
/// covers creating, renaming, recolouring, reordering and importing, and stays
/// in the ordinary default set: an MCP tool that files a note into a new
/// workspace is doing something a person can undo by looking at it.
///
/// The destructive half is [`WORKSPACE_DESTROY_SCOPE`].
///
/// The gateway spelling carries the `store.` prefix the principal presents;
/// [`store_caller`] strips it and parses the remainder into
/// [`koru_store_core::model::Scope`], so the two halves are pinned to each
/// other by `the_manage_workspaces_scope_has_one_spelling`.
pub const MANAGE_WORKSPACES_SCOPE: &str = "store.manage_workspaces";
/// **Removing a workspace: archive, soft-delete, restore.**
///
/// R9's narrow destructive permission, and the only scope in the gateway that
/// is in **neither** the default set nor the allowed set. Naming it at connect
/// time is not enough to get it — that is the bar `settings.write` and
/// `ai.manage_providers` stand at, and it is the bar this one is deliberately
/// above. It is granted only when the Agent has vouched for the caller at
/// admission: a short-lived grant it mints for a process it launched itself,
/// which the gateway validates against it before minting a bearer.
///
/// **No `store.` prefix, because it is not a Store permission.** It gates the
/// Gateway's own lifecycle operation as well as the two Store routes that were
/// its bypass, and prefixing it would say the Store is where it lives.
/// [`store_caller`] therefore maps it explicitly rather than by stripping, and
/// `the_destructive_scope_has_one_spelling_on_both_sides` pins the two
/// spellings to each other the way the management scope's are pinned.
pub const WORKSPACE_DESTROY_SCOPE: &str = "workspace.destroy";
/// **Giving a workspace to another profile.**
///
/// The second scope to sit outside both sets, and it is here for the same
/// reason [`WORKSPACE_DESTROY_SCOPE`] is: the operation closes every terminal
/// the workspace owns before ownership changes (R8 — ownership never changes
/// under a running session), and a permission that ends somebody's terminals is
/// not one a caller should get by asking.
///
/// **Not folded into `workspace.destroy`, which was the obvious thing.** A move
/// destroys nothing — the id, the documents and the history are the same
/// afterwards — and the two answer different questions about trust. A window
/// vouched for so a person can empty a workspace is not thereby a window that
/// may hand their work to a different persona, and one grant that did both
/// would make that distinction unaskable. Two scopes, one grant mechanism: the
/// Agent's vouching is what gates them, and a caller names whichever it needs.
///
/// **No `store.` prefix**, for [`WORKSPACE_DESTROY_SCOPE`]'s reason: it gates
/// the Gateway's own migration operation as well as the Store route underneath
/// it. [`store_caller`] maps it by hand into
/// [`koru_store_core::model::Scope::MigrateWorkspaces`], and
/// `the_migration_scope_has_one_spelling_on_both_sides` pins the pair.
pub const WORKSPACE_MIGRATE_SCOPE: &str = "workspace.migrate";
/// **Reading what the app looks like.** An ordinary scope, in the default set:
/// every local caller may ask what the theme is.
///
/// Spelled again in `koru-settings`, which does not link this crate;
/// `the_settings_scope_literals_agree` pins the copies, the way
/// [`CONTROL_SCOPE`]'s are pinned.
pub const SETTINGS_READ_SCOPE: &str = "settings.read";
/// **Changing what the app looks like.** Narrow, and deliberately *not* in the
/// ordinary default set (owner, 2026-09-01): an MCP tool, a plugin or a script
/// holds a default token, and none of them should be able to silently change
/// the theme, the fonts and the startup behavior of every window.
///
/// This is the second scope to sit outside the default set, after
/// `ai.manage_providers`, and it uses the same mechanism: a caller that wants
/// it names it at connect time. **That mechanism is opt-in, not proof of
/// identity** — see [`MANAGE_WORKSPACES_SCOPE`]'s note. The gateway cannot yet
/// tell a first-party window from any other local caller, because
/// `ClientHello.product` is a free-form string every `koru-api-client` caller
/// sets identically. Naming the scope and keeping it out of the default set is
/// what SP-6 asks for; proving who is asking is the cross-cutting first-party
/// split R9 puts in SP-8, and it will narrow this scope and
/// `store.manage_workspaces` together.
pub const SETTINGS_WRITE_SCOPE: &str = "settings.write";
const SEARCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayPrincipal {
    pub label: String,
    pub scopes: Vec<String>,
}

#[derive(Clone, Debug)]
struct Session {
    principal: GatewayPrincipal,
    expires_at_millis: i64,
}

#[derive(Debug, Default)]
pub struct SessionAuthority {
    sessions: Mutex<HashMap<[u8; 32], Session>>,
}

/// What the gateway knows about a caller when it mints a bearer.
///
/// **Two states, and the second one is the whole of R9's first-party split.**
/// `Ordinary` is every local caller: admitted by channel and API version, its
/// `product` a free-form string it chose. `Vouched` means the Agent — the
/// process that launches Koru's own windows — confirmed at admission that it
/// launched this one, by minting a grant the caller presented and the gateway
/// spent against it.
///
/// It is not a scope and it is not a role. It decides one thing:
/// whether [`WORKSPACE_DESTROY_SCOPE`] can be granted at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Admission {
    #[default]
    Ordinary,
    Vouched,
}

impl SessionAuthority {
    /// Mint a bearer for an ordinary local caller.
    pub fn mint(
        &self,
        label: impl Into<String>,
        requested_scopes: &[String],
        now_millis: i64,
    ) -> (String, i64) {
        self.mint_admitted(label, requested_scopes, Admission::Ordinary, now_millis)
    }

    pub fn mint_admitted(
        &self,
        label: impl Into<String>,
        requested_scopes: &[String],
        admission: Admission,
        now_millis: i64,
    ) -> (String, i64) {
        let bearer = format!(
            "koru_api_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expires_at_millis = now_millis.saturating_add(SESSION_TTL_MILLIS);
        let scopes = effective_scopes(requested_scopes, admission);
        self.sessions.lock().expect("gateway sessions").insert(
            token_hash(&bearer),
            Session {
                principal: GatewayPrincipal {
                    label: label.into(),
                    scopes,
                },
                expires_at_millis,
            },
        );
        (bearer, expires_at_millis)
    }

    pub fn resolve(&self, bearer: &str, now_millis: i64) -> Option<GatewayPrincipal> {
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, session| session.expires_at_millis > now_millis);
        sessions
            .get(&token_hash(bearer))
            .map(|session| session.principal.clone())
    }

    pub fn revoke(&self, bearer: &str) -> bool {
        self.sessions
            .lock()
            .is_ok_and(|mut sessions| sessions.remove(&token_hash(bearer)).is_some())
    }
}

fn token_hash(bearer: &str) -> [u8; 32] {
    *blake3::hash(bearer.as_bytes()).as_bytes()
}

fn effective_scopes(requested: &[String], admission: Admission) -> Vec<String> {
    const DEFAULT: &[&str] = &[
        "store.read_documents",
        "store.edit_documents",
        "store.read_tasks",
        MANAGE_WORKSPACES_SCOPE,
        "ai.chat",
        EVENTS_READ_SCOPE,
        "events.write",
        // Reading settings is ordinary: a window, a plugin and a script all
        // need to know what the theme is to draw anything at all.
        SETTINGS_READ_SCOPE,
        // Reading what a run produced and registering a result are what a
        // window and an MCP tool do; the family's own permission rows are
        // the second gate (D11, owner-only in M1).
        koru_artifact_api::ArtifactScope::Read.as_str(),
        koru_artifact_api::ArtifactScope::Write.as_str(),
        // Delegating work and reading a run's activity are what a window
        // does; the run's own authority (P1.11) is the second gate.
        koru_delegate_api::DelegateScope::Read.as_str(),
        koru_delegate_api::DelegateScope::Write.as_str(),
    ];
    const ALLOWED: &[&str] = &[
        "store.read_documents",
        "store.edit_documents",
        "store.read_tasks",
        MANAGE_WORKSPACES_SCOPE,
        "ai.chat",
        "ai.manage_providers",
        "ai.read_usage",
        EVENTS_READ_SCOPE,
        "events.write",
        SETTINGS_READ_SCOPE,
        // Requestable but never default. See SETTINGS_WRITE_SCOPE.
        SETTINGS_WRITE_SCOPE,
        koru_artifact_api::ArtifactScope::Read.as_str(),
        koru_artifact_api::ArtifactScope::Write.as_str(),
        koru_delegate_api::DelegateScope::Read.as_str(),
        koru_delegate_api::DelegateScope::Write.as_str(),
    ];
    // **`workspace.destroy` and `workspace.migrate` are in neither list,
    // and that is the split.** Every other narrow scope is reachable by naming
    // it; these two are reachable only by naming one *and* having been vouched
    // for. A caller that asks without a grant is admitted without the scope.
    let vouched = matches!(admission, Admission::Vouched);
    if requested.is_empty() {
        // An empty request is a caller that named nothing, and a destructive
        // permission is never something nobody asked for.
        return DEFAULT
            .iter()
            .chain(koru_session_api::ALL_SCOPES)
            .map(|scope| (*scope).to_owned())
            .collect();
    }
    requested
        .iter()
        .filter(|scope| {
            ALLOWED.contains(&scope.as_str())
                || koru_session_api::ALL_SCOPES.contains(&scope.as_str())
                || scope.as_str() == CONTROL_SCOPE
                || (vouched
                    && matches!(
                        scope.as_str(),
                        WORKSPACE_DESTROY_SCOPE | WORKSPACE_MIGRATE_SCOPE
                    ))
        })
        .cloned()
        .collect()
}

/// One mobile projection of the public operation catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MobileOperation {
    pub id: &'static str,
    pub client: &'static str,
    pub http: Option<&'static str>,
    pub ffi: &'static str,
    pub scope: Option<&'static str>,
    pub request: &'static str,
    pub response: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MobileUnavailableOperation {
    pub id: &'static str,
    pub client: &'static str,
    pub http: &'static str,
    pub reason: &'static str,
}

pub const MOBILE_UNAVAILABLE_OPERATIONS: &[MobileUnavailableOperation] = &[
    MobileUnavailableOperation {
        id: "ai.provider_settings",
        client: "client.ai().provider_settings",
        http: "GET /v1/ai/providers/settings",
        reason: "Phase 1 provider setup is desktop-only",
    },
    MobileUnavailableOperation {
        id: "ai.select_provider",
        client: "client.ai().select_provider",
        http: "PUT /v1/ai/providers/selection",
        reason: "Phase 1 provider setup is desktop-only",
    },
    MobileUnavailableOperation {
        id: "ai.update_openai_settings",
        client: "client.ai().update_openai_settings",
        http: "PUT /v1/ai/providers/openai",
        reason: "Phase 1 provider setup is desktop-only",
    },
    MobileUnavailableOperation {
        id: "ai.update_ollama_settings",
        client: "client.ai().update_ollama_settings",
        http: "PUT /v1/ai/providers/ollama",
        reason: "Phase 1 provider setup is desktop-only",
    },
    MobileUnavailableOperation {
        id: "ai.validate_provider",
        client: "client.ai().validate_provider",
        http: "POST /v1/ai/providers/{provider}/validate",
        reason: "Phase 1 provider setup is desktop-only",
    },
    MobileUnavailableOperation {
        id: "ai.discover_agent_models",
        client: "client.ai().discover_agent_models",
        http: "POST /v1/ai/providers/{provider}/models",
        reason: "Local agents run on the desktop that installed them",
    },
    MobileUnavailableOperation {
        id: "ai.pull_ollama_model",
        client: "client.ai().pull_ollama_model",
        http: "POST /v1/ai/providers/ollama/pulls",
        reason: "Desktop-local Ollama lifecycle is unavailable on mobile",
    },
    MobileUnavailableOperation {
        id: "ai.cancel_ollama_pull",
        client: "client.ai().cancel_ollama_pull",
        http: "POST /v1/ai/providers/ollama/pulls/cancel",
        reason: "Desktop-local Ollama lifecycle is unavailable on mobile",
    },
    MobileUnavailableOperation {
        id: "ai.create_chat",
        client: "client.ai().create_chat",
        http: "POST /v1/ai/chats",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.list_chats",
        client: "client.ai().list_chats",
        http: "GET /v1/ai/chats",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.read_chat",
        client: "client.ai().read_chat",
        http: "GET /v1/ai/chats/{chat}",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.rename_chat",
        client: "client.ai().rename_chat",
        http: "PATCH /v1/ai/chats/{chat}",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.update_chat_model",
        client: "client.ai().update_chat_model",
        http: "PATCH /v1/ai/chats/{chat}/model",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.update_chat_privacy",
        client: "client.ai().update_chat_privacy",
        http: "PATCH /v1/ai/chats/{chat}/privacy",
        reason: "Per-chat AI privacy is not yet exposed by a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.update_chat_folder",
        client: "client.ai().update_chat_folder",
        http: "PATCH /v1/ai/chats/{chat}/folder",
        reason: "Local agents run on the desktop that installed them",
    },
    MobileUnavailableOperation {
        id: "ai.tool_list_chats",
        client: "client.ai().tool_list_chats",
        http: "POST /v1/ai/tools/chats/list",
        reason: "Chat tools are an MCP and provider-facing surface, not a mobile chat operation",
    },
    MobileUnavailableOperation {
        id: "ai.tool_search_chats",
        client: "client.ai().tool_search_chats",
        http: "POST /v1/ai/tools/chats/search",
        reason: "Chat tools are an MCP and provider-facing surface, not a mobile chat operation",
    },
    MobileUnavailableOperation {
        id: "ai.tool_read_chat",
        client: "client.ai().tool_read_chat",
        http: "POST /v1/ai/tools/chats/read",
        reason: "Chat tools are an MCP and provider-facing surface, not a mobile chat operation",
    },
    MobileUnavailableOperation {
        id: "ai.update_chat_tool_policy",
        client: "client.ai().update_chat_tool_policy",
        http: "PUT /v1/ai/chats/{chat}/tool-policy",
        reason: "Workspace-tool chat policy is not yet exposed by a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.read_chat_tool_policy",
        client: "client.ai().read_chat_tool_policy",
        http: "GET /v1/ai/chats/{chat}/tool-policy",
        reason: "Workspace-tool chat policy is not yet exposed by a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.search",
        client: "client.ai().search",
        http: "POST /v1/ai/search",
        reason: "Phase 1 ships AI chat search before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.feedback",
        client: "client.ai().feedback",
        http: "PUT /v1/ai/messages/{message}/feedback",
        reason: "Phase 1 ships desktop response actions before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.branch_chat",
        client: "client.ai().branch_chat",
        http: "POST /v1/ai/chats/{chat}/branches",
        reason: "Phase 1 ships desktop response actions before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.retry_turn",
        client: "client.ai().retry_turn",
        http: "POST /v1/ai/chats/{chat}/retries",
        reason: "Mobile SSE/stream projection is not yet implemented",
    },
    MobileUnavailableOperation {
        id: "ai.export_chat",
        client: "client.ai().export_chat",
        http: "POST /v1/ai/chats/{chat}/exports",
        reason: "Phase 1 ships desktop response actions before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.read_export",
        client: "client.ai().read_export",
        http: "GET /v1/ai/exports/{export}",
        reason: "Phase 1 ships desktop response actions before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.send_turn",
        client: "client.ai().send_turn",
        http: "POST /v1/ai/chats/{chat}/turns",
        reason: "Mobile SSE/stream projection is not yet implemented",
    },
    MobileUnavailableOperation {
        id: "ai.read_attempt",
        client: "client.ai().read_attempt",
        http: "GET /v1/ai/attempts/{attempt}",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.cancel_attempt",
        client: "client.ai().cancel_attempt",
        http: "POST /v1/ai/attempts/{attempt}/cancel",
        reason: "Phase 1 ships desktop chat before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.approve_tool_call",
        client: "client.ai().approve_tool_call",
        http: "POST /v1/ai/tool-calls/{tool}/approve",
        reason: "Workspace-tool approval is not yet exposed by a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.deny_tool_call",
        client: "client.ai().deny_tool_call",
        http: "POST /v1/ai/tool-calls/{tool}/deny",
        reason: "Workspace-tool approval is not yet exposed by a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.delete_chat",
        client: "client.ai().delete_chat",
        http: "DELETE /v1/ai/chats/{chat}",
        reason: "Phase 1 ships desktop history management before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.clear_history",
        client: "client.ai().clear_history",
        http: "DELETE /v1/ai/chats",
        reason: "Phase 1 ships desktop history management before a mobile chat host",
    },
    MobileUnavailableOperation {
        id: "ai.usage",
        client: "client.ai().usage",
        http: "GET /v1/ai/usage",
        reason: "Private local AI usage is desktop-only in Phase 1",
    },
    MobileUnavailableOperation {
        id: "ai.chat_usage",
        client: "client.ai().usage(Some(chat))",
        http: "GET /v1/ai/chats/{chat}/usage",
        reason: "Private local AI usage is desktop-only in Phase 1",
    },
    MobileUnavailableOperation {
        id: "ai.clear_usage",
        client: "client.ai().clear_usage",
        http: "DELETE /v1/ai/usage",
        reason: "Private local AI usage is desktop-only in Phase 1",
    },
];

/// Mobile operations currently consumed by the Android and iOS products.
/// Lifecycle/memory functions are intentionally absent.
pub const MOBILE_OPERATIONS: &[MobileOperation] = &[
    MobileOperation {
        id: "status",
        client: "client.status",
        http: Some("GET /v1/status"),
        ffi: "koru_api_status",
        scope: None,
        request: "HostStatusRequest",
        response: "HostStatus",
    },
    MobileOperation {
        id: "link_device",
        client: "client.link_device",
        http: None,
        ffi: "koru_api_link_device",
        scope: None,
        request: "LinkDeviceRequest",
        response: "LinkDeviceResult",
    },
    MobileOperation {
        id: "advertise_build",
        client: "client.advertise_build",
        http: None,
        ffi: "koru_api_advertise_build",
        scope: None,
        request: "BuildPayload",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.inventory",
        client: "client.store().inventory",
        http: None,
        ffi: "koru_api_store_inventory",
        scope: Some("store.read_documents"),
        request: "InventoryRequest",
        response: "Inventory",
    },
    MobileOperation {
        id: "store.document",
        client: "client.store().document",
        http: None,
        ffi: "koru_api_store_document",
        scope: Some("store.read_documents"),
        request: "DocumentRequest",
        response: "DocumentProjection",
    },
    MobileOperation {
        id: "store.blob",
        client: "client.store().blob",
        http: Some("GET /v1/workspaces/{id}/blobs/{hash}"),
        ffi: "koru_api_store_blob",
        scope: Some("store.read_documents"),
        request: "BlobRequest",
        response: "BlobBytes",
    },
    MobileOperation {
        id: "store.blob_status",
        client: "client.store().blob_status",
        http: None,
        ffi: "koru_api_store_blob_status",
        scope: Some("store.read_documents"),
        request: "BlobStatusRequest",
        response: "BlobTransferStatus",
    },
    MobileOperation {
        id: "store.cloud_sync_status",
        client: "client.store().cloud_sync_status",
        http: None,
        ffi: "koru_api_store_cloud_sync_status",
        scope: Some("store.read_documents"),
        request: "CloudSyncStatusRequest",
        response: "CloudSyncStatus",
    },
    MobileOperation {
        id: "store.cloud_waiting",
        client: "client.store().cloud_waiting",
        http: None,
        ffi: "koru_api_store_cloud_waiting",
        scope: Some("store.read_documents"),
        request: "CloudWaitingRequest",
        response: "CloudWaitingDocuments",
    },
    MobileOperation {
        id: "store.set_blob_policy",
        client: "client.store().set_blob_policy",
        http: None,
        ffi: "koru_api_store_set_blob_policy",
        scope: Some("store.read_documents"),
        request: "SetBlobPolicyRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.request_blob",
        client: "client.store().request_blob",
        http: None,
        ffi: "koru_api_store_request_blob",
        scope: Some("store.read_documents"),
        request: "RequestBlobRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.file_blob",
        client: "client.store().file_blob",
        http: None,
        ffi: "koru_api_store_file_blob",
        scope: Some("store.read_documents"),
        request: "FileBlobRequest",
        response: "FileBlob",
    },
    MobileOperation {
        id: "store.export_blob",
        client: "client.store().export_blob",
        http: None,
        ffi: "koru_api_store_export_blob",
        scope: Some("store.read_documents"),
        request: "ExportBlobRequest",
        response: "ExportedBlob",
    },
    MobileOperation {
        id: "store.release_blobs",
        client: "client.store().release_blobs",
        http: None,
        ffi: "koru_api_store_release_blobs",
        scope: Some("store.read_documents"),
        request: "ReleaseBlobsRequest",
        response: "ReleasedBlobs",
    },
    MobileOperation {
        id: "store.recover_blobs",
        client: "client.store().recover_blobs",
        http: None,
        ffi: "koru_api_store_recover_blobs",
        scope: Some("store.read_documents"),
        request: "BlobRecoverRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.blob_arrivals",
        client: "client.store().blob_arrivals",
        http: None,
        ffi: "koru_api_store_blob_arrivals",
        scope: Some("store.read_documents"),
        request: "BlobArrivalsRequest",
        response: "BlobArrivals",
    },
    MobileOperation {
        id: "store.run",
        client: "client.store().run",
        http: None,
        ffi: "koru_api_store_run",
        scope: Some("store.read_documents"),
        request: "RunRequest",
        response: "RunView",
    },
    MobileOperation {
        id: "store.run_timeline",
        client: "client.store().run_timeline",
        http: None,
        ffi: "koru_api_store_run_timeline",
        scope: Some("store.read_documents"),
        request: "RunTimelineRequest",
        response: "RunTimelineWindow",
    },
    MobileOperation {
        id: "store.mutate_workspace",
        client: "client.store().mutate_workspace",
        http: None,
        ffi: "koru_api_store_mutate_workspace",
        scope: Some(MANAGE_WORKSPACES_SCOPE),
        request: "WorkspaceMutation",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.mutate_profile",
        client: "client.store().mutate_profile",
        http: None,
        ffi: "koru_api_store_mutate_profile",
        scope: Some(MANAGE_WORKSPACES_SCOPE),
        request: "ProfileMutation",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.mutate_document",
        client: "client.store().mutate_document",
        http: None,
        ffi: "koru_api_store_mutate_document",
        scope: Some("store.edit_documents"),
        request: "DocumentMutation",
        response: "MutationResult",
    },
    MobileOperation {
        id: "store.import_file",
        client: "client.store().import_file",
        http: None,
        ffi: "koru_api_store_import_file",
        scope: Some("store.edit_documents"),
        request: "ImportFileRequest",
        response: "DocumentIdResult",
    },
    MobileOperation {
        id: "store.ensure_file_note",
        client: "client.store().ensure_file_note",
        http: None,
        ffi: "koru_api_store_ensure_file_note",
        scope: Some("store.edit_documents"),
        request: "FileNoteRequest",
        response: "DocumentIdResult",
    },
    MobileOperation {
        id: "start_pairing",
        client: "client.start_pairing",
        http: None,
        ffi: "koru_api_start_pairing",
        scope: None,
        request: "StartPairingRequest",
        response: "PairingOffer",
    },
    MobileOperation {
        id: "cancel_pairing",
        client: "client.cancel_pairing",
        http: None,
        ffi: "koru_api_cancel_pairing",
        scope: None,
        request: "CancelPairingRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "devices",
        client: "client.devices",
        http: None,
        ffi: "koru_api_devices",
        scope: None,
        request: "DevicesRequest",
        response: "DeviceList",
    },
    MobileOperation {
        id: "device_history",
        client: "client.device_history",
        http: None,
        ffi: "koru_api_device_history",
        scope: None,
        request: "DeviceHistoryRequest",
        response: "DeviceHistory",
    },
    MobileOperation {
        id: "unlink_device",
        client: "client.unlink_device",
        http: None,
        ffi: "koru_api_unlink_device",
        scope: None,
        request: "UnlinkDeviceRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "rename_device",
        client: "client.rename_device",
        http: None,
        ffi: "koru_api_rename_device",
        scope: None,
        request: "RenameDeviceRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "reset_identity",
        client: "client.reset_identity",
        http: None,
        ffi: "koru_api_reset_identity",
        scope: None,
        request: "ResetIdentityRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "set_relay",
        client: "client.set_relay",
        http: None,
        ffi: "koru_api_set_relay",
        scope: None,
        request: "SetRelayRequest",
        response: "MutationResult",
    },
    MobileOperation {
        id: "account_command",
        client: "client.account_command",
        http: None,
        ffi: "koru_api_account_command",
        scope: None,
        request: "AccountCommandRequest",
        response: "AccountCommandReply",
    },
    MobileOperation {
        id: "account_poll",
        client: "client.account_poll",
        http: None,
        ffi: "koru_api_account_poll",
        scope: None,
        request: "AccountPollRequest",
        response: "AccountPollReply",
    },
    MobileOperation {
        id: "account_platform",
        client: "client.account_platform",
        http: None,
        ffi: "koru_api_account_platform",
        scope: None,
        request: "AccountPlatformRequest",
        response: "MutationResult",
    },
];

/// Platform-authenticated in-process gateway admission. Mobile carries no
/// loopback bearer, but it still crosses the same capability/scope boundary.
#[derive(Clone, Debug)]
pub struct MobileGateway {
    principal: GatewayPrincipal,
    capabilities: Vec<String>,
}

impl MobileGateway {
    pub fn platform_local(label: impl Into<String>) -> Self {
        Self {
            principal: GatewayPrincipal {
                label: label.into(),
                scopes: effective_scopes(&[], Admission::Ordinary),
            },
            capabilities: vec!["store".into(), "mobile-direct".into(), "ai-disabled".into()],
        }
    }

    pub fn admit(&self, operation_id: &str) -> Result<&'static MobileOperation, &'static str> {
        let operation = MOBILE_OPERATIONS
            .iter()
            .find(|operation| operation.id == operation_id)
            .ok_or("operation_not_catalogued")?;
        if operation.id.starts_with("store.")
            && !self
                .capabilities
                .iter()
                .any(|capability| capability == "store")
        {
            return Err("capability_unavailable");
        }
        if let Some(scope) = operation.scope
            && !self.principal.scopes.iter().any(|held| held == scope)
        {
            return Err("permission_denied");
        }
        Ok(operation)
    }

    pub fn principal(&self) -> &GatewayPrincipal {
        &self.principal
    }
}

pub fn mobile_ffi_reference() -> String {
    let mut output = String::from(
        "# Koru API mobile FFI reference\n\nGenerated from the public mobile operation catalog. Do not edit.\n\n| Operation | Rust client | HTTP | Mobile symbol | Scope | Request | Response |\n| --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for operation in MOBILE_OPERATIONS {
        output.push_str(&format!(
            "| `{}` | `{}` | {} | `{}` | {} | `{}` | `{}` |\n",
            operation.id,
            operation.client,
            operation.http.unwrap_or("not HTTP-exposed"),
            operation.ffi,
            operation.scope.unwrap_or("platform principal"),
            operation.request,
            operation.response,
        ));
    }
    output.push_str("\n## Ownership\n\nRequests and responses are Protobuf values at the canonical ABI. Each native call is synchronous on a platform IO executor: the caller owns request bytes for the duration of the call, then owns the returned response until `koru_buffer_free` (iOS) or JVM collection (Android). Kotlin coroutines and the Swift Store actor keep these calls off the main thread. Buffer free is lifecycle mechanics rather than a product operation.\n");
    output.push_str("\n## Explicitly unavailable operations\n\nThese public HTTP/client operations are catalogued but have no mobile FFI or Protobuf projection in this phase.\n\n| Operation | Rust client | HTTP | Reason |\n| --- | --- | --- | --- |\n");
    for operation in MOBILE_UNAVAILABLE_OPERATIONS {
        output.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            operation.id, operation.client, operation.http, operation.reason
        ));
    }
    output
}

pub struct GatewayState {
    pub instance_id: String,
    pub channel: String,
    pub started_at: i64,
    pub sessions: Arc<SessionAuthority>,
    store: WorkerHandle,
    ai: Option<AiDomain>,
    /// The embedded events worker — the same worker-beside-the-gateway shape
    /// as the Store. `None` only in hosts that did not open one (older tests);
    /// its routes then answer 503 rather than 404, because the API exists.
    events: Option<koru_events_server::EventsHandle>,
    session_bridge: Option<SessionBridge>,
    /// Workspace-lifecycle confirmations this gateway issued and nobody has
    /// spent. See [`workspaces::Ledger`] — it is per-process because the
    /// consequence it is about was computed by this process.
    confirmations: workspaces::Ledger,
    /// The settings authority, handed down once by the host before serving.
    ///
    /// A `OnceLock` rather than a constructor argument, for the same reason
    /// the Store's catalog sink is one: there are six `GatewayState`
    /// constructors and the authority is optional in every host that does not
    /// resolve settings paths. Empty means `/v1/settings/*` answers
    /// `capability_unavailable` rather than 404 — the API exists.
    settings: std::sync::OnceLock<koru_settings::SettingsAuthority>,
    /// One internal-build-only, content-free hold of a named settings reply.
    /// The handler consumes it only after the authority has answered, so it
    /// can exercise an unknown transport outcome without moving the fault
    /// into the settings authority itself.
    settings_write_fault: settings::SettingsWriteFault,
    /// The Artifact Store (D1, D13), handed down once by the host that opened
    /// `artifacts.db`. A `OnceLock` for the settings authority's reason: six
    /// constructors, and a host that could not open the database composes
    /// without it. Empty means `/v1/artifacts/*` answers 503
    /// `artifacts_unavailable` and the `artifacts` capability is absent.
    /// The delegate transport (D12), handed down by the host: this device's
    /// `LocalDelegate` over the delegate worker's private IPC, or a cloud
    /// worker's transport later. Empty means `/v1/runs/*` answers 503.
    delegate: std::sync::OnceLock<Arc<dyn koru_delegate_api::DelegatePort>>,
    artifacts: std::sync::OnceLock<Arc<dyn koru_artifact_api::ArtifactDomain>>,
    /// `http://127.0.0.1:<port>`, handed down by the host once its listener
    /// is bound: the base of the endpoint URL a run's authority names. Empty
    /// in a host that has not said — an in-process router under test — and
    /// the route then uses the `Host` the caller reached.
    public_endpoint: std::sync::OnceLock<String>,
    openapi: serde_json::Value,
    asyncapi: serde_json::Value,
    pub shutdown: Arc<tokio::sync::Notify>,
}

struct AiDomain {
    client: koru_ai_client::Client,
    worker: Mutex<Option<koru_ai_server::WorkerHandle>>,
}

impl GatewayState {
    pub fn new(channel: String, store: WorkerHandle) -> Arc<Self> {
        Self::build(channel, store, None, None, None)
    }

    pub fn with_ai(
        channel: String,
        store: WorkerHandle,
        ai_worker: koru_ai_server::WorkerHandle,
    ) -> Arc<Self> {
        Self::with_ai_and_events(channel, store, ai_worker, None)
    }

    pub fn with_ai_and_events(
        channel: String,
        store: WorkerHandle,
        ai_worker: koru_ai_server::WorkerHandle,
        events: Option<koru_events_server::EventsHandle>,
    ) -> Arc<Self> {
        Self::with_ai_events_and_services(channel, store, ai_worker, events, None)
    }

    pub fn with_ai_events_and_services(
        channel: String,
        store: WorkerHandle,
        ai_worker: koru_ai_server::WorkerHandle,
        events: Option<koru_events_server::EventsHandle>,
        session_bridge: Option<SessionBridge>,
    ) -> Arc<Self> {
        let client = koru_ai_client::Client::new(Arc::new(ai_worker.client()));
        Self::build(
            channel,
            store,
            Some(AiDomain {
                client,
                worker: Mutex::new(Some(ai_worker)),
            }),
            events,
            session_bridge,
        )
    }

    pub fn new_with_events(
        channel: String,
        store: WorkerHandle,
        events: Option<koru_events_server::EventsHandle>,
    ) -> Arc<Self> {
        Self::new_with_services(channel, store, events, None)
    }

    pub fn new_with_services(
        channel: String,
        store: WorkerHandle,
        events: Option<koru_events_server::EventsHandle>,
        session_bridge: Option<SessionBridge>,
    ) -> Arc<Self> {
        Self::build(channel, store, None, events, session_bridge)
    }

    fn build(
        channel: String,
        store: WorkerHandle,
        ai: Option<AiDomain>,
        events: Option<koru_events_server::EventsHandle>,
        session_bridge: Option<SessionBridge>,
    ) -> Arc<Self> {
        Arc::new(Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            channel,
            started_at: now_millis(),
            sessions: Arc::new(SessionAuthority::default()),
            store,
            ai,
            events,
            session_bridge,
            confirmations: workspaces::Ledger::default(),
            settings: std::sync::OnceLock::new(),
            settings_write_fault: settings::SettingsWriteFault::default(),
            artifacts: std::sync::OnceLock::new(),
            delegate: std::sync::OnceLock::new(),
            public_endpoint: std::sync::OnceLock::new(),
            openapi: public_openapi(),
            asyncapi: public_asyncapi(),
            shutdown: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Hand the settings authority down once, before serving.
    ///
    /// Returns whether it was accepted: a second call is a bug, and one
    /// authority per process is what "nothing else reads or writes the
    /// settings files" means in practice.
    ///
    /// The journal is connected here rather than by the host, because the two
    /// vocabularies — a settings section and an event row — meet in this
    /// crate and nowhere else. A host that opened no events worker leaves the
    /// authority's sink empty and every publish is a silent no-op.
    pub fn set_settings_authority(&self, authority: koru_settings::SettingsAuthority) -> bool {
        if let Some(events) = &self.events {
            authority.set_event_sink(Arc::new(settings::SettingsFacts::new(events.clone())));
        }
        self.settings.set(authority).is_ok()
    }

    fn settings(&self) -> Option<&koru_settings::SettingsAuthority> {
        self.settings.get()
    }

    /// Hand the Artifact Store down once, before serving. Returns whether it
    /// was accepted: a second call is a bug, for the settings authority's
    /// reason.
    pub fn set_artifact_service(
        &self,
        service: Arc<dyn koru_artifact_api::ArtifactDomain>,
    ) -> bool {
        self.artifacts.set(service).is_ok()
    }

    /// The whole artifact domain: the person's routes, the run authority
    /// and the run tools, one object.
    fn artifact_domain(&self) -> Option<Arc<dyn koru_artifact_api::ArtifactDomain>> {
        self.artifacts.get().cloned()
    }

    /// Hand down the transport `/v1/runs` answers through. Once per
    /// process; a second call is refused and reported.
    pub fn set_delegate_port(&self, port: Arc<dyn koru_delegate_api::DelegatePort>) -> bool {
        self.delegate.set(port).is_ok()
    }

    fn delegate_port(&self) -> Option<Arc<dyn koru_delegate_api::DelegatePort>> {
        self.delegate.get().cloned()
    }

    /// Tell the state where it is listening, once, so an issued run
    /// authority can name its endpoint in full. Returns whether it was
    /// accepted.
    pub fn set_public_endpoint(&self, endpoint: String) -> bool {
        self.public_endpoint.set(endpoint).is_ok()
    }

    pub fn public_endpoint(&self) -> Option<&str> {
        self.public_endpoint.get().map(String::as_str)
    }

    /// Arm the internal acceptance-only settings reply hold.
    ///
    /// `internal_build` comes from the host's compile-time release channel.
    /// A public release therefore cannot enable this behavior even when its
    /// environment is controlled. The operation token is categorical and no
    /// setting value or other user content is retained.
    pub fn configure_settings_write_fault(
        &self,
        internal_build: bool,
        operation: Option<&str>,
    ) -> settings::FaultConfiguration {
        self.settings_write_fault
            .configure(internal_build, operation)
    }

    pub fn store_status(&self) -> Result<StoreWorkerInfo, WorkerError> {
        self.store.store_status()
    }

    pub fn stop_workers(&self) {
        if let Some(ai) = &self.ai
            && let Some(worker) = ai.worker.lock().expect("AI worker handle").as_ref()
        {
            worker.shutdown();
        }
        self.store.shutdown();
    }

    /// **End every open `/v1/events` tail, before the host drains its HTTP
    /// server.**
    ///
    /// A tail is a response body a client holds open until its own ninety
    /// second deadline; a graceful HTTP shutdown waits for in-flight
    /// responses, so one attached window made a gateway asked to stop take
    /// about ninety seconds to exit. The host closes the tails first, and the
    /// drain is then under a second. See `koru_events_server::EventsState::close_streams`
    /// for what a cut tail costs a client, which is nothing: the cursor is the
    /// protocol and it resumes.
    ///
    /// Separate from [`GatewayState::stop_workers`] on purpose — this runs
    /// *before* the server drains and that runs after, and folding them would
    /// shut the journal while requests were still being answered.
    pub fn close_event_streams(&self) {
        if let Some(events) = &self.events {
            events.close_streams();
        }
    }

    /// **Hand the Store worker's completion back, once the router is gone.**
    ///
    /// The Store's `Ack` already means committed; joining the worker proves
    /// the thread that made those commits has finished. Nothing new is
    /// tracked — this is the join that already existed, taken deliberately
    /// instead of by `WorkerHandle`'s destructor, which discards what it finds.
    ///
    /// It hands back the *completion* rather than the handle itself because
    /// `GatewayState` lives behind an `Arc` that discovery and the settings
    /// sink also hold, so ownership cannot be moved out of it; what matters is
    /// that the join happens exactly once, on the host's own exit path, and
    /// that its `WorkerExit` reaches the host. A second call answers `None` —
    /// **absence of a report is never evidence of a clean join**, so a caller
    /// must not read `None` as success.
    ///
    /// Blocking: it joins a thread. Call it from the host's shutdown, never
    /// from a handler.
    pub fn join_store(&self) -> Option<koru_store_server::WorkerExit> {
        self.store.shutdown_and_join()
    }

    pub fn session_bridge(&self) -> Option<&SessionBridge> {
        self.session_bridge.as_ref()
    }

    pub fn sessions_available(&self) -> bool {
        self.session_bridge.is_some()
    }

    pub fn join_ai(&self) -> Option<koru_ai_server::WorkerExit> {
        self.ai.as_ref().and_then(|ai| {
            ai.worker
                .lock()
                .expect("AI worker handle")
                .take()
                .map(koru_ai_server::WorkerHandle::join)
        })
    }

    /// **How many first-party windows are attached through a live
    /// `/v1/events` stream.**
    ///
    /// The other half of "is anybody still using me". The Store worker counts
    /// sync sockets, which is the whole answer for a Koru window and *none* of
    /// it for a Koru Terminal: a Terminal joins no sync room — its banner
    /// catalog rides this stream and its session traffic goes straight to the
    /// session host — so a gateway asked "are you idle" while a Terminal window
    /// was working answered yes and stopped underneath it.
    ///
    /// A subscriber is a held HTTP response, counted by the events worker's own
    /// guard and released when the stream ends, so this is the same kind of
    /// answer the room count is: what would be cut off, not who asked.
    ///
    /// **It is a count of streams open at this instant.** A client's tail is
    /// cut and resumed on a 90-second deadline, so there is a narrow moment
    /// between two connections when a live window counts as zero — the same
    /// shape as a window closing one sync socket and opening another. Whether a
    /// gateway should linger past the last consumer is a lifetime policy, and
    /// is a question for the owner rather than a number invented here.
    pub fn attached_event_streams(&self) -> u64 {
        self.events
            .as_ref()
            .map(|events| {
                events
                    .state
                    .sse_subscribers
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
            .unwrap_or(0)
    }

    pub fn public_capabilities(&self) -> Vec<String> {
        let mut capabilities = vec!["store".into(), "api-docs".into(), "asyncapi".into()];
        if self.events.is_some() {
            capabilities.push("events".into());
        }
        if self.ai_ready() {
            capabilities.extend([
                "ai".into(),
                "ai.search".into(),
                "ai.streaming".into(),
                "ai.usage".into(),
            ]);
        } else {
            capabilities.push("ai-disabled".into());
        }
        if self.sessions_available() {
            capabilities.push(koru_session_api::CAPABILITY.into());
        }
        if self.artifacts.get().is_some() {
            capabilities.push(koru_artifact_wire::CAPABILITY.into());
        }
        if self.delegate.get().is_some() {
            capabilities.push(koru_delegate_api::CAPABILITY.into());
        }
        capabilities
    }

    fn ai_ready(&self) -> bool {
        self.ai.as_ref().is_some_and(|ai| {
            ai.worker.lock().is_ok_and(|worker| {
                worker.as_ref().is_some_and(|worker| {
                    matches!(
                        worker.status(),
                        koru_ai_server::WorkerStatus::Ready(info)
                            if info.capabilities.iter().any(|capability| capability == "ai.chat")
                    )
                })
            })
        })
    }

    fn ai_readiness(&self) -> DomainReadiness {
        let Some(ai) = &self.ai else {
            return DomainReadiness::Disabled;
        };
        let Ok(worker) = ai.worker.lock() else {
            return DomainReadiness::Unavailable;
        };
        let Some(worker) = worker.as_ref() else {
            return DomainReadiness::Unavailable;
        };
        match worker.status() {
            koru_ai_server::WorkerStatus::Starting => DomainReadiness::Starting,
            koru_ai_server::WorkerStatus::Ready(info)
                if info
                    .capabilities
                    .iter()
                    .any(|capability| capability == "ai.chat") =>
            {
                DomainReadiness::Ready
            }
            koru_ai_server::WorkerStatus::Ready(_) => DomainReadiness::Disabled,
            koru_ai_server::WorkerStatus::Stopping
            | koru_ai_server::WorkerStatus::Stopped
            | koru_ai_server::WorkerStatus::Failed(_) => DomainReadiness::Unavailable,
        }
    }

    fn ai_client(&self) -> Option<koru_ai_client::Client> {
        self.ai_ready()
            .then(|| self.ai.as_ref().expect("ready AI domain").client.clone())
    }
}

pub fn router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/asyncapi.json", get(asyncapi))
        .route("/v1/search", post(federated_search))
        .route("/v1/store/status", get(store_status))
        .route("/v1/store/search", post(store_search))
        .route("/docs", get(docs))
        .route("/docs/asyncapi", get(async_docs))
        // The catalog is public before the bridge is enabled. These routes
        // enforce their final scopes and then answer capability_unavailable,
        // rather than falling through to the Store as a misleading 404.
        .merge(session_routes::routes())
        // No account catalog: the account API is the hosted Cloud's and is
        // documented on its own (`koru_account_wire::openapi`). `/v1/account*`
        // and `/v1/oauth/*` fall through to the Store like any unknown path
        // (docs/developers/API.md).
        .merge(ai::router())
        // The Artifact Store's own namespace (D13). Its routes exist whether
        // or not `artifacts.db` opened, and answer 503 when it did not.
        .merge(artifacts::router())
        // Delegated runs (D12), answered by the delegate worker through the
        // transport the host composed; 503 when it did not.
        .merge(runs::router())
        // Schedule commands share the delegate worker but are serialized on
        // its dedicated evaluator thread; the stream carries invalidations
        // only and never schedule content.
        .merge(schedules::router())
        // A delegated run's endpoint (§7.3): its own credential, resolved
        // by the route from the authority record, never a session bearer.
        .merge(run_mcp::router())
        // Ahead of the store fallback, like the events namespace: `/v1/settings`
        // is this process's own, and falling through to the Store would answer
        // a misleading 404 for a route that exists.
        .merge(settings::router())
        // Also ahead of it, and inside the Store's own `/v1/workspaces` prefix
        // on purpose: this is the operation the Store's removal routes exist
        // underneath, and a caller that finds it beside them is a caller that
        // does not have to be told which door to use.
        .merge(workspaces::router())
        // The events worker's namespace, ahead of the store fallback. `any`:
        // the worker's own router answers per-method, and a wrong method
        // should say 405 from the routes that exist there, not fall through
        // to the store.
        .route("/v1/events", axum::routing::any(forward_events))
        .route("/v1/events/{*rest}", axum::routing::any(forward_events))
        .fallback(forward_store)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            authenticate,
        ))
        .with_state(state)
}

async fn authenticate(
    State(state): State<Arc<GatewayState>>,
    mut request: Request,
    next: Next,
) -> Response {
    if is_public_documentation(request.uri().path()) {
        return next.run(request).await;
    }
    // A run's endpoint admits a run's token and nothing else; the session
    // authority never minted one and must not be asked. The route resolves
    // the bearer itself, on every call.
    if run_mcp::is_run_endpoint(request.uri().path()) {
        return next.run(request).await;
    }
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default();
    let Some(principal) = state.sessions.resolve(bearer, now_millis()) else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the gateway bearer was not recognised",
        );
    };
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn is_public_documentation(path: &str) -> bool {
    matches!(
        path,
        "/docs" | "/docs/asyncapi" | "/v1/openapi.json" | "/v1/asyncapi.json"
    )
}

async fn status(State(state): State<Arc<GatewayState>>) -> Response {
    match state.store.store_status() {
        Ok(_store) => Json(GatewayStatus {
            api_version: koru_api_wire::API_VERSION,
            capabilities: state.public_capabilities(),
            channel: state.channel.clone(),
            started_at: state.started_at,
            gateway: DomainReadiness::Ready,
            store: DomainReadiness::Ready,
            ai: state.ai_readiness(),
        })
        .into_response(),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the Store worker is unavailable",
        ),
    }
}

async fn shutdown(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    axum::extract::Query(query): axum::extract::Query<koru_store_api::routes::admin::ShutdownQuery>,
) -> Response {
    if !principal.scopes.iter().any(|scope| scope == CONTROL_SCOPE) {
        return failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "gateway.control is required to stop the gateway",
        );
    }
    // `?only_if_idle=true` is the last window's "am I the last" question, and
    // the Store worker is the only party that can answer it: its count is
    // taken under the same lock a room join takes, so a window closing cannot
    // race another window connecting (routes/admin.rs says why at length).
    // The gateway forwards the whole decision rather than counting here —
    // a count-then-stop in two steps would reopen exactly that race. The
    // caller's token is deliberately control-only, so the internal ask
    // synthesizes the store scope the worker's route requires.
    if query.only_if_idle {
        // **A gateway with an attached first-party window is not idle**, and
        // the Store worker cannot see every window that is attached. Its count
        // is of sync sockets; a Koru Terminal holds none, and its catalog watch
        // — a live `/v1/events` stream — was invisible to the whole test. So
        // this half is answered here, where the events worker is, and it is
        // answered *before* the worker is asked: the worker's route trips its
        // own stop as it answers, so asking it first and declining afterwards
        // would already have taken the store down.
        let watching = state.attached_event_streams();
        if watching > 0 {
            tracing::info!(
                watching,
                "shutdown declined: a window is still watching the event stream"
            );
            return Json(koru_store_wire::Stopping { stopping: false }).into_response();
        }
        let ask = Request::builder()
            .method("POST")
            .uri("/v1/shutdown?only_if_idle=true")
            .body(axum::body::Body::empty())
            .expect("a literal shutdown request is well-formed");
        let caller = koru_store_api::auth::Caller {
            label: principal.label,
            scopes: vec![koru_store_core::model::Scope::ManageWorkspaces],
        };
        let declined = match state.store.request(ask, caller).await {
            Ok(response) => {
                let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await;
                !matches!(
                    bytes
                        .as_deref()
                        .map(serde_json::from_slice::<koru_store_wire::Stopping>),
                    Ok(Ok(koru_store_wire::Stopping { stopping: true }))
                )
            }
            // A worker that cannot answer is not idle in any sense a caller
            // can rely on; decline rather than stop over its head.
            Err(_) => true,
        };
        if declined {
            return Json(koru_store_wire::Stopping { stopping: false }).into_response();
        }
        // The worker's route has already tripped its own stop; the calls
        // below are idempotent and cover the gateway's half.
    }
    state.stop_workers();
    state.shutdown.notify_one();
    Json(koru_store_wire::Stopping { stopping: true }).into_response()
}

async fn openapi(State(state): State<Arc<GatewayState>>) -> Json<serde_json::Value> {
    Json(state.openapi.clone())
}
async fn asyncapi(State(state): State<Arc<GatewayState>>) -> Json<serde_json::Value> {
    Json(state.asyncapi.clone())
}
async fn docs() -> Html<&'static str> {
    Html(public_openapi_viewer())
}
async fn async_docs() -> Html<&'static str> {
    Html(public_asyncapi_viewer())
}

async fn forward_events(State(state): State<Arc<GatewayState>>, mut request: Request) -> Response {
    // The worker never sees the gateway bearer — same hygiene as the store
    // forward. Authentication already ran; the principal proves it.
    request.headers_mut().remove(header::AUTHORIZATION);
    let principal = request
        .extensions()
        .get::<GatewayPrincipal>()
        .cloned()
        .expect("authentication installs a gateway principal");
    // Reads (the journal, the stream, the status) take `events.read`;
    // anything that changes state — publishing, consent — takes
    // `events.write`. A default session holds both; a client that asked for
    // store scopes alone deliberately does not.
    let required = if request.method() == axum::http::Method::GET {
        EVENTS_READ_SCOPE
    } else {
        "events.write"
    };
    if !principal.scopes.iter().any(|scope| scope == required) {
        return failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "this session does not hold the events scope its request needs",
        );
    }
    match &state.events {
        Some(events) => events.request(request).await,
        None => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the events worker is unavailable",
        ),
    }
}

async fn forward_store(State(state): State<Arc<GatewayState>>, mut request: Request) -> Response {
    request.headers_mut().remove(header::AUTHORIZATION);
    let principal = request
        .extensions()
        .get::<GatewayPrincipal>()
        .cloned()
        .expect("authentication installs a gateway principal");
    let caller = store_caller(&principal);
    match state.store.request(request, caller).await {
        Ok(response) => response,
        Err(WorkerError::Saturated) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_saturated",
            "the Store worker queue is saturated",
        ),
        Err(WorkerError::TimedOut) => failure(
            StatusCode::GATEWAY_TIMEOUT,
            "deadline_exceeded",
            "the Store worker did not answer before the deadline",
        ),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the Store worker is unavailable",
        ),
    }
}

/// One internal Store call on behalf of an already-authorized principal.
///
/// **The gateway's own operations reach the Store the way a client would**, so
/// the Store's handlers stay the one place a catalog mutation is decided — and,
/// with them, the one place its committed fact is published. The alternative
/// was calling `koru_store_core`'s methods directly, which would have meant a
/// second publisher for `workspace.catalog.deleted` and a second chance to
/// publish one for a commit that did not happen.
///
/// The principal is translated through [`store_caller`] exactly as
/// [`forward_store`] translates it, so this call holds nothing the caller does
/// not. An error answer comes back as the `Response` it was, status and all.
pub(crate) async fn store_json(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, Response> {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&body).expect("a literal request body serializes"),
            )),
        None => builder.body(Body::empty()),
    }
    .expect("a literal internal Store request is well-formed");
    let response = state
        .store
        .request(request, store_caller(principal))
        .await
        .map_err(|error| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                store_worker_code(error),
                "the Store worker did not answer",
            )
        })?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap_or_default();
    if !status.is_success() {
        let mut answer = (status, bytes).into_response();
        answer
            .headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        return Err(answer);
    }
    Ok(serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn store_search(
    State(state): State<Arc<GatewayState>>,
    Extension(_principal): Extension<GatewayPrincipal>,
    mut request: Request,
) -> Response {
    *request.uri_mut() = "/v1/search".parse().expect("literal Store search URI");
    forward_store(State(state), request).await
}

async fn store_status(
    State(state): State<Arc<GatewayState>>,
    Extension(_principal): Extension<GatewayPrincipal>,
    mut request: Request,
) -> Response {
    *request.uri_mut() = "/v1/status".parse().expect("literal Store status URI");
    forward_store(State(state), request).await
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchCursor {
    store_offset: usize,
    ai_cursor: Option<String>,
}

enum SourceResult<T> {
    Complete(T),
    Failed(&'static str),
    TimedOut,
    Disabled,
}

async fn federated_search(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<FederatedSearchRequest>, JsonRejection>,
) -> Response {
    let request = match payload {
        Ok(Json(request)) => request,
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the federated search request body is invalid",
            );
        }
    };
    let cursor = match request
        .cursor
        .as_deref()
        .map(decode_search_cursor)
        .transpose()
    {
        Ok(cursor) => cursor.unwrap_or(SearchCursor {
            store_offset: request.query.offset,
            ai_cursor: None,
        }),
        Err(()) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "the federated search cursor is invalid",
            );
        }
    };
    let page_size = request.query.page_size.clamp(1, MAX_PAGE_SIZE);
    let mut store_request = SearchRequest {
        query: request.query.clone(),
        include_facets: request.include_facets,
        recent_queries: request.recent_queries,
    };
    store_request.query.offset = cursor.store_offset;
    store_request.query.page_size = page_size;
    let store_state = Arc::clone(&state);
    let store_principal = principal.clone();
    let store_future = async move {
        match tokio::time::timeout(
            SEARCH_DEADLINE,
            query_store(store_state, store_principal, store_request),
        )
        .await
        {
            Ok(Ok(response)) => SourceResult::Complete(response),
            Ok(Err(code)) => SourceResult::Failed(code),
            Err(_) => SourceResult::TimedOut,
        }
    };
    let ai_state = Arc::clone(&state);
    let ai_principal = principal.clone();
    let ai_text = request.query.text.clone();
    let ai_cursor = cursor.ai_cursor.clone();
    let include_ai = request.include_ai;
    let request_seq = request.query.request_seq;
    let ai_future = async move {
        if !include_ai {
            return SourceResult::Disabled;
        }
        match tokio::time::timeout(
            SEARCH_DEADLINE,
            query_ai(
                ai_state,
                ai_principal,
                koru_ai_wire::AiSearchRequest {
                    text: ai_text,
                    limit: u16::try_from(page_size).expect("Store page limit fits u16"),
                    cursor: ai_cursor,
                    request_seq,
                },
            ),
        )
        .await
        {
            Ok(Ok(response)) => SourceResult::Complete(response),
            Ok(Err(code)) => SourceResult::Failed(code),
            Err(_) => SourceResult::TimedOut,
        }
    };
    let (store_result, ai_result) = tokio::join!(store_future, ai_future);

    let mut sources = Vec::with_capacity(2);
    let (mut store_hits, store_next, updating_workspaces, corrected_query, facets, recent_queries) =
        match store_result {
            SourceResult::Complete(response) => {
                sources.push(source_status(
                    SearchSource::Store,
                    SearchSourceState::Complete,
                    None,
                ));
                (
                    response.page.hits.into(),
                    response.page.next_offset,
                    response.page.updating_workspaces,
                    response.page.corrected_query,
                    response.facets,
                    response.recent_queries,
                )
            }
            SourceResult::Failed(code) => {
                sources.push(source_status(
                    SearchSource::Store,
                    SearchSourceState::Failed,
                    Some(code),
                ));
                (VecDeque::new(), None, Vec::new(), None, None, None)
            }
            SourceResult::TimedOut => {
                sources.push(source_status(
                    SearchSource::Store,
                    SearchSourceState::TimedOut,
                    Some("deadline_exceeded"),
                ));
                (VecDeque::new(), None, Vec::new(), None, None, None)
            }
            SourceResult::Disabled => unreachable!("Store search is always eligible"),
        };
    let (mut ai_hits, ai_next) = match ai_result {
        SourceResult::Complete(response) => {
            sources.push(source_status(
                SearchSource::Ai,
                SearchSourceState::Complete,
                None,
            ));
            (response.page.hits.into(), response.page.next_cursor)
        }
        SourceResult::Failed(code) => {
            sources.push(source_status(
                SearchSource::Ai,
                SearchSourceState::Failed,
                Some(code),
            ));
            (VecDeque::new(), None)
        }
        SourceResult::TimedOut => {
            sources.push(source_status(
                SearchSource::Ai,
                SearchSourceState::TimedOut,
                Some("deadline_exceeded"),
            ));
            (VecDeque::new(), None)
        }
        SourceResult::Disabled => {
            sources.push(source_status(
                SearchSource::Ai,
                SearchSourceState::Disabled,
                None,
            ));
            (VecDeque::new(), None)
        }
    };

    let mut hits = Vec::with_capacity(page_size);
    let (mut store_used, mut ai_used) = (0_usize, 0_usize);
    while hits.len() < page_size && (!store_hits.is_empty() || !ai_hits.is_empty()) {
        if !store_hits.is_empty() && hits.len() < page_size {
            hits.push(FederatedSearchHit::Store {
                hit: store_hits.pop_front().expect("Store hit was present"),
            });
            store_used += 1;
        }
        if !ai_hits.is_empty() && hits.len() < page_size {
            hits.push(FederatedSearchHit::Ai {
                hit: ai_hits.pop_front().expect("AI hit was present"),
            });
            ai_used += 1;
        }
    }
    let store_more = !store_hits.is_empty() || store_next.is_some();
    let ai_more = !ai_hits.is_empty() || ai_next.is_some();
    let next_cursor = (store_more || ai_more).then(|| {
        encode_search_cursor(&SearchCursor {
            store_offset: cursor.store_offset.saturating_add(store_used),
            ai_cursor: Some(
                cursor
                    .ai_cursor
                    .as_deref()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or_default()
                    .saturating_add(ai_used)
                    .to_string(),
            ),
        })
    });
    Json(FederatedSearchResponse {
        page: FederatedSearchPage {
            hits,
            next_cursor,
            updating_workspaces,
            request_seq,
            corrected_query,
        },
        facets,
        recent_queries,
        sources,
    })
    .into_response()
}

async fn query_store(
    state: Arc<GatewayState>,
    principal: GatewayPrincipal,
    request: SearchRequest,
) -> Result<SearchResponse, &'static str> {
    let body = serde_json::to_vec(&request).map_err(|_| "invalid_request")?;
    let ask = Request::builder()
        .method("POST")
        .uri("/v1/search")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .map_err(|_| "invalid_request")?;
    let response = state
        .store
        .request(ask, store_caller(&principal))
        .await
        .map_err(store_worker_code)?;
    if !response.status().is_success() {
        return Err("store_search_failed");
    }
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|_| "store_search_failed")?;
    serde_json::from_slice(&bytes).map_err(|_| "store_search_failed")
}

async fn query_ai(
    state: Arc<GatewayState>,
    principal: GatewayPrincipal,
    request: koru_ai_wire::AiSearchRequest,
) -> Result<koru_ai_wire::AiSearchResponse, &'static str> {
    if !principal.scopes.iter().any(|scope| scope == "ai.chat") {
        return Err("forbidden");
    }
    let Some(client) = state.ai_client() else {
        return Err("ai_unavailable");
    };
    let context = koru_ai_api::RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: principal.label,
            scopes: vec![koru_ai_api::AiScope::Chat],
        },
        trace_id: uuid::Uuid::new_v4().to_string(),
        deadline_millis: Some(
            now_millis()
                .saturating_add(i64::try_from(SEARCH_DEADLINE.as_millis()).unwrap_or(i64::MAX)),
        ),
    };
    tokio::task::spawn_blocking(move || client.search(context, request))
        .await
        .map_err(|_| "ai_unavailable")?
        .map_err(|failure| match failure.code.as_str() {
            "forbidden" => "forbidden",
            "invalid_cursor" => "invalid_cursor",
            "invalid_request" => "invalid_request",
            _ => "ai_search_failed",
        })
}

fn store_caller(principal: &GatewayPrincipal) -> koru_store_api::auth::Caller {
    let mut scopes: Vec<_> = principal
        .scopes
        .iter()
        .filter_map(|scope| scope.strip_prefix("store."))
        .filter_map(koru_store_core::model::Scope::parse)
        .collect();
    // **The one scope that is not a `store.` name.** `workspace.destroy` gates
    // the Gateway's own lifecycle operation as well as the two Store routes
    // that were its bypass, so it is not prefixed — and a prefix-stripping
    // filter would silently drop it, which would leave the lower routes open
    // to exactly the caller the split exists to stop. Mapped by hand, and
    // pinned by `the_destructive_scope_has_one_spelling_on_both_sides`.
    if principal
        .scopes
        .iter()
        .any(|scope| scope == WORKSPACE_DESTROY_SCOPE)
    {
        scopes.push(koru_store_core::model::Scope::DestroyWorkspaces);
    }
    // The same by hand, and for the same reason: `workspace.migrate` gates the
    // Gateway's migration operation as well as the Store route beneath it, so
    // it carries no `store.` prefix for the strip above to find. Pinned by
    // `the_migration_scope_has_one_spelling_on_both_sides`.
    if principal
        .scopes
        .iter()
        .any(|scope| scope == WORKSPACE_MIGRATE_SCOPE)
    {
        scopes.push(koru_store_core::model::Scope::MigrateWorkspaces);
    }
    koru_store_api::auth::Caller {
        label: principal.label.clone(),
        scopes,
    }
}

fn store_worker_code(error: WorkerError) -> &'static str {
    match error {
        WorkerError::Saturated => "gateway_saturated",
        WorkerError::TimedOut => "deadline_exceeded",
        _ => "store_unavailable",
    }
}

fn source_status(
    source: SearchSource,
    state: SearchSourceState,
    error_code: Option<&str>,
) -> SearchSourceStatus {
    SearchSourceStatus {
        source,
        state,
        error_code: error_code.map(str::to_owned),
    }
}

fn encode_search_cursor(cursor: &SearchCursor) -> String {
    serde_json::to_vec(cursor)
        .expect("search cursor serializes")
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_search_cursor(cursor: &str) -> Result<SearchCursor, ()> {
    if cursor.len() % 2 != 0 || cursor.len() > 4_096 {
        return Err(());
    }
    let bytes = (0..cursor.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&cursor[index..index + 2], 16).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    // This is the gateway-owned prefix shared by Store and AI error readers.
    // Both domain types accept additive fields, so authentication and gateway
    // availability failures retain one wire shape without copying either
    // domain's error vocabulary into the other.
    let mut response = Json(serde_json::json!({
        "code": code,
        "message": message,
        "waiting_on": [],
        "retryable": status.is_server_error(),
    }))
    .into_response();
    *response.status_mut() = status;
    response
}

pub fn public_openapi() -> serde_json::Value {
    let mut document = koru_store_api::api_catalog::openapi(
        koru_store_api::api_catalog::Audience::Public,
        koru_api_wire::API_VERSION,
    );
    document["info"]["title"] = serde_json::json!("Koru API Gateway");
    document["info"]["description"] = serde_json::json!(
        "The single public API for Koru product clients. Store and AI operations preserve their domain wire contracts behind one authenticated gateway."
    );
    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let gateway_status = schemas.subschema_for::<GatewayStatus>();
    let federated_request = schemas.subschema_for::<FederatedSearchRequest>();
    let federated_response = schemas.subschema_for::<FederatedSearchResponse>();
    document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas")
        .extend(
            schemas
                .take_definitions(true)
                .into_iter()
                .map(|(name, schema)| {
                    (
                        name,
                        serde_json::to_value(schema).expect("gateway status schema serializes"),
                    )
                }),
        );
    let store_status = document["paths"]["/v1/status"].clone();
    document["paths"]["/v1/store/status"] = store_status;
    document["paths"]["/v1/store/status"]["get"]["operationId"] = serde_json::json!("store_status");
    document["paths"]["/v1/status"]["get"]["responses"]["200"]["content"]["application/json"]["schema"] =
        serde_json::to_value(gateway_status).expect("gateway status reference serializes");
    let store_search = document["paths"]["/v1/search"].clone();
    document["paths"]["/v1/store/search"] = store_search;
    document["paths"]["/v1/store/search"]["post"]["operationId"] =
        serde_json::json!("store_search");
    document["paths"]["/v1/search"]["post"] = serde_json::json!({
        "operationId": "federated_search",
        "summary": "Search Store content and finalized AI chats",
        "description": "Queries eligible sources concurrently, preserves partial results and source-labelled failures, and returns a deterministic merged page.",
        "security": [{"bearerAuth": ["store.read_documents"]}],
        "requestBody": {"required": true, "content": {"application/json": {"schema": federated_request}}},
        "responses": {
            "200": {"description": "A merged page plus per-source status", "content": {"application/json": {"schema": federated_response}}},
            "400": {"description": "The request or opaque cursor is invalid"},
            "401": {"description": "The gateway bearer is absent or invalid"}
        }
    });
    document["paths"]["/v1/openapi.json"]["get"]["security"] = serde_json::json!([]);
    ai::extend_openapi(&mut document);
    artifacts::extend_openapi(&mut document);
    runs::extend_openapi(&mut document);
    schedules::extend_openapi(&mut document);
    run_mcp::extend_openapi(&mut document);
    settings::extend_openapi(&mut document);
    workspaces::extend_openapi(&mut document);
    let paths = document["paths"]
        .as_object_mut()
        .expect("Store OpenAPI paths");
    for (path, summary) in [
        ("/docs", "Offline HTTP API documentation"),
        ("/docs/asyncapi", "Offline streaming API documentation"),
        (
            "/v1/asyncapi.json",
            "The generated public AsyncAPI contract",
        ),
    ] {
        paths.insert(path.into(), serde_json::json!({ "get": { "summary": summary, "security": [], "responses": {"200": {"description": summary}}}}));
    }
    // The events worker's public routes, from its own route table — the same
    // one its router is folded from, so the document cannot describe a path
    // the worker does not serve. Schemas come from the koru-events wire
    // types, which is what makes a renamed field break this document too.
    for route in koru_events_server::routes::ROUTES
        .iter()
        .filter(|route| route.public)
    {
        let method = route.method.to_ascii_lowercase();
        let entry = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| serde_json::json!({}));
        entry[method] = serde_json::json!({
            "summary": route.summary,
            "responses": {"200": {"description": route.summary}},
        });
    }
    // Use the same OpenAPI generator as the other gateway domains. Inserting
    // `schema_for!` roots directly leaves their nested references under
    // `#/$defs`, but those definitions are not rooted at the final document;
    // clients then receive unresolved schemas. The OpenAPI generator emits
    // component references and lets us fold every definition into the one
    // shared component map.
    let mut event_schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let event_batch = event_schemas.subschema_for::<koru_events::wire::EventBatch>();
    let append_receipt = event_schemas.subschema_for::<koru_events::wire::AppendReceipt>();
    let event_page = event_schemas.subschema_for::<koru_events::wire::EventPage>();
    let activity_page = event_schemas.subschema_for::<koru_events::wire::ActivityPage>();
    document["components"]["schemas"]
        .as_object_mut()
        .expect("Store OpenAPI schemas")
        .extend(
            event_schemas
                .take_definitions(true)
                .into_iter()
                .map(|(name, schema)| {
                    (
                        name,
                        serde_json::to_value(schema).expect("event schema serializes"),
                    )
                }),
        );
    document["paths"]["/v1/events"]["post"]["requestBody"] = serde_json::json!({
        "content": {"application/json": {"schema": event_batch}}
    });
    document["paths"]["/v1/events"]["post"]["responses"]["200"]["content"] = serde_json::json!({
        "application/json": {"schema": append_receipt}
    });
    document["paths"]["/v1/events"]["get"]["responses"]["200"]["content"] = serde_json::json!({
        "application/json": {"schema": event_page}
    });
    // The per-subject activity read. Its parameters are described here rather
    // than derived, because the route table carries a path and a summary and
    // deliberately not a query grammar — the events worker's table is the
    // scaled-down catalog, and growing it a parameter column for one route
    // would be the third copy the store's own catalog exists to avoid.
    document["paths"]["/v1/events/activity"]["get"]["parameters"] = serde_json::json!([
        {"name": "kind", "in": "query", "required": true, "description": "The subject kind: workspace, document or task.", "schema": {"type": "string", "enum": ["workspace", "document", "task"]}},
        {"name": "id", "in": "query", "required": true, "description": "The subject's id as the store spells it; a task is the four-segment task:<workspace>:<document>:<node> reference.", "schema": {"type": "string"}},
        {"name": "after", "in": "query", "required": false, "description": "Journal cursor; 0 or absent starts at the oldest event still retained.", "schema": {"type": "integer", "format": "int64", "minimum": 0}},
        {"name": "limit", "in": "query", "required": false, "description": "Rows per page; clamped server-side.", "schema": {"type": "integer", "minimum": 1, "maximum": koru_events::wire::MAX_ACTIVITY_LIMIT, "default": koru_events::wire::DEFAULT_ACTIVITY_LIMIT}}
    ]);
    document["paths"]["/v1/events/activity"]["get"]["responses"]["200"]["content"] = serde_json::json!({
        "application/json": {"schema": activity_page}
    });
    document["paths"]["/v1/events/activity"]["get"]["responses"]["400"] = serde_json::json!({
        "description": "The subject kind is not one of the closed set, or the id is empty"
    });
    koru_session_api::extend_openapi(&mut document);
    document
}

pub fn public_asyncapi() -> serde_json::Value {
    // Each channel names the server it is reached on: `/v1/sync` is a
    // WebSocket (`local`), and the SSE streams are long-lived HTTP responses
    // (`localSse`), the same server the session streams name. A channel with
    // no `servers` would be read as available on every server, `ws` included.
    let channels = koru_api_wire::STREAM_CHANNELS.iter().map(|channel| {
        let server = match channel.protocol {
            "sse" => "#/servers/localSse",
            _ => "#/servers/local",
        };
        (channel.path.to_owned(), serde_json::json!({
            "address": channel.path,
            "servers": [{"$ref": server}],
            "messages": channel.messages.iter().map(|message| (message.name.to_owned(), serde_json::json!({"title": message.name, "summary": message.description, "x-direction": message.direction}))).collect::<serde_json::Map<_,_>>(),
            "x-protocol": channel.protocol, "x-version": channel.version,
            "x-ordered": channel.ordered, "x-backpressured": channel.backpressured,
        }))
    }).collect::<serde_json::Map<_,_>>();
    let mut document = serde_json::json!({
        "asyncapi": "3.0.0",
        "info": {"title": "Koru API Gateway streams", "version": koru_api_wire::ASYNC_API_VERSION},
        "servers": {
            "local": {"host": "127.0.0.1", "protocol": "ws", "security": [{"$ref": "#/components/securitySchemes/bearerAuth"}]},
            "localSse": {"host": "127.0.0.1", "protocol": "http", "security": [{"$ref": "#/components/securitySchemes/bearerAuth"}]}
        },
        "channels": channels,
        "components": {"securitySchemes": {"bearerAuth": {"type": "httpApiKey", "in": "header", "name": "Authorization"}}}
    });
    koru_session_api::extend_asyncapi(&mut document);
    document
}

/// A self-contained projection of the public OpenAPI document. Generated once
/// per process: opening documentation never walks catalogs on every request.
pub fn public_openapi_viewer() -> &'static str {
    static VIEWER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VIEWER.get_or_init(|| docs_viewer::openapi(&public_openapi()))
}

/// A self-contained projection of the public AsyncAPI document.
pub fn public_asyncapi_viewer() -> &'static str {
    static VIEWER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VIEWER.get_or_init(|| docs_viewer::asyncapi(&public_asyncapi()))
}

pub fn markdown_reference() -> String {
    let mut output =
        String::from("# Koru API reference\n\nGenerated from the route catalog. Do not edit.\n\n");
    output.push_str("## POST `/v1/search`\n\nSearch Store content and finalized AI chats concurrently, retaining typed source identity and partial-failure status.\n\nReturns: A deterministic federated result page and source statuses.\n\n");
    output.push_str("## POST `/v1/store/search`\n\nSearch only Store content through the public gateway.\n\nReturns: The Store search response.\n\n");
    output.push_str("## GET `/v1/store/status`\n\nRead the Store's own fine-grained capability list through the public gateway.\n\nReturns: The Store status response.\n\n");
    for route in koru_store_api::api_catalog::ROUTES.iter().filter(|route| {
        route.audience == koru_store_api::api_catalog::Audience::Public
            && route.path != "/v1/search"
    }) {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\n",
            route.method, route.path, route.summary, route.returns
        ));
    }
    for route in ai::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\n",
            route.method, route.path, route.summary, route.returns
        ));
    }
    for route in artifacts::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\nRequires: `{}` and the `{}` capability.\n\n",
            route.method,
            route.path,
            route.summary,
            route.returns,
            route.scope.as_str(),
            koru_artifact_wire::CAPABILITY,
        ));
    }
    for route in runs::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\nRequires: `{}` and the `{}` capability.\n\n",
            route.method,
            route.path,
            route.summary,
            route.returns,
            route.scope.as_str(),
            koru_delegate_api::CAPABILITY,
        ));
    }
    for route in schedules::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\nRequires: `{}` and the `{}` capability.\n\n",
            route.method,
            route.path,
            route.summary,
            route.returns,
            route.scope.as_str(),
            koru_delegate_api::CAPABILITY,
        ));
    }
    for route in settings::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\nRequires: `{}`.\n\n",
            route.method, route.path, route.summary, route.returns, route.scope
        ));
    }
    for route in workspaces::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nReturns: {}\n\nRequires: `{}`.\n\n",
            route.method, route.path, route.summary, route.returns, route.scope
        ));
    }
    for route in koru_events_server::routes::ROUTES
        .iter()
        .filter(|route| route.public)
    {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\n",
            route.method, route.path, route.summary
        ));
    }
    for route in koru_session_api::ROUTES {
        output.push_str(&format!(
            "## {} `{}`\n\n{}\n\nRequires: `{}` and the `{}` capability.\n\n",
            route.method.as_str(),
            route.path,
            route.summary,
            route.scopes.join("`, `"),
            koru_session_api::CAPABILITY,
        ));
    }
    output.push_str("## GET `/docs`\n\nOffline HTTP API documentation.\n\n## GET `/docs/asyncapi`\n\nOffline streaming API documentation.\n\n## GET `/v1/asyncapi.json`\n\nGenerated AsyncAPI 3.0 contract.\n");
    output
}

pub fn internal_markdown_reference() -> String {
    let mut output = String::from(
        "# Koru API internal reference\n\nGenerated from the route catalog. Do not edit.\n\n",
    );
    for route in koru_store_api::api_catalog::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({:?})\n",
            route.method, route.path, route.summary, route.audience
        ));
    }
    for route in koru_session_api::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method.as_str(),
            route.path,
            route.summary,
            route.id.operation_id(),
        ));
    }
    for route in ai::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} (Public)\n",
            route.method, route.path, route.summary
        ));
    }
    for route in artifacts::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method,
            route.path,
            route.summary,
            route.scope.as_str()
        ));
    }
    for route in runs::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method,
            route.path,
            route.summary,
            route.scope.as_str()
        ));
    }
    for route in schedules::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method,
            route.path,
            route.summary,
            route.scope.as_str()
        ));
    }
    for route in settings::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method, route.path, route.summary, route.scope
        ));
    }
    for route in workspaces::ROUTES {
        output.push_str(&format!(
            "- {} `{}` — {} ({})\n",
            route.method, route.path, route.summary, route.scope
        ));
    }
    output
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn state() -> (tempfile::TempDir, Arc<GatewayState>, String) {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let state = GatewayState::new("internal-debug".into(), worker);
        let (token, _) = state.sessions.mint("test", &[], now_millis());
        (root, state, token)
    }

    fn state_with_ai() -> (tempfile::TempDir, Arc<GatewayState>, String) {
        let root = tempfile::tempdir().unwrap();
        let store = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
            root.path().to_path_buf(),
            "internal-debug",
        ))
        .spawn()
        .unwrap();
        let ai = koru_ai_server::AiWorker::new(koru_ai_server::WorkerConfig::deterministic_mock(
            root.path().join("ai"),
            "internal-debug",
        ))
        .spawn()
        .unwrap();
        let state = GatewayState::with_ai("internal-debug".into(), store, ai);
        let (token, _) = state.sessions.mint("test", &[], now_millis());
        (root, state, token)
    }

    fn state_with_production_ai() -> (tempfile::TempDir, Arc<GatewayState>) {
        let root = tempfile::tempdir().unwrap();
        let store = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
            root.path().to_path_buf(),
            "internal-debug",
        ))
        .spawn()
        .unwrap();
        let ai = koru_ai_server::AiWorker::new(
            koru_ai_server::WorkerConfig::production(root.path().join("ai"), "internal-debug")
                .unwrap(),
        )
        .spawn()
        .unwrap();
        (
            root,
            GatewayState::with_ai("internal-debug".into(), store, ai),
        )
    }

    #[tokio::test]
    async fn gateway_authenticates_and_routes_store_without_internal_http() {
        let (_root, state, token) = state();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/profiles")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The desktop gateway does not serve the Cloud account API
    /// (docs/developers/API.md): its paths are unknown paths here,
    /// admitted like any other and answered by the Store fallback, and the
    /// `account` capability is never published.
    #[tokio::test]
    async fn account_paths_are_not_served_and_the_capability_is_never_published() {
        let (_root, state, token) = state();
        let capabilities = state.public_capabilities();
        assert!(
            !capabilities
                .iter()
                .any(|capability| capability == "account"),
            "{capabilities:?}"
        );
        for path in [
            "/v1/account",
            "/v1/account/sessions",
            "/v1/oauth/google/start",
        ] {
            let unauthenticated = router(Arc::clone(&state))
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED, "{path}");
            let authenticated = router(Arc::clone(&state))
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(authenticated.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let openapi = public_openapi();
        let paths = openapi["paths"].as_object().unwrap();
        assert!(
            !paths
                .keys()
                .any(|path| path.starts_with("/v1/account") || path.starts_with("/v1/oauth/")),
        );
    }

    #[tokio::test]
    async fn namespaced_store_status_preserves_fine_grained_capabilities() {
        let (_root, state, token) = state();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/store/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let status: koru_store_wire::Status = serde_json::from_slice(&bytes).unwrap();
        assert!(
            status
                .capabilities
                .iter()
                .any(|value| value == "document-create-trashed-at")
        );
    }

    #[tokio::test]
    async fn federated_search_keeps_store_results_when_ai_is_unavailable() {
        let (_root, state, token) = state();
        let body = serde_json::to_vec(&FederatedSearchRequest::default()).unwrap();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let answer: FederatedSearchResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(answer.sources.len(), 2);
        assert_eq!(answer.sources[0].source, SearchSource::Store);
        assert_eq!(answer.sources[0].state, SearchSourceState::Complete);
        assert_eq!(answer.sources[1].source, SearchSource::Ai);
        assert_eq!(answer.sources[1].state, SearchSourceState::Failed);
    }

    #[tokio::test]
    async fn excluding_ai_skips_that_source_and_reports_it_disabled() {
        let (_root, state, token) = state_with_ai();
        let body = serde_json::to_vec(&FederatedSearchRequest {
            include_ai: false,
            ..FederatedSearchRequest::default()
        })
        .unwrap();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let answer: FederatedSearchResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(answer.sources[1].source, SearchSource::Ai);
        assert_eq!(answer.sources[1].state, SearchSourceState::Disabled);
        assert!(
            answer
                .page
                .hits
                .iter()
                .all(|hit| matches!(hit, FederatedSearchHit::Store { .. }))
        );
    }

    #[tokio::test]
    async fn old_store_bearer_is_refused_by_gateway() {
        let (_root, state, _token) = state();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, "Bearer store-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn shutdown_requires_the_gateway_control_scope() {
        let (_root, state, ordinary) = state();
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/shutdown")
                    .header(header::AUTHORIZATION, format!("Bearer {ordinary}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let (control, _) = state
            .sessions
            .mint("control", &[CONTROL_SCOPE.into()], now_millis());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/shutdown")
                    .header(header::AUTHORIZATION, format!("Bearer {control}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_idle_only_shutdown_is_the_workers_answer() {
        let (_root, state, _) = state();
        let (control, _) = state
            .sessions
            .mint("control", &[CONTROL_SCOPE.into()], now_millis());
        // Nothing has joined a room, so the worker's own count is zero and the
        // stop is granted — which proves the decision crossed to the worker's
        // route. A gateway answering from its own guesswork has no count to
        // consult and the declined path in the sync integration test (a stop
        // asked while two sockets are live) would never fire.
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/shutdown?only_if_idle=true")
                    .header(header::AUTHORIZATION, format!("Bearer {control}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let stopping: koru_store_wire::Stopping = serde_json::from_slice(&bytes).unwrap();
        assert!(stopping.stopping, "an idle worker grants the stop");
    }

    /// **A gateway with an attached window is not idle**, and the window that
    /// proved it was a Koru Terminal: it joins no sync room, so the Store
    /// worker's count is zero while its banner catalog is very much alive on
    /// `/v1/events/stream`. The gateway stopped underneath a working terminal.
    ///
    /// The subscriber count is the events worker's own, incremented by the
    /// stream handler's guard and decremented when it drops — armed here
    /// directly so the assertion is about the *decision*, not about holding an
    /// SSE body open inside a unit test. The end-to-end shape (a real client
    /// holding a catalog watch) is the gateway integration test.
    #[tokio::test]
    async fn a_live_event_stream_declines_an_idle_only_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let events = koru_events_server::EventsHandle::open(
            koru_events_server::WorkerConfig::quiet(root.path().join("events")),
        )
        .unwrap();
        let state = GatewayState::new_with_events("internal-debug".into(), worker, Some(events));
        let (control, _) = state
            .sessions
            .mint("control", &[CONTROL_SCOPE.into()], now_millis());

        let ask = || {
            router(Arc::clone(&state)).oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/shutdown?only_if_idle=true")
                    .header(header::AUTHORIZATION, format!("Bearer {control}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let stopping = |response: Response| async move {
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            serde_json::from_slice::<koru_store_wire::Stopping>(&bytes)
                .unwrap()
                .stopping
        };

        let watching = &state.events.as_ref().unwrap().state.sse_subscribers;
        watching.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(state.attached_event_streams(), 1);
        assert!(
            !stopping(ask().await.unwrap()).await,
            "a client holding a catalog watch must keep the gateway up"
        );

        watching.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(state.attached_event_streams(), 0);
        assert!(
            stopping(ask().await.unwrap()).await,
            "dropping the last watch lets the stop through"
        );
    }

    /// **This suite opens a real events worker, so its `no-sink` guard has to
    /// be armed.** Without the feature in `[dev-dependencies]` the worker would
    /// construct the configured telemetry sink and post these fixtures into the
    /// shared analytics project — and `cfg(test)` cannot say so, because a
    /// dependency is never compiled with it (the `no-reader` lesson). This is
    /// the test that fails if somebody tidies the feature out of Cargo.toml.
    #[test]
    fn this_suite_sends_no_telemetry() {
        assert!(!koru_events_server::forward::sends_by_default());
        assert!(koru_events_server::forward::configured_sink().is_none());
    }

    /// The activity read is served **by the events worker, through the
    /// gateway's `/v1/events/{*rest}` mount** — not by the store catalog. This
    /// is the test that says so: it drives the real URL a pane would call and
    /// asserts the answer is the worker's page shape, and that the route takes
    /// the `events.read` scope every other events GET already takes. SPEC §10
    /// is one trusted user with peered devices; a task read invents no scope of
    /// its own.
    #[tokio::test]
    async fn the_activity_read_is_the_events_workers_and_rides_its_scope() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let events = koru_events_server::EventsHandle::open(
            koru_events_server::WorkerConfig::quiet(root.path().join("events")),
        )
        .unwrap();
        let state =
            GatewayState::new_with_events("internal-debug".into(), worker, Some(events.clone()));
        let (token, _) = state.sessions.mint("test", &[], now_millis());
        let path = "/v1/events/activity?kind=task&id=task%3Aa%3Ab%3Ac&after=0&limit=10";

        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let page: koru_events::wire::ActivityPage = serde_json::from_slice(&bytes).unwrap();
        assert!(page.entries.is_empty(), "a task with no history is empty");
        assert_eq!(page.retention_days, 90, "and says how far back it reaches");

        // The store's own scope is not the events scope, and the mount sits
        // ahead of the store fallback — so this is a 403 from the events
        // forward, never a 404 from the store.
        let (store_only, _) =
            state
                .sessions
                .mint("store-only", &["store.read_documents".into()], now_millis());
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {store_only}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn events_routes_reach_the_embedded_events_worker() {
        let root = tempfile::tempdir().unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug"),
        )
        .spawn()
        .unwrap();
        let events = koru_events_server::EventsHandle::open(
            koru_events_server::WorkerConfig::quiet(root.path().join("events")),
        )
        .unwrap();
        let state =
            GatewayState::new_with_events("internal-debug".into(), worker, Some(events.clone()));
        let (token, _) = state.sessions.mint("test", &[], now_millis());

        // Publish through the gateway, exactly as a product client would.
        let batch = koru_events::wire::EventBatch {
            events: vec![
                koru_events::Draft::from(koru_events::SearchOpened {
                    surface: koru_events::SearchSurface::Window,
                })
                .into_envelope(&koru_events::EmitContext::user_session()),
            ],
        };
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/events")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&batch).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let receipt: koru_events::wire::AppendReceipt = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt.accepted, 1);

        // An unauthenticated read is refused before it reaches the worker.
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/v1/events?after=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // A session that asked for store scopes alone holds no events scope.
        let (store_only, _) =
            state
                .sessions
                .mint("store-only", &["store.read_documents".into()], now_millis());
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/v1/events?after=0")
                    .header(header::AUTHORIZATION, format!("Bearer {store_only}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // And a gateway with no worker answers 503, not 404: the API exists.
        let (root2, orphan, token2) = self::state();
        let response = router(orphan)
            .oneshot(
                Request::builder()
                    .uri("/v1/events?after=0")
                    .header(header::AUTHORIZATION, format!("Bearer {token2}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(root2);
    }

    #[test]
    fn lifecycle_control_is_only_granted_when_explicitly_requested() {
        assert!(
            !effective_scopes(&[], Admission::Ordinary)
                .iter()
                .any(|scope| scope == CONTROL_SCOPE)
        );
        assert_eq!(
            effective_scopes(&[CONTROL_SCOPE.into()], Admission::Ordinary),
            vec![CONTROL_SCOPE.to_owned()]
        );
        for scope in koru_session_api::ALL_SCOPES {
            assert!(
                effective_scopes(&[], Admission::Ordinary)
                    .iter()
                    .any(|held| held == scope)
            );
        }
        assert!(
            !effective_scopes(&[], Admission::Ordinary).contains(&"ai.manage_providers".into())
        );
        assert_eq!(
            effective_scopes(&["ai.manage_providers".into()], Admission::Ordinary),
            vec!["ai.manage_providers".to_owned()]
        );
    }

    #[tokio::test]
    async fn every_session_catalog_route_is_mounted_but_capability_disabled() {
        let (_root, state, token) = state();
        let session_id = "11111111-1111-4111-8111-111111111111";
        let attachment_id = "22222222-2222-4222-8222-222222222222";
        for route in koru_session_api::ROUTES {
            let path = route
                .path
                .replace("{session_id}", session_id)
                .replace("{attachment_id}", attachment_id);
            let response = router(Arc::clone(&state))
                .oneshot(
                    Request::builder()
                        .method(route.method.as_str())
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{} {}",
                route.method.as_str(),
                route.path
            );
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let failure: koru_store_wire::Failure = serde_json::from_slice(&body).unwrap();
            assert_eq!(failure.code, "capability_unavailable");
        }
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let status: GatewayStatus = serde_json::from_slice(&body).unwrap();
        assert!(!status.capabilities.iter().any(|value| value == "sessions"));
    }

    #[tokio::test]
    async fn session_routes_enforce_the_catalog_scope_before_capability() {
        let (_root, state, _) = state();
        let (metadata, _) = state.sessions.mint(
            "metadata",
            &[koru_session_api::METADATA_SCOPE.into()],
            now_millis(),
        );
        let allowed_read = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/v1/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {metadata}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed_read.status(), StatusCode::SERVICE_UNAVAILABLE);

        for (method, uri) in [
            ("POST", "/v1/sessions"),
            (
                "GET",
                "/v1/sessions/11111111-1111-4111-8111-111111111111/subscribe",
            ),
            (
                "GET",
                "/v1/sessions/11111111-1111-4111-8111-111111111111/attachments",
            ),
        ] {
            let response = router(Arc::clone(&state))
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {metadata}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let failure: koru_store_wire::Failure = serde_json::from_slice(&body).unwrap();
            assert_eq!(failure.code, "permission_denied");
        }
    }

    #[test]
    fn default_scopes_cover_chat_tools() {
        assert!(effective_scopes(&[], Admission::Ordinary).contains(&"ai.chat".to_owned()));
        for operation in [
            "ai.tool_list_chats",
            "ai.tool_search_chats",
            "ai.tool_read_chat",
        ] {
            let route = MOBILE_UNAVAILABLE_OPERATIONS
                .iter()
                .find(|candidate| candidate.id == operation)
                .expect("chat tool route is catalogued for non-mobile clients");
            assert!(route.http.starts_with("POST /v1/ai/tools/chats/"));
        }
    }

    #[tokio::test]
    async fn public_documentation_needs_no_gateway_credential() {
        let (_root, state, _token) = state();
        for path in [
            "/docs",
            "/docs/asyncapi",
            "/v1/openapi.json",
            "/v1/asyncapi.json",
        ] {
            let response = router(Arc::clone(&state))
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }

    #[tokio::test]
    async fn gateway_scopes_remain_store_handler_authority() {
        let (_root, state, _token) = state();
        let (token, _) =
            state
                .sessions
                .mint("read-only", &["store.read_documents".into()], now_millis());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/profiles")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"must not be created"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ai_capability_and_routes_appear_only_with_a_ready_worker() {
        let (_root, disabled, disabled_token) = state();
        let response = router(disabled)
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, format!("Bearer {disabled_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let status: GatewayStatus = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status.ai, DomainReadiness::Disabled);
        assert!(!status.capabilities.iter().any(|value| value == "ai"));

        let (_root, ready, ready_token) = state_with_ai();
        let response = router(ready)
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, format!("Bearer {ready_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let status: GatewayStatus = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status.ai, DomainReadiness::Ready);
        assert!(status.capabilities.iter().any(|value| value == "ai"));
    }

    #[tokio::test]
    async fn ai_requests_are_strict_and_scope_checked_before_dispatch() {
        let (_root, state, _) = state_with_ai();
        let (read_only, _) =
            state
                .sessions
                .mint("store-only", &["store.read_documents".into()], now_millis());
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/ai/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {read_only}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "chat_id": koru_ai_core::ChatId::new(),
                            "title": "Denied",
                            "model": {
                                "provider": "mock",
                                "requested_model": "deterministic-v1",
                                "default_reasoning": "default"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let (ai_token, _) = state.sessions.mint("ai", &["ai.chat".into()], now_millis());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/ai/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {ai_token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "chat_id": koru_ai_core::ChatId::new(),
                            "title": "Strict",
                            "model": {
                                "provider": "mock",
                                "requested_model": "deterministic-v1",
                                "default_reasoning": "default"
                            },
                            "unknown": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn private_chat_is_user_visible_but_absent_from_public_tool_routes() {
        let (_root, state, token) = state_with_ai();
        let chat_id = koru_ai_core::ChatId::new();
        let authorized = |builder: axum::http::request::Builder| {
            builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
        };
        let create = router(Arc::clone(&state))
            .oneshot(
                authorized(Request::builder().method("POST").uri("/v1/ai/chats"))
                    .body(Body::from(
                        serde_json::json!({
                            "chat_id": chat_id,
                            "title": "Private route fixture",
                            "model": {
                                "provider": "mock",
                                "requested_model": "deterministic-v1",
                                "default_reasoning": "default"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);

        let privacy = router(Arc::clone(&state))
            .oneshot(
                authorized(
                    Request::builder()
                        .method("PATCH")
                        .uri(format!("/v1/ai/chats/{chat_id}/privacy")),
                )
                .body(Body::from(r#"{"private":true}"#))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(privacy.status(), StatusCode::OK);

        let user_list = router(Arc::clone(&state))
            .oneshot(
                authorized(Request::builder().uri("/v1/ai/chats"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(user_list.status(), StatusCode::OK);
        let user_bytes = axum::body::to_bytes(user_list.into_body(), 64 * 1024)
            .await
            .unwrap();
        let user_chats: koru_ai_wire::ListChatsResponse =
            serde_json::from_slice(&user_bytes).unwrap();
        assert!(user_chats.chats.iter().any(|chat| chat.id == chat_id));

        let tool_list = router(Arc::clone(&state))
            .oneshot(
                authorized(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/ai/tools/chats/list"),
                )
                .body(Body::from("{}"))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(tool_list.status(), StatusCode::OK);
        let tool_bytes = axum::body::to_bytes(tool_list.into_body(), 64 * 1024)
            .await
            .unwrap();
        let listed: koru_ai_wire::ListChatsToolResponse =
            serde_json::from_slice(&tool_bytes).unwrap();
        assert!(listed.chats.is_empty());

        let read = router(state)
            .oneshot(
                authorized(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/ai/tools/chats/read"),
                )
                .body(Body::from(
                    serde_json::json!({ "chat_id": chat_id }).to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::NOT_FOUND);
        let read_bytes = axum::body::to_bytes(read.into_body(), 64 * 1024)
            .await
            .unwrap();
        let failure: koru_ai_wire::Failure = serde_json::from_slice(&read_bytes).unwrap();
        assert_eq!(failure.code, "chat_not_found");
    }

    #[tokio::test]
    async fn private_usage_routes_require_their_own_scope() {
        let (_root, state, _) = state_with_ai();
        let (chat_only, _) = state
            .sessions
            .mint("chat-only", &["ai.chat".into()], now_millis());
        let denied = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/v1/ai/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {chat_only}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let (usage, _) = state
            .sessions
            .mint("usage", &["ai.read_usage".into()], now_millis());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/ai/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {usage}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let report: koru_ai_wire::UsageResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report.report.chat_id, None);
        assert_eq!(report.report.current_chat, None);
        assert!(report.report.groups.is_empty());
    }

    #[tokio::test]
    async fn provider_settings_cross_the_public_gateway_without_secrets() {
        let (_root, state) = state_with_production_ai();
        let (token, _) = state.sessions.mint(
            "provider-manager",
            &["ai.manage_providers".into()],
            now_millis(),
        );
        let update = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/ai/providers/ollama")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "endpoint": "http://127.0.0.1:11434",
                            "model": "gpt-oss:20b",
                            "reasoning": "high"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(update.status(), StatusCode::OK);

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/ai/providers/settings")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let settings: koru_ai_wire::ProviderSettingsResponse =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            settings
                .selected_provider
                .as_ref()
                .map(koru_ai_core::AiProviderId::as_str),
            Some("ollama")
        );
        assert_eq!(settings.ollama.model.as_deref(), Some("gpt-oss:20b"));
        let serialized = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("key_account"));
    }

    #[test]
    fn documents_cover_catalog_and_streams_without_internal_routes() {
        let openapi = public_openapi();
        for route in koru_store_api::api_catalog::ROUTES
            .iter()
            .filter(|route| route.audience == koru_store_api::api_catalog::Audience::Public)
        {
            assert!(openapi["paths"].get(route.path).is_some(), "{}", route.path);
        }
        for route in ai::ROUTES {
            assert!(openapi["paths"].get(route.path).is_some(), "{}", route.path);
        }
        for route in artifacts::ROUTES {
            assert!(openapi["paths"].get(route.path).is_some(), "{}", route.path);
        }
        for route in runs::ROUTES {
            assert!(openapi["paths"].get(route.path).is_some(), "{}", route.path);
        }
        for route in schedules::ROUTES {
            assert!(openapi["paths"].get(route.path).is_some(), "{}", route.path);
        }
        assert!(openapi["paths"].get("/v1/debug/store").is_none());
        for path in [
            "/docs",
            "/docs/asyncapi",
            "/v1/openapi.json",
            "/v1/asyncapi.json",
        ] {
            assert_eq!(
                openapi["paths"][path]["get"]["security"],
                serde_json::json!([]),
                "{path}"
            );
        }
        let asyncapi = public_asyncapi();
        assert!(asyncapi["channels"].get("/v1/sync").is_some());
        for route in koru_session_api::ROUTES {
            let operation = &openapi["paths"][route.path][route.method.lower()];
            assert_eq!(operation["operationId"], route.id.operation_id());
        }
        assert!(
            asyncapi["channels"]
                .get("/v1/sessions/{session_id}/subscribe")
                .is_some()
        );
        assert!(
            asyncapi["channels"]
                .get("/v1/ai/chats/{chat}/turns")
                .is_some()
        );
        assert!(
            asyncapi["channels"]
                .get("/v1/ai/providers/ollama/pulls")
                .is_some()
        );
        // Every channel names the server its transport is on: SSE channels
        // never claim the WebSocket server.
        for (path, channel) in asyncapi["channels"].as_object().unwrap() {
            let server = channel["servers"][0]["$ref"].as_str().unwrap_or_default();
            let protocol = asyncapi["servers"][server.trim_start_matches("#/servers/")]["protocol"]
                .as_str()
                .unwrap_or_default();
            let expected = if path == "/v1/sync" { "ws" } else { "http" };
            assert_eq!(protocol, expected, "{path}");
        }
        assert_eq!(
            openapi["paths"]["/v1/status"]["get"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/GatewayStatus"
        );
        fn assert_refs_resolve(document: &serde_json::Value, value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(reference) = object.get("$ref").and_then(|value| value.as_str()) {
                        let name = reference
                            .strip_prefix("#/components/schemas/")
                            .expect("public schema refs stay in components");
                        assert!(
                            document["components"]["schemas"].get(name).is_some(),
                            "unresolved schema {reference}"
                        );
                    }
                    for child in object.values() {
                        assert_refs_resolve(document, child);
                    }
                }
                serde_json::Value::Array(values) => {
                    for child in values {
                        assert_refs_resolve(document, child);
                    }
                }
                _ => {}
            }
        }
        assert_refs_resolve(&openapi, &openapi);
    }

    #[test]
    fn checked_in_contracts_are_generated_from_the_live_catalogs() {
        let openapi: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/api/koru-api-gateway.openapi.json"
        ))
        .unwrap();
        let asyncapi: serde_json::Value = serde_json::from_str(include_str!(
            "../../../reference/api/koru-api-gateway.asyncapi.json"
        ))
        .unwrap();
        assert_eq!(openapi, public_openapi());
        assert_eq!(asyncapi, public_asyncapi());
        assert_eq!(
            include_str!("../../../reference/api/API_REFERENCE.md"),
            markdown_reference()
        );
        assert_eq!(
            include_str!("../../../reference/api/API_REFERENCE_INTERNAL.md"),
            internal_markdown_reference()
        );
        assert_eq!(
            include_str!("../../../reference/api/koru-api-mobile-ffi.md"),
            mobile_ffi_reference()
        );
    }

    #[tokio::test]
    async fn offline_viewers_embed_contracts_without_fetching_or_credentials() {
        let (_root, state, _token) = state();
        for path in ["/docs", "/docs/asyncapi"] {
            let response = router(Arc::clone(&state))
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .unwrap();
            let body = std::str::from_utf8(&body).unwrap();
            assert!(body.contains("Complete generated contract"), "{path}");
            assert!(!body.contains("fetch("), "{path}");
            assert!(!body.contains("sessionStorage"), "{path}");
            assert!(!body.contains("koru_api_"), "{path}");
        }
    }

    #[test]
    fn mobile_catalog_has_mechanical_unique_symbols_and_scope_admission() {
        let mut ids = std::collections::BTreeSet::new();
        let mut symbols = std::collections::BTreeSet::new();
        for operation in MOBILE_OPERATIONS {
            assert!(ids.insert(operation.id), "duplicate {}", operation.id);
            assert!(symbols.insert(operation.ffi), "duplicate {}", operation.ffi);
            let expected = if let Some(method) = operation.client.strip_prefix("client.store().") {
                format!("koru_api_store_{method}")
            } else if let Some(method) = operation.client.strip_prefix("client.") {
                format!("koru_api_{method}")
            } else {
                panic!("non-mechanical client spelling: {}", operation.client);
            };
            assert_eq!(operation.ffi, expected);
        }
        let mobile = MobileGateway::platform_local("android");
        assert!(mobile.admit("store.document").is_ok());
        assert_eq!(mobile.admit("ai.chat"), Err("operation_not_catalogued"));
        let projected_http = MOBILE_UNAVAILABLE_OPERATIONS
            .iter()
            .map(|operation| operation.http)
            .collect::<std::collections::BTreeSet<_>>();
        for route in ai::ROUTES {
            let route = format!("{} {}", route.method, route.path);
            assert!(
                projected_http.contains(route.as_str()),
                "missing mobile-unavailable projection for {route}"
            );
        }
    }

    #[test]
    fn mobile_catalog_is_callable_through_every_generated_platform_edge() {
        let proto = include_str!("../../../contracts/koru/api/v1/mobile.proto");
        let header = include_str!("../../koru-mobile-ios/include/koru_mobile.h");
        let ios = include_str!("../../koru-mobile-ios/src/lib.rs");
        let swift = include_str!("../../../apps/ios/KoruMobile/Native/KoruApiNative.swift");
        let android = include_str!("../../koru-mobile-native/src/lib.rs");
        let kotlin = include_str!(
            "../../../apps/android/app/src/main/java/app/koru/mobile/services/KoruApiNative.kt"
        );

        for operation in MOBILE_OPERATIONS {
            assert!(
                proto.contains(&format!("message {}", operation.request)),
                "missing request message for {}",
                operation.id
            );
            assert!(
                proto.contains(&format!("message {}", operation.response)),
                "missing response message for {}",
                operation.id
            );
            assert!(header.contains(operation.ffi), "header: {}", operation.ffi);
            assert!(ios.contains(operation.ffi), "iOS export: {}", operation.ffi);
            assert!(
                swift.contains(operation.ffi),
                "Swift call: {}",
                operation.ffi
            );
            assert!(
                android.contains(&format!("\"{}\" =>", operation.id)),
                "Android dispatch: {}",
                operation.id
            );
            assert!(
                kotlin.contains(&format!("\"{}\"", operation.id)),
                "Kotlin call: {}",
                operation.id
            );
        }
    }

    #[tokio::test]
    async fn runtime_documents_equal_the_checked_in_artifacts() {
        let (_root, state, token) = state();
        for (path, expected) in [
            ("/v1/openapi.json", public_openapi()),
            ("/v1/asyncapi.json", public_asyncapi()),
        ] {
            let response = router(Arc::clone(&state))
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn expiry_revocation_and_restart_invalidation_are_explicit() {
        let authority = SessionAuthority::default();
        let (token, expires) = authority.mint("test", &[], 100);
        assert!(authority.resolve(&token, expires - 1).is_some());
        assert!(authority.resolve(&token, expires).is_none());
        let (token, _) = authority.mint("test", &[], 100);
        assert!(authority.revoke(&token));
        assert!(authority.resolve(&token, 101).is_none());
        assert!(SessionAuthority::default().resolve(&token, 101).is_none());
    }

    // ------------------------------------------- the catalog, and who may move it

    /// A gateway whose Store worker also holds the events journal — the
    /// production wiring, which the older helpers above deliberately do not
    /// have. `with_events` is what gives `koru-store-api`'s handlers somewhere
    /// to publish a committed catalog fact.
    fn state_with_catalog_events() -> (tempfile::TempDir, Arc<GatewayState>, String) {
        let root = tempfile::tempdir().unwrap();
        let events = koru_events_server::EventsHandle::open(
            koru_events_server::WorkerConfig::quiet(root.path().join("events")),
        )
        .unwrap();
        let worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::embedded(root.path().to_path_buf(), "internal-debug")
                .with_events(events.clone()),
        )
        .spawn()
        .unwrap();
        let state =
            GatewayState::new_with_events("internal-debug".into(), worker, Some(events.clone()));
        let (token, _) = state.sessions.mint("test", &[], now_millis());
        (root, state, token)
    }

    async fn call(
        state: &Arc<GatewayState>,
        token: &str,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router(Arc::clone(state)).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Every fact in the journal, oldest first, as `(name, envelope)`.
    async fn journal(state: &Arc<GatewayState>, token: &str) -> Vec<(String, serde_json::Value)> {
        let (status, page) = call(state, token, "GET", "/v1/events?after=0&limit=1000", None).await;
        assert_eq!(status, StatusCode::OK);
        page["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|stored| {
                (
                    stored["envelope"]["name"].as_str().unwrap().to_owned(),
                    stored["envelope"].clone(),
                )
            })
            .collect()
    }

    fn names(journal: &[(String, serde_json::Value)]) -> Vec<&str> {
        journal.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// The Store's *published* facts, with the worker's derived ones left out.
    ///
    /// `workspace.observed` and the `document.*` rows are derived from the
    /// projection's commit feed on the projector's own thread, so they arrive
    /// whenever the next sweep does and interleave differently from run to run.
    /// That is exactly why the catalog facts are published rather than derived,
    /// and this filter is the evidence: a menu cannot be told to refresh by
    /// something that has not happened yet.
    fn published(journal: &[(String, serde_json::Value)]) -> Vec<&str> {
        names(journal)
            .into_iter()
            .filter(|name| name.starts_with("profile.") || name.starts_with("workspace.catalog."))
            .collect()
    }

    /// **The whole producer contract, in one pass over the catalog routes.**
    ///
    /// Each mutation publishes exactly one fact — two only where a committed
    /// appearance write also flipped `archived`, which is a second thing that
    /// happened rather than a second report of the first — and it publishes it
    /// *after* the commit, which is why every name below is preceded in the
    /// journal by nothing about the request and followed by a catalog that
    /// already has the change.
    #[tokio::test]
    async fn every_committed_catalog_mutation_publishes_its_fact_once() {
        let (_root, state, token) = state_with_catalog_events();

        let (status, profile) = call(
            &state,
            &token,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada", "locale": "en" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        assert_eq!(
            names(&journal(&state, &token).await),
            ["profile.created"],
            "the fact is in the journal by the time the route has answered"
        );

        let (status, workspace) = call(
            &state,
            &token,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile_id, "name": "Notes" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();

        // The four rows below that *remove* a workspace need the destructive
        // scope, which since SP-8a no default token holds. Minted vouched, the
        // way a first-party window's is: this test is about facts, and the
        // authorization matrix is asserted next door.
        let (destroyer, _) = state.sessions.mint_admitted(
            "koru",
            &[
                WORKSPACE_DESTROY_SCOPE.into(),
                MANAGE_WORKSPACES_SCOPE.into(),
                EVENTS_READ_SCOPE.into(),
            ],
            Admission::Vouched,
            now_millis(),
        );
        for (method, uri, body, destructive) in [
            (
                "PATCH",
                format!("/v1/profiles/{profile_id}"),
                Some(serde_json::json!({ "name": "Ada L" })),
                false,
            ),
            (
                "PATCH",
                format!("/v1/profiles/{profile_id}/appearance"),
                Some(serde_json::json!({ "icon": "star", "color": "teal" })),
                false,
            ),
            (
                "PATCH",
                format!("/v1/workspaces/{workspace_id}"),
                Some(serde_json::json!({ "name": "Notebook" })),
                false,
            ),
            (
                "PATCH",
                format!("/v1/workspaces/{workspace_id}/appearance"),
                Some(serde_json::json!({ "icon": "leaf", "color": "moss",
                                        "pinned": true, "archived": true })),
                true,
            ),
            (
                "PATCH",
                format!("/v1/workspaces/{workspace_id}/appearance"),
                Some(serde_json::json!({ "icon": "leaf", "color": "moss",
                                        "pinned": true, "archived": false })),
                true,
            ),
            (
                "PUT",
                format!("/v1/profiles/{profile_id}/workspace-order"),
                Some(serde_json::json!({ "workspaces": [workspace_id] })),
                false,
            ),
            (
                "DELETE",
                format!("/v1/workspaces/{workspace_id}"),
                None::<serde_json::Value>,
                true,
            ),
            (
                "POST",
                format!("/v1/workspaces/{workspace_id}/restore"),
                None::<serde_json::Value>,
                true,
            ),
        ] {
            let caller = if destructive { &destroyer } else { &token };
            let (status, _) = call(&state, caller, method, &uri, body).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri}");
        }

        assert_eq!(
            published(&journal(&state, &token).await),
            [
                "profile.created",
                "workspace.catalog.created",
                "profile.renamed",
                "profile.appearance_changed",
                "workspace.catalog.renamed",
                // One appearance write, two facts: the switcher's invalidation
                // and the lifecycle transition a manager acts on immediately.
                "workspace.catalog.appearance_changed",
                "workspace.catalog.archived",
                "workspace.catalog.appearance_changed",
                "workspace.catalog.unarchived",
                "workspace.catalog.reordered",
                "workspace.catalog.deleted",
                "workspace.catalog.restored",
            ]
        );
    }

    /// Re-sending the same appearance is not news. The lifecycle rows report a
    /// *transition*, read from the record before and after the same committed
    /// write, so an idempotent repeat leaves only the invalidation behind.
    #[tokio::test]
    async fn an_unchanged_archive_flag_reports_no_transition() {
        let (_root, state, token) = state_with_catalog_events();
        let (_, profile) = call(
            &state,
            &token,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada" })),
        )
        .await;
        let (_, workspace) = call(
            &state,
            &token,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile["id"], "name": "Notes" })),
        )
        .await;
        let uri = format!(
            "/v1/workspaces/{}/appearance",
            workspace["id"].as_str().unwrap()
        );
        let body = serde_json::json!({ "pinned": false, "archived": false });
        for _ in 0..2 {
            let (status, _) = call(&state, &token, "PATCH", &uri, Some(body.clone())).await;
            assert_eq!(status, StatusCode::OK);
        }
        let journal = journal(&state, &token).await;
        let published = published(&journal);
        assert_eq!(
            published
                .iter()
                .filter(|name| name.starts_with("workspace.catalog.appearance"))
                .count(),
            2
        );
        assert!(
            !published
                .iter()
                .any(|name| name.ends_with("archived") || name.ends_with("unarchived")),
            "nothing transitioned, so nothing may say it did: {published:?}"
        );
    }

    /// **A fact reports a commit, so a request that committed nothing is
    /// silent.** Refused for want of a scope, refused for naming nothing —
    /// either way the journal has no opinion about it.
    #[tokio::test]
    async fn a_refused_mutation_publishes_nothing() {
        let (_root, state, token) = state_with_catalog_events();
        let missing = "11111111-1111-4111-8111-111111111111";

        let (status, _) = call(
            &state,
            &token,
            "PATCH",
            &format!("/v1/workspaces/{missing}"),
            Some(serde_json::json!({ "name": "Ghost" })),
        )
        .await;
        assert_ne!(status, StatusCode::OK);

        let (reader, _) = state.sessions.mint(
            "reader",
            &["store.read_documents".into(), "events.read".into()],
            now_millis(),
        );
        let (status, _) = call(
            &state,
            &reader,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        assert!(published(&journal(&state, &token).await).is_empty());
    }

    /// **Ids and closed tokens, and nothing else.** The names in this test are
    /// what a person would recognise if one leaked; none of them may appear
    /// anywhere in the journal, and the assertion is over the serialized
    /// envelope rather than over the attrs, so a name could not arrive through
    /// a subject, an actor label or a future field either.
    #[tokio::test]
    async fn a_published_fact_carries_no_name_and_no_content() {
        const NAMES: [&str; 3] = ["Ada Lovelace", "Analytical Engine", "seafoam"];
        let (_root, state, token) = state_with_catalog_events();
        let (_, profile) = call(
            &state,
            &token,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": NAMES[0] })),
        )
        .await;
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        let (_, workspace) = call(
            &state,
            &token,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile_id, "name": NAMES[1] })),
        )
        .await;
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();
        call(
            &state,
            &token,
            "PATCH",
            &format!("/v1/workspaces/{workspace_id}/appearance"),
            Some(serde_json::json!({ "color": NAMES[2], "pinned": false, "archived": true })),
        )
        .await;

        let journal = journal(&state, &token).await;
        let rendered = serde_json::to_string(&journal).unwrap();
        for name in NAMES {
            assert!(!rendered.contains(name), "`{name}` reached the bus");
        }
        // The ids did travel, because that is what a reference is for: the
        // workspace names itself in the envelope, the profile in an attr.
        assert!(rendered.contains(&workspace_id));
        assert!(rendered.contains(&profile_id));
        // And every envelope validates against the registry on the way back —
        // the boundary the worker already applied, asserted from this side too.
        for (_, envelope) in journal {
            let envelope: koru_events::Envelope = serde_json::from_value(envelope).unwrap();
            koru_events::validate(&envelope).unwrap();
            assert_eq!(envelope.actor.kind, koru_events::ActorKind::System);
        }
    }

    // ------------------------------------------------------- the scope inventory

    /// **What gates each catalog route, after the split.** This is the table,
    /// asserted against `koru_store_api::api_catalog::ROUTES` — which is the
    /// only list of paths there is, so a route cannot slip past it.
    ///
    /// It used to record R9's finding: every catalog write took one
    /// undifferentiated `manage_workspaces`, from creating a profile to
    /// soft-deleting a workspace with live sessions in it. SP-8a separated the
    /// two that remove a workspace from the eleven that do not, and this is
    /// where the difference is now written down.
    ///
    /// The one row that is not the whole story is the appearance PATCH: it
    /// keeps `manage_workspaces`, because icon, colour and pinning are ordinary
    /// management, and its handler additionally takes `destroy_workspaces` when
    /// the `archived` flag actually changes. `archiving_through_the_appearance_route_takes_the_destructive_scope`
    /// is what holds that.
    #[test]
    fn every_catalog_route_declares_the_scope_it_has_today() {
        use koru_store_core::model::Scope;
        const INVENTORY: &[(&str, &str, Option<Scope>)] = &[
            // reads
            ("GET", "/v1/profiles", Some(Scope::ReadDocuments)),
            ("GET", "/v1/profiles/{id}", Some(Scope::ReadDocuments)),
            ("GET", "/v1/workspaces", Some(Scope::ReadDocuments)),
            ("GET", "/v1/workspaces/{id}", Some(Scope::ReadDocuments)),
            (
                "GET",
                "/v1/profiles/{id}/workspace-usage",
                Some(Scope::ReadDocuments),
            ),
            (
                "GET",
                "/v1/profiles/{id}/personal-attributes",
                Some(Scope::ReadDocuments),
            ),
            // writes — every one of them the same scope
            ("POST", "/v1/profiles", Some(Scope::ManageWorkspaces)),
            ("PATCH", "/v1/profiles/{id}", Some(Scope::ManageWorkspaces)),
            (
                "PATCH",
                "/v1/profiles/{id}/appearance",
                Some(Scope::ManageWorkspaces),
            ),
            (
                "PUT",
                "/v1/profiles/{id}/workspace-order",
                Some(Scope::ManageWorkspaces),
            ),
            ("POST", "/v1/workspaces", Some(Scope::ManageWorkspaces)),
            (
                "PATCH",
                "/v1/workspaces/{id}",
                Some(Scope::ManageWorkspaces),
            ),
            (
                "PATCH",
                "/v1/workspaces/{id}/appearance",
                Some(Scope::ManageWorkspaces),
            ),
            // …but not the two that remove a workspace. These are the bypass
            // R9 names: the lower route now refuses exactly the caller the
            // Gateway's lifecycle operation refuses, so no ordinary token can
            // reach around the session shutdown by calling the Store directly.
            (
                "DELETE",
                "/v1/workspaces/{id}",
                Some(Scope::DestroyWorkspaces),
            ),
            (
                "POST",
                "/v1/workspaces/{id}/restore",
                Some(Scope::DestroyWorkspaces),
            ),
            ("POST", "/v1/import", Some(Scope::ManageWorkspaces)),
            // the one catalog write that is not management: a personal answer
            // belongs to the profile in the path and overwrites nobody else's.
            (
                "PUT",
                "/v1/profiles/{id}/personal-attributes",
                Some(Scope::EditDocuments),
            ),
            // a reader's own private view of a document: a write that needs
            // only read access, because it changes nothing anyone else sees.
            (
                "GET",
                "/v1/profiles/{id}/documents/{document}/private-state",
                Some(Scope::ReadDocuments),
            ),
            (
                "PATCH",
                "/v1/profiles/{id}/documents/{document}/private-state",
                Some(Scope::ReadDocuments),
            ),
            // and the read-only export, which is a POST because it is work.
            (
                "POST",
                "/v1/workspaces/{id}/export",
                Some(Scope::ReadDocuments),
            ),
        ];
        for (method, path, scope) in INVENTORY {
            let route = koru_store_api::api_catalog::ROUTES
                .iter()
                .find(|route| route.method == *method && route.path == *path)
                .unwrap_or_else(|| panic!("{method} {path} is no longer a route"));
            assert_eq!(route.scope, *scope, "{method} {path}");
        }
    }

    /// The management scope has **one spelling**, and the two halves of it —
    /// the gateway's prefixed name and the Store's enum — are pinned to each
    /// other here, which is the whole reason the constant exists.
    #[test]
    fn the_manage_workspaces_scope_has_one_spelling() {
        assert_eq!(
            MANAGE_WORKSPACES_SCOPE
                .strip_prefix("store.")
                .and_then(koru_store_core::model::Scope::parse),
            Some(koru_store_core::model::Scope::ManageWorkspaces)
        );
        // A default session still holds it, and that is the half of the split
        // that did not move: creating, renaming, recolouring, reordering and
        // importing stay ordinary local work.
        assert!(
            effective_scopes(&[], Admission::Ordinary)
                .iter()
                .any(|held| held == MANAGE_WORKSPACES_SCOPE)
        );
        // And the principal-to-caller translation actually carries it — and
        // carries nothing else, so holding the management half is not quietly
        // holding the destructive one.
        let caller = store_caller(&GatewayPrincipal {
            label: "test".into(),
            scopes: vec![MANAGE_WORKSPACES_SCOPE.to_owned()],
        });
        assert_eq!(
            caller.scopes,
            vec![koru_store_core::model::Scope::ManageWorkspaces]
        );
    }

    /// **The destructive half, pinned the same way and in one more place.**
    ///
    /// It is the only scope with no `store.` prefix, because it is not a Store
    /// permission: it gates the Gateway's own lifecycle operation as well as
    /// the two Store routes that were its bypass. That means the prefix-strip
    /// in [`store_caller`] cannot carry it, and a version that quietly dropped
    /// it would leave the lower routes open to exactly the caller the split
    /// exists to stop. This is what would catch that.
    #[test]
    fn the_destructive_scope_has_one_spelling_on_both_sides() {
        assert_eq!(WORKSPACE_DESTROY_SCOPE, "workspace.destroy");
        assert!(
            WORKSPACE_DESTROY_SCOPE.strip_prefix("store.").is_none(),
            "a `store.` prefix would say the Store is where this lives"
        );
        assert_eq!(
            koru_store_core::model::Scope::parse("destroy_workspaces"),
            Some(koru_store_core::model::Scope::DestroyWorkspaces)
        );
        let caller = store_caller(&GatewayPrincipal {
            label: "test".into(),
            scopes: vec![WORKSPACE_DESTROY_SCOPE.to_owned()],
        });
        assert_eq!(
            caller.scopes,
            vec![koru_store_core::model::Scope::DestroyWorkspaces],
            "the destructive scope did not reach the Store caller"
        );
    }

    /// **The migration scope, pinned exactly as the destructive one is.**
    ///
    /// It is the second scope with no `store.` prefix, for the same reason: it
    /// gates the Gateway's own migration operation as well as the Store route
    /// beneath it. A version that let the prefix-strip in [`store_caller`]
    /// swallow it would leave `POST /v1/workspaces/{id}/move` reachable by a
    /// caller holding nothing the split cares about.
    #[test]
    fn the_migration_scope_has_one_spelling_on_both_sides() {
        assert_eq!(WORKSPACE_MIGRATE_SCOPE, "workspace.migrate");
        assert!(
            WORKSPACE_MIGRATE_SCOPE.strip_prefix("store.").is_none(),
            "a `store.` prefix would say the Store is where this lives"
        );
        assert_ne!(
            WORKSPACE_MIGRATE_SCOPE, WORKSPACE_DESTROY_SCOPE,
            "moving and removing are two permissions on purpose"
        );
        assert_eq!(
            koru_store_core::model::Scope::parse("migrate_workspaces"),
            Some(koru_store_core::model::Scope::MigrateWorkspaces)
        );
        let caller = store_caller(&GatewayPrincipal {
            label: "test".into(),
            scopes: vec![WORKSPACE_MIGRATE_SCOPE.to_owned()],
        });
        assert_eq!(
            caller.scopes,
            vec![koru_store_core::model::Scope::MigrateWorkspaces],
            "the migration scope did not reach the Store caller"
        );
        // And holding one of the pair never carries the other across.
        let destroyer = store_caller(&GatewayPrincipal {
            label: "test".into(),
            scopes: vec![WORKSPACE_DESTROY_SCOPE.to_owned()],
        });
        assert!(
            !destroyer
                .scopes
                .contains(&koru_store_core::model::Scope::MigrateWorkspaces),
            "a destroy grant became a licence to move"
        );
        assert!(
            !caller
                .scopes
                .contains(&koru_store_core::model::Scope::DestroyWorkspaces),
            "a migrate grant became a licence to remove"
        );
    }

    /// **The same three-row matrix, for the migration scope.**
    ///
    /// Ordinary, named-and-unvouched, vouched — and the middle row is the
    /// point: naming `workspace.migrate` gets a caller exactly as far as never
    /// having mentioned it.
    #[test]
    fn the_migration_scope_is_grantable_only_to_a_vouched_caller() {
        // Not in the default set, however the session was opened.
        for admission in [Admission::Ordinary, Admission::Vouched] {
            assert!(
                !effective_scopes(&[], admission).contains(&WORKSPACE_MIGRATE_SCOPE.to_owned()),
                "a migration permission was granted to a caller that named nothing"
            );
        }
        // Asking is not enough.
        assert_eq!(
            effective_scopes(
                &[
                    WORKSPACE_MIGRATE_SCOPE.into(),
                    MANAGE_WORKSPACES_SCOPE.into()
                ],
                Admission::Ordinary,
            ),
            vec![MANAGE_WORKSPACES_SCOPE.to_owned()],
            "an unvouched caller was granted the migration scope by asking"
        );
        // Both together.
        assert_eq!(
            effective_scopes(&[WORKSPACE_MIGRATE_SCOPE.into()], Admission::Vouched),
            vec![WORKSPACE_MIGRATE_SCOPE.to_owned()],
        );
        // **And being vouched for does not hand over the other half.** A window
        // that asked only to move workspaces cannot remove one, which is the
        // whole reason these are two scopes rather than one.
        assert_eq!(
            effective_scopes(&[WORKSPACE_MIGRATE_SCOPE.into()], Admission::Vouched).len(),
            1
        );
        assert_eq!(
            effective_scopes(&[WORKSPACE_DESTROY_SCOPE.into()], Admission::Vouched),
            vec![WORKSPACE_DESTROY_SCOPE.to_owned()],
        );
    }

    /// **The grant matrix, in the one function that decides it.**
    ///
    /// Three token kinds, and the third is new. An ordinary session gets the
    /// default set; a session that names scopes gets the intersection with the
    /// allowed set; and `workspace.destroy` is in *neither* list, so naming it
    /// is not enough — only a caller the Agent vouched for at admission keeps
    /// it. That is the difference between this scope and `settings.write`,
    /// which any caller can have by asking.
    #[test]
    fn the_destructive_scope_is_grantable_only_to_a_vouched_caller() {
        // Not in the default set, however the session was opened.
        for admission in [Admission::Ordinary, Admission::Vouched] {
            assert!(
                !effective_scopes(&[], admission).contains(&WORKSPACE_DESTROY_SCOPE.to_owned()),
                "a destructive permission was granted to a caller that named nothing"
            );
        }
        // Asking is not enough.
        assert!(
            effective_scopes(
                &[
                    WORKSPACE_DESTROY_SCOPE.into(),
                    MANAGE_WORKSPACES_SCOPE.into()
                ],
                Admission::Ordinary,
            ) == vec![MANAGE_WORKSPACES_SCOPE.to_owned()],
            "an unvouched caller was granted the destructive scope by asking"
        );
        // Being vouched for is not enough either — naming still filters.
        assert_eq!(
            effective_scopes(&[MANAGE_WORKSPACES_SCOPE.into()], Admission::Vouched),
            vec![MANAGE_WORKSPACES_SCOPE.to_owned()],
        );
        // Both together.
        assert_eq!(
            effective_scopes(&[WORKSPACE_DESTROY_SCOPE.into()], Admission::Vouched),
            vec![WORKSPACE_DESTROY_SCOPE.to_owned()],
        );
        // And a vouched caller gets no *other* scope it did not name — being
        // first-party widens one permission, not the whole set.
        assert_eq!(
            effective_scopes(&[WORKSPACE_DESTROY_SCOPE.into()], Admission::Vouched).len(),
            1
        );
    }

    /// **The authorization matrix, route by route and token by token**,
    /// asserted against a live gateway rather than a table.
    ///
    /// Three token kinds:
    ///
    /// - *ordinary* — the default set, which is what an MCP tool, a plugin or
    ///   a script holds. It manages workspaces and cannot remove one.
    /// - *requested* — a token that named `workspace.destroy` and was not
    ///   vouched for. Identical to ordinary on every destructive route, which
    ///   is the property that makes asking useless on its own.
    /// - *vouched* — the Agent confirmed at admission that it launched the
    ///   caller. The only one that removes a workspace.
    ///
    /// Archiving is here twice, because it hides inside the appearance route: a
    /// colour change is management, and the same PATCH flipping `archived` is
    /// not.
    #[tokio::test]
    async fn removing_a_workspace_takes_a_scope_no_ordinary_token_can_ask_for() {
        let (_root, state, owner) = state_with_catalog_events();
        let (_, profile) = call(
            &state,
            &owner,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada" })),
        )
        .await;
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        let (_, workspace) = call(
            &state,
            &owner,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile_id, "name": "Notes" })),
        )
        .await;
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();

        // An ordinary session: named nothing, so it holds the default set.
        let (ordinary, _) = state.sessions.mint("mcp-tool", &[], now_millis());
        // The same caller, having asked for the destructive scope by name.
        let (requested, _) = state.sessions.mint_admitted(
            "mcp-tool",
            &[
                WORKSPACE_DESTROY_SCOPE.into(),
                MANAGE_WORKSPACES_SCOPE.into(),
            ],
            Admission::Ordinary,
            now_millis(),
        );
        // A first-party window the Agent vouched for.
        let (vouched, _) = state.sessions.mint_admitted(
            "koru",
            &[
                WORKSPACE_DESTROY_SCOPE.into(),
                MANAGE_WORKSPACES_SCOPE.into(),
            ],
            Admission::Vouched,
            now_millis(),
        );

        // The non-destructive half is unchanged for every token kind.
        for token in [&ordinary, &requested, &vouched] {
            let (status, _) = call(
                &state,
                token,
                "PATCH",
                &format!("/v1/workspaces/{workspace_id}"),
                Some(serde_json::json!({ "name": "Notes" })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "renaming stopped being ordinary");
            let (status, _) = call(
                &state,
                token,
                "PATCH",
                &format!("/v1/workspaces/{workspace_id}/appearance"),
                Some(serde_json::json!({ "color": "seafoam", "pinned": true, "archived": false })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "recolouring stopped being ordinary");
        }

        // The destructive half refuses both unvouched tokens, on every route.
        for token in [&ordinary, &requested] {
            for (method, uri, body) in [
                (
                    "PATCH",
                    format!("/v1/workspaces/{workspace_id}/appearance"),
                    Some(
                        serde_json::json!({ "color": "seafoam", "pinned": true, "archived": true }),
                    ),
                ),
                ("DELETE", format!("/v1/workspaces/{workspace_id}"), None),
                (
                    "POST",
                    format!("/v1/workspaces/{workspace_id}/restore"),
                    None,
                ),
            ] {
                let (status, _) = call(&state, token, method, &uri, body).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            }
        }

        // And the vouched one performs all three.
        let (status, archived) = call(
            &state,
            &vouched,
            "PATCH",
            &format!("/v1/workspaces/{workspace_id}/appearance"),
            Some(serde_json::json!({ "color": "seafoam", "pinned": true, "archived": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(archived["archived"], serde_json::Value::Bool(true));
        let (status, deleted) = call(
            &state,
            &vouched,
            "DELETE",
            &format!("/v1/workspaces/{workspace_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!deleted["deleted_at"].is_null());
        let (status, restored) = call(
            &state,
            &vouched,
            "POST",
            &format!("/v1/workspaces/{workspace_id}/restore"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(restored["deleted_at"].is_null());
    }

    /// **The migration route's matrix, against a live gateway.**
    ///
    /// The same three token kinds, and one more property the constant-level
    /// test cannot show: a token vouched for and holding `workspace.destroy` is
    /// still refused here. That is what makes the two scopes two — a window
    /// trusted to empty a workspace is not thereby trusted to hand it to
    /// another persona, and this is where that would quietly stop being true.
    #[tokio::test]
    async fn moving_a_workspace_takes_its_own_vouched_scope() {
        let (_root, state, owner) = state_with_catalog_events();
        let (_, first) = call(
            &state,
            &owner,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Work" })),
        )
        .await;
        let (_, second) = call(
            &state,
            &owner,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Home" })),
        )
        .await;
        let from = first["id"].as_str().unwrap().to_owned();
        let to = second["id"].as_str().unwrap().to_owned();
        let (_, workspace) = call(
            &state,
            &owner,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": from, "name": "Notes" })),
        )
        .await;
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();
        let uri = format!("/v1/workspaces/{workspace_id}/move");
        let body = || Some(serde_json::json!({ "owner": to }));

        // Named nothing: the default set, which does not reach this route.
        let (ordinary, _) = state.sessions.mint("mcp-tool", &[], now_millis());
        // Named it, and nobody vouched.
        let (requested, _) = state.sessions.mint_admitted(
            "mcp-tool",
            &[
                WORKSPACE_MIGRATE_SCOPE.into(),
                MANAGE_WORKSPACES_SCOPE.into(),
            ],
            Admission::Ordinary,
            now_millis(),
        );
        // Vouched for, but holding the *other* half of the split.
        let (destroyer, _) = state.sessions.mint_admitted(
            "koru",
            &[WORKSPACE_DESTROY_SCOPE.into()],
            Admission::Vouched,
            now_millis(),
        );
        // Vouched for, and holding this one.
        let (migrator, _) = state.sessions.mint_admitted(
            "koru",
            &[WORKSPACE_MIGRATE_SCOPE.into()],
            Admission::Vouched,
            now_millis(),
        );

        for token in [&ordinary, &requested, &destroyer] {
            let (status, _) = call(&state, token, "POST", &uri, body()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "POST {uri}");
        }
        let (status, moved) = call(&state, &migrator, "POST", &uri, body()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(moved["owner"].as_str(), Some(to.as_str()));

        // The committed fact, and only one of it: the replay below is a retry.
        let (status, _) = call(&state, &migrator, "POST", &uri, body()).await;
        assert_eq!(status, StatusCode::OK, "the same move again is a retry");
        assert_eq!(
            published(&journal(&state, &owner).await)
                .into_iter()
                .filter(|name| *name == "workspace.catalog.moved")
                .count(),
            1,
            "a no-op replay published a fact about a commit that did not happen"
        );
    }

    /// Archiving travels in the appearance body because those four fields are
    /// edited together, so the route's declared scope is the ordinary one and
    /// the *transition* is what takes the destructive one. Re-sending a flag
    /// that is already set changes nothing and is therefore not destructive —
    /// which is also why it publishes no `archived` fact.
    #[tokio::test]
    async fn archiving_through_the_appearance_route_takes_the_destructive_scope() {
        let (_root, state, owner) = state_with_catalog_events();
        let (_, profile) = call(
            &state,
            &owner,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada" })),
        )
        .await;
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        let (_, workspace) = call(
            &state,
            &owner,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile_id, "name": "Notes" })),
        )
        .await;
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();
        let (ordinary, _) = state.sessions.mint("mcp-tool", &[], now_millis());
        let appearance = format!("/v1/workspaces/{workspace_id}/appearance");

        // Not archived, and asked to stay that way: ordinary.
        let (status, _) = call(
            &state,
            &ordinary,
            "PATCH",
            &appearance,
            Some(serde_json::json!({ "pinned": false, "archived": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Archive it with a token that may. **The destructive scope is
        // additional, not a replacement**: this route still takes
        // `store.manage_workspaces` first, and a token holding only the
        // destructive half is refused like any other caller that may not edit
        // a workspace at all.
        let (vouched, _) = state.sessions.mint_admitted(
            "koru",
            &[
                WORKSPACE_DESTROY_SCOPE.into(),
                MANAGE_WORKSPACES_SCOPE.into(),
            ],
            Admission::Vouched,
            now_millis(),
        );
        let (status, _) = call(
            &state,
            &vouched,
            "PATCH",
            &appearance,
            Some(serde_json::json!({ "pinned": false, "archived": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Now an ordinary token may still recolour it — the flag is unchanged,
        // so nothing destructive is happening…
        let (status, _) = call(
            &state,
            &ordinary,
            "PATCH",
            &appearance,
            Some(serde_json::json!({ "color": "amber", "pinned": false, "archived": true })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an ordinary appearance edit was refused because the workspace happened to be archived"
        );
        // …and may not un-archive it, which is a transition like any other.
        let (status, _) = call(
            &state,
            &ordinary,
            "PATCH",
            &appearance,
            Some(serde_json::json!({ "color": "amber", "pinned": false, "archived": false })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// Today's authorization, asserted against a live gateway rather than a
    /// table: a token holding only `store.read_documents` reads the catalog and
    /// changes none of it.
    #[tokio::test]
    async fn a_reading_token_may_not_move_the_catalog() {
        let (_root, state, owner) = state_with_catalog_events();
        let (_, profile) = call(
            &state,
            &owner,
            "POST",
            "/v1/profiles",
            Some(serde_json::json!({ "name": "Ada" })),
        )
        .await;
        let profile_id = profile["id"].as_str().unwrap().to_owned();
        let (_, workspace) = call(
            &state,
            &owner,
            "POST",
            "/v1/workspaces",
            Some(serde_json::json!({ "owner": profile_id, "name": "Notes" })),
        )
        .await;
        let workspace_id = workspace["id"].as_str().unwrap().to_owned();

        let (reader, _) =
            state
                .sessions
                .mint("reader", &["store.read_documents".into()], now_millis());
        for (method, uri, body) in [
            ("GET", "/v1/profiles".to_owned(), None),
            ("GET", "/v1/workspaces".to_owned(), None),
            (
                "GET",
                "/v1/workspaces?include_deleted=true".to_owned(),
                None,
            ),
        ] {
            let (status, _) = call(&state, &reader, method, &uri, body).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri}");
        }
        for (method, uri, body) in [
            (
                "POST",
                "/v1/profiles".to_owned(),
                Some(serde_json::json!({ "name": "Grace" })),
            ),
            (
                "PATCH",
                format!("/v1/profiles/{profile_id}"),
                Some(serde_json::json!({ "name": "Grace" })),
            ),
            (
                "PATCH",
                format!("/v1/profiles/{profile_id}/appearance"),
                Some(serde_json::json!({ "icon": "star" })),
            ),
            (
                "PUT",
                format!("/v1/profiles/{profile_id}/workspace-order"),
                Some(serde_json::json!({ "workspaces": [] })),
            ),
            (
                "POST",
                "/v1/workspaces".to_owned(),
                Some(serde_json::json!({ "owner": profile_id, "name": "Other" })),
            ),
            (
                "PATCH",
                format!("/v1/workspaces/{workspace_id}"),
                Some(serde_json::json!({ "name": "Other" })),
            ),
            (
                "PATCH",
                format!("/v1/workspaces/{workspace_id}/appearance"),
                Some(serde_json::json!({ "pinned": true, "archived": true })),
            ),
            ("DELETE", format!("/v1/workspaces/{workspace_id}"), None),
            (
                "POST",
                format!("/v1/workspaces/{workspace_id}/restore"),
                None,
            ),
        ] {
            let (status, _) = call(&state, &reader, method, &uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }
}
