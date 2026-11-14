// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn refinement_snapshot_is_frozen_for_request_and_retry() {
    let (mut surface, _, accepted, _) = fixture();
    let target = koru_ai_core::mini_apps::RefinementTarget {
        tab_id: 9,
        surface_id: uuid::Uuid::new_v4(),
        base_revision: 2,
    };
    let mut declaration =
        Declaration::parse(koru_ai_core::mini_apps::composition::FIXTURE.as_bytes())
            .unwrap()
            .envelope;
    declaration.surface.surface_id = target.surface_name();
    let context = RefinementContext {
        target,
        declaration,
    };
    surface.set_mini_app_refinement_context(Some(context.clone()));
    let chat = surface.current_chat_id().unwrap();
    let host = surface.begin_mini_app_request(chat).unwrap();
    assert_eq!(
        surface.mini_app_request_refinement(Some(host)),
        Some(context.clone())
    );
    surface.accept_mini_app_attempt(&accepted, host);
    let retry = surface
        .retry_mini_app_request(chat, accepted.assistant_message_id)
        .unwrap();
    assert_ne!(retry.request_id, host.request_id);
    assert_eq!(
        surface.mini_app_request_refinement(Some(retry)),
        Some(context)
    );
    surface.accept_mini_app_attempt(&accepted, retry);
    surface.set_mini_app_refinement_target(Some(koru_ai_core::mini_apps::RefinementTarget {
        base_revision: 3,
        ..target
    }));
    assert!(!surface.can_retry_mini_app_request(chat, accepted.assistant_message_id));
}

#[test]
fn mini_app_managed_chat_offers_host_context_without_changing_ordinary_chat() {
    for provider in [
        "agent-codex",
        "agent-claude",
        "agent-opencode",
        "agent-kimi",
        "agent-grok",
        "agent-antigravity",
        "agent-devin",
    ] {
        let (mut surface, host, accepted, _) = fixture();
        surface.settings = Some(managed_settings(provider));
        surface
            .transcript
            .as_mut()
            .unwrap()
            .chat
            .current_model
            .provider = koru_ai_core::AiProviderId::parse(provider).unwrap();
        assert!(surface.mini_app_available());
        let next = surface
            .begin_mini_app_request(surface.transcript.as_ref().unwrap().chat.id)
            .unwrap();
        assert_eq!(next.workspace_id, host.workspace_id);
        assert_ne!(next.request_id, host.request_id);
        surface.accept_mini_app_attempt(&accepted, next);
        stream(
            &mut surface,
            PublicAiEvent::Notice {
                attempt_id: accepted.attempt_id,
                notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
                at: koru_ai_core::Timestamp::now(),
            },
        );
        assert!(surface.native_apps.unavailable);
        assert!(surface.take_mini_app_creation().is_none());
    }
}

fn managed_settings(provider: &str) -> koru_ai_wire::ProviderSettingsResponse {
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
        agents: vec![koru_ai_wire::AgentProviderView {
            id: AiProviderId::parse(provider).unwrap(),
            label: provider.into(),
            steers: false,
        }],
    }
}

#[test]
fn mini_app_managed_scope_requires_the_services_catalog() {
    let (mut surface, _, _, _) = fixture();
    let chat = surface.current_chat_id().unwrap();
    surface
        .transcript
        .as_mut()
        .unwrap()
        .chat
        .current_model
        .provider = AiProviderId::parse("agent-grok").unwrap();
    // Settings have not arrived yet.
    assert!(surface.settings.is_none());
    assert!(surface.begin_mini_app_request(chat).is_none());
    surface.settings = Some(managed_settings("agent-claude"));
    assert!(surface.begin_mini_app_request(chat).is_none());
    surface
        .transcript
        .as_mut()
        .unwrap()
        .chat
        .current_model
        .provider = AiProviderId::parse("agent-nobody").unwrap();
    assert!(surface.begin_mini_app_request(chat).is_none());
    assert!(surface.native_apps.pending.is_none());
}

