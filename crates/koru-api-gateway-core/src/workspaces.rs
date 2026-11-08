// SPDX-License-Identifier: MPL-2.0

//! `POST /v1/workspaces/{id}/lifecycle` — the one enforceable way to remove a
//! workspace.
//!
//! Five steps, in this order and no other (rev 4 §4.2):
//!
//! 1. resolve the workspace and, through it, the scope its sessions hang off;
//! 2. the caller has already presented the consequence and taken its
//!    acceptance, and the request carries that acceptance;
//! 3. close the workspace's sessions idempotently, over the private
//!    session-host bridge — resolving them and closing them are one host
//!    operation, so nothing can be listed and then closed a moment later;
//! 4. apply the Store mutation;
//! 5. the Store publishes the committed fact, and every window invalidates.
//!
//! **Step 5 is not written here on purpose.** The mutation goes through the
//! Store's own routes rather than its methods, so `workspace.catalog.archived`,
//! `.deleted` and `.restored` are published by the same handler that publishes
//! them for every other caller. A second publisher would be a second chance to
//! publish a fact about a commit that did not happen.
//!
//! **If 3 succeeds and 4 fails**, the workspace is unchanged, the terminals are
//! closed, and the answer says exactly that: `code:
//! workspace_lifecycle_incomplete`, an `outcome` carrying `sessions_closed:
//! true` and `mutation_applied: false`, and `retryable: true`. The HTTP status
//! is the mutation's own refusal rather than a flat 500 — a workspace another
//! window put in the trash answers 404, and flattening that would throw away
//! the one fact a caller needs to decide whether retrying can ever work.
//!
//! The retry is the same request. Closing sessions again finds none, and the
//! mutation is attempted against a workspace nothing has moved. There is no
//! journal, because there is nothing to replay: step 3 is idempotent at the
//! host, and step 4 is made idempotent here — the Store's routes are not
//! uniformly so, and a caller must not have to know which ones are.
//!
//! ## Its sibling: `POST /v1/workspaces/{id}/migration` (SP-15a)
//!
//! **Giving a workspace to another profile is the same five steps under a
//! different scope.** It closes the workspace's terminals for the same reason —
//! R8's rule that ownership never changes under a running session, whose point
//! is not that a session would break but that it would silently continue under
//! a profile whose person never started it — and it applies its mutation
//! through the Store's own route so that route stays the only publisher.
//!
//! The two operations share the confirmation ledger and differ in three places,
//! each written where it happens:
//!
//! - the scope is `workspace.migrate`, not `workspace.destroy`. Both take the
//!   Agent's vouching and neither is in a default set, but they are two
//!   permissions: emptying a workspace and handing it to a different persona
//!   are different things to be trusted with.
//! - the token is bound to the **destination** as well as the workspace, so a
//!   caller cannot show one profile in a picker and move to another.
//! - "already there" is decided *before* anything closes, which is the one
//!   ordering that is not the lifecycle's. See [`migration`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use koru_api_wire::{
    WorkspaceConfirmation, WorkspaceConfirmationRequest, WorkspaceLifecycleAction,
    WorkspaceLifecycleOutcome, WorkspaceLifecycleRequest, WorkspaceMigration,
    WorkspaceMigrationConfirmationRequest, WorkspaceMigrationOutcome, WorkspaceMigrationRequest,
    WorkspaceSessionConsequence,
};

use crate::{
    GatewayPrincipal, GatewayState, WORKSPACE_DESTROY_SCOPE, WORKSPACE_MIGRATE_SCOPE, failure,
    now_millis,
};

#[cfg(test)]
mod tests;

/// How long an issued confirmation is accepted for.
///
/// **Long enough to read, short enough to still be about now.** The token's
/// whole value is that the acceptance is about the consequence the gateway
/// computed, and a consequence goes stale as terminals come and go — so this is
/// the width of the window in which a terminal can open unnoticed behind a
/// dialog. Five minutes is a person reading a list of six terminals and
/// deciding; the grant's 120 seconds would be a dialog that refuses somebody
/// who paused to think, and refusing an acceptance is worse than re-asking for
/// one only if the re-ask never happens — which is why an expired token is a
/// re-ask rather than a failure (see [`Ledger::spend`]).
const CONFIRMATION_LIFETIME_MS: i64 = 5 * 60 * 1000;

/// **What a confirmation was issued about.**
///
/// One ledger for both operations, because the thing being held is the same
/// thing — an acceptance of a consequence this gateway computed, spent by being
/// used — and a second ledger would be a second expiry sweep and a second place
/// to forget one. What differs is only the question, so the question is a value.
///
/// The migration's destination is part of the subject and not a detail beside
/// it: a person shown "these four terminals close, and Notes goes to Home" has
/// accepted nothing about a move to some other profile, so a token issued for
/// one destination must not spend against another.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Subject {
    Lifecycle(WorkspaceLifecycleAction),
    Migration { to: String },
}

/// What was issued, and what it was issued about.
struct Issued {
    /// **The id the question is about** — a workspace for the two workspace
    /// operations, a profile for the removal. One field rather than three,
    /// because the ledger's job is to answer "was this token issued for this
    /// question", and which noun the id names is the [`Subject`]'s business.
    about: String,
    subject: Subject,
    expires_at: i64,
}

