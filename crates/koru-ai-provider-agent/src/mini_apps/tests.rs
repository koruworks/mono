// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{
    AgentActivityEvent, AgentToolKind, InferenceAttemptId, Timestamp, ToolInvocationId,
};

#[test]
fn mini_app_codex_mcp_header_keeps_only_closed_owned_tool_identity() {
    use koru_ai_core::{CallShape, mini_apps};
    let token = mini_apps::Capability::mint().0;
    let attempt_id = InferenceAttemptId::new();
    let invocation = ToolInvocationId::new();
    let at = Timestamp::now();
    let kind = koru_session_protocol::ToolKind::McpTool.as_str();
    for server in [
        koru_session_profiles::MCP_SERVER_NAME,
        koru_session_profiles::CODEX_DESKTOP_MCP_SERVER_NAME,
    ] {
        for tool in [
            mini_apps::CREATE_TOOL,
            mini_apps::LIST_TOOL,
            mini_apps::UPDATE_TOOL,
        ] {
            // Exact spelling produced by codex::normalize::item_input.
            let input =
                format!("{server}: {tool} {{\"capability\":\"{token}\",\"createSurface\":{{}}}}");
            let events = vec![
                AiEvent::AgentActivity {
                    attempt_id,
                    at,
                    activity: AgentActivityEvent::ToolStarted {
                        invocation,
                        label: kind.into(),
                        input: Some(input),
                    },
                },
                AiEvent::AgentActivity {
                    attempt_id,
                    at,
                    activity: AgentActivityEvent::ToolCompleted {
                        invocation,
                        tool: AgentToolKind::Mcp,
                        succeeded: true,
                        failure: None,
                        detail: Some(token.clone()),
                        output: Some("createSurface raw body".into()),
                    },
                },
            ];
            let events = Redacted::new(events.into_iter()).collect::<Vec<_>>();
            let AiEvent::AgentActivity {
                activity: AgentActivityEvent::ToolStarted { label, input, .. },
                ..
            } = &events[0]
            else {
                panic!()
            };
            assert_eq!(CallShape::of(label), Some(CallShape::Koru(tool.into())));
            assert!(input.is_none());
            let output = format!("{events:?}");
            assert!(!output.contains(&token));
            assert!(!output.contains("createSurface"));
            assert!(matches!(
                &events[1],
                AiEvent::AgentActivity {
                    activity: AgentActivityEvent::ToolCompleted {
                        detail: None,
                        output: None,
                        ..
                    },
                    ..
                }
            ));
        }
    }
    for (label, header) in [
        (kind, "foreign: create_task_document_app "),
        (kind, "koru_desktops: create_task_document_app "),
        (kind, "koru_desktop: unknown_tool "),
        (kind, "embedded koru_desktop: create_task_document_app "),
        (kind, "koru_desktop: create_task_document_app_extra "),
        (kind, "koru_desktop:create_task_document_app "),
        (kind, "koru_desktop: create_task_document_app"),
        (
            "command_execution",
            "koru_desktop: create_task_document_app ",
        ),
    ] {
        let event = AiEvent::AgentActivity {
            attempt_id,
            at,
            activity: AgentActivityEvent::ToolStarted {
                invocation,
                label: label.into(),
                input: Some(format!("{header}{{\"capability\":\"{token}\"}}")),
            },
        };
        let event = Redacted::new(std::iter::once(event)).next().unwrap();
        let AiEvent::AgentActivity {
            activity:
                AgentActivityEvent::ToolStarted {
                    label: actual,
                    input,
                    ..
                },
            ..
        } = event
        else {
            panic!()
        };
        assert_eq!(actual, label);
        assert!(CallShape::of(&actual).is_none());
        assert!(input.is_none());
    }
}
#[test]
fn mini_app_permissions_are_redacted_across_every_text_split_and_eof_keeps_plain_suffix() {
    let token = koru_ai_core::mini_apps::Capability::mint().0;
    let attempt_id = InferenceAttemptId::new();
    let at = Timestamp::now();
    for split in 0..=token.len() {
        let events = vec![
            AiEvent::TextDelta {
                attempt_id,
                text: format!("before {}", &token[..split]),
                at,
            },
            AiEvent::TextDelta {
                attempt_id,
                text: format!("{} after task", &token[split..]),
                at,
            },
        ];
        let text = Redacted::new(events.into_iter())
            .filter_map(|e| {
                if let AiEvent::TextDelta { text, .. } = e {
                    Some(text)
                } else {
                    None
                }
            })
            .collect::<String>();
        assert!(!text.contains(&token), "split {split}");
        assert!(text.starts_with("before "));
        assert!(text.ends_with(" after task"));
    }
    let ordinary = vec![AiEvent::TextDelta {
        attempt_id,
        text: "task".into(),
        at,
    }];
    assert_eq!(
        Redacted::new(ordinary.into_iter())
            .filter_map(|e| if let AiEvent::TextDelta { text, .. } = e {
                Some(text)
            } else {
                None
            })
            .collect::<String>(),
        "task"
    );
}
#[test]
fn mini_app_activity_never_yields_raw_declarations_or_permissions() {
    let token = koru_ai_core::mini_apps::Capability::mint().0;
    let attempt_id = InferenceAttemptId::new();
    let at = Timestamp::now();
    let invocation = ToolInvocationId::new();
    for label in [
        "mcp__koru__create_task_document_app",
        "unrecognized provider label",
    ] {
        let events = vec![
            AiEvent::AgentActivity {
                attempt_id,
                at,
                activity: AgentActivityEvent::ToolStarted {
                    invocation,
                    label: format!("{label} {token}"),
                    input: Some(format!(
                        "{{\"capability\":\"{token}\",\"createSurface\":{{}}}}"
                    )),
                },
            },
            AiEvent::AgentActivity {
                attempt_id,
                at,
                activity: AgentActivityEvent::ToolCompleted {
                    invocation,
                    tool: AgentToolKind::Other,
                    succeeded: true,
                    failure: None,
                    detail: Some(token.clone()),
                    output: Some("createSurface full envelope".into()),
                },
            },
            AiEvent::AgentActivity {
                attempt_id,
                at,
                activity: AgentActivityEvent::Notice {
                    text: format!("old permission {token}"),
                },
            },
        ];
        let output = Redacted::new(events.into_iter()).collect::<Vec<_>>();
        let debug = format!("{output:?}");
        assert!(!debug.contains(&token));
        assert!(!debug.contains("createSurface"));
    }
}

