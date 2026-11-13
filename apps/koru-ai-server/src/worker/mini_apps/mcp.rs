// SPDX-License-Identifier: MPL-2.0
use super::*;

fn unavailable() -> Failure {
    failure(
        "mini_app_unavailable",
        "This mini-app permission is missing, expired, or busy. Send a new request from the live Koru host.",
        false,
    )
}

fn owner(
    deliveries: &Deliveries,
    request: &koru_ai_wire::MiniAppToolRequest,
) -> ApiResult<(InferenceAttemptId, Authority)> {
    if !request.capability.valid()
        || request.call_id.is_empty()
        || request.call_id.len() > 128
        || ![
            mini_apps::CREATE_TOOL,
            mini_apps::LIST_TOOL,
            mini_apps::UPDATE_TOOL,
        ]
        .contains(&request.name.as_str())
        || mini_apps::bounded_json(&request.arguments).is_err()
        || deliveries.pending.len() >= 32
    {
        return Err(unavailable());
    }
    crate::mini_apps::validate_arguments(&request.name, &request.arguments).map_err(ai_failure)?;
    let attempt = *deliveries
        .capabilities
        .get(&request.capability)
        .ok_or_else(unavailable)?;
    let authority = deliveries
        .authorities
        .get(&attempt)
        .filter(|a| a.live())
        .cloned()
        .ok_or_else(unavailable)?;
    Ok((attempt, authority))
}

/// Resolve exact bounded ownership before consulting profile visibility. A
/// failed visibility read revokes permission rather than using stale knowledge.
fn visible_owner(
    owned: ApiResult<(InferenceAttemptId, Authority)>,
    refresh: impl FnOnce(InferenceAttemptId) -> ApiResult<bool>,
) -> ApiResult<(InferenceAttemptId, Authority)> {
    let (attempt, authority) = owned?;
    match refresh(attempt) {
        Ok(true) if authority.live() => Ok((attempt, authority)),
        Ok(_) => {
            authority.revoke();
            Err(unavailable())
        }
        Err(error) => {
            authority.revoke();
            Err(error)
        }
    }
}

impl WorkerState {
    pub(in crate::worker) fn mini_app_runtime(
        &self,
        attempt: InferenceAttemptId,
    ) -> Option<mini_apps::RuntimeContext> {
        let (capability, _) = self
            .mini_apps
            .capabilities
            .iter()
            .find(|(_, owner)| **owner == attempt)?;
        let host = self.mini_apps.authorities.get(&attempt)?.host;
        Some(mini_apps::RuntimeContext {
            capability: capability.clone(),
            workspace_id: host.workspace_id,
            refinement_surface: host.refinement.map(|target| target.surface_name()),
        })
    }

