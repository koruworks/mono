// SPDX-License-Identifier: MPL-2.0

//! `/v1/artifacts` — the Artifact Store's public routes (D1, D13).
//!
//! The same shape as the AI routes: a catalogued table, one handler per
//! row, scope checked before dispatch, the domain answered through its typed
//! port ([`koru_artifact_api::ArtifactServerPort`]) from `spawn_blocking`.
//! The store is composed by the host and handed to
//! [`GatewayState::set_artifact_service`]; when it is absent — `artifacts.db`
//! would not open — every route answers 503 `artifacts_unavailable` and the
//! `artifacts` capability is not published, the way `/v1/events` behaves
//! without a journal.
//!
//! **Bytes never pass through these routes.** A registration names a
//! [`BlobHash`] already in the family's workspace, uploaded through the
//! Store's `PUT /v1/workspaces/{id}/blobs` (its own 2 GiB limit, streamed);
//! the metadata body here is capped at
//! [`koru_artifact_wire::METADATA_BODY_LIMIT`], and the gateway asks the
//! Store whether the hash is there before the version exists. Reading a
//! version's content forwards to the Store's blob route and returns its
//! response as it is — the artifact route buffers nothing.
//!
//! **A version is immutable at this layer too.** `PATCH`, `PUT` and `DELETE`
//! on a version are mounted and answer 409 `artifact_version_immutable`,
//! the code the database's trigger raises, so a client that tries to rewrite
//! one reads the rule rather than a 405.
//!
//! **The run authority lives here too** (§7.3): `/v1/artifacts/authorities`
//! is where the orchestrator issues a run's endpoint token, advances its
//! generation and revokes it, and `/v1/artifacts/runs/{run}/versions` is
//! what a run has registered — the hook the orchestrator reads at a turn
//! boundary (D29). The endpoint the token opens is `run_mcp`'s.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, patch, post};
use axum::{Json, Router};
use koru_artifact_api::{
    ArtifactDomain, ArtifactPrincipal, ArtifactScope, RequestContext, VerifiedBlob,
};
use koru_artifact_core::{ArtifactError, ArtifactFamilyId, ArtifactVersionId, Principal, RunId};
use koru_artifact_wire::authority::{
    AdvanceGenerationRequest, AuthorityView, ForgetAuthorityResponse, IssueAuthorityRequest,
    IssuedAuthority, RunVersionsResponse, RunWorkspaceResponse,
};
use koru_artifact_wire::run_folder::{RegisterFileRequest, RegisteredFile, UnregisteredResponse};
use koru_artifact_wire::{
    ArtifactFamily, ArtifactVersion, BlobHash, CreateFamilyRequest, Failure, FamilyResponse,
    LinkRunRequest, ListFamiliesQuery, ListFamiliesResponse, METADATA_BODY_LIMIT, Provenance,
    RegisterVersionRequest, RegisterVersionResponse, RunLink,
};
use schemars::{Schema, SchemaGenerator};

mod run_folder;

use crate::{GatewayPrincipal, GatewayState, store_caller, store_json};

type SchemaFor = fn(&mut SchemaGenerator) -> Schema;

pub struct ArtifactRoute {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub scope: ArtifactScope,
    pub summary: &'static str,
    pub returns: &'static str,
    pub handler: fn() -> MethodRouter<Arc<GatewayState>>,
    pub request: Option<SchemaFor>,
    pub response: Option<SchemaFor>,
    pub path_parameter: Option<&'static str>,
    /// The route takes `?workspace=<id>&include_retired=<bool>`.
    pub workspace_query: bool,
}

