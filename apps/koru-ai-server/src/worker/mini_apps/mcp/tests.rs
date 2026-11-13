// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ProfileId;

#[cfg(feature = "test-support")]
#[test]
fn mini_app_mcp_route_preserves_closed_validation_guidance_before_metadata_io() {
    use koru_mcp_server::{Backend, KoruMcp, RequestContext, ToolError};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    struct Route {
        deliveries: Mutex<Deliveries>,
        reads: AtomicUsize,
    }
    impl Backend for Route {
        fn doctor(&self) -> Result<serde_json::Value, ToolError> {
            panic!("no disk fixture")
        }
        fn invoke(
            &self,
            _: &RequestContext,
            name: &str,
            arguments: serde_json::Value,
        ) -> Result<serde_json::Value, ToolError> {
            assert_eq!(name, "mini_app_tool");
            let request: koru_ai_wire::MiniAppToolRequest =
                serde_json::from_value(arguments).unwrap();
            let deliveries = self.deliveries.lock().unwrap();
            visible_owner(owner(&deliveries, &request), |_| {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            })
            .map_err(|error| ToolError {
                code: error.code,
                message: error.message,
                details: None,
            })?;
            Ok(json!({"status":"offered","application_acknowledged":false}))
        }
    }
    let (deliveries, context, request) = setup();
    let route = Arc::new(Route {
        deliveries: Mutex::new(deliveries),
        reads: AtomicUsize::new(0),
    });
    let server = KoruMcp::new(route.clone(), RequestContext::local());
    let marker = "SYNTHETIC_PRIVATE_MARKER";
    let mut invalid = request.arguments.clone();
    invalid[marker] = marker.into();
    invalid["capability"] = request.capability.0.clone().into();
    let result = server.mini_app_call_for_test(mini_apps::CREATE_TOOL, invalid);
    assert_eq!(result.is_error, Some(true));
    let public = serde_json::to_string(&result).unwrap();
    assert!(public.contains("mini_app_validation_unknown_field"));
    assert!(public.contains("Remove extra fields"));
    assert!(!public.contains(marker));
    assert!(!public.contains(&request.capability.0));
    assert_eq!(route.reads.load(Ordering::Relaxed), 0);
    assert!(context.mini_app.as_ref().unwrap().live());
    // Rejected declarations do not reserve the one-success fence.
    assert!(owner(&route.deliveries.lock().unwrap(), &request).is_ok());
    assert!(context.mini_app.unwrap().begin());
}

