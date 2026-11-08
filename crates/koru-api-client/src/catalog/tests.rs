// SPDX-License-Identifier: MPL-2.0

//! What a refusal means, decided here rather than at every call site.
//!
//! Nothing here dials anything: these are the mappings a surface branches on,
//! and the bodies they are built from are the ones the gateway's own suite
//! pins on the other side of the wire.

use super::*;

fn failure(status: u16, code: &str, body: serde_json::Value) -> WorkspaceError {
    map_public(koru_store_client::http::PublicError::Failure(
        koru_store_client::http::PublicFailure {
            status,
            code: code.to_owned(),
            message: "…".into(),
            waiting_on: Vec::new(),
            body: Some(body),
        },
    ))
}

/// **S24, and only S24.** The partial failure is the one refusal a surface
/// reports as a state of the workspace — terminals gone, catalog unchanged —
/// so it is recognised by both halves being true, never by the code alone. A
/// refusal that closed nothing is an attempt that failed, which is a different
/// sentence to a person.
#[test]
fn a_partial_failure_is_recognised_by_what_it_did_rather_than_by_its_code() {
    let s24 = failure(
        500,
        "workspace_lifecycle_incomplete",
        serde_json::json!({
            "retryable": true,
            "outcome": {
                "workspace": "w",
                "action": "soft_delete",
                "terminated_sessions": ["a", "b"],
                "sessions_closed": true,
                "mutation_applied": false,
            },
        }),
    );
    let outcome = s24.partial_outcome().expect("S24 carries its outcome");
    assert_eq!(outcome.terminated_sessions.len(), 2);
    assert!(s24.is_retryable(), "and the retry is the same operation");

    // Nothing was closed, so nothing partial happened.
    let nothing = failure(
        503,
        "workspace_sessions_not_closed",
        serde_json::json!({
            "retryable": true,
            "outcome": {
                "workspace": "w",
                "action": "archive",
                "terminated_sessions": [],
                "sessions_closed": false,
                "mutation_applied": false,
            },
        }),
    );
    assert!(nothing.partial_outcome().is_none());
}

#[test]
fn committed_removal_preserves_typed_authority_listing_and_replacement_on_failure() {
    let error = failure(
        409,
        "workspace_lifecycle_incomplete",
        serde_json::json!({
            "retryable":true, "cause":"storage_failed", "outcome": {
                "workspace":"w", "action":"archive", "terminated_sessions":[],
                "sessions_closed":true, "mutation_applied":false, "authority_committed":true,
                "listing":{"state":"pending", "cause":"storage_failed"}, "replacement":{"state":"unknown"}
            }
        }),
    );
    let outcome = error.partial_outcome().unwrap();
    assert_eq!(outcome.authority_committed, Some(true));
    assert_eq!(
        outcome.listing,
        Some(koru_store_core::service::ListingState::Pending {
            cause: "storage_failed".into()
        })
    );
    assert_eq!(
        outcome.replacement,
        Some(koru_store_core::service::Replacement::Unknown)
    );
    assert!(error.is_retryable());
}

/// **The documents-connected refusal is recognised, not translated away.**
///
/// Current gateways use the confirmed Store room-disconnect contract. An older
/// gateway can still return this typed cause, and a current client must retain
/// it so the surface can explain a mixed-version refusal accurately.
#[test]
fn the_documents_connected_refusal_survives_as_itself() {
    // Through the operation, where the Store's code rides as `cause`.
    let through = failure(
        409,
        "workspace_lifecycle_incomplete",
        serde_json::json!({
            "retryable": true,
            "cause": "workspace_in_use",
            "cause_message": "2 active connection(s)",
            "outcome": {
                "workspace": "w",
                "action": "soft_delete",
                "terminated_sessions": [],
                "sessions_closed": true,
                "mutation_applied": false,
            },
        }),
    );
    assert!(through.documents_connected());

    // And directly, if a caller ever reaches the Store route itself.
    let direct = failure(409, WORKSPACE_IN_USE, serde_json::json!({}));
    assert!(direct.documents_connected());

    // A different refusal is not this one. A workspace another window removed
    // first is not a workspace with documents open, and telling somebody to
    // close their tabs would be advice about the wrong problem.
    let other = failure(404, "not_found", serde_json::json!({ "retryable": false }));
    assert!(!other.documents_connected());
    assert!(!other.is_retryable());
}

/// A caller that may not remove a workspace is told that and nothing else —
/// the presentation case, because the surface renders those rows disabled with
/// a reason rather than hiding them.
#[test]
fn a_permission_refusal_is_told_apart_from_a_failed_attempt() {
    let denied = failure(403, "permission_denied", serde_json::json!({}));
    assert!(denied.is_permission_denied());
    assert!(denied.partial_outcome().is_none());
    assert!(!denied.documents_connected());

    let unavailable = failure(503, "capability_unavailable", serde_json::json!({}));
    assert!(!unavailable.is_permission_denied());
}

/// A refusal with no body at all is still a refusal, and answers every
/// question with the safe half: nothing happened, and do not claim a retry
/// will help.
#[test]
fn a_refusal_carrying_nothing_claims_nothing() {
    let bare = map_public(koru_store_client::http::PublicError::Failure(
        koru_store_client::http::PublicFailure {
            status: 500,
            code: "server_unavailable".into(),
            message: "…".into(),
            waiting_on: Vec::new(),
            body: None,
        },
    ));
    assert!(bare.partial_outcome().is_none());
    assert!(!bare.documents_connected());
    assert!(!bare.is_retryable());
}