/// **Confirmations that were issued and not yet spent.**
///
/// The same shape as the Agent's grant ledger and for the same reason: a token
/// is spent by being used, so what is held here is only what nobody has
/// redeemed. It is bounded by the expiry rather than by a cap — every issue
/// sweeps what has run out, so a caller that opens dialogs and closes them
/// without answering leaves at most five minutes' worth behind.
#[derive(Default)]
pub struct Ledger {
    issued: Mutex<HashMap<String, Issued>>,
}

impl Ledger {
    fn issue(&self, about: &str, subject: Subject, now: i64) -> (String, i64) {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = now + CONFIRMATION_LIFETIME_MS;
        let mut issued = self.issued.lock().unwrap_or_else(|held| held.into_inner());
        issued.retain(|_, entry| entry.expires_at > now);
        issued.insert(
            token.clone(),
            Issued {
                about: about.to_owned(),
                subject,
                expires_at,
            },
        );
        (token, expires_at)
    }

    /// **Redeem a token, once.** Taken out of the ledger whether or not it
    /// matches: a token presented for the wrong workspace has been seen, and
    /// leaving it live would let a caller keep guessing which workspace it was
    /// about.
    fn spend(&self, token: &str, about: &str, subject: &Subject, now: i64) -> bool {
        let entry = self
            .issued
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .remove(token);
        entry.is_some_and(|entry| {
            entry.expires_at > now && entry.about == about && &entry.subject == subject
        })
    }
}

/// One catalogued lifecycle route. The same table shape the settings surface
/// uses, for the same reason: the scope column is the whole of the
/// authorization story and there is nowhere else to look.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceRoute {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub scope: &'static str,
    pub summary: &'static str,
    pub returns: &'static str,
}

pub const ROUTES: &[WorkspaceRoute] = &[
    WorkspaceRoute {
        operation_id: "workspace_lifecycle_confirmation",
        method: "POST",
        path: "/v1/workspaces/{id}/lifecycle/confirmation",
        scope: WORKSPACE_DESTROY_SCOPE,
        summary: "Compute what an action would close, and issue the token that proves it was shown",
        returns: "The consequence — the terminals this action would close, named — and a single-use token bound to this workspace and this action. The caller presents the consequence, and sends the token back in the lifecycle request if the person accepts. The same scope as the operation itself: computing what would be destroyed is not a smaller permission than destroying it.",
    },
    WorkspaceRoute {
        operation_id: "workspace_lifecycle",
        method: "POST",
        path: "/v1/workspaces/{id}/lifecycle",
        scope: WORKSPACE_DESTROY_SCOPE,
        summary: "Archive, soft-delete or restore a workspace, closing its terminals first",
        returns: "What the operation did: the sessions it closed, whether they all closed, and whether the catalog mutation committed. A refusal carrying workspace_lifecycle_incomplete with sessions_closed: true and mutation_applied: false is a partial failure — the terminals are gone and the catalog is unchanged — and the same request again is the retry, which needs a fresh confirmation because the first one was spent. Asking for a state the workspace is already in succeeds and closes nothing.",
    },
    WorkspaceRoute {
        operation_id: "workspace_migration_confirmation",
        method: "POST",
        path: "/v1/workspaces/{id}/migration/confirmation",
        scope: WORKSPACE_MIGRATE_SCOPE,
        summary: "Compute what moving a workspace to a profile would close, and issue the token that proves it was shown",
        returns: "Both ends of the move — the profile that owns the workspace and the one it would go to — the terminals the move would close, named, and a single-use token bound to this workspace and this destination. A token issued for one destination does not spend against another: a person accepted a move to a particular profile, not a move. The same scope as the operation itself.",
    },
    WorkspaceRoute {
        operation_id: "workspace_migration",
        method: "POST",
        path: "/v1/workspaces/{id}/migration",
        scope: WORKSPACE_MIGRATE_SCOPE,
        summary: "Give a workspace to another profile, closing its terminals first",
        returns: "What the operation did: both ends, the sessions it closed, and whether the reassignment committed. A refusal carrying workspace_migration_incomplete with sessions_closed: true and mutation_applied: false is a partial failure — the terminals are gone and the workspace has not moved — and the same request again is the retry, which needs a fresh confirmation because the first one was spent. Moving a workspace to the profile that already owns it succeeds and closes nothing.",
    },
];

pub fn router() -> Router<Arc<GatewayState>> {
    Router::new()
        .route(
            "/v1/workspaces/{id}/lifecycle/confirmation",
            post(confirmation),
        )
        .route("/v1/workspaces/{id}/lifecycle", post(lifecycle))
        .route(
            "/v1/workspaces/{id}/migration/confirmation",
            post(migration_confirmation),
        )
        .route("/v1/workspaces/{id}/migration", post(migration))
}

pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::{Map, Value, json};

    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let request = schemas.subschema_for::<WorkspaceLifecycleRequest>();
    let outcome = schemas.subschema_for::<WorkspaceLifecycleOutcome>();
    let asking = schemas.subschema_for::<WorkspaceConfirmationRequest>();
    let consequence = schemas.subschema_for::<WorkspaceConfirmation>();
    let asking_move = schemas.subschema_for::<WorkspaceMigrationConfirmationRequest>();
    let move_consequence = schemas.subschema_for::<WorkspaceMigration>();
    let move_request = schemas.subschema_for::<WorkspaceMigrationRequest>();
    let move_outcome = schemas.subschema_for::<WorkspaceMigrationOutcome>();

    let paths = document["paths"]
        .as_object_mut()
        .expect("gateway OpenAPI paths");
    for route in ROUTES {
        let (body_schema, ok_schema) = match route.operation_id {
            "workspace_lifecycle_confirmation" => (&asking, &consequence),
            "workspace_lifecycle" => (&request, &outcome),
            "workspace_migration_confirmation" => (&asking_move, &move_consequence),
            "workspace_migration" => (&move_request, &move_outcome),
            _ => (&move_request, &move_outcome),
        };
        let refusals = match route.operation_id {
            "workspace_lifecycle_confirmation" => json!({
                "400": {"description": "The body is not a confirmation request"},
                "401": {"description": "The gateway bearer is absent or invalid"},
                "403": {"description": "The session does not hold workspace.destroy"},
                "404": {"description": "There is no such workspace, deleted ones included"},
                "503": {"description": "The catalog could not be read, so no consequence could be computed"}
            }),
            "workspace_lifecycle" => json!({
                "400": {"description": "The body is not a lifecycle request, or the confirmation is not one this gateway issued for this workspace and this action — expired, spent, or never minted here"},
                "401": {"description": "The gateway bearer is absent or invalid"},
                "403": {"description": "The session does not hold workspace.destroy"},
                "404": {"description": "There is no such workspace, deleted ones included — or the mutation refused because another window removed it first, which is a partial failure carrying its outcome"},
                "409": {"description": "The mutation refused: the body carries the outcome, so a partial failure is told apart from nothing having happened"},
                "500": {"description": "A partial failure: the body says which steps completed and whether a retry is safe"},
                "503": {"description": "The session-host bridge is unavailable, so no terminal could be closed"}
            }),
            "workspace_migration_confirmation" => json!({
                "400": {"description": "The body is not a migration confirmation request, or the destination is not a profile id"},
                "401": {"description": "The gateway bearer is absent or invalid"},
                "403": {"description": "The session does not hold workspace.migrate"},
                "404": {"description": "There is no such workspace, or no such destination profile — a consequence is never computed for a move that could not happen"},
                "503": {"description": "The catalog could not be read, so no consequence could be computed"}
            }),
            _ => json!({
                "400": {"description": "The body is not a migration request, or the confirmation is not one this gateway issued for this workspace and this destination — expired, spent, or never minted here"},
                "401": {"description": "The gateway bearer is absent or invalid"},
                "403": {"description": "The session does not hold workspace.migrate"},
                "404": {"description": "There is no such workspace, or the reassignment refused because the destination profile is gone — which after the terminals closed is a partial failure carrying its outcome"},
                "409": {"description": "The reassignment refused: the body carries the outcome, so a partial failure is told apart from nothing having happened"},
                "500": {"description": "A partial failure: the terminals are closed, the workspace has not moved, and the body says so"},
                "503": {"description": "The session-host bridge is unavailable, so no terminal could be closed"}
            }),
        };
        let mut responses = json!({
            "200": {
                "description": route.returns,
                "content": {"application/json": {"schema": ok_schema}}
            }
        });
        responses
            .as_object_mut()
            .expect("lifecycle responses")
            .extend(
                refusals
                    .as_object()
                    .expect("lifecycle refusals")
                    .clone()
                    .into_iter(),
            );
        let operation = json!({
            "operationId": route.operation_id,
            "summary": route.summary,
            "description": route.returns,
            "security": [{"bearerAuth": [route.scope]}],
            "parameters": [{
                "name": "id",
                "in": "path",
                "required": true,
                "schema": {"type": "string", "format": "uuid"}
            }],
            "requestBody": {
                "required": true,
                "content": {"application/json": {"schema": body_schema}}
            },
            "responses": responses
        });
        let entry = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        entry
            .as_object_mut()
            .expect("workspace lifecycle OpenAPI path")
            .insert(route.method.to_lowercase(), operation);
    }
    let components = document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas");
    components.extend(
        schemas
            .take_definitions(true)
            .into_iter()
            .map(|(name, schema)| {
                (
                    name,
                    serde_json::to_value(schema).expect("lifecycle schema"),
                )
            }),
    );
}

// ---------------------------------------------------------- the consequence

