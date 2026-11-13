// SPDX-License-Identifier: MPL-2.0
use super::*;
fn host() -> HostContext {
    HostContext {
        renderer_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        workspace_id: WorkspaceId::new(),
        refinement: None,
    }
}

#[test]
fn mini_app_descriptors_name_only_the_request_local_workspace() {
    let global = mini_apps::definitions();
    let original = global.clone();
    let h = host();
    let scoped = scoped_definitions(&global, Some(h));
    assert!(!scoped.iter().any(|d| d.name == mini_apps::UPDATE_TOOL));
    let target = mini_apps::RefinementTarget {
        tab_id: 23,
        surface_id: Uuid::new_v4(),
        base_revision: 7,
    };
    let refined = scoped_definitions(
        &global,
        Some(HostContext {
            refinement: Some(target),
            ..h
        }),
    );
    let update = refined
        .iter()
        .find(|d| d.name == mini_apps::UPDATE_TOOL)
        .unwrap();
    assert_eq!(
        update.input_schema["properties"]["updateComponents"]["properties"]["surfaceId"]["const"],
        target.surface_name()
    );
    assert!(
        update.input_schema["properties"]
            .get("baseRevision")
            .is_none()
    );
    assert!(
        scoped
            .iter()
            .all(|d| d.description.contains(&h.workspace_id.to_string()))
    );
    assert!(
        scoped
            .iter()
            .all(|d| !d.description.contains(&h.request_id.to_string())
                && !d.description.contains(&h.renderer_id.to_string()))
    );
    assert_eq!(global, original);
    assert!(scoped_definitions(&global, None).is_empty());
    let other = host();
    let differently_scoped = scoped_definitions(&global, Some(other));
    assert!(
        differently_scoped
            .iter()
            .all(|d| !d.description.contains(&h.workspace_id.to_string()))
    );
}
#[test]
fn mini_app_only_successes_consume_capacity_and_cross_attempt_replays_are_refused() {
    let mut state = Deliveries::default();
    for _ in 0..2048 {
        let attempt = InferenceAttemptId::new();
        let h = host();
        assert!(state.available(h));
        state.authorities.insert(attempt, Authority::new(h));
        state.retire(attempt);
    }
    assert!(state.successes.is_empty());
    assert!(state.authorities.is_empty());
    let h = host();
    assert!(state.record(h));
    let _new_attempt = Authority::new(h);
    assert!(!state.available(h));
    assert!(!state.record(h));
    for _ in 1..SUCCESS_LIMIT {
        assert!(state.record(host()));
    }
    assert!(!state.available(host()));
    assert!(!state.record(host()));
    assert_eq!(state.successes.len(), SUCCESS_LIMIT);
}
#[test]
fn mini_app_full_disconnected_and_terminal_streams_revoke_job_authority() {
    for mode in 0..3 {
        let attempt = InferenceAttemptId::new();
        let authority = Authority::new(host());
        let mut state = Deliveries::default();
        state.authorities.insert(attempt, authority.clone());
        let (sender, receiver) = sync_channel(1);
        let mut streams = HashMap::from([(
            attempt,
            StreamState {
                sender: sender.clone(),
                next_sequence: 1,
            },
        )]);
        let event = if mode == 2 {
            PublicAiEvent::Completed {
                attempt_id: attempt,
                stop_reason: None,
                at: Timestamp::now(),
            }
        } else {
            PublicAiEvent::Notice {
                attempt_id: attempt,
                notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
                at: Timestamp::now(),
            }
        };
        if mode == 0 {
            sender
                .try_send(StreamEvent {
                    version: AI_STREAM_VERSION,
                    sequence: 0,
                    event: event.clone(),
                })
                .unwrap();
        }
        if mode == 1 {
            drop(receiver);
        }
        publish_live(&mut streams, &mut state, event);
        assert!(streams.is_empty());
        assert!(state.authorities.is_empty());
        assert!(!authority.live());
        assert!(state.successes.is_empty());
    }
}
#[test]
fn mini_app_result_correlation_precedes_publication() {
    let call = AiToolCall {
        call_id: "requested".into(),
        name: mini_apps::CREATE_TOOL.into(),
        arguments: json!({}),
        thought_signatures: Vec::new(),
    };
    let mut result = AiToolResult {
        call_id: "other".into(),
        name: call.name.clone(),
        content: json!({}),
        is_error: false,
    };
    assert!(correlated(&call, &result).is_err());
    result.call_id = call.call_id.clone();
    assert!(correlated(&call, &result).is_ok());
    result.name = "another tool".into();
    assert!(correlated(&call, &result).is_err());
}
