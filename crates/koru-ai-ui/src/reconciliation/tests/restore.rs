// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::{AiIntent, AiSurface, AiUsage, InferenceAttempt, ReasoningChoice};

fn restored() -> Running {
    let mut fixture = Running::new(true);
    let model = fixture.pending.chat.current_model.clone();
    fixture.pending.messages[1].markdown.clear();
    fixture.pending.blocks.clear();
    fixture.pending.tool_activities.clear();
    fixture.pending.active_attempt = Some(InferenceAttempt {
        id: fixture.attempt,
        operation_id: uuid::Uuid::new_v4(),
        chat_id: Some(fixture.pending.chat.id),
        assistant_message_id: Some(fixture.reply),
        surface: AiSurface::Standalone,
        intent: AiIntent::Chat,
        provider: model.provider,
        requested_model: model.requested_model,
        resolved_model: None,
        requested_reasoning: ReasoningChoice::Default,
        effective_reasoning: None,
        prompt_template_version: "test".into(),
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
        message_count: 0,
        context_document_count: 0,
        context_message_block_count: 0,
        usage: AiUsage::default(),
        context_used_tokens: None,
        context_limit_tokens: None,
        stop_reason: None,
        error_class: None,
        provider_response_id: None,
    });
    let mut surface = ChatSurface::new();
    surface.owned = fixture.surface.owned.clone();
    surface.set_viewport(Rect::new(0.0, 0.0, 640.0, 600.0));
    let (reader, commands, _) = Reader::test_pair();
    surface.reader = Some(reader);
    fixture.surface = surface;
    fixture.commands = commands;
    fixture
}

fn boot_settings() -> koru_ai_wire::ProviderSettingsResponse {
    koru_ai_wire::ProviderSettingsResponse {
        selected_provider: None,
        openai: koru_ai_wire::OpenAiSettingsView {
            model: None,
            reasoning: ReasoningChoice::Default,
            credential_configured: false,
        },
        ollama: koru_ai_wire::OllamaSettingsView {
            endpoint: "http://localhost:11434".into(),
            model: None,
            reasoning: ReasoningChoice::Default,
        },
        agents: vec![],
    }
}

#[test]
fn reopen_before_first_output_preserves_waiting_stop_and_model_changes_are_next_turn_only() {
    for boot in [false, true] {
        let mut f = restored();
        let update = if boot {
            Update::Boot {
                settings: boot_settings(),
                chats: vec![f.pending.chat.clone()],
                transcript: Some(f.pending.clone()),
                link_missing: false,
                destinations: vec![],
                destination_error: None,
            }
        } else {
            Update::Transcript(f.pending.clone())
        };
        assert!(f.surface.apply_update(update));
        assert_eq!(f.surface.attempt, Some(f.attempt));
        assert_eq!(f.surface.observed_attempt, Some(f.attempt));
        assert_eq!(f.surface.pending_assistant_message_id, Some(f.reply));
        assert!(
            !f.surface.reply_pending(),
            "the retained question/reply must not be duplicated optimistically"
        );
        assert_eq!(f.surface.send_mode(), crate::queue::SendMode::Stop);
        let width = f.surface.conversation_rect().w;
        f.surface.rebuild_messages(width);
        assert_eq!(
            f.surface
                .message_layouts
                .iter()
                .filter(|m| m.id == Some(f.reply))
                .count(),
            1
        );
        assert!(
            f.surface
                .work_layouts
                .iter()
                .any(|work| work.message == f.reply
                    && work.live
                    && work.header_text
                        == koru_i18n::t(f.surface.lang, koru_i18n::Key::AiActivityWaiting))
        );
        let accepted_model = f
            .pending
            .active_attempt
            .as_ref()
            .unwrap()
            .requested_model
            .clone();
        let mut next = f.pending.chat.clone();
        next.current_model.requested_model = "next-model".into();
        f.surface.apply_update(Update::ModelUpdated(next, None));
        assert!(f.surface.waiting && f.surface.is_working());
        assert_eq!(f.surface.attempt, Some(f.attempt));
        assert_eq!(f.surface.send_mode(), crate::queue::SendMode::Stop);
        assert!(
            f.surface.status.is_none(),
            "a model acknowledgement adds no permanent status"
        );
        assert_eq!(
            f.surface
                .transcript
                .as_ref()
                .unwrap()
                .active_attempt
                .as_ref()
                .unwrap()
                .requested_model,
            accepted_model
        );
        assert_eq!(
            f.surface
                .transcript
                .as_ref()
                .unwrap()
                .chat
                .current_model
                .requested_model,
            "next-model"
        );
        assert!(
            !f.surface
                .apply_update(Update::Transcript(f.pending.clone())),
            "unchanged pending read does not rebuild or resubscribe"
        );
        let observed = f.commands.try_iter().filter(|command| matches!(command, Command::ObserveAttempt(chat, id, _) if *chat == f.pending.chat.id && *id == f.attempt)).count();
        assert_eq!(observed, 1);
        assert!(f.surface.activate_send_button());
        assert!(
            f.commands
                .try_iter()
                .any(|command| matches!(command, Command::Cancel(id) if id == f.attempt))
        );
    }
}