/// **Step 2's first half: what would happen, computed here rather than
/// described by the caller.**
///
/// The dialog asks this before it draws, presents what comes back (S13), and
/// sends the token back with the acceptance. Two things follow from the gateway
/// computing the list rather than the caller:
///
/// - the acceptance is *about* something. A token the caller minted said only
///   that somebody clicked; this one says which consequence was on screen when
///   they did.
/// - the terminals are named by the authority that would close them, so the
///   dialog cannot be wrong about them by being out of date in a different way
///   than the operation is.
///
/// **The same scope as the operation.** Working out what would be destroyed is
/// not a smaller permission than destroying it — the list names a person's live
/// terminals and where they are working, and a caller that may not remove a
/// workspace has no business enumerating them.
async fn confirmation(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<String>,
    payload: Result<Json<WorkspaceConfirmationRequest>, JsonRejection>,
) -> Response {
    if !principal
        .scopes
        .iter()
        .any(|held| held == WORKSPACE_DESTROY_SCOPE)
    {
        return failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "this session does not hold `workspace.destroy`",
        );
    }
    let Ok(Json(request)) = payload else {
        return invalid("the confirmation body names the action being confirmed");
    };
    let Some(workspace) = uuid::Uuid::parse_str(&id).ok().map(|id| id.to_string()) else {
        return invalid("that is not a workspace id");
    };
    let row = match resolve(&state, &principal, &workspace).await {
        Ok(row) => row,
        Err(response) => return response,
    };

    // **The terminals, if this action would close any.** `restore` closes
    // nothing, so it asks the host nothing: a soft-deleted workspace's
    // consequence is that its projects reappear, which is not a list.
    //
    // A bridge that is absent or refuses leaves the list empty rather than
    // failing the issue. The operation refuses in its own words when it cannot
    // close terminals, and a dialog that could not even be opened would leave a
    // person unable to find out why.
    let mut sessions = Vec::new();
    if request.action.closes_sessions()
        && let Some(owner) = row["owner"].as_str().and_then(|owner| owner.parse().ok())
        && let Some(bridge) = state.session_bridge()
    {
        let scope = koru_session_protocol::WorkspaceScope {
            profile: owner,
            workspace: workspace
                .parse()
                .expect("a workspace id that parsed as a uuid"),
        };
        if let Ok(live) = bridge.sessions_in_scope(scope).await {
            sessions = live
                .into_iter()
                .map(|session| WorkspaceSessionConsequence {
                    id: session.id.to_string(),
                    display_name: session.display_name,
                    working_directory: session.working_directory,
                })
                .collect();
        }
    }

    let (token, expires_at) =
        state
            .confirmations
            .issue(&workspace, Subject::Lifecycle(request.action), now_millis());
    Json(WorkspaceConfirmation {
        confirmation: token,
        workspace,
        action: request.action,
        sessions,
        expires_at,
    })
    .into_response()
}

// ------------------------------------------------------------------- handler

async fn lifecycle(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<String>,
    payload: Result<Json<WorkspaceLifecycleRequest>, JsonRejection>,
) -> Response {
    // **The scope first, before the id is even parsed.** A caller that may not
    // remove a workspace learns that, and not whether this one exists.
    if !principal
        .scopes
        .iter()
        .any(|held| held == WORKSPACE_DESTROY_SCOPE)
    {
        return failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "this session does not hold `workspace.destroy`",
        );
    }
    let Ok(Json(request)) = payload else {
        return invalid("the workspace lifecycle body is not a lifecycle request");
    };
    if request.removal_attempt.is_some() && request.action != WorkspaceLifecycleAction::SoftDelete {
        return invalid("a Profile removal attempt applies only to soft delete");
    }
    if request.confirmation.trim().is_empty() {
        return invalid("a lifecycle request carries the confirmation that was accepted");
    }
    let Some(workspace) = uuid::Uuid::parse_str(&id).ok().map(|id| id.to_string()) else {
        return invalid("that is not a workspace id");
    };
    // **The acceptance, before the workspace is even resolved.** The token was
    // issued alongside a consequence this gateway computed, so redeeming it is
    // what makes the acceptance be about that consequence rather than about
    // whatever the caller says it displayed. It is spent here: a retry after a
    // partial failure asks for a fresh one, which re-reads the terminals — the
    // right behaviour, because by then the first list is describing a
    // workspace whose sessions are already closed.
    //
    // A refusal is 400 and not 403: the caller holds the permission, it is the
    // question it is answering that this gateway does not recognise.
    if !state.confirmations.spend(
        &request.confirmation,
        &workspace,
        &Subject::Lifecycle(request.action),
        now_millis(),
    ) {
        return invalid(
            "that confirmation was not issued by this gateway for this workspace and this action, \
             or it has already been used or has expired — ask for the consequence again",
        );
    }

    // Step 1a: the workspace, from the including-deleted projection — `restore`
    // is about a row the ordinary listing hides, so the ordinary listing is the
    // wrong question to ask about any of the three.
    let row = match resolve(&state, &principal, &workspace).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    let owner = match row["owner"].as_str().and_then(|owner| owner.parse().ok()) {
        Some(owner) => owner,
        None => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_unavailable",
                "the catalog answered a workspace with no owner",
            );
        }
    };
    if let Err(response) =
        removal_preflight(&state, &principal, &row, request.removal_attempt).await
    {
        return response;
    }
    // Replacing verbs must run even after a catalog reopen repaired the source
    // projection: the replacement obligation may still be outstanding.
    let already_applied = match request.action {
        WorkspaceLifecycleAction::Archive | WorkspaceLifecycleAction::SoftDelete => false,
        WorkspaceLifecycleAction::Restore => row["deleted_at"].is_null(),
    };

    // Step 1b: its live sessions. The session host owns the join — a session
    // carries a project, a project carries a workspace scope — so this asks
    // rather than reconstructing it.
    let scope = koru_session_protocol::WorkspaceScope {
        profile: owner,
        workspace: workspace
            .parse()
            .expect("a workspace id that parsed as a uuid"),
    };

    let mut outcome = WorkspaceLifecycleOutcome {
        workspace: workspace.clone(),
        action: request.action,
        terminated_sessions: Vec::new(),
        sessions_closed: !request.action.closes_sessions(),
        mutation_applied: false,
        authority_committed: (request.action != WorkspaceLifecycleAction::Restore).then_some(false),
        listing: None,
        replacement: None,
    };

    // Steps 2 and 3. Restore closes nothing: a soft-deleted workspace has no
    // terminals to announce, and bringing one back is not a consequence
    // anybody has to accept the closing of.
    if request.action.closes_sessions() {
        let Some(bridge) = state.session_bridge() else {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability_unavailable",
                "this gateway has no session-host bridge, so it cannot close a workspace's terminals",
            );
        };
        if !bridge.lifecycle_supervised() {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability_unavailable",
                "this gateway was not supervised, so it holds no session-lifecycle authority",
            );
        }
        match bridge.terminate_workspace_sessions(scope).await {
            Ok(terminated) => {
                outcome.terminated_sessions = terminated.iter().map(|id| id.to_string()).collect();
                outcome.sessions_closed = true;
            }
            Err(error) => {
                return refused(
                    bridge_status(&error),
                    "workspace_sessions_not_closed",
                    "the workspace's terminals could not be closed, so nothing was changed",
                    outcome,
                    true,
                );
            }
        }
    }

    if already_applied {
        outcome.mutation_applied = true;
        return Json(outcome).into_response();
    }

    // Step 4, and step 5 with it: the Store's own routes publish the committed
    // fact, so this reaches them rather than the methods behind them. A
    // confirmed soft delete uses the Store's existing force contract: all
    // acknowledged edits are already durable, affected document rooms are
    // disconnected, and their windows leave on the catalog event.
    match apply(
        &state,
        &principal,
        &workspace,
        request.action,
        &request.replacement_name,
        request.removal_attempt,
    )
    .await
    {
        Ok(removed) => {
            outcome.mutation_applied = true;
            if let Some(removed) = removed {
                apply_removal_facts(&mut outcome, removed);
            }
            Json(outcome).into_response()
        }
        // **S24, told exactly.** The terminals are closed and the catalog is
        // unchanged; the workspace is where it was, the answer says so, and the
        // same request again is the retry — closing sessions finds none and the
        // mutation is tried against a row nothing moved. The retry needs a
        // fresh confirmation, which is right: by then the consequence is a
        // workspace whose terminals are already gone.
        //
        // **And it says what refused.** `cause` carries the Store's own code,
        // so a surface can tell "another window removed it first" from "close
        // its open documents and try again" — the second of which is a thing a
        // person can do something about.
        Err(response) => {
            let refusal = refusal_of(response).await;
            if let Some(removed) = refusal.removal.clone() {
                apply_removal_facts(&mut outcome, removed);
            }
            caused(
                refusal.status,
                "workspace_lifecycle_incomplete",
                "the workspace's terminals were closed and the catalog change did not commit",
                outcome,
                &refusal,
            )
        }
    }
}

