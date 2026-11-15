// SPDX-License-Identifier: MPL-2.0
//! Mounted private decision shell, in-memory bodies only; no Store, AI
//! worker, provider or disk opens. A scripted credential stands in for the
//! delegate API's domain authority; this is not transport qualification.
use super::*;
use koru_delegate_core as d;
use koru_delegate_core::decisions::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tower::ServiceExt;

const DECISION_HEADER: &str =
    "Koru-Decision 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PARENT: &str = "PRIVATE-DECISION-HTTP-PARENT-0123456789";

struct Exact(&'static str);
impl DecisionBridgeCredential for Exact {
    fn matches_authorization(&self, candidate: &[u8]) -> bool {
        candidate == self.0.as_bytes()
    }
}

fn routine_header() -> String {
    koru_delegate_api::bots::BotDelegateAuthority::new(PARENT.into())
        .unwrap()
        .routine_bridge_authority()
        .authorization_header()
}

fn installed() -> Arc<HttpState> {
    let state = Arc::new(HttpState::new(None));
    assert!(state.install(Arc::new(Exact(DECISION_HEADER))));
    state
}

fn intent() -> BotWaitCleanupIntentV1 {
    BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: d::BotOwner {
            user_scope: koru_ai_core::bots::UserScopeId::new(),
            device: d::DeviceId::new(),
        },
        bot: d::BotId::mint(),
        run: d::RunId::mint(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(2).unwrap(),
        wait_revision: DecisionRevision::new(2).unwrap(),
        origin_attempt: d::RunAttemptId::mint(),
        origin_generation: d::AttemptGeneration::from_raw(1).unwrap(),
        origin_session: d::SessionId::mint(),
        origin_turn: d::AgentTurnId::mint(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    }
}

fn envelope(payload: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "request_id": Uuid::new_v4().to_string(),
        "payload": payload,
    }))
    .unwrap()
}
fn read_body_bytes() -> Vec<u8> {
    let key = intent().key().unwrap();
    envelope(serde_json::to_value(ReadBotWaitCleanup { key }).unwrap())
}
fn enroll_body_bytes() -> Vec<u8> {
    let cleanup_intent = intent();
    let key = cleanup_intent.key().unwrap();
    envelope(
        serde_json::to_value(EnrollBotWaitCleanup {
            key,
            cleanup_intent,
        })
        .unwrap(),
    )
}

fn request(path: &str, authorization: Option<&str>, body: Body) -> Request {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    builder.body(body).unwrap()
}
async fn send(state: Arc<HttpState>, request: Request) -> Response {
    routes().with_state(state).oneshot(request).await.unwrap()
}
async fn json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn unpolled(polls: Arc<AtomicUsize>) -> Body {
    Body::from_stream(futures_util::stream::poll_fn(
        move |_| -> Poll<Option<Result<Bytes, std::io::Error>>> {
            polls.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Some(Err(std::io::Error::other("body must not be polled"))))
        },
    ))
}

#[tokio::test]
async fn decision_routine_credential_is_refused_on_decision_routes_before_any_body_read() {
    for path in [DECISION_ENROLL_ROUTE, DECISION_READ_ROUTE] {
        for state in [installed(), Arc::new(HttpState::new(None))] {
            let polls = Arc::new(AtomicUsize::new(0));
            let routine = routine_header();
            let response = send(
                state,
                request(path, Some(routine.as_str()), unpolled(polls.clone())),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(polls.load(Ordering::SeqCst), 0, "{path}");
        }
    }
}

#[tokio::test]
async fn decision_uninstalled_or_missing_or_duplicate_credential_refuses() {
    let polls = Arc::new(AtomicUsize::new(0));
    // Not installed: even the would-be decision credential is refused.
    let response = send(
        Arc::new(HttpState::new(None)),
        request(
            DECISION_READ_ROUTE,
            Some(DECISION_HEADER),
            unpolled(polls.clone()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = send(
        installed(),
        request(DECISION_READ_ROUTE, None, unpolled(polls.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let mut duplicate = request(
        DECISION_READ_ROUTE,
        Some(DECISION_HEADER),
        unpolled(polls.clone()),
    );
    duplicate.headers_mut().append(
        header::AUTHORIZATION,
        axum::http::HeaderValue::from_static(DECISION_HEADER),
    );
    assert_eq!(
        send(installed(), duplicate).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    // Installed once only: a later credential cannot replace it.
    let state = installed();
    assert!(!state.install(Arc::new(Exact(DECISION_HEADER))));
}

#[tokio::test]
async fn decision_without_an_ai_owner_answers_typed_unavailable_never_missing() {
    for (path, body) in [
        (DECISION_READ_ROUTE, read_body_bytes()),
        (DECISION_ENROLL_ROUTE, enroll_body_bytes()),
    ] {
        let response = send(
            installed(),
            request(path, Some(DECISION_HEADER), Body::from(body)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let value = json(response).await;
        assert_eq!(value["version"], 1, "{path}");
        assert_eq!(value["payload"]["result"], "refused", "{path}");
        assert_eq!(value["payload"]["code"], "unavailable", "{path}");
    }
}

#[tokio::test]
async fn decision_method_and_malformed_envelopes_refuse() {
    let mut get = request(
        DECISION_READ_ROUTE,
        Some(DECISION_HEADER),
        Body::from(read_body_bytes()),
    );
    *get.method_mut() = Method::GET;
    let response = send(installed(), get).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let mut unknown: serde_json::Value = serde_json::from_slice(&read_body_bytes()).unwrap();
    unknown["extra"] = serde_json::json!(true);
    let mut old_version: serde_json::Value = serde_json::from_slice(&read_body_bytes()).unwrap();
    old_version["version"] = serde_json::json!(2);
    let mut wrong_kind: serde_json::Value = serde_json::from_slice(&enroll_body_bytes()).unwrap();
    wrong_kind["payload"]["key"]["receipt_digest"] = serde_json::to_value([0u8; 32]).unwrap();
    // An Enroll body on the Read route has unknown fields there.
    let crossed = enroll_body_bytes();
    for (case, (path, body)) in [
        (DECISION_READ_ROUTE, serde_json::to_vec(&unknown).unwrap()),
        (
            DECISION_READ_ROUTE,
            serde_json::to_vec(&old_version).unwrap(),
        ),
        (
            DECISION_ENROLL_ROUTE,
            serde_json::to_vec(&wrong_kind).unwrap(),
        ),
        (DECISION_READ_ROUTE, crossed),
        (DECISION_READ_ROUTE, b"{".to_vec()),
    ]
    .into_iter()
    .enumerate()
    {
        let response = send(
            installed(),
            request(path, Some(DECISION_HEADER), Body::from(body)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "case {case}");
    }
}

#[test]
fn decision_routes_are_disjoint_from_the_routine_bridge() {
    for path in [DECISION_ENROLL_ROUTE, DECISION_READ_ROUTE] {
        assert!(is_endpoint(path));
        assert!(!crate::routine_bridge::is_endpoint(path));
        assert!(path.starts_with("/private/bots/decisions/"));
    }
    for path in [
        koru_delegate_api::routines::ROUTINE_PREPARE_ROUTE,
        koru_delegate_api::routines::ROUTINE_PREPARATION_ROUTE,
        koru_delegate_api::routines::ROUTINE_FINALIZE_ROUTE,
        koru_delegate_api::routines::ROUTINE_LINK_ROUTE,
        koru_delegate_api::routines::ROUTINE_PRECLAIM_ROUTE,
    ] {
        assert!(!is_endpoint(path));
    }
}
