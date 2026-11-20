// SPDX-License-Identifier: MPL-2.0
//! Typed local references; no stored content or scalar settings fields.
use crate::{GatewayPrincipal, GatewayState, SETTINGS_READ_SCOPE, SETTINGS_WRITE_SCOPE, failure};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use koru_settings::pins::{self, Mutation, Request, Scope};
use std::sync::Arc;
pub const ROUTES: &[crate::settings::SettingsRoute] = &[
    crate::settings::SettingsRoute {
        operation_id: "mini_app_pins_list",
        method: "GET",
        path: "/v1/mini-app-pins",
        scope: SETTINGS_READ_SCOPE,
        summary: "Read this device's mini-app pins for a profile/workspace",
        returns: "Requires settings.read and store.read_documents. Ordered immutable artifact references. Unknown or unreadable files are refused and preserved.",
    },
    crate::settings::SettingsRoute {
        operation_id: "mini_app_pins_mutate",
        method: "PATCH",
        path: "/v1/mini-app-pins",
        scope: SETTINGS_WRITE_SCOPE,
        summary: "Add, remove or place one device-local mini-app pin",
        returns: "Requires settings.write and store.read_documents; Add also requires artifacts.read. The authoritative ordered references after an idempotent mutation. Unpin never deletes artifacts.",
    },
];
pub fn router() -> Router<Arc<GatewayState>> {
    Router::new()
        .route("/v1/mini-app-pins", get(list).patch(mutate))
        .layer(DefaultBodyLimit::max(4096))
}
fn refuse(message: &str) -> Response {
    failure(StatusCode::BAD_REQUEST, "mini_app_pins_refused", message)
}
fn permits(principal: &GatewayPrincipal, required: &str) -> bool {
    [required, "store.read_documents"]
        .iter()
        .all(|required| principal.scopes.iter().any(|s| s == required))
}
async fn authorize(
    state: &Arc<GatewayState>,
    principal: &GatewayPrincipal,
    scope: &Scope,
    required: &str,
) -> std::result::Result<pins::Authority, Response> {
    if !permits(principal, required) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "This session cannot access mini-app pins.",
        ));
    }
    scope.validate().map_err(|e| refuse(&e.to_string()))?;
    let body = crate::store_json(state, principal, "GET", "/v1/workspaces", None).await?;
    let workspaces: Vec<koru_store_core::model::Workspace> =
        serde_json::from_value(body).map_err(|_| refuse("Workspace authority is unavailable."))?;
    if !workspaces.iter().any(|w| {
        w.id.to_string() == scope.workspace
            && w.owner.to_string() == scope.profile
            && w.deleted_at.is_none()
    }) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "The workspace does not belong to this profile.",
        ));
    }
    state
        .settings()
        .map(|s| pins::Authority::new(s.paths().clone()))
        .ok_or_else(|| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "capability_unavailable",
                "Pin settings are unavailable.",
            )
        })
}
async fn list(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Query(scope): Query<Scope>,
) -> Response {
    let authority = match authorize(&state, &principal, &scope, SETTINGS_READ_SCOPE).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    answer(tokio::task::spawn_blocking(move || authority.list(&scope)).await)
}
async fn mutate(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Json(request): Json<Request>,
) -> Response {
    if !permits(&principal, SETTINGS_WRITE_SCOPE)
        || matches!(&request.mutation, Mutation::Add { .. })
            && !principal.scopes.iter().any(|s| s == "artifacts.read")
    {
        return failure(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "Pin mutation needs settings.write and store.read_documents; Add also needs artifacts.read.",
        );
    }
    if let Mutation::Add { pin } = &request.mutation {
        if let Err(e) = pin.validate() {
            return refuse(&e.to_string());
        }
    }
    let authority = match authorize(&state, &principal, &request.scope, SETTINGS_WRITE_SCOPE).await
    {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Mutation::Add { pin } = &request.mutation {
        let version = match pin.version.parse::<koru_artifact_core::ArtifactVersionId>() {
            Ok(v) => v,
            Err(_) => return refuse("Invalid artifact version."),
        };
        let (service, context) = match crate::artifacts::admitted(
            &state,
            principal,
            koru_artifact_api::ArtifactScope::Read,
        ) {
            Ok(a) => a,
            Err(r) => return r,
        };
        let expected = pin.clone();
        let workspace = request.scope.workspace.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let version = service.version(context.clone(), version)?;
            let family = service.family(context, version.family)?;
            Ok::<_, koru_artifact_wire::Failure>(
                version.family.to_storage() == expected.family
                    && family.family.workspace.to_storage() == workspace
                    && version.mime.as_str() == "application/vnd.koru.mini-app+json",
            )
        })
        .await;
        if !matches!(checked, Ok(Ok(true))) {
            return refuse("The saved mini-app version is unavailable in this workspace.");
        }
    }
    let result = tokio::task::spawn_blocking(move || authority.mutate(&request)).await;
    if matches!(&result, Ok(Ok(_)))
        && let Some(events) = &state.events
    {
        let draft: koru_events::Draft = koru_events::SettingsMiniAppPinsChanged.into();
        let envelope = draft.into_envelope(&events.state.ctx);
        if events.state.append(&[envelope]).is_err() {
            tracing::warn!("A committed mini-app pin change could not publish invalidation.");
        }
    }
    answer(result)
}
fn answer(
    result: std::result::Result<pins::Result<Vec<pins::Pin>>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok(pins)) => Json(pins).into_response(),
        Ok(Err(e)) => refuse(&e.to_string()),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "mini_app_pins_unavailable",
            "The pin worker failed.",
        ),
    }
}
pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::json;
    let mut generator = schemars::generate::SchemaSettings::openapi3().into_generator();
    let pin = generator.subschema_for::<pins::Pin>();
    let request = generator.subschema_for::<Request>();
    for route in ROUTES {
        let mut op = json!({"operationId":route.operation_id,"summary":route.summary,"description":route.returns,"security":[{"bearerAuth":[route.scope,"store.read_documents"]}],"responses":{"200":{"description":route.returns,"content":{"application/json":{"schema":{"type":"array","items":pin}}}},"400":{"description":"Unsupported pin file or invalid mutation; bytes preserved"},"403":{"description":"Scope or workspace authority refused"},"503":{"description":"Authority unavailable"}}});
        if route.method == "GET" {
            op["parameters"] = json!([{"name":"profile","in":"query","required":true,"schema":{"type":"string"}},{"name":"workspace","in":"query","required":true,"schema":{"type":"string"}}]);
        } else {
            op["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":request}}});
        }
        document["paths"][route.path][route.method.to_lowercase()] = op;
    }
    document["components"]["schemas"]
        .as_object_mut()
        .unwrap()
        .extend(
            generator
                .take_definitions(true)
                .into_iter()
                .map(|(n, s)| (n, serde_json::to_value(s).unwrap())),
        );
}
#[cfg(test)]
mod tests;