// ------------------------------------------------------ the migration, SP-15a

/// **The move's consequence, computed here for the lifecycle's reasons.**
///
/// Two things it does that the lifecycle's does not:
///
/// - **it refuses a destination that does not exist**, rather than issuing a
///   token for a move that cannot happen. A dialog whose whole content is "these
///   four terminals will close" must not be shown for an operation guaranteed to
///   refuse afterwards, because a person would have accepted the closing and got
///   nothing for it.
/// - **it enumerates nothing when the workspace is already there.** A retry
///   after a move that committed finds terminals belonging to the destination,
///   and they are somebody's live work in the profile that now owns them.
///   Naming them would be asking a person to accept closing terminals that the
///   operation has no reason to close.
async fn migration_confirmation(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<String>,
    payload: Result<Json<WorkspaceMigrationConfirmationRequest>, JsonRejection>,
) -> Response {
    if !principal
        .scopes
        .iter()
        .any(|held| held == WORKSPACE_MIGRATE_SCOPE)
    {
        return denied();
    }
    let Ok(Json(request)) = payload else {
        return invalid("the confirmation body names the profile the workspace would move to");
    };
    let Some(workspace) = uuid::Uuid::parse_str(&id).ok().map(|id| id.to_string()) else {
        return invalid("that is not a workspace id");
    };
    let Some(to) = uuid::Uuid::parse_str(&request.to)
        .ok()
        .map(|id| id.to_string())
    else {
        return invalid("that is not a profile id");
    };
    let row = match resolve(&state, &principal, &workspace).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    let Some(from) = row["owner"].as_str().map(str::to_owned) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the catalog answered a workspace with no owner",
        );
    };
    if let Err(response) = profile_exists(&state, &principal, &to).await {
        return response;
    }

    let mut sessions = Vec::new();
    if from != to
        && let Ok(owner) = from.parse()
        && let Some(bridge) = state.session_bridge()
    {
        let scope = koru_session_protocol::WorkspaceScope {
            profile: owner,
            workspace: workspace
                .parse()
                .expect("a workspace id that parsed as a uuid"),
        };
        if let Ok(live) = bridge.sessions_in_scope(scope).await {
            sessions = live
                .into_iter()
                .map(|session| WorkspaceSessionConsequence {
                    id: session.id.to_string(),
                    display_name: session.display_name,
                    working_directory: session.working_directory,
                })
                .collect();
        }
    }

    let (token, expires_at) = state.confirmations.issue(
        &workspace,
        Subject::Migration { to: to.clone() },
        now_millis(),
    );
    Json(WorkspaceMigration {
        confirmation: token,
        workspace,
        from,
        to,
        sessions,
        expires_at,
    })
    .into_response()
}

