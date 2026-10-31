// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{AiSurface, AiUsage, AttemptStatus, UsageReport, UsageTotals};
use koru_draw::theme::role;
use koru_i18n::{Key, Lang};

use crate::{ACTION_H, BLOCK_GAP, MessageLayout, PAD, USAGE_GAP};

pub(crate) fn message_height(message: &MessageLayout) -> f32 {
    let actions = if message.role == koru_ai_core::ChatRole::Assistant
        && message.id.is_some()
        && message.state.is_terminal()
    {
        ACTION_H + 8.0
    } else {
        0.0
    };
    let usage = message
        .usage
        .as_ref()
        .map_or(0.0, |_| role::hint().metrics.line_height + USAGE_GAP);
    let sources = message
        .sources
        .as_ref()
        .map_or(0.0, |sources| sources.layout.height() + USAGE_GAP);
    message_content_height(message) + usage + sources + PAD * 2.0 + actions
}

pub(crate) fn message_content_height(message: &MessageLayout) -> f32 {
    if !message.attachments.is_empty() {
        // The person's own words, then their pictures and files; a message
        // of attachments only is its tiles alone.
        let (_, tiles) = crate::attachments::card_tiles(&message.attachments, message.wrap_width);
        return if message.display.trim().is_empty() {
            tiles
        } else {
            message.layout.height() + crate::attachments::TILE_GAP + tiles
        };
    }
    if message.blocks.is_empty() {
        if message.state == koru_ai_core::MessageState::Pending && message.markdown.is_empty() {
            role::ui().metrics.line_height
        } else {
            message.layout.height()
        }
    } else {
        message
            .blocks
            .iter()
            .map(|block| block.height())
            .sum::<f32>()
            + BLOCK_GAP * message.blocks.len().saturating_sub(1) as f32
    }
}

/// Format the quiet, per-response usage line shown below an assistant reply.
pub(crate) fn usage_line(usage: &AiUsage, lang: Lang) -> Option<String> {
    match (usage.prompt_tokens, usage.completion_tokens) {
        (Some(prompt), Some(completion)) => {
            let prompt = compact(prompt);
            let completion = compact(completion);
            if let Some(reasoning) = usage.reasoning_tokens.filter(|&tokens| tokens > 0) {
                let reasoning = compact(reasoning);
                Some(koru_i18n::fill(
                    koru_i18n::t(lang, Key::AiUsageInOutReasoning),
                    &[&prompt, &completion, &reasoning],
                ))
            } else {
                Some(koru_i18n::fill(
                    koru_i18n::t(lang, Key::AiUsageInOut),
                    &[&prompt, &completion],
                ))
            }
        }
        _ => usage.total_tokens.map(|total| {
            let total = compact(total);
            koru_i18n::fill(koru_i18n::t(lang, Key::AiUsageTotalTokens), &[&total])
        }),
    }
}

fn compact(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 10_000 {
        return decimal_unit(n, 1_000, "k");
    }
    if n < 1_000_000 {
        return format!("{}k", (n + 500) / 1_000);
    }
    if n < 10_000_000 {
        return decimal_unit(n, 1_000_000, "M");
    }
    format!("{}M", n.saturating_add(500_000) / 1_000_000)
}

fn decimal_unit(n: u64, divisor: u64, suffix: &str) -> String {
    let tenths = n.saturating_mul(10).saturating_add(divisor / 2) / divisor;
    if tenths.is_multiple_of(10) {
        format!("{}{suffix}", tenths / 10)
    } else {
        format!("{}.{:01}{suffix}", tenths / 10, tenths % 10)
    }
}

pub(crate) fn format_usage_report(report: &UsageReport) -> String {
    let mut text = String::from(
        "Usage — private and local\nNo provider call or product telemetry. Monetary cost is not estimated.\n\nCurrent chat\n",
    );
    match &report.current_chat {
        Some(totals) => push_usage_totals(&mut text, totals),
        None => text.push_str("Unavailable\n"),
    }
    text.push_str("\nAll usage\n");
    if report.groups.is_empty() {
        text.push_str("No retained usage.\n");
    }
    for group in &report.groups {
        let surface = match group.surface {
            AiSurface::Standalone => "standalone",
            AiSurface::ContextPane => "context pane",
        };
        text.push_str(&format!(
            "\n{} · {} · {} / {} · effort {} · {} · prompt {}\n",
            group.utc_day,
            surface,
            group.provider,
            group.model,
            group.reasoning.as_token(),
            attempt_status_label(group.status),
            group.prompt_template_version,
        ));
        push_usage_totals(&mut text, &group.totals);
    }
    text
}