    pub(in crate::worker) fn begin_mcp_mini_app(
        &mut self,
        context: RequestContext,
        request: koru_ai_wire::MiniAppToolRequest,
        reply: SyncSender<ApiResult<koru_ai_wire::MiniAppToolResponse>>,
    ) {
        let admitted = (|| {
            authorize(&context, AiScope::Chat)?;
            let owned = owner(&self.mini_apps, &request);
            let (attempt_id, authority) = visible_owner(owned, |attempt_id| {
                self.refresh_removed()?;
                let attempt = self
                    .repository
                    .attempt(attempt_id)
                    .map_err(ai_failure)?
                    .filter(|a| !a.status.is_terminal())
                    .ok_or_else(unavailable)?;
                let profile = self
                    .repository
                    .chat(attempt.chat_id.ok_or_else(unavailable)?)
                    .map_err(ai_failure)?
                    .ok_or_else(unavailable)?
                    .profile_id;
                Ok(!self.is_hidden(profile))
            })?;
            let attempt = self
                .repository
                .attempt(attempt_id)
                .map_err(ai_failure)?
                .filter(|a| !a.status.is_terminal())
                .ok_or_else(unavailable)?;
            let chat_id = attempt.chat_id.ok_or_else(unavailable)?;
            self.admit_chat_work(Some(chat_id))?;
            let chat = self
                .repository
                .chat(chat_id)
                .map_err(ai_failure)?
                .ok_or_else(unavailable)?;
            let profile_id = chat.profile_id.ok_or_else(unavailable)?;
            let policy = self
                .repository
                .chat_tool_policy(chat_id)
                .map_err(ai_failure)?
                .ok_or_else(unavailable)?;
            if !policy.workspace_ids.contains(&authority.host.workspace_id) {
                return Err(unavailable());
            }
            let cancellation = self
                .cancellations
                .get(&attempt_id)
                .filter(|c| !c.is_cancelled())
                .cloned()
                .ok_or_else(unavailable)?;
            let invocation_id = ToolInvocationId::new();
            let call = AiToolCall {
                call_id: request.call_id,
                name: request.name,
                arguments: request.arguments,
                thought_signatures: Vec::new(),
            };
            let execution = AiToolExecutionContext {
                mini_app: Some(authority),
                chat_id,
                attempt_id,
                invocation_id,
                profile_id: Some(profile_id),
                workspace_ids: policy.workspace_ids,
            };
            let job = ToolJob {
                external: true,
                context: execution.clone(),
                call: call.clone(),
                cancellation,
            };
            self.tool_pool.try_submit(job).map_err(ai_failure)?;
            self.work_admitted(attempt_id);
            Ok((
                invocation_id,
                McpPending {
                    attempt: attempt_id,
                    context: execution,
                    call,
                    reply: reply.clone(),
                },
            ))
        })();
        match admitted {
            Ok((id, pending)) => {
                self.mini_apps.pending.insert(id, pending);
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    pub(in crate::worker) fn finish_mcp_mini_app(&mut self, resolution: ToolResolution) {
        if let Some((attempt, profile, authority)) = self
            .mini_apps
            .pending
            .get(&resolution.invocation_id)
            .map(|pending| {
                (
                    pending.attempt,
                    pending.context.profile_id,
                    pending.context.mini_app.clone().unwrap(),
                )
            })
        {
            if visible_owner(Ok((attempt, authority)), |_| {
                self.refresh_removed()?;
                Ok(!self.is_hidden(profile))
            })
            .is_err()
            {
                self.mini_apps.retire(attempt);
                return;
            }
        }
        if let Some(pending) = self.mini_apps.pending.get(&resolution.invocation_id) {
            let context = &pending.context;
            let workspace = context.mini_app.as_ref().unwrap().host.workspace_id;
            let permitted = self
                .repository
                .chat(context.chat_id)
                .ok()
                .flatten()
                .is_some_and(|chat| chat.profile_id == context.profile_id)
                && self
                    .repository
                    .chat_tool_policy(context.chat_id)
                    .ok()
                    .flatten()
                    .is_some_and(|policy| policy.workspace_ids.contains(&workspace));
            if !permitted {
                self.mini_apps.retire(context.attempt_id);
                return;
            }
        }
        complete(&mut self.mini_apps, &mut self.streams, resolution);
    }
}
fn complete(
    deliveries: &mut Deliveries,
    streams: &mut HashMap<InferenceAttemptId, StreamState>,
    resolution: ToolResolution,
) {
    let Some(pending) = deliveries.pending.remove(&resolution.invocation_id) else {
        return;
    };
    let outcome = (|| {
        if pending.attempt != resolution.attempt_id
            || !streams.contains_key(&pending.attempt)
            || !deliveries
                .authorities
                .get(&pending.attempt)
                .is_some_and(|a| a.live())
        {
            return Err(unavailable());
        }
        let result = resolution.result.map_err(ai_failure)?;
        if pending.call.call_id != resolution.call.call_id
            || pending.call.name != resolution.call.name
        {
            return Err(unavailable());
        }
        correlated(&pending.call, &result).map_err(ai_failure)?;
        publish_validated(streams, deliveries, &pending.context, &result).map_err(ai_failure)?;
        let content = if !result.is_error
            && [mini_apps::CREATE_TOOL, mini_apps::UPDATE_TOOL].contains(&result.name.as_str())
        {
            json!({"status":"offered","application_acknowledged":false,"notice":"Validated and offered to the live native host. Local application is not acknowledged; do not claim that a tab opened or changed."})
        } else {
            result.content
        };
        Ok(koru_ai_wire::MiniAppToolResponse {
            content,
            is_error: result.is_error,
        })
    })();
    let _ = pending.reply.send(outcome);
}

/// In-memory end-to-end tool fixture. MCP's actual wrapper dispatches through
/// the same owner check, bounded tool pool, correlation and publication used
/// by the production worker. The registry is supplied by the caller.
#[cfg(feature = "test-support")]
pub(crate) fn fixture(
    executor: Arc<dyn AiToolExecutor>,
    context: AiToolExecutionContext,
    tool: &str,
    mut arguments: serde_json::Value,
) -> Result<Vec<StreamEvent>, AiError> {
    struct Harness {
        context: AiToolExecutionContext,
        deliveries: Deliveries,
        streams: HashMap<InferenceAttemptId, StreamState>,
        pool: ToolPool,
        output: Receiver<Envelope>,
    }
    struct Bridge(Mutex<Harness>);
    impl koru_mcp_server::Backend for Bridge {
        fn doctor(&self) -> Result<serde_json::Value, koru_mcp_server::ToolError> {
            panic!("no doctor I/O in fixture")
        }
        fn invoke(
            &self,
            _: &koru_mcp_server::RequestContext,
            name: &str,
            arguments: serde_json::Value,
        ) -> Result<serde_json::Value, koru_mcp_server::ToolError> {
            let mut h = self.0.lock().unwrap();
            let fail = || koru_mcp_server::ToolError {
                code: "invalid_request".into(),
                message: "fixture MCP call refused".into(),
                details: None,
            };
            if name != "mini_app_tool" {
                return Err(fail());
            }
            let request: koru_ai_wire::MiniAppToolRequest =
                serde_json::from_value(arguments).map_err(|_| fail())?;
            let (attempt, authority) = owner(&h.deliveries, &request).map_err(|_| fail())?;
            let mut context = h.context.clone();
            context.attempt_id = attempt;
            context.invocation_id = ToolInvocationId::new();
            context.mini_app = Some(authority);
            let call = AiToolCall {
                call_id: request.call_id,
                name: request.name,
                arguments: request.arguments,
                thought_signatures: vec![],
            };
            let (reply, response) = sync_channel(1);
            h.pool
                .try_submit(ToolJob {
                    external: true,
                    context: context.clone(),
                    call: call.clone(),
                    cancellation: Cancellation::default(),
                })
                .map_err(|_| fail())?;
            h.deliveries.pending.insert(
                context.invocation_id,
                McpPending {
                    attempt,
                    context,
                    call,
                    reply,
                },
            );
            let Envelope::Tools(resolution) = h
                .output
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| fail())?
            else {
                return Err(fail());
            };
            let Harness {
                deliveries,
                streams,
                ..
            } = &mut *h;
            complete(deliveries, streams, resolution);
            let result = response
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| fail())?
                .map_err(|_| fail())?;
            if result.is_error {
                return Err(fail());
            }
            Ok(result.content)
        }
    }
    let capability = mini_apps::Capability::mint();
    arguments["capability"] = capability.0.clone().into();
    let mut deliveries = Deliveries::default();
    deliveries
        .capabilities
        .insert(capability.clone(), context.attempt_id);
    deliveries.authorities.insert(
        context.attempt_id,
        context.mini_app.clone().ok_or_else(tool_loop_missing)?,
    );
    let (sender, events) = sync_channel(4);
    let streams = HashMap::from([(
        context.attempt_id,
        StreamState {
            sender,
            next_sequence: 1,
        },
    )]);
    let (output_tx, output) = sync_channel(4);
    let pool = ToolPool::spawn(
        Some(executor),
        output_tx,
        Arc::new(AtomicBool::new(false)),
        1,
    )
    .map_err(|_| tool_loop_missing())?;
    let backend = Arc::new(Bridge(Mutex::new(Harness {
        context: context.clone(),
        deliveries,
        streams,
        pool,
        output,
    })));
    let bridge =
        koru_mcp_server::KoruMcp::new(backend.clone(), koru_mcp_server::RequestContext::local());
    let result = bridge.mini_app_call_for_test(tool, arguments);
    let mut harness = backend.0.lock().unwrap();
    harness.pool.stop();
    if result.is_error == Some(true) {
        return Err(tool_loop_missing());
    }
    let content = result.structured_content.ok_or_else(tool_loop_missing)?;
    assert_eq!(content["status"], "offered");
    assert_eq!(content["application_acknowledged"], false);
    assert!(!content.to_string().contains(&capability.0));
    assert!(content.get("createSurface").is_none() && content.get("updateComponents").is_none());
    let Harness {
        streams,
        deliveries,
        ..
    } = &mut *harness;
    publish_live(
        streams,
        deliveries,
        PublicAiEvent::Completed {
            attempt_id: context.attempt_id,
            stop_reason: None,
            at: Timestamp::now(),
        },
    );
    Ok(events.try_iter().collect())
}

#[cfg(test)]
mod tests;
