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
