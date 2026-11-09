// SPDX-License-Identifier: MPL-2.0

//! The account catalog rendered into an OpenAPI document. It reads only the
//! route table and the shared bounds, so every host that mounts the catalog
//! documents it the same way whether or not it composes an account service.
//!
//! Only the hosted Koru Cloud serves these routes; the desktop gateway does
//! not mount them (docs/developers/API.md). [`openapi`] is the
//! Cloud account API's own published document,
//! `reference/api/koru-account.openapi.json`.

use crate::*;

/// The Cloud account API as a standalone OpenAPI document: every
/// [`ROUTES`] entry and its schemas, through [`extend_openapi`].
pub fn openapi() -> serde_json::Value {
    let mut document = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Koru Cloud account API",
            "version": "1",
            "description": "Account, sign-in, device and Cloud Workspace routes served by the hosted Koru Cloud. The desktop API gateway does not serve them. Errors are { code, message }, where `code` is a stable machine-readable token and `message` is untranslated developer text that must not be shown to a person.",
        },
        "paths": {},
        "components": {"schemas": {}, "securitySchemes": {}},
    });
    extend_openapi(&mut document);
    document
}

/// Adds the account catalog and its canonical schemas to a composed OpenAPI
/// document. Routes are documented whether the runtime capability is enabled.
pub fn extend_openapi(document: &mut serde_json::Value) {
    let mut generator = schemars::generate::SchemaSettings::openapi3().into_generator();
    {
        let paths = document["paths"].as_object_mut().expect("OpenAPI paths");
        for route in ROUTES {
            let request = (route.request)(&mut generator);
            let response = (route.response)(&mut generator);
            let error = (route.error)(&mut generator);
            let method = route.method.as_str().to_ascii_lowercase();
            let entry = paths
                .entry(route.path)
                .or_insert_with(|| serde_json::json!({}));
            let security = match route.access {
                Access::Public => serde_json::json!([]),
                Access::NativeRecentAuthentication | Access::NativeDeviceSession => {
                    serde_json::json!([{"accountBearer": []}])
                }
                _ => serde_json::json!([{"accountBearer": []}, {"accountWebSession": []}]),
            };
            let mut operation = serde_json::json!({
                "operationId": route.id.operation_id(), "summary": route.summary, "security": security,
                "responses": {
                    "200": {"description": route.summary, "content": {"application/json": {"schema": response}}},
                    "400": {"description": "Invalid request", "content": {"application/json": {"schema": error}}},
                    "503": {"description": "Account capability unavailable", "content": {"application/json": {"schema": error}}}
                }
            });
            if matches!(
                route.id,
                RouteId::GoogleWebCallback
                    | RouteId::GoogleNativeCallback
                    | RouteId::AppleWebCallback
                    | RouteId::AppleNativeCallback
            ) {
                operation["responses"] = serde_json::json!({
                    "302": {
                        "description": "Fixed configured callback delivery",
                        "headers": {"Location": {"schema": {"type": "string"}}}
                    },
                    "400": {"description": "Invalid callback"},
                    "503": {"description": "Account capability unavailable"}
                });
            }
            match route.request_location {
                RequestLocation::Body => {
                    operation["requestBody"] = serde_json::json!({"required": true, "content": {"application/json": {"schema": request}}})
                }
                RequestLocation::Query => {
                    operation["parameters"] = if route.id == RouteId::OAuthConsentRead {
                        serde_json::json!([
                            {"name": "request", "in": "query", "required": true, "description": "The signed pending authorization request the consent page keeps in its cookie.", "schema": {"type": "string", "maxLength": MAX_QUERY_BYTES}}
                        ])
                    } else if matches!(
                        route.id,
                        RouteId::GoogleWebCallback | RouteId::GoogleNativeCallback
                    ) {
                        serde_json::json!([
                            {"name": "state", "in": "query", "required": true, "schema": {"type": "string", "maxLength": MAX_QUERY_BYTES}},
                            {"name": "code", "in": "query", "required": false, "schema": {"type": "string", "maxLength": MAX_QUERY_BYTES}},
                            {"name": "error", "in": "query", "required": false, "schema": {"type": "string", "maxLength": MAX_QUERY_BYTES}},
                            {"name": "iss", "in": "query", "required": false, "description": "Authorization-server issuer; when present it must match the configured Google issuer.", "schema": {"type": "string", "maxLength": MAX_QUERY_BYTES}}
                        ])
                    } else {
                        serde_json::json!([{"name": "account_callback", "in": "query", "required": true, "schema": request}])
                    };
                }
                RequestLocation::Form => {
                    operation["requestBody"] = serde_json::json!({"required": true, "content": {"application/x-www-form-urlencoded": {"schema": {
                        "type": "object",
                        "required": ["state"],
                        "properties": {
                            "state": {"type": "string"},
                            "code": {"type": "string"},
                            "error": {"type": "string"},
                            "user": {"type": "string", "description": "Apple's first-authorization JSON; unsigned, used only as a display-name seed."}
                        }
                    }}}})
                }
                RequestLocation::None => {}
            }
            let mut parameters = operation["parameters"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if route.id == RouteId::DescribeCloudWorkspace {
                parameters.push(serde_json::json!({
                    "name": "profile_id",
                    "in": "query",
                    "required": true,
                    "description": "The membership Profile the caller is authorized under for this Workspace.",
                    "schema": {"type": "string", "format": "uuid", "minLength": 36, "maxLength": 36}
                }));
            }
            if route.id == RouteId::ListCloudWorkspaces {
                for name in ["after_profile_id", "after_workspace_id"] {
                    parameters.push(serde_json::json!({
                        "name": name,
                        "in": "query",
                        "required": false,
                        "description": "Discovery cursor from the previous page's next value; both parameters are given together or not at all.",
                        "schema": {"type": "string", "format": "uuid", "minLength": 36, "maxLength": 36}
                    }));
                }
            }
            if route.id == RouteId::DiscoverCloudCatalog {
                parameters.push(serde_json::json!({
                    "name": "scope",
                    "in": "query",
                    "required": true,
                    "description": "Which discovery to answer: the Profiles with the Workspaces placed under each, or the Workspaces the account may sync.",
                    "schema": {"type": "string", "enum": ["user_catalog", "workspaces"]}
                }));
                parameters.push(serde_json::json!({
                    "name": "after_workspace_id",
                    "in": "query",
                    "required": false,
                    "description": "Discovery cursor from the previous receipt's next value.",
                    "schema": {"type": "string", "format": "uuid", "minLength": 36, "maxLength": 36}
                }));
            }
            if route.id == RouteId::ReadCloudRootCatalog {
                parameters.push(serde_json::json!({
                    "name": "after",
                    "in": "query",
                    "required": false,
                    "description": "The previous page's next unit as `kind:subject_id`; absent for the first page.",
                    "schema": {"type": "string", "maxLength": 64}
                }));
            }
            for name in ["profile_id", "device_id", "workspace_id", "connection_id"] {
                if route.path.contains(&format!("{{{name}}}")) {
                    parameters.push(serde_json::json!({
                        "name": name,
                        "in": "path",
                        "required": true,
                        "schema": {"type": "string", "format": "uuid", "minLength": 36, "maxLength": 36}
                    }));
                }
            }
            if !parameters.is_empty() {
                operation["parameters"] = serde_json::Value::Array(parameters);
            }
            entry[method] = operation;
        }
    }
    let schemas = document["components"]["schemas"]
        .as_object_mut()
        .expect("OpenAPI schemas");
    schemas.extend(
        generator
            .take_definitions(true)
            .into_iter()
            .map(|(name, schema)| {
                (
                    name,
                    serde_json::to_value(schema).expect("account schema serializes"),
                )
            }),
    );
    let schemes = document["components"]["securitySchemes"]
        .as_object_mut()
        .expect("OpenAPI security schemes");
    schemes.insert(
        "accountBearer".into(),
        serde_json::json!({"type": "http", "scheme": "bearer", "bearerFormat": "JWT"}),
    );
    schemes.insert(
        "accountWebSession".into(),
        serde_json::json!({"type": "apiKey", "in": "cookie", "name": WEB_SESSION_COOKIE}),
    );
}
