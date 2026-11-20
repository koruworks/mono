// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::{AiProviderId, AiRequestOptions, InferenceAttemptId, ReasoningChoice};
use koru_delegate_preamble::{Notice, Source as ResolvedSource};

fn request(provider: &str) -> AiRequest {
    AiRequest {
        attempt_id: InferenceAttemptId::new(),
        provider: AiProviderId::parse(provider).unwrap(),
        model: "model".into(),
        reasoning: ReasoningChoice::Default,
        prompt_template_version: super::super::PROMPT_TEMPLATE_VERSION.into(),
        system: super::super::CHAT_PROMPT.into(),
        messages: vec![],
        context: vec![],
        tools: vec![],
        tool_history: vec![],
        options: AiRequestOptions::default(),
    }
}

fn snapshot(text: &str) -> Resolved {
    Resolved {
        text: text.to_owned().into(),
        source: ResolvedSource::Edited,
        notice: None,
        path: Some("private/settings/prompts/session.md".into()),
    }
}

#[test]
fn ordinary_api_composes_its_mode_but_managed_capture_belongs_to_the_provider() {
    let mut api = request("openai");
    let snapshot = apply(&mut api, || snapshot("CUSTOM-ORIENTATION")).unwrap();
    assert!(api.system.starts_with("CUSTOM-ORIENTATION\n\n"));
    assert!(api.system.contains("No application tools are offered"));
    assert!(!api.system.contains("Never claim to have changed"));
    assert!(api.system.contains("only when its result confirms"));
    assert!(snapshot.path.is_some());

    let mut managed = request("agent-claude");
    let mode = managed.system.clone();
    assert!(apply(&mut managed, || panic!("provider owns first input")).is_none());
    assert_eq!(managed.system, mode);
}

#[test]
fn debug_override_never_reads_or_prepends_ordinary_instructions() {
    let mut debug = request("openai");
    debug.prompt_template_version = "debug-override-v1".into();
    debug.system = "DEBUG-REPLACEMENT".into();
    assert!(apply(&mut debug, || panic!("override must not read a source")).is_none());
    assert_eq!(debug.system, "DEBUG-REPLACEMENT");
}

#[test]
fn fallback_metadata_survives_composition_without_leaking_paths_into_instructions() {
    let mut request = request("openai");
    let mut fallback = session::bundled();
    fallback.notice = Some(Notice::Unreadable);
    let resolved = apply(&mut request, || fallback).unwrap();
    assert_eq!(resolved.notice, Some(Notice::Unreadable));
    assert_eq!(resolved.source, ResolvedSource::Bundled);
    assert!(!request.system.contains("private/settings"));
}