/// **The move, in the lifecycle's five steps with one reordered.**
///
/// Steps 1 and 2 are unchanged: resolve the workspace, spend the acceptance.
/// Step 3 closes the workspace's terminals — R8's rule is that ownership never
/// changes under a running session, and the reason is not that the session would
/// break but that it would silently continue under a profile whose person never
/// started it. Step 4 is the Store's own route, so step 5 — the committed fact —
/// is published by the same handler that publishes it for every other caller.
///
/// **The one reordering: "already there" is decided before anything closes.**
/// The lifecycle closes first and then short-circuits, which is right for it —
/// an archived workspace should have no terminals, so closing them on a repeat
/// is the operation finishing its own job. It is wrong here. A workspace already
/// owned by the destination has terminals that belong to the destination, opened
/// by the person who now owns it, and a retry that closed them would be
/// destroying live work to make a no-op look symmetrical.
async fn migration(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<String>,
    payload: Result<Json<WorkspaceMigrationRequest>, JsonRejection>,
) -> Response {
    // The scope first, before the id is even parsed.
    if !principal
        .scopes
        .iter()
        .any(|held| held == WORKSPACE_MIGRATE_SCOPE)
    {
        return denied();
    }
    let Ok(Json(request)) = payload else {
        return invalid("the workspace migration body is not a migration request");
    };
    if request.confirmation.trim().is_empty() {
        return invalid("a migration request carries the confirmation that was accepted");
    }
    let Some(workspace) = uuid::Uuid::parse_str(&id).ok().map(|id| id.to_string()) else {
        return invalid("that is not a workspace id");
    };
    let Some(to) = uuid::Uuid::parse_str(&request.to)
        .ok()
        .map(|id| id.to_string())
    else {
        return invalid("that is not a profile id");
    };
    // The acceptance, bound to this workspace *and this destination*.
    if !state.confirmations.spend(
        &request.confirmation,
        &workspace,
        &Subject::Migration { to: to.clone() },
        now_millis(),
    ) {
        return invalid(
            "that confirmation was not issued by this gateway for this workspace and this \
             destination, or it has already been used or has expired — ask for the consequence \
             again",
        );
    }

    let row = match resolve(&state, &principal, &workspace).await {
        Ok(row) => row,
        Err(response) => return response,
    };
    let Some(from) = row["owner"].as_str().map(str::to_owned) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the catalog answered a workspace with no owner",
        );
    };

    let mut outcome = WorkspaceMigrationOutcome {
        workspace: workspace.clone(),
        from: from.clone(),
        to: to.clone(),
        terminated_sessions: Vec::new(),
        sessions_closed: true,
        mutation_applied: false,
        projects_rescoped: None,
        replacement: None,
    };

    // A same-owner retry still reaches the replacing operation and project
    // repair. It must not terminate sessions opened under the new owner.
    if from != to {
        let Some(bridge) = state.session_bridge() else {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability_unavailable",
                "this gateway has no session-host bridge, so it cannot close a workspace's terminals",
            );
        };
        if !bridge.lifecycle_supervised() {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability_unavailable",
                "this gateway was not supervised, so it holds no session-lifecycle authority",
            );
        }
        let Ok(owner) = from.parse() else {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_unavailable",
                "the catalog answered a workspace whose owner is not an id",
            );
        };
        let scope = koru_session_protocol::WorkspaceScope {
            profile: owner,
            workspace: workspace
                .parse()
                .expect("a workspace id that parsed as a uuid"),
        };
        outcome.sessions_closed = false;
        match bridge.terminate_workspace_sessions(scope).await {
            Ok(terminated) => {
                outcome.terminated_sessions = terminated.iter().map(|id| id.to_string()).collect();
                outcome.sessions_closed = true;
            }
            Err(error) => {
                return refused_move(
                    bridge_status(&error),
                    "workspace_sessions_not_closed",
                    "the workspace's terminals could not be closed, so it was not moved",
                    outcome,
                    true,
                );
            }
        }
    }

    match crate::store_json(
        &state,
        &principal,
        "POST",
        &format!("/v1/workspaces/{workspace}/move-replacing"),
        Some(serde_json::json!({ "owner": to, "name": request.replacement_name })),
    )
    .await
    {
        Ok(moved) => {
            outcome.mutation_applied = true;
            outcome.replacement = serde_json::from_value(moved["replacement"].clone()).ok();
            if let Some(from) = moved["from"].as_str() {
                outcome.from = from.to_owned();
            }
            // **Step 6, and it runs after the commit rather than before it.**
            // A project's scope names a profile and a workspace; between the
            // Store write and this call those projects name the profile the
            // workspace has just left, and the terminal's derived visibility
            // puts them in the missing-workspace area — which is the truth for
            // that instant. Repairing first would have inverted it: the
            // projects would name a profile that did not own the workspace yet,
            // which is a lie rather than an absence.
            //
            // A repair that cannot run does not fail the move. The workspace
            // has moved and the fact is published; `projects_rescoped: null`
            // says nobody has repaired them yet, and the same request again is
            // what does.
            outcome.projects_rescoped = rescope(&state, &workspace, &to).await;
            Json(outcome).into_response()
        }
        // **S24 for a move.** The terminals are closed and the workspace is
        // where it was. The same request again is the retry: closing terminals
        // finds none, and the reassignment is attempted against current
        // ownership rather than replayed from a remembered state.
        //
        // `cause` carries the Store's own code, so a surface can tell "that
        // profile is gone" from "the store is not answering" — the first of
        // which is a thing a person can do something about, by picking another
        // destination.
        Err(response) => {
            let refusal = refusal_of(response).await;
            caused_move(
                refusal.status,
                "workspace_migration_incomplete",
                "the workspace's terminals were closed and it did not move",
                outcome,
                &refusal,
            )
        }
    }
}