pub const ROUTES: &[ArtifactRoute] = &[
    ArtifactRoute {
        operation_id: "artifacts_list_families",
        method: "GET",
        path: "/v1/artifacts/families",
        scope: ArtifactScope::Read,
        summary: "The artifact families of a workspace",
        returns: "The families the caller may read, oldest first; retired ones only with include_retired=true.",
        handler: || get(list_families),
        request: None,
        response: Some(|generator| generator.subschema_for::<ListFamiliesResponse>()),
        path_parameter: None,
        workspace_query: true,
    },
    ArtifactRoute {
        operation_id: "artifacts_create_family",
        method: "POST",
        path: "/v1/artifacts/families",
        scope: ArtifactScope::Write,
        summary: "Create an artifact family",
        returns: "The family. The caller mints the id; a second creation under the same id is refused with artifact_family_exists rather than made twice.",
        handler: || post(create_family).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        request: Some(|generator| generator.subschema_for::<CreateFamilyRequest>()),
        response: Some(|generator| generator.subschema_for::<ArtifactFamily>()),
        path_parameter: None,
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_family",
        method: "GET",
        path: "/v1/artifacts/families/{family}",
        scope: ArtifactScope::Read,
        summary: "A family with its versions",
        returns: "The family and every version under it, by ordinal.",
        handler: || get(family),
        request: None,
        response: Some(|generator| generator.subschema_for::<FamilyResponse>()),
        path_parameter: Some("family"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_retire_family",
        method: "POST",
        path: "/v1/artifacts/families/{family}/retire",
        scope: ArtifactScope::Write,
        summary: "Retire a family",
        returns: "The family with retired_at_ms set. Its versions stay readable; it accepts no new version. Retiring twice keeps the first mark.",
        handler: || post(retire_family),
        request: None,
        response: Some(|generator| generator.subschema_for::<ArtifactFamily>()),
        path_parameter: Some("family"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_register_version",
        method: "POST",
        path: "/v1/artifacts/families/{family}/versions",
        scope: ArtifactScope::Write,
        summary: "Register an immutable version of a family",
        returns: "The version, and whether an earlier call with the same registration_id had already made it. Metadata only: the blob must already be in the family's workspace (PUT /v1/workspaces/{id}/blobs), or the answer is blob_not_found. The size recorded is the Store's.",
        handler: || post(register_version).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        request: Some(|generator| generator.subschema_for::<RegisterVersionRequest>()),
        response: Some(|generator| generator.subschema_for::<RegisterVersionResponse>()),
        path_parameter: Some("family"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_version",
        method: "GET",
        path: "/v1/artifacts/versions/{version}",
        scope: ArtifactScope::Read,
        summary: "One artifact version",
        returns: "The version: identity, family, ordinal, blob address, size, media type, filename, producer, registration id and derived_from.",
        handler: || {
            get(version)
                .patch(immutable)
                .put(immutable)
                .delete(immutable)
        },
        request: None,
        response: Some(|generator| generator.subschema_for::<ArtifactVersion>()),
        path_parameter: Some("version"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_version_immutable",
        method: "PATCH",
        path: "/v1/artifacts/versions/{version}",
        scope: ArtifactScope::Write,
        summary: "Refused: a version cannot be changed (also PUT and DELETE)",
        returns: "409 artifact_version_immutable, always — the same code the database raises. Register a new version, or retire the family.",
        handler: || patch(immutable),
        request: None,
        response: None,
        path_parameter: Some("version"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_provenance",
        method: "GET",
        path: "/v1/artifacts/versions/{version}/provenance",
        scope: ArtifactScope::Read,
        summary: "A version's provenance",
        returns: "The version, every ancestor reached through derived_from (nearest first), and every run that consumed or produced any of them.",
        handler: || get(provenance),
        request: None,
        response: Some(|generator| generator.subschema_for::<Provenance>()),
        path_parameter: Some("version"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_content",
        method: "GET",
        path: "/v1/artifacts/versions/{version}/content",
        scope: ArtifactScope::Read,
        summary: "A version's bytes",
        returns: "The Store's answer for the version's blob, forwarded as it is: application/octet-stream with the blob's media type and filename headers, or the Store's own failure. Requires store.read_documents as well.",
        handler: || get(content),
        request: None,
        response: None,
        path_parameter: Some("version"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_link_run",
        method: "POST",
        path: "/v1/artifacts/versions/{version}/runs",
        scope: ArtifactScope::Write,
        summary: "Record that a run consumed or produced a version",
        returns: "The link. Idempotent on (run, version, role).",
        handler: || post(link_run).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        request: Some(|generator| generator.subschema_for::<LinkRunRequest>()),
        response: Some(|generator| generator.subschema_for::<RunLink>()),
        path_parameter: Some("version"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_issue_authority",
        method: "POST",
        path: "/v1/artifacts/authorities",
        scope: ArtifactScope::Write,
        summary: "Issue a run's authority and its first endpoint token",
        returns: "The run's endpoint URL (<gateway>/v1/runs/{run}/mcp, stable across attempts) and the bearer for this attempt — shown once; a later read never returns it. The grant set is the request's, frozen for the attempt (D28). A run that already has an authority is refused with authority_exists: advance its generation instead.",
        handler: || post(issue_authority).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        request: Some(|generator| generator.subschema_for::<IssueAuthorityRequest>()),
        response: Some(|generator| generator.subschema_for::<IssuedAuthority>()),
        path_parameter: None,
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_authority",
        method: "GET",
        path: "/v1/artifacts/authorities/{run}",
        scope: ArtifactScope::Read,
        summary: "A run's authority record",
        returns: "The record without its token and without its working-scope path: workspace, current generation, grants, state.",
        handler: || get(authority),
        request: None,
        response: Some(|generator| generator.subschema_for::<AuthorityView>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_advance_generation",
        method: "POST",
        path: "/v1/artifacts/authorities/{run}/generation",
        scope: ArtifactScope::Write,
        summary: "A fresh attempt: advance the run's generation under a fresh token",
        returns: "The same endpoint URL and a new bearer. The previous bearer is retired and answers stale_generation from then on; the grants are re-read from the record and never widened. A generation that is not strictly newer is refused with stale_generation; a revoked run with authority_revoked.",
        handler: || post(advance_generation).layer(DefaultBodyLimit::max(METADATA_BODY_LIMIT)),
        request: Some(|generator| generator.subschema_for::<AdvanceGenerationRequest>()),
        response: Some(|generator| generator.subschema_for::<IssuedAuthority>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_revoke_authority",
        method: "POST",
        path: "/v1/artifacts/authorities/{run}/revoke",
        scope: ArtifactScope::Write,
        summary: "Revoke a run's authority",
        returns: "The record marked revoked. A state change every later call at the run's endpoint reads (authority_revoked) — no callback, nothing to refresh. Idempotent.",
        handler: || post(revoke_authority),
        request: None,
        response: Some(|generator| generator.subschema_for::<AuthorityView>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_authority_workspace",
        method: "GET",
        path: "/v1/artifacts/authorities/{run}/workspace",
        scope: ArtifactScope::Write,
        summary: "Where the run worked, as this device resolved it",
        returns: "The absolute working folder the run's authority record holds -- the one read that carries a path back out of this store, for the one caller entitled to it: emptying the trash, which has to remove that folder when Koru made it. Behind the write scope rather than the read one, because reading it is not what it is for. Refused with authority_not_found for a run this device never started.",
        handler: || get(authority_workspace),
        request: None,
        response: Some(|generator| generator.subschema_for::<RunWorkspaceResponse>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_forget_authority",
        method: "POST",
        path: "/v1/artifacts/authorities/{run}/forget",
        scope: ArtifactScope::Write,
        summary: "The run was deleted: forget its authority",
        returns: "The record, its grants and its tokens are removed -- there is nothing left for them to authorize -- and one row saying the run was deleted takes their place, so a result the person chose to keep says 'a run that was deleted' rather than printing an id that resolves to nothing. Idempotent; held_authority is false, not an error, for a run that never had one.",
        handler: || post(forget_authority),
        request: None,
        response: Some(|generator| generator.subschema_for::<ForgetAuthorityResponse>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_run_versions",
        method: "GET",
        path: "/v1/artifacts/runs/{run}/versions",
        scope: ArtifactScope::Read,
        summary: "The versions a run has registered",
        returns: "Every version whose producer is the run, oldest first — what the orchestrator reads at a turn boundary to decide whether the run delivered anything (D29) and to publish delegate.artifact.registered.",
        handler: || get(run_versions),
        request: None,
        response: Some(|generator| generator.subschema_for::<RunVersionsResponse>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
    ArtifactRoute {
        operation_id: "artifacts_unregistered",
        method: "GET",
        path: "/v1/artifacts/unregistered",
        scope: ArtifactScope::Read,
        summary: "What a workspace's runs wrote and never registered",
        returns: "Every run of the workspace with an authority on this device, newest first: how many versions it has registered (zero is D29's finished-having-registered-nothing), whether its .artifacts/<RunId>/ folder is still there, and the files inside it that are not results yet. A link is never followed and never listed; the scan stops at 8 levels and 500 files, and says so with truncated.",
        handler: || get(run_folder::unregistered),
        request: None,
        response: Some(|generator| generator.subschema_for::<UnregisteredResponse>()),
        path_parameter: None,
        workspace_query: true,
    },
    ArtifactRoute {
        operation_id: "artifacts_register_file",
        method: "POST",
        path: "/v1/artifacts/runs/{run}/register",
        scope: ArtifactScope::Write,
        summary: "Register a file the run left in its results folder (D19)",
        returns: "The version and its family. The path is resolved under the same register path rule the run's own tools obey — canonical, inside <run-workspace>/.artifacts/<RunId>/, no traversal, no symlink or junction, unchanged between listing and read — and refused with the same stable codes. The workspace, the family's identity and the provenance all come from the run's durable authority record, never from the request; a revoked run still answers, because revocation withdraws the agent's reach and not the person's. Absent a title, the file's last path segment names the result. Registering the same bytes twice is the same version; the same path with new bytes is the next one.",
        handler: || post(run_folder::register_file).layer(run_folder::body_limit()),
        request: Some(|generator| generator.subschema_for::<RegisterFileRequest>()),
        response: Some(|generator| generator.subschema_for::<RegisteredFile>()),
        path_parameter: Some("run"),
        workspace_query: false,
    },
];

pub fn router() -> Router<Arc<GatewayState>> {
    // The immutable row shares its path with the version read and is
    // mounted by that row's handler; mounting it again would panic axum.
    ROUTES
        .iter()
        .filter(|route| route.operation_id != "artifacts_version_immutable")
        .fold(Router::new(), |router, route| {
            router.route(route.path, (route.handler)())
        })
}

pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::{Map, Value, json};

    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let failure = schemas.subschema_for::<Failure>();
    let paths = document["paths"]
        .as_object_mut()
        .expect("gateway OpenAPI paths");
    for route in ROUTES {
        let mut operation = json!({
            "operationId": route.operation_id,
            "summary": route.summary,
            "description": route.returns,
            "security": [{"bearerAuth": [route.scope.as_str()]}],
            "responses": {
                "200": {"description": route.returns},
                "400": {"description": "The request body, query or path identity is invalid", "content": {"application/json": {"schema": failure}}},
                "401": {"description": "The gateway bearer is absent or invalid", "content": {"application/json": {"schema": failure}}},
                "403": {"description": "The bearer lacks the operation's artifact scope, or the principal holds no access on the family", "content": {"application/json": {"schema": failure}}},
                "404": {"description": "No family, version or blob has that id", "content": {"application/json": {"schema": failure}}},
                "409": {"description": "The operation conflicts with an immutable version, a retired family, an existing family id or an earlier registration", "content": {"application/json": {"schema": failure}}},
                "413": {"description": "The metadata body exceeds the artifact route limit; bytes go through the Store's blob route", "content": {"application/json": {"schema": failure}}},
                "503": {"description": "artifacts.db did not open; the artifacts capability is absent", "content": {"application/json": {"schema": failure}}}
            }
        });
        if let Some(request) = route.request {
            operation["requestBody"] = json!({
                "required": true,
                "content": {"application/json": {"schema": request(&mut schemas)}}
            });
        }
        let mut parameters = Vec::new();
        if let Some(parameter) = route.path_parameter {
            parameters.push(json!({
                "name": parameter,
                "in": "path",
                "required": true,
                "schema": {"type": "string", "format": "uuid"}
            }));
        }
        if route.workspace_query {
            parameters.push(json!({
                "name": "workspace",
                "in": "query",
                "required": true,
                "schema": {"type": "string", "format": "uuid"}
            }));
            parameters.push(json!({
                "name": "include_retired",
                "in": "query",
                "required": false,
                "schema": {"type": "boolean", "default": false}
            }));
        }
        if !parameters.is_empty() {
            operation["parameters"] = Value::Array(parameters);
        }
        if let Some(response) = route.response {
            operation["responses"]["200"]["content"] =
                json!({"application/json": {"schema": response(&mut schemas)}});
        } else if route.operation_id == "artifacts_content" {
            operation["responses"]["200"]["content"] = json!({"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}});
        }
        let entry = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        entry
            .as_object_mut()
            .expect("artifact OpenAPI path")
            .insert(route.method.to_lowercase(), operation);
    }
    let components = document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas");
    components.extend(
        schemas
            .take_definitions(true)
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).expect("artifact schema"))),
    );
}

// ------------------------------------------------------------------ handlers

async fn list_families(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    query: Result<Query<ListFamiliesQuery>, QueryRejection>,
) -> Response {
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                Failure::new(
                    "invalid_request",
                    "The artifact query needs a workspace id.",
                    false,
                ),
            );
        }
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.list_families(context, query)).await)
}

async fn create_family(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    payload: Result<Json<CreateFamilyRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.create_family(context, request)).await)
}

async fn family(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(family): Path<String>,
) -> Response {
    let family = match parse_id::<ArtifactFamilyId>(&family) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.family(context, family)).await)
}

async fn retire_family(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(family): Path<String>,
) -> Response {
    let family = match parse_id::<ArtifactFamilyId>(&family) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.retire_family(context, family)).await)
}

/// Authority first, then the Store, then the registration — in that order
/// so a caller who may not register learns nothing about the workspace's
/// blobs, and a blob is never looked up for a family that cannot take it.
async fn register_version(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(family): Path<String>,
    payload: Result<Json<RegisterVersionRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let family = match parse_id::<ArtifactFamilyId>(&family) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal.clone(), ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let target = {
        let service = Arc::clone(&service);
        let context = context.clone();
        call(move || service.registration_target(context, family)).await
    };
    let workspace = match target {
        Ok(workspace) => workspace,
        Err(failure) => return refuse(status_for(&failure), failure),
    };
    let blob = match verified_blob(&state, &principal, workspace, &request.blob).await {
        Ok(blob) => blob,
        Err(response) => return response,
    };
    answer(call(move || service.register_version(context, family, request, blob)).await)
}

/// Ask the family's workspace whether the hash is there, through the
/// Store's own listing and the caller's own Store scopes. The size the
/// version records is what comes back, never what the caller said.
async fn verified_blob(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    workspace: koru_artifact_core::WorkspaceId,
    hash: &BlobHash,
) -> Result<VerifiedBlob, Response> {
    let listing = store_json(
        state,
        principal,
        "GET",
        &format!("/v1/workspaces/{}/blobs", workspace.to_storage()),
        None,
    )
    .await?;
    let found = listing
        .as_array()
        .into_iter()
        .flatten()
        .find(|blob| blob["hash"].as_str() == Some(hash.as_str()))
        .and_then(|blob| blob["size"].as_u64());
    match found {
        Some(size_bytes) => Ok(VerifiedBlob { size_bytes }),
        None => Err(refuse(
            StatusCode::NOT_FOUND,
            Failure::from(ArtifactError::BlobNotFound),
        )),
    }
}

async fn version(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(version): Path<String>,
) -> Response {
    let version = match parse_id::<ArtifactVersionId>(&version) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.version(context, version)).await)
}

/// The rule, stated at the API layer with the database's own code. Answered
/// after authentication and before anything else, so it needs neither a
/// scope nor the store to be there: there is nothing this call could do.
async fn immutable() -> Response {
    refuse(
        StatusCode::CONFLICT,
        Failure::from(ArtifactError::VersionImmutable),
    )
}

async fn provenance(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(version): Path<String>,
) -> Response {
    let version = match parse_id::<ArtifactVersionId>(&version) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.provenance(context, version)).await)
}

