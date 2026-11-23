// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, ModelCapabilities, ModelDescriptor, ModelSelection, ReasoningChoice,
    ReasoningControl,
};

pub const KORU_MODEL: &str = AiProviderId::KORU_HOSTED_MODEL;
pub const KORU_MODEL_LABEL: &str = AiProviderId::KORU_HOSTED_MODEL_LABEL;

pub fn koru_model() -> ModelDescriptor {
    ModelDescriptor {
        selection: ModelSelection {
            provider: AiProviderId::parse(AiProviderId::KORU).expect("built-in provider id"),
            requested_model: KORU_MODEL.to_owned(),
            default_reasoning: ReasoningChoice::Default,
        },
        resolved_model: Some(KORU_MODEL.to_owned()),
        capabilities: ModelCapabilities {
            native_tools: true,
            streaming: true,
            structured_output: false,
            vision: false,
            web_search: false,
            context_window: Some(8192),
            reasoning: ReasoningControl::Toggle { default_on: true },
        },
    }
}
