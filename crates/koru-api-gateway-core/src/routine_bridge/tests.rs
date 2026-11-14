// SPDX-License-Identifier: MPL-2.0
//! Mounted private shell, in-memory bodies only; no Store or AI worker opens.
use super::*;
use bytes::Bytes;
use koru_delegate_core::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;
use tower::ServiceExt;

fn id(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}
fn authority(parent: &str) -> RoutineBridgeAuthority {
    koru_delegate_api::bots::BotDelegateAuthority::new(parent.into())
        .unwrap()
        .routine_bridge_authority()
}
const PARENT: &str = "PRIVATE-ROUTINE-HTTP-PARENT-0123456789";
const ROTATED_PARENT: &str = "PRIVATE-ROUTINE-HTTP-ROTATED-PARENT-0123456789";
fn state() -> Arc<HttpState> {
    let state = Arc::new(HttpState::default());
    assert!(state.install(authority(PARENT)));
    state
}
fn read() -> RoutineBridgeEnvelope<ReadPreparation> {
    RoutineBridgeEnvelope::new(ReadPreparation {
        slot: serde_json::from_value(serde_json::json!({"owner":{"user_scope":id(1),"device":id(2)},"bot":id(3),"schedule":id(4),"occurrence_ms":60_000})).unwrap(),
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
    }).unwrap()
}
fn claim() -> RoutineClaimIdentity {
    RoutineClaimIdentity {
        slot: read().payload.slot,
        definition: RoutineDefinitionIdentity {
            revision: 1,
            digest: RoutineDigest32::from_bytes([2; 32]),
            owner_handoff: None,
        },
        ids: RoutineExecutionIds {
            submission: id(6).parse().unwrap(),
            run: id(7).parse().unwrap(),
            chat: id(8).parse().unwrap(),
            attempt: id(9).parse().unwrap(),
            user_message: id(10).parse().unwrap(),
            assistant_message: id(11).parse().unwrap(),
        },
        preparation_hash: RoutineDigest32::from_bytes([3; 32]),
        claimed_at_ms: 60_001,
    }
}
fn preclaim() -> RoutineBridgeEnvelope<SynchronizeRoutinePreclaim> {
    RoutineBridgeEnvelope::new(SynchronizeRoutinePreclaim {
        slot: read().payload.slot,
        semantic_hash: read().payload.semantic_hash,
        preparation_hash: claim().preparation_hash,
    })
    .unwrap()
}
fn prepare() -> RoutineBridgeEnvelope<PrepareOccurrence> {
    let slot = read().payload.slot;
    RoutineBridgeEnvelope::new(
        serde_json::from_value(serde_json::json!({
            "slot":slot,"workspace":id(5),"owner_handoff":null,
            "schedule_definition":{
                "revision":1,"effective_at_ms":0,
                "template":{
                    "requested_by":{"kind":"person","profile":id(6)},
                    "objective":"PRIVATE-HTTP-OBJECTIVE",
                    "agent":{"kind":"built_in","value":"codex"},"requested_model":null,
                    "scope":{"kind":"bot_folder","folder":"/fixture/routine"},
                    "grants":[id(5)],"context":[]
                },
                "recurrence":{"type":"floating","cron":"* * * * *"},
                "lifecycle":"active",
                "routine":{"owner":slot.owner,"bot":slot.bot,"home":id(5),"creation_request":id(12)}
            }
        }))
        .unwrap(),
    )
    .unwrap()
}
fn request(path: &str, body: Body) -> Request {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(
            header::AUTHORIZATION,
            authority(PARENT).authorization_header(),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}
async fn send(state: Arc<HttpState>, request: Request) -> Response {
    routes().with_state(state).oneshot(request).await.unwrap()
}
async fn bytes(response: Response) -> Bytes {
    axum::body::to_bytes(response.into_body(), ROUTINE_BRIDGE_BODY_LIMIT)
        .await
        .unwrap()
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
async fn routine_http_response_permits_follow_unpolled_bodies_until_drop() {
    let state = state();
    let mut responses = Vec::new();
    for used in 1..=HTTP_CAPACITY {
        let response = send(
            state.clone(),
            request(
                ROUTINE_PREPARATION_ROUTE,
                Body::from(read().encode().unwrap()),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        responses.push(response);
        assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - used);
    }
    let polls = Arc::new(AtomicUsize::new(0));
    let saturated = send(
        state.clone(),
        request(ROUTINE_PRECLAIM_ROUTE, unpolled(polls.clone())),
    )
    .await;
    assert_eq!(saturated.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    drop(saturated);
    assert_eq!(state.slots.available_permits(), 0);
    drop(responses.pop().unwrap());
    assert_eq!(state.slots.available_permits(), 1);
    let replacement = send(
        state.clone(),
        request(
            ROUTINE_PREPARATION_ROUTE,
            Body::from(read().encode().unwrap()),
        ),
    )
    .await;
    assert_eq!(replacement.status(), StatusCode::OK);
    assert_eq!(state.slots.available_permits(), 0);
    drop(replacement);
    drop(responses);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
}

#[tokio::test]
async fn routine_http_response_permits_follow_yielded_chunks_and_their_last_clone() {
    let state = state();
    let envelope = preclaim();
    let response = send(
        state.clone(),
        request(
            ROUTINE_PRECLAIM_ROUTE,
            Body::from(envelope.encode().unwrap()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let chunk = stream.next().await.unwrap().unwrap();
    RoutineBridgeEnvelope::<SynchronizeRoutinePreclaimReply>::decode(&chunk)
        .unwrap()
        .validate_reply_to(&envelope)
        .unwrap();
    let cloned = chunk.clone();
    let sliced = chunk.slice(1..);
    assert!(stream.next().await.is_none());
    drop(stream);
    drop(chunk);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - 1);
    let mut responses = Vec::new();
    for _ in 1..HTTP_CAPACITY {
        let response = send(
            state.clone(),
            request(
                ROUTINE_PREPARATION_ROUTE,
                Body::from(read().encode().unwrap()),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        responses.push(response);
    }
    assert_eq!(state.slots.available_permits(), 0);
    assert_eq!(
        send(
            state.clone(),
            request(ROUTINE_PREPARATION_ROUTE, Body::empty())
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(responses);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - 1);
    drop(cloned);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - 1);
    drop(sliced);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
}

#[tokio::test]
async fn routine_http_response_error_paths_release_before_error_body_consumption() {
    for case in 0..5 {
        let mut state = state();
        let mut req = request(
            ROUTINE_PREPARATION_ROUTE,
            Body::from(read().encode().unwrap()),
        );
        let expected = match case {
            0 => {
                req.headers_mut()
                    .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
                StatusCode::BAD_REQUEST
            }
            1 => {
                *req.body_mut() = Body::from("{}");
                StatusCode::BAD_REQUEST
            }
            2 => {
                *req.body_mut() = Body::from_stream(futures_util::stream::once(async {
                    Err::<Bytes, _>(std::io::Error::other("fixture body failure"))
                }));
                StatusCode::BAD_REQUEST
            }
            3 => {
                req.headers_mut().insert(
                    header::CONTENT_LENGTH,
                    (ROUTINE_BRIDGE_BODY_LIMIT + 1).to_string().parse().unwrap(),
                );
                StatusCode::PAYLOAD_TOO_LARGE
            }
            _ => {
                Arc::get_mut(&mut state).unwrap().test_budget = Duration::from_millis(10);
                *req.body_mut() = Body::from_stream(futures_util::stream::pending::<
                    Result<Bytes, std::io::Error>,
                >());
                StatusCode::GATEWAY_TIMEOUT
            }
        };
        let response = send(state.clone(), req).await;
        assert_eq!(response.status(), expected, "case {case}");
        assert_eq!(
            state.slots.available_permits(),
            HTTP_CAPACITY,
            "case {case}"
        );
        assert!(bytes(response).await.is_empty());
        let envelope = read();
        let response = send(
            state.clone(),
            request(
                ROUTINE_PREPARATION_ROUTE,
                Body::from(envelope.encode().unwrap()),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - 1);
        let body = bytes(response).await;
        RoutineBridgeEnvelope::<ReadPreparationReply>::decode(&body)
            .unwrap()
            .validate_reply_to(&envelope)
            .unwrap();
        assert_eq!(state.slots.available_permits(), HTTP_CAPACITY - 1);
        drop(body);
        assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
    }
}

#[tokio::test]
async fn routine_http_all_five_routes_return_only_correlated_unavailable() {
    let state = state();
    async fn call<Q, R>(state: Arc<HttpState>, path: &str, envelope: RoutineBridgeEnvelope<Q>)
    where
        Q: RoutineBridgePayload + Serialize,
        R: RoutineBridgeReply<Q> + DeserializeOwned,
    {
        let response = send(state, request(path, Body::from(envelope.encode().unwrap()))).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = bytes(response).await;
        assert!(body.len() < 256 && body.len() <= ROUTINE_BRIDGE_BODY_LIMIT);
        let reply = RoutineBridgeEnvelope::<R>::decode(&body).unwrap();
        reply.validate_reply_to(&envelope).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["payload"],
            serde_json::json!({"kind":"refused","code":"unavailable"})
        );
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("PRIVATE-HTTP-OBJECTIVE"));
        assert!(!text.contains(&authority(PARENT).authorization_header()));
    }
    call::<_, PrepareOccurrenceReply>(state.clone(), ROUTINE_PREPARE_ROUTE, prepare()).await;
    call::<_, ReadPreparationReply>(state.clone(), ROUTINE_PREPARATION_ROUTE, read()).await;
    call::<_, FinalizeOccurrenceReply>(
        state.clone(),
        ROUTINE_FINALIZE_ROUTE,
        RoutineBridgeEnvelope::new(FinalizeOccurrence { claim: claim() }).unwrap(),
    )
    .await;
    call::<_, ReadOccurrenceLinkReply>(
        state.clone(),
        ROUTINE_LINK_ROUTE,
        RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: claim() }).unwrap(),
    )
    .await;
    call::<_, SynchronizeRoutinePreclaimReply>(state, ROUTINE_PRECLAIM_ROUTE, preclaim()).await;
}

#[tokio::test]
async fn routine_http_authentication_and_rotation_precede_any_body_poll() {
    let state = state();
    assert!(!state.install(authority(ROTATED_PARENT)));
    for (path, case) in [ROUTINE_PREPARATION_ROUTE, ROUTINE_PRECLAIM_ROUTE]
        .into_iter()
        .flat_map(|path| (0..9).map(move |case| (path, case)))
    {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut request = request(path, unpolled(polls.clone()));
        match case {
            0 => {
                request.headers_mut().remove(header::AUTHORIZATION);
            }
            1 => {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    "Bearer ordinary-session".parse().unwrap(),
                );
            }
            2 => {
                request.headers_mut().append(
                    header::AUTHORIZATION,
                    authority(PARENT).authorization_header().parse().unwrap(),
                );
            }
            3 => {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    authority(ROTATED_PARENT)
                        .authorization_header()
                        .parse()
                        .unwrap(),
                );
            }
            4 => {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    format!("{} ", authority(PARENT).authorization_header())
                        .parse()
                        .unwrap(),
                );
            }
            5 => {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    authority(PARENT)
                        .authorization_header()
                        .to_uppercase()
                        .parse()
                        .unwrap(),
                );
            }
            6 => {
                request
                    .headers_mut()
                    .insert(header::AUTHORIZATION, "Koru-Routine bad".parse().unwrap());
            }
            7 => {
                request.headers_mut().insert(
                    header::AUTHORIZATION,
                    axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
                );
            }
            8 => {
                request.headers_mut().append(
                    header::AUTHORIZATION,
                    "Bearer ordinary-session".parse().unwrap(),
                );
            }
            _ => unreachable!(),
        }
        let response = send(state.clone(), request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "case {case}");
        assert!(bytes(response).await.is_empty());
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
    }
    let empty = Arc::new(HttpState::default());
    let polls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        send(
            empty,
            request(ROUTINE_PREPARATION_ROUTE, unpolled(polls.clone()))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let rotated = Arc::new(HttpState::default());
    assert!(rotated.install(authority(ROTATED_PARENT)));
    let polls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        send(
            rotated,
            request(ROUTINE_PREPARATION_ROUTE, unpolled(polls.clone()))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn routine_http_exact_paths_methods_and_public_catalog_exclusion() {
    let state = state();
    for path in [
        ROUTINE_PREPARE_ROUTE,
        ROUTINE_PREPARATION_ROUTE,
        ROUTINE_FINALIZE_ROUTE,
        ROUTINE_LINK_ROUTE,
        ROUTINE_PRECLAIM_ROUTE,
    ] {
        assert!(is_endpoint(path));
        for method in [
            Method::GET,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            let polls = Arc::new(AtomicUsize::new(0));
            let mut request = request(path, unpolled(polls.clone()));
            *request.method_mut() = method;
            let response = send(state.clone(), request).await;
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::ALLOW], "POST");
            assert!(bytes(response).await.is_empty());
            assert_eq!(polls.load(Ordering::SeqCst), 0);
        }
    }
    for path in [
        "/internal/bots/routines/v1/preparation/",
        "/internal/bots/routines/v2/prepare",
        "/internal/bots/routines/v1/prepare/extra",
        "/v1/ai/bots/routines",
        "/internal/bots/routines/v1",
        "/INTERNAL/bots/routines/v1/link",
        "/internal/bots/routines/v1/synchronize-preclaim/",
        "/internal/bots/routines/v2/synchronize-preclaim",
    ] {
        assert!(!is_endpoint(path));
        assert_eq!(
            send(state.clone(), request(path, Body::empty()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    for catalog in [crate::public_openapi(), crate::public_asyncapi()] {
        let text = serde_json::to_string(&catalog).unwrap();
        assert!(!text.contains("/internal/bots/routines"));
        assert!(!text.contains("RoutineBridge"));
    }
}

#[tokio::test]
async fn routine_http_strict_envelopes_and_nested_objects_refuse_unknown_or_invalid_data() {
    let state = state();
    for key in ["version", "payload", "slot", "owner"] {
        let mut value = serde_json::to_value(preclaim()).unwrap();
        match key {
            "version" => value["version"] = 2.into(),
            "payload" => value["payload"]["unexpected"] = true.into(),
            "slot" => value["payload"]["slot"]["unexpected"] = true.into(),
            _ => value["payload"]["slot"]["owner"]["unexpected"] = true.into(),
        }
        let response = send(
            state.clone(),
            request(ROUTINE_PRECLAIM_ROUTE, Body::from(value.to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(bytes(response).await.is_empty());
    }
    let base = serde_json::to_value(read()).unwrap();
    let mut cases = vec![serde_json::json!({})];
    for path in [
        vec!["version"],
        vec!["payload", "unexpected"],
        vec!["payload", "slot", "unexpected"],
        vec!["payload", "slot", "owner", "unexpected"],
    ] {
        let mut value = base.clone();
        let mut slot = &mut value;
        for key in &path[..path.len() - 1] {
            slot = &mut slot[*key];
        }
        slot[*path.last().unwrap()] = 2.into();
        cases.push(value);
    }
    let mut value = base.clone();
    value["unexpected"] = true.into();
    cases.push(value);
    let mut value = base.clone();
    value["request_id"] = "ABCDEF00-0000-4000-8000-000000000001".into();
    cases.push(value);
    let mut value = base.clone();
    value["request_id"] = "00000000-0000-0000-0000-000000000000".into();
    cases.push(value);
    let mut value = base.clone();
    value["payload"]["slot"]["schedule"] = "00000000-0000-0000-0000-000000000000".into();
    cases.push(value);
    for value in cases {
        let response = send(
            state.clone(),
            request(ROUTINE_PREPARATION_ROUTE, Body::from(value.to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{value}");
        assert!(bytes(response).await.is_empty());
    }
    // Slot timestamps retain the owning type's full signed domain. The shell
    // must not invent a new epoch/precision rule for read identities.
    let mut signed = read();
    signed.payload.slot.occurrence_ms = -1;
    assert_eq!(
        send(
            state.clone(),
            request(
                ROUTINE_PREPARATION_ROUTE,
                Body::from(signed.encode().unwrap())
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    for key in [
        "schedule_definition",
        "template",
        "agent",
        "recurrence",
        "routine",
    ] {
        let mut value = serde_json::to_value(prepare()).unwrap();
        let definition = &mut value["payload"]["schedule_definition"];
        match key {
            "schedule_definition" => definition["unexpected"] = true.into(),
            "template" => definition["template"]["unexpected"] = true.into(),
            "agent" => definition["template"]["agent"]["unexpected"] = true.into(),
            other => definition[other]["unexpected"] = true.into(),
        }
        assert_eq!(
            send(
                state.clone(),
                request(ROUTINE_PREPARE_ROUTE, Body::from(value.to_string()))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "{key}"
        );
    }
    let original = String::from_utf8(read().encode().unwrap()).unwrap();
    for body in [
        String::new(),
        "invalid json".into(),
        original.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
    ] {
        assert_eq!(
            send(
                state.clone(),
                request(ROUTINE_PREPARATION_ROUTE, Body::from(body))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn routine_http_strict_framing_rejects_before_unnecessary_body_reads() {
    let state = state();
    for case in 0..10 {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut request = request(ROUTINE_PREPARATION_ROUTE, unpolled(polls.clone()));
        let expected = if case == 9 {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        };
        match case {
            0 => {
                request.headers_mut().remove(header::CONTENT_TYPE);
            }
            1 => {
                request
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
            }
            2 => {
                request
                    .headers_mut()
                    .append(header::CONTENT_TYPE, "application/json".parse().unwrap());
            }
            3 => {
                request
                    .headers_mut()
                    .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
            }
            4 => {
                request
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, "+1".parse().unwrap());
            }
            5 => {
                request
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, "1".parse().unwrap());
                request
                    .headers_mut()
                    .append(header::CONTENT_LENGTH, "1".parse().unwrap());
            }
            6 => {
                request
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, "1".parse().unwrap());
                request
                    .headers_mut()
                    .insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
            }
            7 => {
                request
                    .headers_mut()
                    .insert(header::TRANSFER_ENCODING, "gzip".parse().unwrap());
            }
            8 => {
                *request.uri_mut() = format!("{ROUTINE_PREPARATION_ROUTE}?unexpected=1")
                    .parse()
                    .unwrap();
            }
            9 => {
                request.headers_mut().insert(
                    header::CONTENT_LENGTH,
                    (ROUTINE_BRIDGE_BODY_LIMIT + 1).to_string().parse().unwrap(),
                );
            }
            _ => unreachable!(),
        }
        let response = send(state.clone(), request).await;
        assert_eq!(response.status(), expected, "case {case}");
        assert!(bytes(response).await.is_empty());
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }
    let mut request = request(
        ROUTINE_PREPARATION_ROUTE,
        Body::from(read().encode().unwrap()),
    );
    request
        .headers_mut()
        .insert(header::CONTENT_LENGTH, "1".parse().unwrap());
    assert_eq!(send(state, request).await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn routine_http_stream_bound_accepts_exact_limit_and_stops_after_overflow() {
    let state = state();
    let envelope = read();
    let mut exact = envelope.encode().unwrap();
    exact.resize(ROUTINE_BRIDGE_BODY_LIMIT, b' ');
    let response = send(
        state.clone(),
        request(ROUTINE_PREPARATION_ROUTE, Body::from(exact.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply =
        RoutineBridgeEnvelope::<ReadPreparationReply>::decode(&bytes(response).await).unwrap();
    reply.validate_reply_to(&envelope).unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    // The same full-bound request through tiny chunks exercises amortized
    // growth instead of one reallocation/copy per incoming fragment.
    let whole = Bytes::copy_from_slice(&exact);
    let length = whole.len();
    let tiny =
        futures_util::stream::iter((0..length).step_by(17).map(move |start| {
            Ok::<_, std::io::Error>(whole.slice(start..(start + 17).min(length)))
        }));
    let response = send(
        state.clone(),
        request(ROUTINE_PREPARATION_ROUTE, Body::from_stream(tiny)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    RoutineBridgeEnvelope::<ReadPreparationReply>::decode(&bytes(response).await)
        .unwrap()
        .validate_reply_to(&envelope)
        .unwrap();
    let observed = polls.clone();
    let mut count = 0;
    let stream =
        futures_util::stream::poll_fn(move |_| -> Poll<Option<Result<Bytes, std::io::Error>>> {
            observed.fetch_add(1, Ordering::SeqCst);
            count += 1;
            match count {
                1 => Poll::Ready(Some(Ok(Bytes::from(std::mem::take(&mut exact))))),
                2 => Poll::Ready(Some(Ok(Bytes::from_static(b" ")))),
                _ => panic!("must stop at4MiB+1"),
            }
        });
    assert_eq!(
        send(
            state.clone(),
            request(ROUTINE_PREPARATION_ROUTE, Body::from_stream(stream))
        )
        .await
        .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
}

#[tokio::test]
async fn routine_http_eight_body_permits_are_nonwaiting_and_cancelled_handlers_release_only_http_capacity()
 {
    let state = state();
    let (entered, mut receiver) = tokio::sync::mpsc::channel(HTTP_CAPACITY);
    let mut jobs = vec![];
    for _ in 0..HTTP_CAPACITY {
        let entered = entered.clone();
        let mut first = true;
        let body = Body::from_stream(futures_util::stream::poll_fn(
            move |_| -> Poll<Option<Result<Bytes, std::io::Error>>> {
                if first {
                    first = false;
                    entered.try_send(()).unwrap();
                }
                Poll::Pending
            },
        ));
        let state = state.clone();
        jobs.push(tokio::spawn(async move {
            send(state, request(ROUTINE_PREPARATION_ROUTE, body)).await
        }));
    }
    for _ in 0..HTTP_CAPACITY {
        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(state.slots.available_permits(), 0);
    let polls = Arc::new(AtomicUsize::new(0));
    let response = send(
        state.clone(),
        request(ROUTINE_PREPARATION_ROUTE, unpolled(polls.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(bytes(response).await.is_empty());
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    for job in jobs {
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
    }
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
    assert_eq!(
        send(
            state,
            request(
                ROUTINE_PREPARATION_ROUTE,
                Body::from(read().encode().unwrap())
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn routine_http_one_absolute_deadline_spans_streaming_and_codec_phases() {
    assert_eq!(ROUTINE_BRIDGE_DEADLINE, Duration::from_secs(5));
    let mut state = state();
    Arc::get_mut(&mut state).unwrap().test_budget = Duration::from_millis(100);
    let body = read().encode().unwrap();
    let (first, second) = body.split_at(body.len() / 2);
    let chunks = vec![
        Bytes::copy_from_slice(first),
        Bytes::copy_from_slice(second),
    ];
    let stream = futures_util::stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let chunk = chunks.next()?;
        tokio::time::sleep(Duration::from_millis(70)).await;
        Some((Ok::<_, std::io::Error>(chunk), chunks))
    });
    let started = Instant::now();
    let response = send(
        state.clone(),
        request(ROUTINE_PREPARATION_ROUTE, Body::from_stream(stream)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(bytes(response).await.is_empty());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(state.slots.available_permits(), HTTP_CAPACITY);
    let expired = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        dispatch(
            None,
            b"invalid".to_vec(),
            expired,
            AiWorkerClient::read_routine_preparation,
            |code| ReadPreparationReply::Refused { code },
        )
        .await,
        Err(StatusCode::GATEWAY_TIMEOUT)
    );
}
