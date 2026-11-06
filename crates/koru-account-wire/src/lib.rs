// SPDX-License-Identifier: MPL-2.0

//! Versioned request, response, error, and route declarations for accounts.
//!
//! This crate has no HTTP client or server. The eventual API adapter and typed
//! client fold from [`ROUTES`] so a route and its schema have one owner.

pub mod app_connections;
mod catalog;
pub mod cloud_sync;
mod openapi;
pub mod regional_bootstrap;
mod schema;
mod types;
mod version;

pub use catalog::{
    Access, Method, ROUTES, RequestLocation, Route, RouteId, is_route_path, route_by_id, route_for,
};
pub use cloud_sync::{
    DatasetRouteResultV1, SyncCredential, SyncTokenRequestV1, SyncTokenResponseV1,
};
pub use openapi::{extend_openapi, openapi};
pub use regional_bootstrap::{CloudSyncRouteV1, CloudSyncSeedV1, RegionalBootstrapV1};
pub use schema::{TYPE_NAMES, catalogue};
pub use types::*;
pub use version::Version1;

/// The capability a host publishes when it composes an account service. A host
/// that mounts the catalog without one omits it and answers
/// `capability_unavailable`.
pub const CAPABILITY: &str = "account";
/// Largest account request body either side sends or admits.
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
/// Largest query string an account route admits.
pub const MAX_QUERY_BYTES: usize = 8 * 1024;
/// Largest account response body either side produces or reads.
pub const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;
/// The browser web session's cookie name.
pub const WEB_SESSION_COOKIE: &str = "koru_account_session";
/// The double-submit CSRF cookie a web session's mutating calls echo.
pub const CSRF_COOKIE: &str = "koru_account_csrf";
/// The header that carries the CSRF cookie's value back.
pub const CSRF_HEADER: &str = "x-koru-csrf";

/// Exact browser-to-native callback path shared by the hosted redirect and
/// the Agent-owned loopback listener.
pub const NATIVE_LOOPBACK_CALLBACK_PATH: &str = "/v1/oauth/native/callback";
/// The website path a phone's verified App Link claims for a browser sign-in
/// return. Same origin as the configured website callback.
pub const APP_LINK_CALLBACK_PATH: &str = "/v1/oauth/app/callback";

#[cfg(test)]
mod tests;