fn push_usage_totals(text: &mut String, totals: &UsageTotals) {
    text.push_str(&format!(
        "Attempts: {} ({} with provider-reported usage)",
        totals.attempts, totals.attempts_with_reported_usage
    ));
    if totals.web_search_calls > 0 {
        text.push_str(&format!(" · {} web searches", totals.web_search_calls));
    }
    text.push('\n');
    text.push_str(&format!(
        "Tokens: prompt {} · cached {} · completion {} · reasoning {} · total {}\n",
        optional_usage(totals.prompt_tokens),
        optional_usage(totals.cached_prompt_tokens),
        optional_usage(totals.completion_tokens),
        optional_usage(totals.reasoning_tokens),
        optional_usage(totals.total_tokens),
    ));
    text.push_str(&format!(
        "Sizes: request {} B · context {} B · messages {} · documents {} · chat blocks {}\n",
        optional_usage(totals.request_payload_bytes),
        totals.context_payload_bytes,
        totals.message_count,
        totals.context_document_count,
        totals.context_message_block_count,
    ));
    text.push_str(&format!(
        "Average timing: context {} ms · provider start {} ms · first token {} ms · generation {} ms · total {} ms\n",
        optional_usage(totals.average_context_assembly_ms),
        optional_usage(totals.average_provider_start_ms),
        optional_usage(totals.average_time_to_first_token_ms),
        optional_usage(totals.average_generation_ms),
        optional_usage(totals.average_total_ms),
    ));
    text.push_str(&format!(
        "Current feedback: {} good · {} bad\n",
        totals.good_feedback, totals.bad_feedback
    ));
}

fn optional_usage(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| value.to_string())
}