/// The version's bytes, through the Store's blob route. The Store's response
/// — body, status, headers — is returned as it is; nothing is buffered here,
/// so a large artifact costs this route nothing proportional to its size.
async fn content(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(version): Path<String>,
) -> Response {
    let version = match parse_id::<ArtifactVersionId>(&version) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal.clone(), ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let found = {
        let service = Arc::clone(&service);
        let context = context.clone();
        call(move || service.version(context, version)).await
    };
    let found = match found {
        Ok(found) => found,
        Err(failure) => return refuse(status_for(&failure), failure),
    };
    let family = call(move || service.family(context, found.family)).await;
    let workspace = match family {
        Ok(response) => response.family.workspace,
        Err(failure) => return refuse(status_for(&failure), failure),
    };
    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/v1/workspaces/{}/blobs/{}",
            workspace.to_storage(),
            found.blob.as_str()
        ))
        .body(Body::empty())
        .expect("a literal internal Store request is well-formed");
    match state.store.request(request, store_caller(&principal)).await {
        Ok(response) => response,
        Err(error) => refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            Failure::new(
                crate::store_worker_code(error),
                "The Store worker did not answer for the version's bytes.",
                true,
            ),
        ),
    }
}

async fn link_run(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(version): Path<String>,
    payload: Result<Json<LinkRunRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let version = match parse_id::<ArtifactVersionId>(&version) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.link_run(context, version, request)).await)
}

