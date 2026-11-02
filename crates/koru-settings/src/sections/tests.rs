// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::values::FontFamily;

#[test]
fn a_patch_body_names_only_the_fields_it_changes() {
    let patch: GlobalPatch = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
    assert_eq!(patch.theme, Some(ThemeChoice::Dark));
    assert!(patch.density.is_none());
    assert!(patch.touches_roaming());
    assert!(!patch.touches_device());
}

#[test]
fn a_patch_body_refuses_a_field_that_is_not_a_setting() {
    // `deny_unknown_fields` is the door: a typo in a field name is a 400 and
    // not a silently ignored write that leaves somebody's toggle stuck.
    let error = serde_json::from_str::<GlobalPatch>(r#"{"themee":"dark"}"#).unwrap_err();
    assert!(error.to_string().contains("themee"));
    assert!(serde_json::from_str::<GlobalPatch>(r#"{"emoji_recents":"x"}"#).is_err());
    assert!(
        serde_json::from_str::<GlobalPatch>(r#"{"global_search_height":900}"#).is_err(),
        "geometry is not a setting, and asking for it says so"
    );
}

#[test]
fn a_patch_that_spans_both_storage_classes_says_so() {
    let patch: GlobalPatch =
        serde_json::from_str(r#"{"theme":"dark","start_on_login":true}"#).unwrap();
    assert!(patch.touches_roaming());
    assert!(patch.touches_device());
    assert!(!patch.is_empty());
    assert!(GlobalPatch::default().is_empty());
}

#[test]
fn applying_a_patch_moves_only_the_named_fields() {
    let mut settings = GlobalSettings {
        density: DensityChoice::Comfortable,
        ..GlobalSettings::default()
    };
    GlobalPatch {
        theme: Some(ThemeChoice::Dark),
        ..Default::default()
    }
    .apply(&mut settings);
    assert_eq!(settings.theme, ThemeChoice::Dark);
    assert_eq!(
        settings.density,
        DensityChoice::Comfortable,
        "a field the patch did not name is untouched"
    );
}

#[test]
fn an_invalid_zoom_is_named_by_the_field_the_caller_sent() {
    let error = GlobalPatch {
        zoom: Some(3.0),
        ..Default::default()
    }
    .validate()
    .unwrap_err();
    assert_eq!(error.field, "zoom");
    assert!(error.to_string().starts_with("zoom: "));
}

#[test]
fn every_global_default_is_the_one_the_current_reader_produces() {
    let settings = GlobalSettings::default();
    assert_eq!(settings.theme, ThemeChoice::System);
    assert_eq!(settings.caret_color, CaretColorChoice::Default);
    assert_eq!(settings.caret_width, CaretWidthChoice::Medium);
    assert_eq!(settings.caret_height, CaretHeightChoice::Medium);
    assert_eq!(settings.density, DensityChoice::Normal);
    assert_eq!(settings.zoom.get(), 1.0);
    assert_eq!(settings.default_view_mode, ViewModeChoice::Narrow);
    assert_eq!(settings.default_canvas_background, CanvasBackground::Auto);
    assert!(!settings.pinned_show_title);
    assert_eq!(settings.popout_close, PopoutCloseChoice::Ask);
    assert_eq!(settings.dictation_gesture, DictationGesture::PushToTalk);
    assert_eq!(settings.dictation_language, DictationLanguage::Automatic);
    assert_eq!(settings.dictation_tier, DictationTier::Compact);
    assert_eq!(settings.dictation_input, DictationInput::SystemDefault);
    assert_eq!(settings.close_behavior, CloseBehaviorChoice::Background);
    assert_eq!(settings.global_dictation_enabled, None);
    assert_eq!(
        settings.global_dictation_shortcut,
        DictationShortcut::default()
    );
    assert!(!settings.start_on_login);
    assert!(!settings.reminder_start_on_login_explained);
    assert!(
        settings.automatic_update_checks,
        "`koru-update`'s own default, preserved across the move"
    );
    for role in [
        settings.ui_font,
        settings.body_font,
        settings.headings_font,
        settings.code_font,
    ] {
        assert_eq!(role, FontSetting::FollowDefault);
    }
}

#[test]
fn dictation_enable_is_a_setter_and_the_chord_is_validated() {
    let mut settings = GlobalSettings::default();
    GlobalPatch {
        global_dictation_enabled: Some(false),
        ..Default::default()
    }
    .apply(&mut settings);
    assert_eq!(settings.global_dictation_enabled, Some(false));

    let error = serde_json::from_str::<GlobalPatch>(r#"{"global_dictation_shortcut":"Ctrl+Nope"}"#)
        .unwrap_err();
    assert!(error.to_string().contains("dictation shortcut"));
}

#[test]
fn the_terminal_defaults_follow_the_app_where_they_can() {
    let settings = TerminalSettings::default();
    assert_eq!(settings.font, FontSetting::FollowDefault);
    assert_eq!(settings.cursor_width, TerminalCursorWidth::Application);
    assert_eq!(settings.cursor_height, TerminalCursorHeight::Application);
    // **Blink is the one that does not**, and the exception is the product's:
    // a shell generally inherits VTE's non-blinking default, and Koru's
    // terminal cursor is deliberately visible as an editing affordance until
    // somebody defers to the application's DECSCUSR choice. The token parser
    // still falls back to `Application`, because an unrecognized token in a
    // file is a different question from a person who never chose.
    assert_eq!(settings.cursor_blink, TerminalCursorBlink::On);
    assert_eq!(
        TerminalCursorBlink::parse("something new"),
        TerminalCursorBlink::Application
    );
    assert!(
        settings.composer_default,
        "a new terminal window opens with its composer, matching `input_area`'s \
         default-true inversion"
    );
}

#[test]
fn an_ai_patch_clears_a_model_with_null_and_leaves_it_alone_when_absent() {
    let mut settings = AiSettings {
        openai: OpenAiSection {
            model: Some("gpt".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    serde_json::from_str::<AiPatch>("{}")
        .unwrap()
        .apply(&mut settings);
    assert_eq!(settings.openai.model.as_deref(), Some("gpt"));

    serde_json::from_str::<AiPatch>(r#"{"openai_model":null}"#)
        .unwrap()
        .apply(&mut settings);
    assert_eq!(settings.openai.model, None);
}

#[test]
fn an_ai_patch_cannot_name_the_credential_flag() {
    // Read-only, in the type system rather than in a comment: the flag is a
    // fact about the vault, and the secret behind it never travels through
    // this authority in either direction.
    assert!(
        serde_json::from_str::<AiPatch>(r#"{"openai_credential_configured":true}"#).is_err(),
        "the configured-state flag is reported, never set"
    );
    assert!(serde_json::from_str::<AiPatch>(r#"{"openai_api_key":"sk-x"}"#).is_err());
}

#[test]
fn an_ai_section_never_serializes_a_secret() {
    let settings = AiSettings {
        selected_provider: Some(AiProvider::OpenAi),
        openai: OpenAiSection {
            model: Some("gpt".into()),
            reasoning: ReasoningChoice::High,
            credential_configured: true,
        },
        ollama: OllamaSection::default(),
    };
    let json = serde_json::to_string(&settings).unwrap();
    for forbidden in ["api_key", "key_account", "secret", "sk-"] {
        assert!(!json.contains(forbidden), "leaked `{forbidden}`: {json}");
    }
    assert!(json.contains("\"credential_configured\":true"));
}

#[test]
fn an_ai_patch_validates_what_the_repository_would_refuse() {
    assert!(
        AiPatch {
            ollama_endpoint: Some("not a url".into()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        AiPatch {
            openai_model: Field::Set(String::new()),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        AiPatch {
            openai_model: Field::Set("gpt-4o".into()),
            ollama_endpoint: Some("http://localhost:11434".into()),
            ..Default::default()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn the_ollama_default_endpoint_matches_the_database_default() {
    assert_eq!(OllamaSection::default().endpoint, "http://localhost:11434");
}

#[test]
fn a_section_value_names_its_own_section() {
    assert_eq!(
        SectionValues::Global(GlobalSettings::default()).section(),
        Section::Global
    );
    assert_eq!(
        SectionValues::Terminal(TerminalSettings::default()).section(),
        Section::Terminal
    );
    assert_eq!(
        SectionValues::Ai(AiSettings::default()).section(),
        Section::Ai
    );
}

#[test]
fn a_terminal_patch_refuses_a_proportional_font() {
    let error = TerminalPatch {
        font: Some(FontSetting::Chosen(FontFamily::Lora)),
        ..Default::default()
    }
    .validate()
    .unwrap_err();
    assert_eq!(error.field, "font");
    assert!(
        TerminalPatch {
            font: Some(FontSetting::FollowDefault),
            ..Default::default()
        }
        .validate()
        .is_ok(),
        "follow-the-app is always allowed: the app's default is monospaced"
    );
}

/// **The off list is written whole, kept sorted, and bounded**: a switch
/// sends the whole list, an empty one is *Restore defaults*, and a key that
/// is not an agent key's shape is refused before anything is staged.
#[test]
fn the_disabled_agents_patch_is_whole_sorted_and_validated() {
    let mut section = GlobalSettings::default();
    let patch = GlobalPatch {
        disabled_agents: Some(vec!["muse".into(), "grok".into(), "grok".into()]),
        ..GlobalPatch::default()
    };
    assert!(patch.touches_device());
    assert!(!patch.touches_roaming());
    assert!(patch.validate().is_ok());
    patch.apply(&mut section);
    assert_eq!(section.disabled_agents, ["grok", "muse"]);
    GlobalPatch {
        disabled_agents: Some(Vec::new()),
        ..GlobalPatch::default()
    }
    .apply(&mut section);
    assert!(section.disabled_agents.is_empty(), "restore defaults");
    for bad in ["", "Grok", "grok cli", &"a".repeat(33)] {
        let patch = GlobalPatch {
            disabled_agents: Some(vec![bad.to_owned()]),
            ..GlobalPatch::default()
        };
        assert!(patch.validate().is_err(), "{bad:?}");
    }
    let too_many = GlobalPatch {
        disabled_agents: Some((0..65).map(|n| format!("a{n}")).collect()),
        ..GlobalPatch::default()
    };
    assert!(too_many.validate().is_err());
}

/// **The default permissions are this machine's**, and an agent with no auto
/// mode is never told to start in *Auto*.
#[test]
fn the_default_permissions_patch_is_device_local_and_other_has_no_auto() {
    let mut section = GlobalSettings::default();
    assert_eq!(section.default_permission, AgentPermission::Manual);
    assert_eq!(section.default_permission_other, AgentPermission::Manual);
    let patch = GlobalPatch {
        default_permission: Some(AgentPermission::Yolo),
        default_permission_other: Some(AgentPermission::Yolo),
        ..GlobalPatch::default()
    };
    assert!(patch.touches_device() && !patch.touches_roaming());
    assert!(patch.validate().is_ok());
    patch.apply(&mut section);
    assert_eq!(section.default_permission, AgentPermission::Yolo);
    assert_eq!(section.default_permission_other, AgentPermission::Yolo);
    let auto_other = GlobalPatch {
        default_permission_other: Some(AgentPermission::Auto),
        ..GlobalPatch::default()
    };
    assert!(auto_other.validate().is_err());
    let body: GlobalPatch =
        serde_json::from_str(r#"{"default_permission":"auto"}"#).expect("a token");
    assert_eq!(body.default_permission, Some(AgentPermission::Auto));
}