fn attempt_status_label(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Pending => "pending",
        AttemptStatus::Streaming => "streaming",
        AttemptStatus::Complete => "complete",
        AttemptStatus::Interrupted => "interrupted",
        AttemptStatus::Failed => "failed",
        AttemptStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use koru_ai_core::{
        AiError, AiErrorClass, AiProviderId, Chat, ChatId, ChatRole, Message, MessageId,
        MessageState, ModelSelection, ReasoningChoice, Timestamp, UsageSource,
    };
    use koru_ai_wire::{
        AI_STREAM_VERSION, ChatTranscript, MessageUsage, PublicAiEvent, StreamEvent,
    };
    use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

    use crate::reader::{Command, Reader, Update};
    use crate::{ChatSurface, SurfaceMode};

    fn usage(
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> AiUsage {
        AiUsage {
            prompt_tokens,
            cached_prompt_tokens: None,
            completion_tokens,
            reasoning_tokens,
            total_tokens,
            source: UsageSource::ProviderReported,
        }
    }

    fn transcript() -> ChatTranscript {
        let now = Timestamp::from_millis(1);
        let chat_id = ChatId::new();
        ChatTranscript {
            sources: Vec::new(),
            chat: Chat {
                id: chat_id,
                title: "Test chat".into(),
                private: false,
                current_model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                created_at: now,
                updated_at: now,
            },
            tool_policy: koru_ai_core::ChatToolPolicy {
                chat_id,
                mode: koru_ai_core::ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            tool_activities: Vec::new(),
            messages: Vec::new(),
            blocks: Vec::new(),
            pinned_documents: Vec::new(),
            feedback: Vec::new(),
            usage: Vec::new(),
            reasoning_control: None,
            attachments: Vec::new(),
        }
    }

    fn event(event: PublicAiEvent) -> Update {
        Update::Event(StreamEvent {
            version: AI_STREAM_VERSION,
            sequence: 1,
            event,
        })
    }

    fn send_question(surface: &mut ChatSurface) {
        assert!(surface.key(
            &WinitKey::Character("h".into()),
            Some("hello"),
            ModifiersState::empty(),
        ));
        assert!(surface.key(
            &WinitKey::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));
    }

    #[test]
    fn compact_token_counts_follow_the_badge_scale() {
        for (value, expected) in [
            (999, "999"),
            (1_200, "1.2k"),
            (2_000, "2k"),
            (35_000, "35k"),
            (1_200_000, "1.2M"),
        ] {
            assert_eq!(compact(value), expected);
        }
    }

    #[test]
    fn response_usage_line_selects_the_available_shape() {
        assert_eq!(
            usage_line(
                &usage(Some(1_200), Some(340), Some(210), Some(1_750)),
                Lang::En,
            )
            .as_deref(),
            Some("1.2k in · 340 out · 210 reasoning")
        );
        assert_eq!(
            usage_line(
                &usage(Some(2_000), Some(800), Some(0), Some(2_800)),
                Lang::En
            )
            .as_deref(),
            Some("2k in · 800 out")
        );
        assert_eq!(
            usage_line(&usage(None, None, None, Some(1_500)), Lang::En).as_deref(),
            Some("1.5k tokens")
        );
        assert_eq!(usage_line(&usage(None, None, None, None), Lang::En), None);
    }

    #[test]
    fn pending_turn_pulses_until_first_delta_or_failure() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        surface.reader = Some(reader);
        surface.transcript = Some(transcript());
        surface.validated_ready = true;
        surface.lang = Lang::En;
        send_question(&mut surface);
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Send(_)
        ));
        assert!(surface.animating());
        let width = surface.conversation_rect().w;
        surface.rebuild_messages(width);
        let pending = surface.message_layouts.last().unwrap();
        assert_eq!(
            message_content_height(pending),
            role::ui().metrics.line_height
        );

        let attempt_id = koru_ai_core::InferenceAttemptId::new();
        surface.apply_update(event(PublicAiEvent::TextDelta {
            attempt_id,
            text: "A".into(),
            at: Timestamp::from_millis(2),
        }));
        assert!(!surface.animating());

        let mut failed = ChatSurface::new(SurfaceMode::Standalone);
        let (reader, commands, _updates) = Reader::test_pair();
        failed.reader = Some(reader);
        failed.transcript = Some(transcript());
        failed.validated_ready = true;
        send_question(&mut failed);
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(50)).unwrap(),
            Command::Send(_)
        ));
        assert!(failed.animating());
        failed.apply_update(event(PublicAiEvent::Failed {
            attempt_id,
            error: AiError::new(AiErrorClass::Provider, "failed", true),
            at: Timestamp::from_millis(2),
        }));
        assert!(!failed.animating());
    }

    #[test]
    fn usage_event_labels_the_pending_response() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.lang = Lang::En;
        surface.transcript = Some(transcript());
        surface.pending_user = Some("Question".into());
        surface.draft = "Answer".into();
        let attempt_id = koru_ai_core::InferenceAttemptId::new();
        surface.apply_update(event(PublicAiEvent::Usage {
            attempt_id,
            usage: usage(Some(1_200), Some(340), None, Some(1_540)),
        }));
        let width = surface.conversation_rect().w;
        surface.rebuild_messages(width);

        let pending = surface.message_layouts.last().unwrap();
        assert_eq!(pending.state, MessageState::Pending);
        assert!(pending.usage.is_some());
    }

    #[test]
    fn transcript_usage_labels_historical_messages_and_grows_geometry() {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        surface.lang = Lang::En;
        let mut transcript = transcript();
        let message_id = MessageId::new();
        transcript.messages.push(Message {
            id: message_id,
            chat_id: transcript.chat.id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: "Answer".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: Timestamp::from_millis(1),
            completed_at: Some(Timestamp::from_millis(2)),
        });
        surface.transcript = Some(transcript);
        let width = surface.conversation_rect().w;
        surface.rebuild_messages(width);
        let without_usage = message_height(&surface.message_layouts[0]);
        assert!(surface.message_layouts[0].usage.is_none());

        surface
            .transcript
            .as_mut()
            .unwrap()
            .usage
            .push(MessageUsage {
                message_id,
                usage: usage(Some(2_000), Some(800), None, Some(2_800)),
            });
        surface.rebuild_messages(width);
        let message = &surface.message_layouts[0];
        assert!(message.usage.is_some());
        assert_eq!(
            message_height(message) - without_usage,
            role::hint().metrics.line_height + USAGE_GAP
        );
    }
}
