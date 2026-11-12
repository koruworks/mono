// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ReasoningLevel;
use koru_ai_wire::{AgentModeView, AgentModelView, AgentModelsResponse};

pub(in crate::worker) fn catalog(model: &str) -> AgentModelsResponse {
    AgentModelsResponse {
        provider: AiProviderId::parse("agent-codex").unwrap(),
        models: vec![AgentModelView {
            id: model.into(),
            group: "fixture".into(),
            name: None,
            description: None,
            group_name: None,
            legacy: false,
            reasoning: Some(ReasoningControl::Levels {
                values: vec![ReasoningLevel::Low, ReasoningLevel::High],
                default: Some(ReasoningLevel::Low),
                supports_off: false,
            }),
            context: None,
            fast: Some(koru_ai_wire::AgentModelFast {
                on_by_default: false,
            }),
        }],
        modes: vec![AgentModeView {
            id: "safe".into(),
            name: None,
            description: None,
        }],
        default_mode: Some("safe".into()),
        levels_per_turn: true,
        models_per_turn: true,
    }
}
fn config() -> BotConfig {
    let (_, _, command) = crate::worker::bots::tests::fixture();
    let BotCommand::Create { config, .. } = command else {
        unreachable!()
    };
    config
}

#[test]
fn catalog_validation_keeps_default_and_explicit_effort_and_refuses_unsupported_values() {
    let mut config = config();
    let mut catalog = catalog(&config.model.requested_model);
    let expected = catalog.models[0].reasoning.clone();
    assert_eq!(
        validate_model_catalog(&config, &catalog, false).unwrap(),
        expected
    );
    config.model.default_reasoning = ReasoningChoice::Level(ReasoningLevel::High);
    assert_eq!(
        validate_model_catalog(&config, &catalog, false).unwrap(),
        expected
    );
    config.model.default_reasoning = ReasoningChoice::Off;
    assert!(validate_model_catalog(&config, &catalog, false).is_err());
    config.model.default_reasoning = ReasoningChoice::Default;
    config.mode = Some("unknown".into());
    assert_eq!(
        validate_model_catalog(&config, &catalog, false)
            .unwrap_err()
            .code,
        "unsupported_capability"
    );
    config.mode = Some("safe".into());
    config.fast = Some(false);
    catalog.models[0].fast = None;
    assert_eq!(
        validate_model_catalog(&config, &catalog, true)
            .unwrap_err()
            .code,
        "unsupported_capability"
    );
    config.fast = None;
    catalog.models[0].reasoning = None;
    assert_eq!(
        validate_model_catalog(&config, &catalog, false).unwrap(),
        None
    );
    config.model.default_reasoning = ReasoningChoice::Level(ReasoningLevel::High);
    assert!(validate_model_catalog(&config, &catalog, false).is_err());
    config.model.default_reasoning = ReasoningChoice::Default;
    config.model.requested_model = "absent".into();
    assert_eq!(
        validate_model_catalog(&config, &catalog, false)
            .unwrap_err()
            .code,
        "runtime_unavailable"
    );
    config.model.requested_model = "fixture".into();
    catalog.models.clear();
    assert_eq!(
        validate_model_catalog(&config, &catalog, false)
            .unwrap_err()
            .code,
        "runtime_unavailable"
    );
    assert_eq!(
        validate_model_evidence(None, &config, false)
            .unwrap_err()
            .code,
        "runtime_unavailable"
    );
}

/// **A Bot refused for its agent says why beside the code it always had**
/// (P1.8, the first window run's note under finding 3). With no session
/// host, and with a model the agent does not list, the code and
/// `retryable` are the ones an older client already reads; the reason is
/// added beside them in `details`.
#[test]
fn a_refusal_for_the_agent_keeps_its_code_and_says_why_in_details() {
    use koru_ai_wire::agent_readiness_reasons as reason;
    let mut config = config();
    let refused = validate_model_evidence(None, &config, false).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        ("runtime_unavailable", true)
    );
    assert_eq!(
        refused.details.get("reason"),
        Some(&serde_json::json!(reason::SERVICE_UNAVAILABLE))
    );
    config.model.requested_model = "absent".into();
    let refused = validate_model_catalog(&config, &catalog("fixture"), false).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        ("runtime_unavailable", false)
    );
    assert_eq!(
        refused.details.get("reason"),
        Some(&serde_json::json!(reason::MODEL_NOT_OFFERED))
    );
}