// ---------------------------------------------------------- run authority

/// `<gateway base>/v1/runs/<RunId>/mcp`. The base is the endpoint the
/// host published, or — for a gateway that has not been told it, an
/// in-process router under test — the `Host` the caller reached.
fn endpoint_url(state: &GatewayState, headers: &axum::http::HeaderMap, path: &str) -> String {
    let base = state.public_endpoint().map(str::to_owned).or_else(|| {
        headers
            .get(axum::http::header::HOST)
            .and_then(|host| host.to_str().ok())
            .map(|host| format!("http://{host}"))
    });
    match base {
        Some(base) => format!("{}{path}", base.trim_end_matches('/')),
        None => path.to_owned(),
    }
}

async fn issue_authority(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: axum::http::HeaderMap,
    payload: Result<Json<IssueAuthorityRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let issued = call(move || service.issue(context, request)).await;
    answer(issued.map(|issued| IssuedAuthority {
        run: issued.run,
        generation: issued.generation,
        url: endpoint_url(&state, &headers, &issued.path),
        token: issued.token,
    }))
}

async fn authority(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
) -> Response {
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.authority(context, run)).await)
}

async fn advance_generation(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
    headers: axum::http::HeaderMap,
    payload: Result<Json<AdvanceGenerationRequest>, JsonRejection>,
) -> Response {
    let request = match json_request(payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let issued = call(move || service.advance_generation(context, run, request.generation)).await;
    answer(issued.map(|issued| IssuedAuthority {
        run: issued.run,
        generation: issued.generation,
        url: endpoint_url(&state, &headers, &issued.path),
        token: issued.token,
    }))
}

async fn revoke_authority(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
) -> Response {
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.revoke(context, run)).await)
}

