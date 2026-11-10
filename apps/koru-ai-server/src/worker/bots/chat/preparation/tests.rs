// SPDX-License-Identifier: MPL-2.0
use super::*;

fn pair() -> (Message, Message) {
    let turn = crate::worker::bots::tests::turn(ChatId::new(), Uuid::new_v4(), 1);
    let mut user = turn.user_message;
    user.ordinal = 0;
    user.markdown = "RETRY-TASK [[koru://skill/review-pr]]".into();
    let mut failed = turn.assistant_message;
    failed.ordinal = 1;
    failed.state = MessageState::Failed;
    (user, failed)
}

fn history(messages: &[Message]) -> Vec<AiRequestMessage> {
    messages
        .iter()
        .filter(|message| message.state == MessageState::Complete)
        .map(|message| AiRequestMessage {
            role: message.role,
            markdown: if message.role == ChatRole::User {
                koru_ai_core::render_prompt_text(&message.markdown)
            } else {
                message.markdown.clone()
            },
            attachments: Vec::new(),
        })
        .collect()
}

fn prompt(messages: Vec<AiRequestMessage>) -> String {
    koru_ai_provider_agent::compose_delegated_prompt(&AiRequest {
        attempt_id: InferenceAttemptId::new(),
        provider: AiProviderId::parse("agent-codex").unwrap(),
        model: "default".into(),
        reasoning: ReasoningChoice::Default,
        prompt_template_version: "test".into(),
        system: String::new(),
        messages,
        context: Vec::new(),
        tools: Vec::new(),
        tool_history: Vec::new(),
        options: AiRequestOptions::default(),
    })
}

#[test]
fn immediate_retry_reuses_the_rendered_task_once() {
    let (user, failed) = pair();
    let messages = vec![user.clone(), failed.clone()];
    let mut input = history(&messages);
    assert_eq!(
        retry_input(&messages, &failed, &user.markdown, &mut input).unwrap(),
        user.id
    );
    assert_eq!(input.len(), 1);
    assert_eq!(prompt(input), "RETRY-TASK $review-pr");
}

#[test]
fn earlier_failed_retry_keeps_later_history_and_selects_immutable_task_at_tail() {
    let (user, failed) = pair();
    let mut later_user = user.clone();
    later_user.id = MessageId::new();
    later_user.ordinal = 2;
    later_user.markdown = "UNRELATED-LATER-QUESTION".into();
    let mut later_reply = failed.clone();
    later_reply.id = MessageId::new();
    later_reply.ordinal = 3;
    later_reply.state = MessageState::Complete;
    later_reply.markdown = "LATER-ANSWER".into();
    let messages = vec![user.clone(), failed.clone(), later_user, later_reply];
    let mut input = history(&messages);
    let old_history = input.clone();
    retry_input(&messages, &failed, &user.markdown, &mut input).unwrap();
    assert_eq!(&input[..old_history.len()], old_history);
    assert_eq!(input.last().unwrap().markdown, "RETRY-TASK $review-pr");
    let text = prompt(input);
    assert!(text.find("UNRELATED-LATER-QUESTION").unwrap() < text.find("LATER-ANSWER").unwrap());
    assert!(text.ends_with("---\n\nRETRY-TASK $review-pr"));
    assert!(!text.contains("koru://skill"));
}

#[test]
fn assistant_only_retry_chain_uses_immutable_task_without_a_user_bubble() {
    let (_, failed) = pair();
    let mut retried = failed.clone();
    retried.id = MessageId::new();
    retried.ordinal = 2;
    retried.origin_message_id = Some(failed.id);
    let messages = vec![failed, retried.clone()];
    let mut input = history(&messages);
    assert_eq!(
        retry_input(&messages, &retried, "ANSWER-TASK", &mut input).unwrap(),
        retried.id
    );
    assert_eq!(prompt(input), "ANSWER-TASK");
}
