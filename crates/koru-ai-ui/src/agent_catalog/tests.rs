// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_agent_config_ui::effort::Level;
use koru_ai_core::{ReasoningControl, ReasoningLevel};
use koru_ai_wire::{
    AgentModeView, AgentModelContext, AgentModelFast, AgentModelView, AgentProviderView,
    ProviderValidationStatus,
};

fn agent(id: &str) -> AiProviderId {
    AiProviderId::parse(id).unwrap()
}

fn settings(agents: &[(&str, &str)]) -> ProviderSettingsResponse {
    ProviderSettingsResponse {
        selected_provider: None,
        openai: koru_ai_wire::OpenAiSettingsView {
            model: None,
            reasoning: ReasoningChoice::Default,
            credential_configured: true,
        },
        ollama: koru_ai_wire::OllamaSettingsView {
            endpoint: "http://localhost:11434".into(),
            model: None,
            reasoning: ReasoningChoice::Default,
        },
        agents: agents
            .iter()
            .map(|(id, label)| AgentProviderView {
                id: agent(id),
                label: (*label).into(),
                steers: false,
            })
            .collect(),
    }
}

fn model(id: &str) -> AgentModelView {
    AgentModelView {
        id: id.into(),
        group: "anthropic".into(),
        name: None,
        description: None,
        group_name: None,
        legacy: false,
        reasoning: None,
        context: None,
        fast: None,
    }
}

/// **Agent runtimes only, in the service's order** — an API-access provider
/// the settings carry is never offered to a Bot.
#[test]
fn the_catalog_lists_the_agent_runtimes_and_nothing_else() {
    let mut settings = settings(&[("agent-codex", "Codex"), ("agent-claude", "Claude")]);
    settings.agents.push(AgentProviderView {
        id: agent("openai"),
        label: "OpenAI".into(),
        steers: false,
    });
    let agents = agents_from(&settings);
    assert_eq!(
        agents
            .iter()
            .map(|agent| (
                agent.id.as_str(),
                agent.key.as_deref(),
                agent.label.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("agent-codex", Some("codex"), "Codex"),
            ("agent-claude", Some("claude"), "Claude")
        ]
    );
}

/// **A listing, owned and handed back borrowed**: the models with their
/// levels, Fast and longer window; the modes and the agent's own start.
#[test]
fn a_listing_converts_once_and_feeds_the_control() {
    let mut opus = model("opus");
    opus.name = Some("Opus".into());
    opus.reasoning = Some(ReasoningControl::Levels {
        values: vec![ReasoningLevel::Low, ReasoningLevel::High],
        default: Some(ReasoningLevel::High),
        supports_off: false,
    });
    opus.context = Some(AgentModelContext {
        id: "opus[1m]".into(),
        tokens: 1_000_000,
    });
    opus.fast = Some(AgentModelFast {
        on_by_default: true,
    });
    let response = AgentModelsResponse {
        provider: agent("agent-claude"),
        models: vec![model("default"), opus],
        modes: vec![AgentModeView {
            id: "default".into(),
            name: Some("Ask".into()),
            description: None,
        }],
        default_mode: Some("default".into()),
        levels_per_turn: false,
        models_per_turn: false,
    };
    let listing = listing_from(&response);
    assert_eq!(listing.records().len(), 2);
    assert_eq!(listing.records()[1].context, Some("opus[1m]"));
    assert_eq!(listing.offers()[0].id, "default");
    assert_eq!(listing.default_mode.as_deref(), Some("default"));
    // The footer: the model's levels, Fast as the draft says else its own
    // default, and the window on while the longer id is the one chosen.
    let footer = listing.footer("opus", None);
    assert_eq!(
        footer.levels,
        Some(Levels::Levels {
            values: vec![Level::Low, Level::High],
            default: Some(Level::High),
            supports_off: false,
        })
    );
    assert_eq!(footer.fast, Some(true));
    assert_eq!(footer.context.map(|context| context.long), Some(false));
    let long = listing.footer("opus[1m]", Some(false));
    assert_eq!(long.fast, Some(false));
    assert_eq!(long.context.map(|context| context.long), Some(true));
    // A model the listing does not hold offers nothing.
    let none = listing.footer("haiku", None);
    assert!(none.levels.is_none() && none.fast.is_none() && none.context.is_none());
}

/// Readiness reads the validation's stable code, as the chat's picker does.
#[test]
fn readiness_is_the_chats_reading_of_the_validation() {
    let validation = |status, code: Option<&str>| ProviderValidationResponse {
        provider: agent("agent-codex"),
        status,
        models: Vec::new(),
        selected_model: None,
        failure: code.map(|code| koru_ai_wire::Failure {
            code: code.into(),
            message: "English the person never reads".into(),
            retryable: false,
            details: Default::default(),
        }),
    };
    assert_eq!(
        readiness_from(&validation(ProviderValidationStatus::Ready, None)),
        Readiness::Ready
    );
    assert_eq!(
        readiness_from(&validation(
            ProviderValidationStatus::Unavailable,
            Some("agent_not_installed")
        )),
        Readiness::NotInstalled
    );
    assert_eq!(
        readiness_from(&validation(
            ProviderValidationStatus::Unavailable,
            Some("agent_signed_out")
        )),
        Readiness::SignedOut
    );
}

/// A level crosses both ways unchanged.
#[test]
fn a_level_crosses_both_ways() {
    for choice in [
        ReasoningChoice::Default,
        ReasoningChoice::Off,
        ReasoningChoice::Level(ReasoningLevel::High),
    ] {
        assert_eq!(reasoning(effort(choice)), choice);
    }
}

/// **A listing's failure, as a host hands it in**: the host still building
/// the list is asked again, a refusal keeps its code, and a failure with no
/// code is nothing answering. The module makes no call itself.
#[test]
fn a_hosts_listing_failure_reads_by_its_code() {
    let failure = |code: &str| koru_ai_wire::Failure {
        code: code.to_owned(),
        message: String::new(),
        retryable: false,
        details: Default::default(),
    };
    assert!(is_still_listing(&failure("agent_models_listing")));
    assert_eq!(
        listing_failure(&failure("agent_models_listing")),
        ListingFailure::StillListing
    );
    assert_eq!(listing_failure(&failure("")), ListingFailure::Unreachable);
    assert_eq!(
        listing_failure(&failure("provider_unavailable")),
        ListingFailure::Refused("provider_unavailable".into())
    );
}