async fn authority_workspace(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
) -> Response {
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.workspace_of(context, run)).await)
}

async fn forget_authority(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
) -> Response {
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Write) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.forget(context, run)).await)
}

async fn run_versions(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(run): Path<String>,
) -> Response {
    let run = match parse_id::<RunId>(&run) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let (service, context) = match admitted(&state, principal, ArtifactScope::Read) {
        Ok(values) => values,
        Err(response) => return response,
    };
    answer(call(move || service.versions_produced_by(context, run)).await)
}

// ------------------------------------------------------------------ plumbing

type Service = Arc<dyn ArtifactDomain>;

pub(crate) fn admitted(
    state: &GatewayState,
    principal: GatewayPrincipal,
    required: ArtifactScope,
) -> Result<(Service, RequestContext), Response> {
    if !principal
        .scopes
        .iter()
        .any(|scope| scope == required.as_str())
    {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            Failure::new(
                "forbidden",
                "The gateway session does not permit this artifact operation.",
                false,
            ),
        ));
    }
    let Some(service) = state.artifact_domain() else {
        return Err(refuse(StatusCode::SERVICE_UNAVAILABLE, unavailable()));
    };
    let scopes = principal
        .scopes
        .iter()
        .filter_map(|scope| ArtifactScope::parse(scope))
        .collect();
    Ok((
        service,
        RequestContext {
            principal: ArtifactPrincipal {
                label: principal.label,
                scopes,
                // D11: every gateway session is the person at this device.
                // M7 gives this a second answer; nothing else here changes.
                principal: Principal::LocalOwner,
            },
            trace_id: uuid::Uuid::new_v4().to_string(),
        },
    ))
}