#[test]
fn observed_progress_is_exact_preserves_next_model_and_terminal_recovery_stops_it() {
    let mut f = restored();
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    f.commands.try_iter().for_each(drop);
    let mut progress = f.pending.clone();
    progress.messages[1].markdown = "Working on the requested task".into();
    progress.blocks = koru_ai_core::finalize_markdown(f.reply, &progress.messages[1].markdown);
    progress.active_attempt.as_mut().unwrap().status = AttemptStatus::Streaming;
    let mut next = progress.chat.clone();
    next.current_model.requested_model = "next-model".into();
    f.surface.apply_update(Update::ModelUpdated(next, None));
    assert!(f.surface.apply_update(Update::Observed {
        attempt: f.attempt,
        transcript: progress.clone()
    }));
    assert_eq!(
        f.surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        "next-model"
    );
    let width = f.surface.conversation_rect().w;
    f.surface.rebuild_messages(width);
    assert!(
        f.surface
            .work_layouts
            .iter()
            .any(|work| work.message == f.reply
                && work.header_text
                    == koru_i18n::t(f.surface.lang, koru_i18n::Key::AiActivityTyping))
    );
    assert_eq!(
        f.surface
            .message_layouts
            .iter()
            .filter(|m| m.id == Some(f.reply))
            .count(),
        1
    );
    assert!(!f.surface.apply_update(Update::Observed {
        attempt: f.attempt,
        transcript: progress.clone()
    }));
    assert!(!f.surface.messages_dirty, "identical poll does not reflow");
    assert!(!f.surface.apply_update(Update::Observed {
        attempt: InferenceAttemptId::new(),
        transcript: progress.clone()
    }));
    let mut wrong = progress.clone();
    wrong.chat.id = ChatId::new();
    assert!(!f.surface.apply_update(Update::Observed {
        attempt: f.attempt,
        transcript: wrong
    }));
    f.surface.apply_update(Update::Recovered {
        attempt: f.attempt,
        status: AttemptStatus::Complete,
        transcript: f.complete.clone(),
    });
    assert_eq!(f.surface.attempt, None);
    assert_eq!(f.surface.observed_attempt, None);
    assert_eq!(
        f.surface
            .transcript
            .as_ref()
            .unwrap()
            .chat
            .current_model
            .requested_model,
        "next-model",
        "a terminal snapshot captured before the selection acknowledgement cannot undo it"
    );
    assert!(!f.surface.apply_update(Update::Observed {
        attempt: f.attempt,
        transcript: progress
    }));
    assert!(f.commands.try_recv().is_err());
}

#[test]
fn invalid_active_bindings_never_restore_stop_and_observations_cannot_replace_owned_stream() {
    for mismatch in 0..4 {
        let mut f = restored();
        let active = f.pending.active_attempt.as_mut().unwrap();
        match mismatch {
            0 => active.chat_id = Some(ChatId::new()),
            1 => active.assistant_message_id = Some(MessageId::new()),
            2 => active.status = AttemptStatus::Complete,
            _ => f.pending.messages[1].role = ChatRole::User,
        }
        f.surface
            .apply_update(Update::Transcript(f.pending.clone()));
        assert!(f.surface.attempt.is_none());
        assert!(
            !f.commands
                .try_iter()
                .any(|c| matches!(c, Command::ObserveAttempt(..)))
        );
    }
    let mut owned = Running::new(true);
    let draft = owned.surface.draft.clone();
    assert!(!owned.surface.apply_update(Update::Observed {
        attempt: owned.attempt,
        transcript: owned.pending.clone()
    }));
    assert_eq!(owned.surface.draft, draft);
    assert_eq!(owned.surface.attempt, Some(owned.attempt));
}

