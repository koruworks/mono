// SPDX-License-Identifier: MPL-2.0

use super::*;

pub fn validate_agent_model_catalog(
    selection: &koru_ai_core::ModelSelection,
    mode: Option<&str>,
    require_fast: bool,
    catalog: &koru_ai_wire::AgentModelsResponse,
) -> ApiResult<Option<ReasoningControl>> {
    if catalog.provider != selection.provider {
        return Err(unavailable_failure());
    }
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == selection.requested_model);
    // `default` means no explicit model at launch, not a model every agent
    // must advertise. Some runtimes list only concrete models (or no selector
    // at all). Without a default row there is no evidence for overrides.
    if model.is_none() && selection.requested_model == koru_ai_core::AGENT_DEFAULT_MODEL {
        if selection.default_reasoning != koru_ai_core::ReasoningChoice::Default
            || require_fast
            || mode.is_some_and(|mode| !catalog.modes.iter().any(|m| m.id == mode))
        {
            return Err(failure(
                "unsupported_capability",
                "The selected runtime settings are unavailable.",
                false,
            ));
        }
        return Ok(None);
    }
    let model = model.ok_or_else(|| {
        agents::with_reason(
            failure(
                "runtime_unavailable",
                "The selected model is unavailable.",
                false,
            ),
            Some(koru_ai_wire::agent_readiness_reasons::MODEL_NOT_OFFERED),
        )
    })?;
    koru_ai_core::validate_reasoning(
        model
            .reasoning
            .as_ref()
            .unwrap_or(&ReasoningControl::Unsupported),
        selection.default_reasoning,
    )
    .map_err(ai_failure)?;
    if mode.is_some_and(|mode| !catalog.modes.iter().any(|m| m.id == mode))
        || (require_fast && model.fast.is_none())
    {
        return Err(failure(
            "unsupported_capability",
            "The selected runtime settings are unavailable.",
            false,
        ));
    }
    Ok(model.reasoning.clone())
}