fn unavailable() -> Failure {
    Failure::new(
        "artifacts_unavailable",
        "The artifact store is not available.",
        true,
    )
}

async fn call<T, F>(operation: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Failure> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| unavailable())?
}

fn answer<T: serde::Serialize>(result: Result<T, Failure>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(failure) => refuse(status_for(&failure), failure),
    }
}

fn refuse(status: StatusCode, failure: Failure) -> Response {
    let mut response = Json(failure).into_response();
    *response.status_mut() = status;
    response
}

/// The stable codes, one status each. Spelled against
/// `koru_artifact_core::ArtifactError::code` so a renamed code fails here.
fn status_for(failure: &Failure) -> StatusCode {
    use ArtifactError as E;
    let code = failure.code.as_str();
    if code == "forbidden" || code == E::PermissionDenied.code() {
        StatusCode::FORBIDDEN
    } else if code == E::FamilyNotFound.code()
        || code == E::VersionNotFound.code()
        || code == E::BlobNotFound.code()
        || code == E::AuthorityNotFound.code()
    {
        StatusCode::NOT_FOUND
    } else if code == E::FamilyExists.code()
        || code == E::FamilyRetired.code()
        || code == E::VersionImmutable.code()
        || code == E::RegistrationConflict.code()
        || code == E::AuthorityExists.code()
        || code == E::StaleGeneration.code()
        || code == E::AuthorityRevoked.code()
    {
        StatusCode::CONFLICT
    } else if code == E::InvalidRequest.code() {
        StatusCode::BAD_REQUEST
    } else if code == "artifacts_unavailable" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn parse_id<T: std::str::FromStr>(value: &str) -> Result<T, Response> {
    value.parse().map_err(|_| {
        refuse(
            StatusCode::BAD_REQUEST,
            Failure::new(
                "invalid_request",
                "The artifact path identity is not a canonical v4 UUID.",
                false,
            ),
        )
    })
}

/// A body over the route limit is 413 with its own code, because the fix is
/// a different route — the Store's blob upload — and not a smaller retry.
fn json_request<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    payload.map(|Json(value)| value).map_err(|rejection| {
        if let JsonRejection::BytesRejection(_) = rejection {
            refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                Failure::new(
                    "payload_too_large",
                    "The artifact metadata body exceeds the route limit; bytes go through PUT /v1/workspaces/{id}/blobs.",
                    false,
                ),
            )
        } else {
            refuse(
                StatusCode::BAD_REQUEST,
                Failure::new(
                    "invalid_request",
                    "The artifact request body is invalid.",
                    false,
                ),
            )
        }
    })
}

#[cfg(test)]
mod tests;
