// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn bot_model_rejects_settings_it_does_not_understand() {
    let selection = ModelSelection {
        provider: crate::AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "fixture".into(),
        default_reasoning: crate::ReasoningChoice::Default,
    };
    let workspace = WorkspaceId::new();
    let config = BotConfig {
        name: "fixture".into(),
        role: BotPage {
            workspace,
            document: DocumentId::new(),
        },
        working_folder: "/fixture".into(),
        model: selection.clone(),
        mode: None,
        fast: None,
    };
    let mut encoded = serde_json::to_value(&config).unwrap();
    assert_eq!(
        serde_json::from_value::<BotConfig>(encoded.clone()).unwrap(),
        config
    );
    encoded["model"]["invented"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotConfig>(encoded.clone()).is_err());
    assert!(
        serde_json::from_value::<ModelSelection>(encoded["model"].clone()).is_ok(),
        "ordinary Chat compatibility stays unchanged"
    );
}