fn refinement_fixture() -> (
    ChatSurface,
    HostContext,
    koru_ai_wire::SendTurnAccepted,
    PublicAiEvent,
) {
    let (mut surface, mut host, accepted, created) = fixture();
    let target = koru_ai_core::mini_apps::RefinementTarget {
        tab_id: 9,
        surface_id: uuid::Uuid::new_v4(),
        base_revision: 4,
    };
    surface.set_mini_app_refinement_target(Some(target));
    host.refinement = Some(target);
    surface.native_apps.pending.as_mut().unwrap().host = host;
    let PublicAiEvent::MiniAppCreated {
        attempt_id,
        chat_id,
        profile_id,
        invocation_id,
        call_id,
        ..
    } = created
    else {
        panic!()
    };
    let mut root = Declaration::parse(koru_ai_core::mini_apps::FIXTURE.as_bytes())
        .unwrap()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some("document-first".into());
    let envelope = koru_ai_core::mini_apps::UpdateEnvelope {
        version: "1.0".into(),
        update: koru_ai_core::mini_apps::ComponentUpdate {
            surface_id: target.surface_name(),
            components: vec![root],
        },
    };
    (
        surface,
        host,
        accepted,
        PublicAiEvent::MiniAppUpdated {
            attempt_id,
            chat_id,
            profile_id,
            invocation_id,
            call_id,
            host,
            envelope,
        },
    )
}

