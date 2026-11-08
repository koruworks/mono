// SPDX-License-Identifier: MPL-2.0

use super::*;
#[test]
fn catalog_bounds_duplicates_and_private_debug_are_enforced_at_deserialization() {
    let model = ProviderModel::new("private/model".parse().unwrap(), "private".parse().unwrap());
    assert!(ProviderModels::try_from(vec![model.clone(), model.clone()]).is_err());
    assert!(ProviderModels::try_from(vec![model.clone(); MAX_PROVIDER_MODELS + 1]).is_err());
    assert!(!format!("{model:?}").contains("private"));
    assert!(!format!("{:?}", model.id).contains("private"));
    assert!(
        serde_json::from_value::<ProviderModels>(
            serde_json::json!([{"id":"bad\nmodel","group":"provider"}])
        )
        .is_err()
    );
}

/// A v17 host sends `{id, group}` and nothing else; a v18 reader takes it
/// as a model with no facts, and writes it back in the same shape.
#[test]
fn a_v17_shaped_model_still_parses_and_round_trips_unchanged() {
    let v17 = serde_json::json!([{"id":"claude-opus-4-1","group":"claude"}]);
    let models: ProviderModels = serde_json::from_value(v17.clone()).unwrap();
    let model = &models.models()[0];
    assert!(model.name.is_none() && model.description.is_none() && model.group_name.is_none());
    assert!(!model.legacy && model.reasoning.is_none());
    assert_eq!(serde_json::to_value(&models).unwrap(), v17);
}

#[test]
fn model_facts_round_trip_and_labels_stay_out_of_debug() {
    let mut model = ProviderModel::new("opus".parse().unwrap(), "claude".parse().unwrap());
    model.name = ModelLabel::name("Claude Opus 5.5");
    model.description = ModelLabel::description("The private-sentinel model");
    model.group_name = ModelLabel::name("Anthropic");
    model.reasoning = Some(ModelReasoning {
        levels: vec![LaunchReasoning::Low, LaunchReasoning::XHigh],
        default: Some(LaunchReasoning::Low),
        off: true,
    });
    let models = ProviderModels::try_from(vec![model.clone()]).unwrap();
    let json = serde_json::to_value(&models).unwrap();
    assert_eq!(json[0]["name"], "Claude Opus 5.5");
    assert_eq!(json[0]["reasoning"]["levels"][1], "xhigh");
    assert!(json[0].get("legacy").is_none(), "false is not written");
    let back: ProviderModels = serde_json::from_value(json).unwrap();
    assert_eq!(back, models);
    assert!(!format!("{model:?}").contains("sentinel"));
    assert!(!format!("{:?}", model.description).contains("sentinel"));
}

/// A model's longer context (koru-ai D10) round-trips; a peer that does not
/// know the field reads the model without it; and its id is held to the
/// catalog's rule that no two models share an id.
#[test]
fn a_models_context_round_trips_and_its_id_is_unique_in_the_catalog() {
    let mut opus = ProviderModel::new("opus".parse().unwrap(), "claude".parse().unwrap());
    opus.context = Some(ModelContext {
        id: "opus[1m]".parse().unwrap(),
        tokens: 1_000_000,
    });
    let models = ProviderModels::try_from(vec![opus.clone()]).unwrap();
    let json = serde_json::to_value(&models).unwrap();
    assert_eq!(json[0]["context"]["id"], "opus[1m]");
    assert_eq!(json[0]["context"]["tokens"], 1_000_000);
    assert_eq!(
        serde_json::from_value::<ProviderModels>(json).unwrap(),
        models
    );
    let plain: ProviderModels =
        serde_json::from_value(serde_json::json!([{"id":"opus","group":"claude"}])).unwrap();
    assert!(plain.models()[0].context.is_none());

    let sibling = ProviderModel::new("opus[1m]".parse().unwrap(), "claude".parse().unwrap());
    assert!(ProviderModels::try_from(vec![opus.clone(), sibling]).is_err());
    let mut itself = opus.clone();
    itself.context.as_mut().unwrap().id = "opus".parse().unwrap();
    assert!(ProviderModels::try_from(vec![itself]).is_err());
    let mut empty = opus;
    empty.context.as_mut().unwrap().tokens = 0;
    assert!(ProviderModels::try_from(vec![empty]).is_err());
}