/// **Step 6: the projects follow the workspace.**
///
/// `None` when there was nobody to ask — no bridge, no lifecycle authority, or
/// a host that did not answer. That is not an error and must not be reported as
/// one: the workspace has moved and every other reader of the catalog is right.
/// It is a fact about the session domain, and the caller's answer says which of
/// the two it is.
async fn rescope(state: &Arc<GatewayState>, workspace: &str, to: &str) -> Option<Vec<String>> {
    let bridge = state.session_bridge()?;
    let workspace = workspace.parse().ok()?;
    let profile = to.parse().ok()?;
    bridge
        .rescope_workspace_projects(workspace, profile)
        .await
        .ok()
        .map(|projects| projects.iter().map(|id| id.to_string()).collect())
}

/// Whether a profile is one this catalog has.
///
/// **Asked before the token is issued, not only before the write.** The Store
/// refuses the move anyway — that is the backstop — but by then a person has
/// accepted the closing of their terminals for a move that was never going to
/// happen.
async fn profile_exists(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    profile: &str,
) -> Result<(), Response> {
    match crate::store_json(
        state,
        principal,
        "GET",
        &format!("/v1/profiles/{profile}"),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(response) => {
            let refusal = refusal_of(response).await;
            Err(failure(
                match refusal.status {
                    StatusCode::NOT_FOUND => StatusCode::NOT_FOUND,
                    other if other.is_client_error() => StatusCode::NOT_FOUND,
                    other => other,
                },
                "not_found",
                "there is no such profile to move this workspace to",
            ))
        }
    }
}

/// The workspace's catalog row, deleted ones included.
///
/// **Including-deleted, for all three actions and not only `restore`.** The
/// ordinary listing hides a workspace in the trash, and this row answers two
/// questions: which profile owns it — the other half of the scope the session
/// host joins on, and not something the caller is asked for, so a 404 here
/// means "no such workspace" rather than "you named the wrong owner" — and
/// whether the action has already been applied.
async fn resolve(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    workspace: &str,
) -> Result<serde_json::Value, Response> {
    let body = crate::store_json(
        state,
        principal,
        "GET",
        "/v1/workspaces?include_deleted=true",
        None,
    )
    .await
    .map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_unavailable",
            "the catalog could not be read",
        )
    })?;
    body.as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"].as_str() == Some(workspace))
        .cloned()
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "not_found",
                "there is no such workspace",
            )
        })
}

/// **The Store's own refusal, kept whole.**
///
/// Step 4 goes through the Store's routes, and one of their refusals is a fact
/// a person can act on rather than an internal detail: `workspace_in_use` means
/// the workspace has open document tabs and the soft delete will keep refusing
/// until they are closed. Flattening it into "the catalog change did not
/// commit" would leave a surface able to say only that something went wrong,
/// when the honest answer — and the one SP-8b's contract requires until the
/// owner decides whether the operation should close documents too — is *which*
/// thing and what would fix it.
struct Refusal {
    status: StatusCode,
    code: String,
    message: String,
    removal: Option<koru_store_wire::RemovedWorkspace>,
}

/// Read a Store refusal out of the response it answered with.
async fn refusal_of(response: Response) -> Refusal {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    Refusal {
        status,
        code: body["cause"]
            .as_str()
            .or_else(|| body["code"].as_str())
            .unwrap_or_default()
            .to_owned(),
        message: body["message"].as_str().unwrap_or_default().to_owned(),
        removal: serde_json::from_value(body["outcome"].clone()).ok(),
    }
}

/// Step 4, through the Store's public routes.
fn soft_delete_uri(workspace: &str) -> String {
    format!("/v1/workspaces/{workspace}/delete-replacing")
}

async fn apply(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    workspace: &str,
    action: WorkspaceLifecycleAction,
    replacement_name: &str,
    removal_attempt: Option<koru_store_core::ids::RemovalAttemptId>,
) -> Result<Option<koru_store_wire::RemovedWorkspace>, Response> {
    let (method, uri, body) = match action {
        WorkspaceLifecycleAction::Archive => (
            "POST",
            format!("/v1/workspaces/{workspace}/archive-replacing"),
            Some(serde_json::json!({"name": replacement_name})),
        ),
        WorkspaceLifecycleAction::SoftDelete => (
            "POST",
            soft_delete_uri(workspace),
            Some(
                serde_json::json!({"name": replacement_name, "force": true, "removal_attempt": removal_attempt}),
            ),
        ),
        WorkspaceLifecycleAction::Restore => {
            ("POST", format!("/v1/workspaces/{workspace}/restore"), None)
        }
    };
    let body = crate::store_json(state, principal, method, &uri, body).await?;
    if action == WorkspaceLifecycleAction::Restore {
        return Ok(None);
    }
    serde_json::from_value(body).map(Some).map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_response",
            "Store returned invalid lifecycle facts",
        )
    })
}