#[test]
fn mini_app_redaction_preserves_prefix_prose_unicode_malformed_and_partial_eof() {
    let token = koru_ai_core::mini_apps::Capability::mint().0;
    for ordinary in [
        "The koru-mini-app documentation explains its ordinary text",
        "日本語koru-mini-app🙂",
        "koru-mini-00000000-xxxx-0000-0000-000000000000",
        "koru-mi",
        "koru-mini-",
        "task",
    ] {
        for split in ordinary
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(ordinary.len()))
        {
            let mut text = Text::default();
            let mut clean = text.push(&ordinary[..split]);
            clean.push_str(&text.push(&ordinary[split..]));
            clean.push_str(&text.finish());
            assert_eq!(clean, ordinary);
        }
    }
    let mut text = Text::default();
    let mut clean = text.push(&format!("{token}{token}"));
    clean.push_str(&text.finish());
    assert!(!clean.contains(&token));
    assert_eq!(clean.matches("[redacted mini-app permission]").count(), 2);
}

#[test]
fn mini_app_prompt_is_request_local_on_fresh_and_reused_sessions() {
    use koru_ai_core::*;
    let capability = mini_apps::Capability::mint();
    let mut request = AiRequest {
        attempt_id: InferenceAttemptId::new(),
        provider: AiProviderId::parse("agent-codex").unwrap(),
        model: "default".into(),
        reasoning: ReasoningChoice::Default,
        prompt_template_version: "test".into(),
        system: String::new(),
        messages: vec![AiRequestMessage {
            role: ChatRole::User,
            markdown: "build this".into(),
            attachments: vec![],
        }],
        context: vec![],
        tools: vec![],
        tool_history: vec![],
        options: AiRequestOptions::default(),
    };
    let ordinary = crate::turn::compose_prompt(&request, false);
    assert_eq!(ordinary, "build this");
    request.options.mini_app = Some(mini_apps::RuntimeContext {
        capability: capability.clone(),
        workspace_id: WorkspaceId::new(),
        refinement_surface: Some("mini-app-target".into()),
    });
    for (provider, server) in [
        (
            "agent-codex",
            koru_session_profiles::CODEX_DESKTOP_MCP_SERVER_NAME,
        ),
        ("agent-claude", koru_session_profiles::MCP_SERVER_NAME),
    ] {
        request.provider = AiProviderId::parse(provider).unwrap();
        assert_eq!(
            CallShape::of(&format!("mcp__{server}__find_task_app_lists")),
            Some(CallShape::Koru("find_task_app_lists".into())),
            "shared bridge alias and activity vocabulary must agree"
        );
        for fresh in [false, true] {
            let prompt = crate::turn::compose_prompt(&request, fresh);
            assert!(prompt.contains(&capability.0));
            assert!(prompt.contains(&format!("existing {server} MCP server")));
            assert!(prompt.contains("update_task_document_app"));
            assert!(prompt.contains("mini-app-target"));
        }
    }
    assert_eq!(request.messages[0].markdown, "build this");
}
