// SPDX-License-Identifier: MPL-2.0

//! Local diagnostics before the runtime maps HTTP failures to user-facing codes.
//! No request, URL, response body or open-ended server error reaches tracing.
use super::{ClientError, ErrorCode, RouteId, TransportError};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Failure {
    status: Option<u16>,
    code: &'static str,
}

impl Failure {
    fn new(status: Option<u16>, error: &ClientError) -> Self {
        let code = match error {
            ClientError::InvalidRequest => "invalid_request",
            ClientError::MissingBrowserOrigin => "missing_browser_origin",
            ClientError::InvalidResponse => "invalid_response",
            ClientError::ContractMismatch => "contract_mismatch",
            ClientError::Transport(error) => match error {
                TransportError::InvalidRequest => "transport_invalid_request",
                TransportError::Unavailable => "transport_unavailable",
                TransportError::Timeout => "transport_timeout",
                TransportError::ConnectionFailed => "transport_connect_failed",
                TransportError::BodyReadFailed => "transport_body_read_failed",
                TransportError::ResponseTooLarge => "response_too_large",
            },
            ClientError::Api { code, .. } => safe_api_code(code),
            // The status travels beside this as `http_status`; the body that
            // was not an envelope never reaches a log line.
            ClientError::ApiWithoutCode { .. } => "api_without_code",
            ClientError::RateLimited { .. } => ErrorCode::RATE_LIMITED,
        };
        Self { status, code }
    }
}

fn safe_api_code(code: &ErrorCode) -> &'static str {
    // ErrorCode intentionally accepts future arbitrary tokens. Only the closed
    // vocabulary may enter logs, even if a server puts a secret in a valid token.
    match code.as_str() {
        ErrorCode::INVALID_REQUEST => ErrorCode::INVALID_REQUEST,
        ErrorCode::UNAUTHENTICATED => ErrorCode::UNAUTHENTICATED,
        ErrorCode::PERMISSION_DENIED => ErrorCode::PERMISSION_DENIED,
        ErrorCode::PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED => {
            ErrorCode::PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED
        }
        ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED => {
            ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED
        }
        ErrorCode::FRESH_AUTHENTICATION_REQUIRED => ErrorCode::FRESH_AUTHENTICATION_REQUIRED,
        ErrorCode::CAPABILITY_UNAVAILABLE => ErrorCode::CAPABILITY_UNAVAILABLE,
        ErrorCode::RATE_LIMITED => ErrorCode::RATE_LIMITED,
        ErrorCode::FLOW_EXPIRED => ErrorCode::FLOW_EXPIRED,
        ErrorCode::FLOW_CONSUMED => ErrorCode::FLOW_CONSUMED,
        ErrorCode::PROVIDER_REJECTED => ErrorCode::PROVIDER_REJECTED,
        ErrorCode::IDENTITY_COLLISION => ErrorCode::IDENTITY_COLLISION,
        ErrorCode::PROFILE_ALREADY_BOUND => ErrorCode::PROFILE_ALREADY_BOUND,
        ErrorCode::PROFILE_DELETED => ErrorCode::PROFILE_DELETED,
        ErrorCode::PROFILE_LIFECYCLE_STALE => ErrorCode::PROFILE_LIFECYCLE_STALE,
        ErrorCode::PROFILE_RESTORE_SOURCE_REQUIRED => ErrorCode::PROFILE_RESTORE_SOURCE_REQUIRED,
        ErrorCode::DEVICE_ALREADY_BOUND => ErrorCode::DEVICE_ALREADY_BOUND,
        ErrorCode::CONFLICT => ErrorCode::CONFLICT,
        ErrorCode::NOT_FOUND => ErrorCode::NOT_FOUND,
        ErrorCode::SERVER_UNAVAILABLE => ErrorCode::SERVER_UNAVAILABLE,
        other => sync_issuance_code(other).unwrap_or("unknown_api_error"),
    }
}

/// Why a Cloud sync credential was refused. These are a closed vocabulary
/// too, and without them every refused token read `unknown_api_error`: a
/// device that was not authorized for a Workspace and one whose Workspace had
/// Cloud switched off left the same line (2026-11-11).
fn sync_issuance_code(code: &str) -> Option<&'static str> {
    use koru_account_core::sync_issuance::SyncIssuanceError::*;
    [
        PermissionDenied,
        WrongRegion,
        DeviceRevoked,
        EndpointMismatch,
        WorkspaceNotAuthorized,
        CloudDisabled,
        CloudDeleting,
        CloudAbsent,
        AuthorizationStale,
        CloudIncarnationStale,
        ReplicaMismatch,
        SyncUnavailable,
        AuthorizationUnavailable,
    ]
    .into_iter()
    .map(|error| error.code())
    .find(|known| *known == code)
}

#[derive(Default)]
pub(super) struct Diagnostics {
    // At most one categorical value per static RouteId, shared by client clones.
    // Unchanging background retries do not produce an activity log.
    failures: Mutex<BTreeMap<RouteId, Failure>>,
}

impl Diagnostics {
    fn changed(&self, route: RouteId, failure: Option<Failure>) -> bool {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match failure {
            Some(failure) => failures.insert(route, failure) != Some(failure),
            None => failures.remove(&route).is_some(),
        }
    }

    pub(super) fn observe(
        &self,
        route: RouteId,
        status: Option<u16>,
        error: Option<&ClientError>,
        elapsed: Duration,
    ) {
        let failure = error.map(|error| Failure::new(status, error));
        let changed = self.changed(route, failure);
        if let Some(failure) = failure {
            if changed {
                tracing::warn!(target: "koru_account_client", route = ?route,
                    http_status = ?failure.status, failure = failure.code,
                    elapsed_ms = elapsed.as_millis() as u64, "account request failed");
            }
        } else if changed {
            tracing::warn!(target: "koru_account_client", route = ?route,
                http_status = ?status, elapsed_ms = elapsed.as_millis() as u64,
                "account request recovered");
        }
    }
}

#[cfg(test)]
mod tests;