#[test]
fn full_reader_queue_retries_one_observation_after_capacity_returns() {
    let mut f = restored();
    while f
        .surface
        .reader
        .as_ref()
        .unwrap()
        .send(Command::Read(f.pending.chat.id))
        .is_ok()
    {}
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    assert!(f.surface.observe_retry_at.is_some());
    assert_eq!(f.surface.send_mode(), crate::queue::SendMode::Stop);
    f.commands.try_iter().for_each(drop);
    let retry = f.surface.observe_retry_at.unwrap();
    assert!(f.surface.maintain_observation(retry));
    assert!(f.surface.observe_retry_at.is_none());
    assert!(
        !f.surface
            .maintain_observation(retry + std::time::Duration::from_secs(2))
    );
    assert_eq!(
        f.commands
            .try_iter()
            .filter(|c| matches!(c, Command::ObserveAttempt(..)))
            .count(),
        1
    );
}

#[test]
fn local_new_chat_retires_only_its_observer_and_retains_a_full_queue_stop() {
    let mut f = restored();
    f.surface.owned = None;
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    f.commands.try_iter().for_each(drop);
    while f
        .surface
        .reader
        .as_ref()
        .unwrap()
        .send(Command::Read(f.pending.chat.id))
        .is_ok()
    {}
    let generation = f.surface.observation_generation;
    assert!(f.surface.start_new_chat());
    assert!(f.surface.observed_attempt.is_none());
    assert!(f.surface.attempt.is_none());
    assert_eq!(
        f.surface.pending_observation_stop,
        Some((f.pending.chat.id, f.attempt, generation))
    );
    f.commands.try_iter().for_each(drop);
    let retry = f.surface.observe_retry_at.unwrap();
    assert!(f.surface.maintain_observation(retry));
    assert!(f.surface.pending_observation_stop.is_none());
    let commands: Vec<_> = f.commands.try_iter().collect();
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(commands[0], Command::StopObserving(chat, attempt, _) if chat == f.pending.chat.id && attempt == f.attempt)
    );
    assert!(!f.surface.apply_update(Update::Observed {
        attempt: f.attempt,
        transcript: f.pending.clone()
    }));
}

#[test]
fn returning_to_same_chat_flushes_retired_observer_before_starting_the_new_one() {
    let mut f = restored();
    f.surface.owned = None;
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    f.commands.try_iter().for_each(drop);
    while f
        .surface
        .reader
        .as_ref()
        .unwrap()
        .send(Command::Read(f.pending.chat.id))
        .is_ok()
    {}
    f.surface.start_new_chat();
    f.commands.try_iter().for_each(drop);
    assert!(f.surface.open_link(f.pending.chat.id, None));
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    let commands: Vec<_> = f.commands.try_iter().collect();
    assert!(matches!(commands[0], Command::Read(_)));
    assert!(
        matches!(commands[1], Command::StopObserving(chat, attempt, _) if chat == f.pending.chat.id && attempt == f.attempt)
    );
    assert!(
        matches!(commands[2], Command::ObserveAttempt(chat, attempt, _) if chat == f.pending.chat.id && attempt == f.attempt)
    );
    assert_eq!(commands.len(), 3);
    assert_eq!(f.surface.attempt, Some(f.attempt));
    assert!(f.surface.observe_retry_at.is_none());
}

#[test]
fn delayed_failure_from_a_retired_observer_cannot_clear_a_new_same_attempt_observer() {
    let mut f = restored();
    f.surface.owned = None;
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    let old_generation = f.surface.observation_generation;
    f.commands.try_iter().for_each(drop);
    f.surface.start_new_chat();
    f.surface.open_link(f.pending.chat.id, None);
    f.surface
        .apply_update(Update::Transcript(f.pending.clone()));
    let current_generation = f.surface.observation_generation;
    assert!(current_generation > old_generation);
    let attempt = f.attempt;
    let failure = || Update::RecoveryFailed {
        attempt,
        failure: koru_ai_wire::Failure {
            code: "not_found".into(),
            message: "retired reader".into(),
            retryable: false,
            details: Default::default(),
        },
    };
    assert!(!f.surface.apply_update(Update::Observation {
        generation: old_generation,
        attempt,
        update: Box::new(failure())
    }));
    assert_eq!(f.surface.attempt, Some(attempt));
    assert_eq!(f.surface.send_mode(), crate::queue::SendMode::Stop);
    assert!(f.surface.apply_update(Update::Observation {
        generation: current_generation,
        attempt,
        update: Box::new(failure())
    }));
    assert!(
        f.surface.attempt.is_none(),
        "a current authoritative failure still settles Stop"
    );
}