async fn removal_preflight(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    row: &serde_json::Value,
    attempt: Option<koru_store_core::ids::RemovalAttemptId>,
) -> Result<(), Response> {
    let owner = row["owner"]
        .as_str()
        .ok_or_else(|| invalid("workspace has no owner"))?;
    let value = crate::store_json(
        state,
        principal,
        "GET",
        &format!("/v1/profiles/{owner}/removal"),
        None,
    )
    .await?;
    let mark: Option<koru_store_wire::RemovalMark> =
        serde_json::from_value(value).map_err(|_| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "invalid_response",
                "Store returned an invalid removal mark",
            )
        })?;
    let code = match attempt {
        Some(attempt) => koru_store_core::profile_removal::require_attempt(mark.as_ref(), attempt)
            .err()
            .map(|error| error.code()),
        None if mark.is_some() => Some(koru_store_core::Code::ProfileBeingRemoved),
        None => None,
    };
    if let Some(code) = code {
        return Err(failure(
            StatusCode::CONFLICT,
            code.as_str(),
            "Profile removal attempt refused",
        ));
    }
    Ok(())
}

fn apply_removal_facts(
    outcome: &mut WorkspaceLifecycleOutcome,
    removed: koru_store_wire::RemovedWorkspace,
) {
    outcome.authority_committed = Some(removed.authority_committed);
    outcome.listing = Some(removed.listing);
    outcome.replacement = Some(removed.replacement);
}

fn bridge_status(error: &crate::BridgeError) -> StatusCode {
    match error {
        crate::BridgeError::Saturated => StatusCode::SERVICE_UNAVAILABLE,
        crate::BridgeError::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        crate::BridgeError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn invalid(message: &str) -> Response {
    failure(StatusCode::BAD_REQUEST, "invalid_request", message)
}

/// **What a caller without the migration scope is told, and no more.** It does
/// not say which workspace, or whether one exists: the scope is checked before
/// the id is parsed, so there is nothing here that a caller could not have
/// learned by asking about a workspace it invented.
fn denied() -> Response {
    denied_without(WORKSPACE_MIGRATE_SCOPE)
}

/// The same, naming the scope that was missing.
///
/// Three operations under three scopes now share this refusal, and telling a
/// caller which permission it lacks is the difference between a message it can
/// act on and one it has to guess about — while still saying nothing about
/// whether the id it named exists.
fn denied_without(scope: &str) -> Response {
    failure(
        StatusCode::FORBIDDEN,
        "permission_denied",
        &format!("this session does not hold `{scope}`"),
    )
}

/// A refusal that carries the outcome, because a caller reporting S24 needs the
/// same body it would have read on success.
fn refused(
    status: StatusCode,
    code: &str,
    message: &str,
    outcome: WorkspaceLifecycleOutcome,
    retryable: bool,
) -> Response {
    let body = serde_json::json!({
        "code": code,
        "message": message,
        "waiting_on": [],
        "retryable": retryable,
        "outcome": outcome,
    });
    let mut response = Json(body).into_response();
    *response.status_mut() = status;
    response
}

/// [`refused`], for the migration's outcome. Two functions rather than one
/// generic over the body, because the two outcomes are two public wire types
/// and a shared writer would only be shared until one of them grew a field.
fn refused_move(
    status: StatusCode,
    code: &str,
    message: &str,
    outcome: WorkspaceMigrationOutcome,
    retryable: bool,
) -> Response {
    let body = serde_json::json!({
        "code": code,
        "message": message,
        "waiting_on": [],
        "retryable": retryable,
        "outcome": outcome,
    });
    let mut response = Json(body).into_response();
    *response.status_mut() = status;
    response
}

/// [`caused`], for the migration's outcome.
fn caused_move(
    status: StatusCode,
    code: &str,
    message: &str,
    outcome: WorkspaceMigrationOutcome,
    refusal: &Refusal,
) -> Response {
    let body = serde_json::json!({
        "code": code,
        "message": message,
        "waiting_on": [],
        "retryable": true,
        "outcome": outcome,
        "cause": refusal.code,
        "cause_message": refusal.message,
    });
    let mut response = Json(body).into_response();
    *response.status_mut() = status;
    response
}

/// A refusal that carries the outcome *and* what refused underneath it.
fn caused(
    status: StatusCode,
    code: &str,
    message: &str,
    outcome: WorkspaceLifecycleOutcome,
    refusal: &Refusal,
) -> Response {
    let body = serde_json::json!({
        "code": code,
        "message": message,
        "waiting_on": [],
        "retryable": true,
        "outcome": outcome,
        // The Store's own words, kept beside this operation's. A caller reads
        // `cause` when it has something to say about a particular refusal and
        // ignores it otherwise; `code` stays the stable thing to branch on.
        "cause": refusal.code,
        "cause_message": refusal.message,
    });
    let mut response = Json(body).into_response();
    *response.status_mut() = status;
    response
}