#[test]
fn a_label_is_trimmed_bounded_and_free_of_control_characters() {
    assert_eq!(ModelLabel::name("  Opus  ").unwrap().as_str(), "Opus");
    assert!(ModelLabel::name("   ").is_none());
    assert!(ModelLabel::name("bad\u{1b}[31m").is_none());
    assert!(ModelLabel::name(&"x".repeat(MAX_MODEL_NAME_BYTES)).is_some());
    assert!(
        ModelLabel::name(&"x".repeat(MAX_MODEL_NAME_BYTES + 1)).is_none(),
        "a long name gives way to the formatted id"
    );
    assert_eq!(
        ModelLabel::description("two\nlines").unwrap().as_str(),
        "two lines"
    );
    let long = ModelLabel::description(&"é".repeat(MAX_MODEL_DESCRIPTION_BYTES)).unwrap();
    assert!(long.as_str().len() <= MAX_MODEL_DESCRIPTION_BYTES);
    assert!(long.as_str().ends_with('…'));
    // What comes off the wire is held to the same rules.
    for bad in ["", " padded", "a\tb"] {
        assert!(serde_json::from_value::<ModelLabel>(serde_json::json!(bad)).is_err());
    }
    let mut model = ProviderModel::new("m".parse().unwrap(), "g".parse().unwrap());
    model.name = ModelLabel::description(&"x".repeat(MAX_MODEL_NAME_BYTES + 1));
    assert!(
        ProviderModels::try_from(vec![model]).is_err(),
        "a description-length name is not a name"
    );
}

#[test]
fn reasoning_needs_a_level_no_repeats_no_toggle_and_a_default_among_its_levels() {
    use LaunchReasoning::*;
    let reasoning = |levels: Vec<LaunchReasoning>, default| ModelReasoning {
        levels,
        default,
        off: false,
    };
    assert!(reasoning(vec![Low, High], Some(High)).valid());
    assert!(reasoning(vec![Low], None).valid());
    assert!(!reasoning(vec![], None).valid());
    assert!(!reasoning(vec![Low, Low], None).valid());
    assert!(!reasoning(vec![Off, Low], None).valid());
    assert!(!reasoning(vec![On], None).valid());
    assert!(!reasoning(vec![Low], Some(Max)).valid());
    let mut model = ProviderModel::new("m".parse().unwrap(), "g".parse().unwrap());
    model.reasoning = Some(reasoning(vec![Low], Some(Max)));
    assert!(ProviderModels::try_from(vec![model]).is_err());
}

/// **D15: an agent's level tokens in Koru's vocabulary.** The six words map
/// whatever their case, `off` and `none` say the model can be off,
/// `default` is no stop, anything else is dropped and counted, the order is
/// Koru's, and the stated value is the default only when it is a level.
#[test]
fn agent_level_tokens_map_onto_koru_words_and_the_rest_is_dropped() {
    let (reasoning, dropped) = ModelReasoning::from_agent_tokens(
        [
            "default", "MAX", "low", "High", "xhigh", "none", "turbo", "low",
        ],
        Some("high"),
    );
    let reasoning = reasoning.unwrap();
    assert_eq!(
        reasoning.levels,
        [
            LaunchReasoning::Low,
            LaunchReasoning::High,
            LaunchReasoning::XHigh,
            LaunchReasoning::Max
        ]
    );
    assert_eq!(reasoning.default, Some(LaunchReasoning::High));
    assert!(reasoning.off);
    assert_eq!(dropped, 1, "turbo has no word here");
    assert!(reasoning.valid());

    let (stated_default, _) = ModelReasoning::from_agent_tokens(["low", "high"], Some("default"));
    assert_eq!(stated_default.unwrap().default, None, "the agent decides");
    let (none, dropped) = ModelReasoning::from_agent_tokens(["default", "off", "on"], None);
    assert!(none.is_none(), "no level, no slider");
    assert_eq!(dropped, 1, "on is no level an agent sends");
}