#[test]
fn mini_app_mcp_profile_visibility_is_fresh_fail_closed_and_after_pure_bounds() {
    use std::cell::{Cell, RefCell};
    struct MemoryVisibility {
        reads: Cell<usize>,
        result: RefCell<ApiResult<bool>>,
    }
    impl MemoryVisibility {
        fn read(&self) -> ApiResult<bool> {
            self.reads.set(self.reads.get() + 1);
            self.result.borrow().clone()
        }
    }
    let port = MemoryVisibility {
        reads: Cell::new(0),
        result: RefCell::new(Ok(false)),
    };
    let (deliveries, _context, mut request) = setup();
    request.arguments = json!({"oversized":"x".repeat(4097)});
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert_eq!(port.reads.get(), 0);
    let (deliveries, _context, mut request) = setup();
    request.arguments["createSurface"]["components"][0]["text"] = "content mirror".into();
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert_eq!(port.reads.get(), 0);
    let (deliveries, _context, mut request) = setup();
    request.arguments["createSurface"]["components"][0]["id"] = "wrong graph".into();
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert_eq!(port.reads.get(), 0);
    let (deliveries, _context, mut request) = setup();
    request.name = mini_apps::LIST_TOOL.into();
    request.arguments = json!({"query":"x".repeat(129)});
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert_eq!(port.reads.get(), 0);
    // First call after removal must not rely on a previously accepted host.
    let (deliveries, context, request) = setup();
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert!(!context.mini_app.unwrap().live());
    // Removal while a job runs is checked again immediately before completion.
    let (deliveries, context, request) = setup();
    *port.result.borrow_mut() = Ok(true);
    let owned = visible_owner(owner(&deliveries, &request), |_| port.read()).unwrap();
    *port.result.borrow_mut() = Ok(false);
    assert!(visible_owner(Ok(owned), |_| port.read()).is_err());
    assert!(!context.mini_app.unwrap().live());
    // An unavailable Store cannot leave the old permission usable.
    let (deliveries, context, request) = setup();
    *port.result.borrow_mut() = Err(failure(
        "profiles_unavailable",
        "Profiles unavailable.",
        true,
    ));
    assert!(visible_owner(owner(&deliveries, &request), |_| port.read()).is_err());
    assert!(!context.mini_app.unwrap().live());
}
fn setup() -> (
    Deliveries,
    AiToolExecutionContext,
    koru_ai_wire::MiniAppToolRequest,
) {
    let attempt_id = InferenceAttemptId::new();
    let capability = mini_apps::Capability::mint();
    let host = HostContext {
        renderer_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        workspace_id: WorkspaceId::new(),
        refinement: None,
    };
    let authority = Authority::new(host);
    let context = AiToolExecutionContext {
        chat_id: ChatId::new(),
        attempt_id,
        invocation_id: ToolInvocationId::new(),
        profile_id: Some(ProfileId::new()),
        workspace_ids: vec![host.workspace_id],
        mini_app: Some(authority.clone()),
    };
    let mut deliveries = Deliveries::default();
    deliveries.authorities.insert(attempt_id, authority);
    deliveries
        .capabilities
        .insert(capability.clone(), attempt_id);
    let request = koru_ai_wire::MiniAppToolRequest {
        capability,
        name: mini_apps::CREATE_TOOL.into(),
        call_id: "mcp-request".into(),
        arguments: serde_json::from_str(mini_apps::FIXTURE).unwrap(),
    };
    (deliveries, context, request)
}
#[test]
fn mini_app_mcp_owner_is_exact_bounded_and_never_follows_new_attempt() {
    let (mut deliveries, context, mut request) = setup();
    assert_eq!(owner(&deliveries, &request).unwrap().0, context.attempt_id);
    let valid = request.clone();
    request.capability = mini_apps::Capability::mint();
    assert!(owner(&deliveries, &request).is_err());
    request = valid.clone();
    request.name = "list_documents".into();
    assert!(owner(&deliveries, &request).is_err());
    request = valid.clone();
    request.arguments = json!({"oversized":"x".repeat(4097)});
    assert!(owner(&deliveries, &request).is_err());
    request = valid.clone();
    request.call_id = "x".repeat(129);
    assert!(owner(&deliveries, &request).is_err());
    for _ in 0..32 {
        let (reply, _) = sync_channel(1);
        deliveries.pending.insert(
            ToolInvocationId::new(),
            McpPending {
                attempt: context.attempt_id,
                context: context.clone(),
                call: AiToolCall {
                    call_id: "call".into(),
                    name: mini_apps::CREATE_TOOL.into(),
                    arguments: json!({}),
                    thought_signatures: vec![],
                },
                reply,
            },
        );
    }
    assert!(owner(&deliveries, &valid).is_err());
    deliveries.retire(context.attempt_id);
    assert!(deliveries.pending.is_empty());
    assert!(deliveries.capabilities.is_empty());
    let newer = InferenceAttemptId::new();
    deliveries
        .authorities
        .insert(newer, Authority::new(context.mini_app.unwrap().host));
    deliveries
        .capabilities
        .insert(mini_apps::Capability::mint(), newer);
    assert!(owner(&deliveries, &valid).is_err());
}
#[test]
fn mini_app_mcp_completion_correlates_and_refuses_lost_delivery() {
    for mode in 0..4 {
        let (mut deliveries, context, request) = setup();
        let authority = context.mini_app.as_ref().unwrap();
        assert!(authority.begin());
        assert!(authority.finish(true));
        let call = AiToolCall {
            call_id: request.call_id,
            name: request.name,
            arguments: request.arguments,
            thought_signatures: vec![],
        };
        let result = AiToolResult {
            call_id: if mode == 0 {
                "wrong".into()
            } else {
                call.call_id.clone()
            },
            name: call.name.clone(),
            content: serde_json::to_value(
                mini_apps::Declaration::from_value(&call.arguments)
                    .unwrap()
                    .normalize(context.invocation_id)
                    .envelope,
            )
            .unwrap(),
            is_error: false,
        };
        let (reply, response) = sync_channel(1);
        deliveries.pending.insert(
            context.invocation_id,
            McpPending {
                attempt: context.attempt_id,
                context: context.clone(),
                call: call.clone(),
                reply,
            },
        );
        let (sender, events) = sync_channel(1);
        let mut streams = HashMap::from([(
            context.attempt_id,
            StreamState {
                sender: sender.clone(),
                next_sequence: 1,
            },
        )]);
        if mode == 1 {
            sender
                .send(StreamEvent {
                    version: AI_STREAM_VERSION,
                    sequence: 0,
                    event: PublicAiEvent::Notice {
                        attempt_id: context.attempt_id,
                        notice: koru_ai_core::AttemptNotice::FreshStart,
                        at: Timestamp::now(),
                    },
                })
                .unwrap();
        }
        if mode == 2 {
            drop(events);
        }
        complete(
            &mut deliveries,
            &mut streams,
            ToolResolution {
                external: true,
                invocation_id: context.invocation_id,
                attempt_id: context.attempt_id,
                call,
                result: Ok(result),
            },
        );
        let response = response.recv().unwrap();
        if mode == 3 {
            let response = response.unwrap();
            assert_eq!(response.content["status"], "offered");
            assert_eq!(response.content["application_acknowledged"], false);
            assert!(response.content.get("createSurface").is_none());
        } else {
            assert!(response.is_err());
        }
        assert!(deliveries.pending.is_empty());
        if mode == 1 || mode == 2 {
            assert!(streams.is_empty());
            assert!(!authority.live());
            assert!(deliveries.capabilities.is_empty());
        }
    }
}
