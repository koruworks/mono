// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, ModelCapabilities, ModelDescriptor, ModelSelection, ReasoningChoice,
    ReasoningControl, ReasoningLevel,
};

pub fn ollama_model(model: &str, capabilities: &[String]) -> ModelDescriptor {
    let reasoning = if model
        .split([':', '/'])
        .any(|part| part.eq_ignore_ascii_case("gpt-oss"))
        || model.to_ascii_lowercase().starts_with("gpt-oss")
    {
        levels(
            &[
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
            ],
            ReasoningLevel::Medium,
            false,
        )
    } else if capabilities.iter().any(|value| value == "thinking") {
        ReasoningControl::Toggle { default_on: true }
    } else {
        ReasoningControl::Unsupported
    };
    let mut descriptor = ModelDescriptor {
        selection: ModelSelection {
            provider: AiProviderId::parse(AiProviderId::OLLAMA).expect("built-in provider id"),
            requested_model: model.to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        resolved_model: Some(model.to_owned()),
        capabilities: ModelCapabilities {
            native_tools: capabilities.iter().any(|value| value == "tools"),
            streaming: true,
            structured_output: false,
            vision: false,
            web_search: false,
            context_window: None,
            reasoning,
        },
    };
    descriptor.capabilities.vision = capabilities.iter().any(|value| value == "vision");
    descriptor
}

fn levels(
    values: &[ReasoningLevel],
    default: ReasoningLevel,
    supports_off: bool,
) -> ReasoningControl {
    ReasoningControl::Levels {
        values: values.to_vec(),
        default: Some(default),
        supports_off,
    }
}

#[cfg(test)]
mod mini_app_tests {
    use super::*;
    #[test]
    fn mini_app_native_tools_follow_live_model_capabilities() {
        assert!(
            !ollama_model("plain", &["thinking".into()])
                .capabilities
                .native_tools
        );
        assert!(
            ollama_model("tools", &["tools".into()])
                .capabilities
                .native_tools
        );
    }
}
