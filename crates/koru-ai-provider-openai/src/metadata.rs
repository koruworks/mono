// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, ModelCapabilities, ModelDescriptor, ModelSelection, ReasoningChoice,
    ReasoningControl, ReasoningLevel,
};

pub fn openai_model(model: &str) -> ModelDescriptor {
    let reasoning = if model.starts_with("gpt-5.6") {
        levels(
            &[
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            ReasoningLevel::Medium,
            true,
        )
    } else if model.starts_with("gpt-5.5-pro") {
        levels(
            &[
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
            ],
            ReasoningLevel::High,
            false,
        )
    } else if model.starts_with("gpt-5.4-pro") || model.starts_with("gpt-5.2-pro") {
        levels(
            &[
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
            ],
            ReasoningLevel::Medium,
            false,
        )
    } else if ["gpt-5.5", "gpt-5.4", "gpt-5.3", "gpt-5.2", "gpt-5.1"]
        .iter()
        .any(|prefix| model.starts_with(prefix))
    {
        levels(
            &[
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
            ],
            ReasoningLevel::Medium,
            true,
        )
    } else if model == "gpt-5-pro" || model.starts_with("gpt-5-pro-") {
        levels(&[ReasoningLevel::High], ReasoningLevel::High, false)
    } else if model == "gpt-5" || model.starts_with("gpt-5-") {
        levels(
            &[
                ReasoningLevel::Minimal,
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
            ],
            ReasoningLevel::Medium,
            false,
        )
    } else {
        ReasoningControl::Unsupported
    };
    ModelDescriptor {
        selection: ModelSelection {
            provider: AiProviderId::parse(AiProviderId::OPENAI).expect("built-in provider id"),
            requested_model: model.to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        resolved_model: Some(model.to_owned()),
        capabilities: ModelCapabilities {
            streaming: true,
            structured_output: false,
            vision: false,
            web_search: model.starts_with("gpt-5") || model.starts_with("gpt-4.1"),
            context_window: None,
            reasoning,
        },
    }
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
mod tests {
    use koru_ai_core::{ReasoningChoice, validate_reasoning};

    use super::*;

    #[test]
    fn reasoning_and_web_search_are_model_specific() {
        let current = openai_model("gpt-5.6-sol");
        assert!(current.capabilities.web_search);
        assert_eq!(
            validate_reasoning(
                &current.capabilities.reasoning,
                ReasoningChoice::Level(ReasoningLevel::Max)
            )
            .unwrap(),
            Some(ReasoningChoice::Level(ReasoningLevel::Max))
        );
        assert!(openai_model("gpt-4.1-mini").capabilities.web_search);
        assert!(!openai_model("unknown-model").capabilities.web_search);
    }
}