#[test]
fn mini_app_refinement_completed_same_batch_is_delivered_once_and_replay_cannot_command() {
    let (mut surface, host, accepted, event) = refinement_fixture();
    stream(&mut surface, event.clone());
    stream(
        &mut surface,
        PublicAiEvent::Completed {
            attempt_id: accepted.attempt_id,
            stop_reason: None,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    assert!(surface.take_mini_app_refinement().is_some());
    surface.mini_app_refined(host.request_id, true);
    stream(&mut surface, event);
    assert!(surface.take_mini_app_refinement().is_none());
    assert!(surface.take_mini_app_creation().is_none());
    let (mut surface, host, mut accepted, event) = refinement_fixture();
    stream(&mut surface, event);
    accepted.replayed = true;
    surface.accept_mini_app_attempt(&accepted, host);
    assert!(surface.take_mini_app_refinement().is_none());
}

#[test]
fn mini_app_creation_from_existing_app_clears_only_owned_retirement_status_on_open() {
    for unrelated in [false, true] {
        let (mut surface, mut host, _, mut event) = fixture();
        let old = koru_ai_core::mini_apps::RefinementTarget {
            tab_id: 1,
            surface_id: uuid::Uuid::new_v4(),
            base_revision: 0,
        };
        surface.set_mini_app_refinement_target(Some(old));
        host.refinement = Some(old);
        surface.native_apps.pending.as_mut().unwrap().host = host;
        let PublicAiEvent::MiniAppCreated {
            host: event_host, ..
        } = &mut event
        else {
            panic!()
        };
        *event_host = host;
        stream(&mut surface, event);
        assert!(surface.take_mini_app_creation().is_some());
        surface.set_mini_app_refinement_target(Some(koru_ai_core::mini_apps::RefinementTarget {
            tab_id: 2,
            surface_id: uuid::Uuid::new_v4(),
            base_revision: 0,
        }));
        assert_eq!(
            surface.status.as_deref(),
            Some(koru_i18n::t(
                surface.lang,
                koru_i18n::Key::MiniAppRefinementRefused
            ))
        );
        if unrelated {
            surface.status = Some("Other status".into());
        }
        surface.mini_app_opened(host.request_id, true);
        assert_eq!(
            surface.status.as_deref(),
            unrelated.then_some("Other status")
        );
        assert!(!surface.native_apps.refinement_refused);
        assert!(!surface.native_apps.interrupted);
    }
}

#[test]
fn mini_app_refinement_target_hide_switch_and_wrong_scope_retire_delivery() {
    for change in 0..4 {
        let (mut surface, host, _, event) = refinement_fixture();
        match change {
            0 => surface.offer_mini_apps(false),
            1 => surface.set_mini_app_refinement_target(None),
            2 => surface.set_mini_app_refinement_target(Some(
                koru_ai_core::mini_apps::RefinementTarget {
                    surface_id: uuid::Uuid::new_v4(),
                    ..host.refinement.unwrap()
                },
            )),
            _ => surface.context_workspace = Some(WorkspaceId::new()),
        }
        stream(&mut surface, event);
        assert!(surface.take_mini_app_refinement().is_none());
    }
}

#[test]
fn mini_app_failed_refinement_retry_never_recaptures_a_different_target_or_revision() {
    for changed_revision in [false, true] {
        let (mut surface, host, accepted, _) = refinement_fixture();
        let (reader, commands, _updates) = crate::reader::Reader::test_pair();
        surface.reader = Some(reader);
        stream(
            &mut surface,
            PublicAiEvent::Failed {
                attempt_id: accepted.attempt_id,
                error: koru_ai_core::AiError::new(
                    koru_ai_core::AiErrorClass::Provider,
                    "fixture",
                    true,
                ),
                at: koru_ai_core::Timestamp::now(),
            },
        );
        let mut different = host.refinement.unwrap();
        if changed_revision {
            different.base_revision += 1;
        } else {
            different.surface_id = uuid::Uuid::new_v4();
        }
        surface.set_mini_app_refinement_target(Some(different));
        assert!(
            surface.perform_response_action(ResponseAction::Retry(accepted.assistant_message_id))
        );
        assert!(
            !commands
                .try_iter()
                .any(|command| matches!(command, Command::Retry(..)))
        );
        assert_eq!(surface.native_apps.pending.as_ref().unwrap().host, host);
        assert!(surface.native_apps.refinement_refused);
    }
}
#[test]
fn a_transcript_cannot_create_a_native_app() {
    let mut surface = ChatSurface::new();
    assert!(surface.take_mini_app_creation().is_none());
    assert!(!surface.mini_app_available());
    surface.offer_mini_apps(true);
    assert!(!surface.mini_app_available());
}

fn fixture() -> (
    ChatSurface,
    HostContext,
    koru_ai_wire::SendTurnAccepted,
    PublicAiEvent,
) {
    let mut surface = ChatSurface::new();
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let chat = ChatId::new();
    let attempt = InferenceAttemptId::new();
    let host = surface.arm_mini_app_for_test(profile, workspace, chat, attempt);
    let accepted = koru_ai_wire::SendTurnAccepted {
        attempt_id: attempt,
        assistant_message_id: surface
            .native_apps
            .pending
            .as_ref()
            .unwrap()
            .message
            .unwrap(),
        user_message_id: MessageId::new(),
        replayed: false,
    };
    let invocation = koru_ai_core::ToolInvocationId::new();
    let envelope = Declaration::parse(koru_ai_core::mini_apps::FIXTURE.as_bytes())
        .unwrap()
        .normalize(invocation)
        .envelope;
    let event = PublicAiEvent::MiniAppCreated {
        attempt_id: attempt,
        chat_id: chat,
        profile_id: profile,
        invocation_id: invocation,
        call_id: "c1".into(),
        host,
        envelope,
    };
    (surface, host, accepted, event)
}
fn stream(surface: &mut ChatSurface, event: PublicAiEvent) {
    surface.apply_update(Update::Event(koru_ai_wire::StreamEvent {
        version: koru_ai_wire::AI_STREAM_VERSION,
        sequence: 1,
        event,
    }));
}
#[test]
fn mini_app_completed_same_batch_survives_until_host_acceptance_and_never_reopens() {
    let (mut surface, host, accepted, event) = fixture();
    stream(&mut surface, event.clone());
    stream(
        &mut surface,
        PublicAiEvent::Completed {
            attempt_id: accepted.attempt_id,
            stop_reason: None,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    assert!(surface.take_mini_app_creation().is_some());
    surface.mini_app_opened(host.request_id, true);
    surface.offer_mini_apps(false);
    surface.offer_mini_apps(true);
    assert!(
        surface
            .retry_mini_app_request(
                surface.current_chat_id().unwrap(),
                accepted.assistant_message_id
            )
            .is_none()
    );
    stream(&mut surface, event);
    assert!(surface.take_mini_app_creation().is_none());
}
#[test]
fn mini_app_hidden_or_stopped_before_acceptance_cannot_be_resurrected() {
    let (mut surface, host, accepted, event) = fixture();
    surface.native_apps.pending.as_mut().unwrap().attempt = None;
    surface.native_apps.pending.as_mut().unwrap().message = None;
    surface.offer_mini_apps(false);
    surface.accept_mini_app_attempt(&accepted, host);
    surface.offer_mini_apps(true);
    stream(&mut surface, event.clone());
    assert!(surface.take_mini_app_creation().is_none());
    let retry = surface
        .retry_mini_app_request(
            surface.current_chat_id().unwrap(),
            accepted.assistant_message_id,
        )
        .unwrap();
    assert_ne!(retry.request_id, host.request_id);
    surface.accept_mini_app_attempt(&accepted, host);
    stream(&mut surface, event);
    assert!(surface.take_mini_app_creation().is_none());
}
#[test]
fn mini_app_interruption_and_fallback_remain_visible_through_recovery() {
    let (mut surface, _, accepted, _) = fixture();
    let transcript = surface.transcript.clone().unwrap();
    surface.apply_update(Update::RecoveryStarted(accepted.attempt_id));
    surface.apply_update(Update::Recovered {
        attempt: accepted.attempt_id,
        status: koru_ai_core::AttemptStatus::Complete,
        transcript,
    });
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(
            surface.lang,
            koru_i18n::Key::MiniAppGenerationInterrupted
        ))
    );
    let (mut surface, _, accepted, _) = fixture();
    stream(
        &mut surface,
        PublicAiEvent::Notice {
            attempt_id: accepted.attempt_id,
            notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    stream(
        &mut surface,
        PublicAiEvent::Completed {
            attempt_id: accepted.attempt_id,
            stop_reason: None,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    surface.apply_update(Update::Transcript(surface.transcript.clone().unwrap()));
    assert_eq!(
        surface.status.as_deref(),
        Some(koru_i18n::t(
            surface.lang,
            koru_i18n::Key::MiniAppToolsUnavailable
        ))
    );
    let (mut surface, _, accepted, _) = fixture();
    assert_eq!(surface.attempt, Some(accepted.attempt_id));
    surface.set_context_workspace(Some(WorkspaceId::new()));
    surface.status = None;
    stream(
        &mut surface,
        PublicAiEvent::Notice {
            attempt_id: accepted.attempt_id,
            notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    assert!(surface.status.is_none());
}

#[test]
fn mini_app_scope_replay_and_post_application_notices_cannot_reopen_or_erase_success() {
    let (mut surface, host, mut accepted, event) = fixture();
    stream(&mut surface, event.clone());
    assert!(surface.take_mini_app_creation().is_some());
    surface.mini_app_opened(host.request_id, true);
    stream(
        &mut surface,
        PublicAiEvent::Notice {
            attempt_id: accepted.attempt_id,
            notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    assert!(!surface.native_apps.unavailable);
    let (mut surface, _, _, event) = fixture();
    surface.set_context_workspace(Some(WorkspaceId::new()));
    stream(&mut surface, event);
    assert!(surface.take_mini_app_creation().is_none());
    let (mut surface, host, _, event) = fixture();
    surface.native_apps.pending.as_mut().unwrap().attempt = None;
    accepted.replayed = true;
    surface.accept_mini_app_attempt(&accepted, host);
    stream(&mut surface, event);
    assert!(surface.take_mini_app_creation().is_none());
}

#[test]
fn mini_app_two_real_retry_actions_follow_distinct_assistant_acceptances() {
    let (mut surface, _, mut accepted, _) = fixture();
    assert_ne!(accepted.user_message_id, accepted.assistant_message_id);
    let (reader, commands, updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    for _ in 0..2 {
        stream(
            &mut surface,
            PublicAiEvent::Failed {
                attempt_id: accepted.attempt_id,
                error: koru_ai_core::AiError::new(
                    koru_ai_core::AiErrorClass::Provider,
                    "fixture failure",
                    true,
                ),
                at: koru_ai_core::Timestamp::now(),
            },
        );
        let prior = accepted.assistant_message_id;
        assert!(surface.perform_response_action(ResponseAction::Retry(prior)));
        let request = commands
            .try_iter()
            .find_map(|command| {
                if let Command::Retry(_, request) = command {
                    Some(request)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(request.message_id, prior);
        let host = request
            .mini_app_host
            .expect("a never-applied assistant response retains native retry authority");
        assert!(
            surface
                .native_apps
                .pending
                .as_ref()
                .unwrap()
                .message
                .is_none()
        );
        accepted = koru_ai_wire::SendTurnAccepted {
            attempt_id: InferenceAttemptId::new(),
            user_message_id: accepted.user_message_id,
            assistant_message_id: MessageId::new(),
            replayed: false,
        };
        assert_ne!(accepted.assistant_message_id, prior);
        updates
            .send(Update::MiniAppAccepted(accepted.clone(), host))
            .unwrap();
        updates.send(Update::Accepted(accepted.clone())).unwrap();
        surface.drain_updates();
        assert_eq!(
            surface.native_apps.pending.as_ref().unwrap().message,
            Some(accepted.assistant_message_id)
        );
    }
}

#[test]
fn mini_app_ordinary_completed_chat_hides_without_an_opening_warning() {
    let (mut surface, _, accepted, _) = fixture();
    stream(
        &mut surface,
        PublicAiEvent::Completed {
            attempt_id: accepted.attempt_id,
            stop_reason: None,
            at: koru_ai_core::Timestamp::now(),
        },
    );
    surface.status = None;
    surface.offer_mini_apps(false);
    surface.offer_mini_apps(true);
    surface.refresh_mini_app_status();
    assert!(!surface.native_apps.interrupted);
    assert!(surface.status.is_none());
    assert!(surface.take_mini_app_creation().is_none());
}
