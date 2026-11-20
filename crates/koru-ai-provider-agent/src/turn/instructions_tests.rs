// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::{AiProviderId, AiRequestMessage, AiRequestOptions};

pub(super) fn request(system: &str) -> AiRequest {
    AiRequest {
        attempt_id: InferenceAttemptId::new(),
        provider: AiProviderId::parse("agent-claude").unwrap(),
        model: AGENT_DEFAULT_MODEL.into(),
        reasoning: ReasoningChoice::Default,
        prompt_template_version: "koru-chat-v2".into(),
        system: system.into(),
        messages: vec![AiRequestMessage {
            role: ChatRole::User,
            markdown: "PERSON-TASK".into(),
            attachments: vec![],
        }],
        context: vec![],
        tools: vec![],
        tool_history: vec![],
        options: AiRequestOptions::default(),
    }
}

#[test]
fn managed_session_gets_the_exact_system_once_before_its_first_task() {
    let request = request("  CUSTOM-COMMON\r\nMODE\n");
    assert_eq!(
        compose_prompt(&request, true),
        "  CUSTOM-COMMON\r\nMODE\n\n\nPERSON-TASK"
    );
    assert_eq!(compose_prompt(&request, false), "PERSON-TASK");
}

#[test]
fn changed_system_waits_for_a_new_session_and_does_not_rewrite_reattached_context() {
    let mut request = request("FIRST-SNAPSHOT");
    let first = compose_prompt(&request, true);
    request.system = "CHANGED-SNAPSHOT".into();
    assert_eq!(compose_prompt(&request, false), "PERSON-TASK");
    assert!(compose_prompt(&request, true).starts_with("CHANGED-SNAPSHOT\n\n"));
    assert!(first.starts_with("FIRST-SNAPSHOT\n\n"));
}

#[test]
fn legacy_bot_helper_never_injects_ordinary_system_or_cancels_its_role() {
    let mut request = request("ORDINARY-MODE-MUST-NOT-REACH-BOT");
    request.messages[0].markdown = "Role: researcher\nTask: save a page".into();
    let delegated = crate::compose_delegated_prompt(&request);
    assert_eq!(delegated, request.messages[0].markdown);
    assert!(!delegated.contains(&request.system));
}

#[test]
fn debug_replacement_is_forwarded_as_given_without_another_default() {
    let mut request = request("EXPLICIT-DEBUG-REPLACEMENT");
    request.prompt_template_version = "debug-override-v1".into();
    assert_eq!(
        compose_prompt(&request, true),
        "EXPLICIT-DEBUG-REPLACEMENT\n\nPERSON-TASK"
    );
}
