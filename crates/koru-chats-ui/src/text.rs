// SPDX-License-Identifier: MPL-2.0

//! Every word this surface says, and the one place each is assembled.
//!
//! - **Nothing is a literal** a person reads in a language: every sentence
//!   comes from `koru-i18n`'s table, so it exists in all eight or it does not
//!   compile. The two exceptions are names, not words — an agent's own name
//!   and a model's id are the same in every language.
//! - **Every time goes through `koru_i18n::time`**, so this list reads the
//!   way every other list in the app does.
//! - **A refusal's code is never drawn.** It is stable so this table can be
//!   written against it and a log can carry it; what a person reads is what
//!   to do about it.

use koru_ai_core::{AGENT_DEFAULT_MODEL, AiProviderId, Chat, ModelSelection};
use koru_ai_wire::AgentProviderView;
use koru_i18n::strings::{Key, t};
use koru_i18n::{Lang, fill, time};

/// Whether a provider is a local agent's (`agent-<kind>`).
pub fn is_agent(provider: &AiProviderId) -> bool {
    provider.as_str().starts_with("agent-")
}

/// **Who answered a chat**, in the chip's own vocabulary (AGENTUX §9: *who
/// answers* is the chip).
///
/// - A local agent by the name a person knows it by, and the model it was
///   given when one was picked rather than the agent's own default —
///   *OpenCode · deepseek/v4-pro*. An agent this device no longer lists is
///   still *a local agent*, never a token.
/// - OpenAI and Ollama by the model, which is what a person chose.
/// - Koru by the hosted model's label.
pub fn who_answered(lang: Lang, model: &ModelSelection, agents: &[AgentProviderView]) -> String {
    let provider = &model.provider;
    let requested = model.requested_model.trim();
    if is_agent(provider) {
        let name = agents
            .iter()
            .find(|agent| &agent.id == provider)
            .map(|agent| agent.label.as_str())
            .unwrap_or_else(|| t(lang, Key::ChatsLocalAgent));
        if requested.is_empty() || requested == AGENT_DEFAULT_MODEL {
            return name.to_owned();
        }
        return format!("{name} · {requested}");
    }
    match provider.as_str() {
        AiProviderId::KORU => AiProviderId::KORU_HOSTED_MODEL_LABEL.to_owned(),
        AiProviderId::OPENAI if requested.is_empty() => "OpenAI".to_owned(),
        AiProviderId::OLLAMA if requested.is_empty() => "Ollama".to_owned(),
        _ if requested.is_empty() => provider.as_str().to_owned(),
        _ => requested.to_owned(),
    }
}

/// How long ago, in the person's language.
pub fn when(lang: Lang, then_ms: i64, now_ms: i64) -> String {
    time::relative_long(lang, then_ms, now_ms)
}

/// A chat's title as a row draws it. An empty title is *Untitled chat*,
/// never a blank row; a chat still carrying the placeholder a new chat is
/// made with (`koru_ai_core::NEW_CHAT_TITLE`, stored in English until its
/// first message names it) reads *New chat* in the reader's language.
pub fn title(lang: Lang, chat: &Chat) -> &str {
    let title = chat.title.trim();
    if title.is_empty() {
        t(lang, Key::ChatsUntitled)
    } else if title == koru_ai_core::NEW_CHAT_TITLE {
        t(lang, Key::ChatsNew)
    } else {
        title
    }
}

/// A chat row's second line: who answered it, and when it last moved.
///
/// The separator is punctuation, not a word, and lives here for the reason
/// the Artifacts rows keep theirs in code.
pub fn meta_line(lang: Lang, chat: &Chat, agents: &[AgentProviderView], now_ms: i64) -> String {
    format!(
        "{} · {}",
        who_answered(lang, &chat.current_model, agents),
        when(lang, chat.updated_at.millis(), now_ms)
    )
}

/// A row, as a screen reader names it: title, who answered and time, and
/// that it is private when it is.
pub fn accessible_name(
    lang: Lang,
    chat: &Chat,
    agents: &[AgentProviderView],
    now_ms: i64,
) -> String {
    let key = if chat.private {
        Key::ChatsRowNamePrivate
    } else {
        Key::ChatsRowName
    };
    fill(
        t(lang, key),
        &[
            title(lang, chat),
            &who_answered(lang, &chat.current_model, agents),
            &when(lang, chat.updated_at.millis(), now_ms),
        ],
    )
}

/// Why a delete was refused, as a sentence. A code with no sentence of its
/// own still gets one, because a row that simply stays teaches nothing.
pub fn refusal(lang: Lang, code: &str) -> &'static str {
    let key = match code {
        // **UC13.** Koru could not stop the chat's agent, so the chat stays.
        "agent_still_running" | "agent_stop_failed" => Key::ChatsRefusedAgent,
        "history_clearing" => Key::ChatsRefusedClearing,
        "ai_attempt_active" | "chat_busy" => Key::ChatsRefusedBusy,
        code if unavailable(code) => Key::ChatsRefusedUnavailable,
        _ => Key::ChatsRefusedOther,
    };
    t(lang, key)
}

/// Whether a failure means the service is not answering at all, rather than
/// this one request being refused.
pub fn unavailable(code: &str) -> bool {
    matches!(
        code,
        "gateway_unavailable"
            | "ai_unavailable"
            | "gateway_failure"
            | "unauthenticated"
            | "invalid_response"
    )
}
