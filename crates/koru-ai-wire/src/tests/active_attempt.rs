// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn active_attempt_is_optional_and_keeps_the_accepted_turn_identity_and_model() {
    let mut transcript = transcript();
    let old = serde_json::to_value(&transcript).unwrap();
    assert!(old.get("active_attempt").is_none());
    assert_eq!(
        serde_json::from_value::<ChatTranscript>(old)
            .unwrap()
            .active_attempt,
        None
    );
    let attempt = InferenceAttempt {
        id: InferenceAttemptId::new(),
        operation_id: Uuid::new_v4(),
        chat_id: Some(transcript.chat.id),
        assistant_message_id: Some(MessageId::new()),
        surface: AiSurface::Standalone,
        intent: koru_ai_core::AiIntent::Chat,
        provider: transcript.chat.current_model.provider.clone(),
        requested_model: "accepted-model".into(),
        resolved_model: None,
        requested_reasoning: ReasoningChoice::Default,
        effective_reasoning: None,
        prompt_template_version: "fixture".into(),
        status: AttemptStatus::Pending,
        queued_at: Timestamp::from_millis(1),
        context_started_at: None,
        context_completed_at: None,
        provider_started_at: None,
        first_token_at: None,
        completed_at: None,
        request_payload_bytes: None,
        web_search_requested: false,
        web_search_calls: 0,
        context_payload_bytes: 0,
        message_count: 1,
        context_document_count: 0,
        context_message_block_count: 0,
        usage: AiUsage::default(),
        context_used_tokens: None,
        context_limit_tokens: None,
        stop_reason: None,
        error_class: None,
        provider_response_id: None,
    };
    transcript.chat.current_model.requested_model = "next-model".into();
    transcript.active_attempt = Some(attempt.clone());
    let read: ChatTranscript =
        serde_json::from_value(serde_json::to_value(&transcript).unwrap()).unwrap();
    assert_eq!(read.active_attempt, Some(attempt));
    assert_eq!(read.chat.current_model.requested_model, "next-model");
    assert_eq!(crate::AI_WIRE_VERSION, 1, "the response field is additive");
}
