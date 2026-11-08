// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn every_token_enum_round_trips_its_own_spelling() {
    macro_rules! round_trip {
        ($($name:ident),+ $(,)?) => {
            $(
                for value in $name::ALL {
                    assert_eq!(
                        $name::parse(value.token()),
                        *value,
                        concat!(stringify!($name), " does not read back what it writes")
                    );
                }
            )+
        };
    }
    round_trip!(
        ThemeChoice,
        CaretColorChoice,
        CaretWidthChoice,
        CaretHeightChoice,
        DensityChoice,
        PopoutCloseChoice,
        ViewModeChoice,
        DictationTier,
        CloseBehaviorChoice,
        TerminalCursorWidth,
        TerminalCursorHeight,
        TerminalCursorBlink,
        AiProvider,
        ReasoningChoice,
        FontFamily,
    );
}

#[test]
fn an_unknown_token_is_the_default_everywhere() {
    assert_eq!(ThemeChoice::parse("nonsense"), ThemeChoice::System);
    assert_eq!(CaretColorChoice::parse(""), CaretColorChoice::Default);
    assert_eq!(FontFamily::parse("comic"), FontFamily::Inter);
    assert_eq!(ViewModeChoice::parse("wide"), ViewModeChoice::Narrow);
    assert_eq!(
        TerminalCursorBlink::parse("sometimes"),
        TerminalCursorBlink::Application
    );
}

#[test]
fn case_folding_matches_the_parser_each_value_replaces() {
    // Some of the originals fold case and some do not. Copying the behavior
    // rather than tidying it is deliberate: a file written by one build must
    // mean the same thing to the next.
    assert_eq!(
        CaretColorChoice::parse("  VIOLET "),
        CaretColorChoice::Violet
    );
    assert_eq!(CaretWidthChoice::parse("X8"), CaretWidthChoice::X8);
    assert_eq!(
        ThemeChoice::parse("Dark"),
        ThemeChoice::System,
        "the theme parser is case-sensitive, and always has been"
    );
    assert_eq!(
        DensityChoice::parse("Compact"),
        DensityChoice::Normal,
        "so is density's"
    );
    assert_eq!(
        FontFamily::parse("Inter"),
        FontFamily::Inter,
        "an unknown family falls back to Inter, which is also the default"
    );
}

#[test]
fn the_font_family_order_matches_the_theme_crates() {
    // Load-bearing there: role globals store an index into this list. Kept
    // identical here so the two cannot drift into disagreeing about it.
    let tokens: Vec<_> = FontFamily::ALL.iter().map(|f| f.token()).collect();
    assert_eq!(
        tokens,
        [
            "inter",
            "sourcesans3",
            "plusjakartasans",
            "ibmplexsans",
            "poppins",
            "manrope",
            "sourceserif4",
            "lora",
            "nunito",
            "jetbrainsmono",
            "sourcecodepro",
            "mono",
            "system",
        ]
    );
}

#[test]
fn exactly_three_families_are_monospaced() {
    let mono: Vec<_> = FontFamily::ALL
        .iter()
        .filter(|family| family.is_mono())
        .map(|family| family.token())
        .collect();
    assert_eq!(mono, ["jetbrainsmono", "sourcecodepro", "mono"]);
}

#[test]
fn follow_the_app_is_a_state_and_not_a_family() {
    assert_eq!(FontSetting::default(), FontSetting::FollowDefault);
    assert_eq!(FontSetting::FollowDefault.family(), None);
    assert_eq!(
        FontSetting::Chosen(FontFamily::Lora).family(),
        Some(FontFamily::Lora)
    );
    // And it survives the wire, which is what makes it expressible in an API.
    let json = serde_json::to_string(&FontSetting::FollowDefault).unwrap();
    assert_eq!(json, "\"follow_default\"");
    assert_eq!(
        serde_json::from_str::<FontSetting>(&json).unwrap(),
        FontSetting::FollowDefault
    );
    let chosen = serde_json::to_string(&FontSetting::Chosen(FontFamily::Mono)).unwrap();
    assert_eq!(chosen, r#"{"chosen":"mono"}"#);
}

#[test]
fn the_dictation_gesture_reads_a_digit_and_speaks_a_word() {
    assert_eq!(DictationGesture::parse("1"), DictationGesture::Toggle);
    assert_eq!(DictationGesture::parse("0"), DictationGesture::PushToTalk);
    assert_eq!(
        DictationGesture::parse("true"),
        DictationGesture::PushToTalk,
        "only `1` means toggle — `koru-dictation` compares against it exactly, \
         and disagreeing here would make the authority disagree with the \
         file's own history"
    );
    assert_eq!(DictationGesture::Toggle.token(), "1");
    assert_eq!(
        serde_json::to_string(&DictationGesture::Toggle).unwrap(),
        "\"toggle\"",
        "the API says what it means; the disk keeps its byte"
    );
}

#[test]
fn zoom_rejects_what_is_out_of_range_rather_than_clamping_it() {
    assert!(Zoom::new(0.5).is_some());
    assert!(Zoom::new(2.0).is_some());
    assert!(Zoom::new(0.49).is_none());
    assert!(Zoom::new(2.01).is_none());
    assert!(Zoom::new(f32::NAN).is_none());
    assert!(Zoom::new(f32::INFINITY).is_none());
    assert_eq!(Zoom::default().get(), 1.0);
    assert_eq!(Zoom::parse_or_default("9").get(), 1.0);
}

#[test]
fn a_canvas_background_round_trips_and_ignores_alpha() {
    assert_eq!(CanvasBackground::parse("auto"), CanvasBackground::Auto);
    assert_eq!(CanvasBackground::parse("1e1e28"), CanvasBackground::Auto);
    assert_eq!(CanvasBackground::parse("#12345"), CanvasBackground::Auto);
    assert_eq!(CanvasBackground::parse("#zzzzzz"), CanvasBackground::Auto);
    assert_eq!(
        CanvasBackground::parse("#1E1E28"),
        CanvasBackground::Color([0x1e, 0x1e, 0x28]),
        "hex is read in either case"
    );
    assert_eq!(
        CanvasBackground::Color([0x1e, 0x1e, 0x28]).token(),
        "#1e1e28",
        "and written in lowercase"
    );
    assert_eq!(CanvasBackground::Auto.token(), "auto");
}

#[test]
fn a_dictation_language_is_validated_but_never_resolved() {
    assert_eq!(
        DictationLanguage::parse("auto"),
        DictationLanguage::Automatic
    );
    assert_eq!(
        DictationLanguage::parse("AUTO"),
        DictationLanguage::Automatic
    );
    assert_eq!(DictationLanguage::parse(" "), DictationLanguage::Automatic);
    assert_eq!(
        DictationLanguage::parse("zh-Hant"),
        DictationLanguage::Tag("zh-Hant".into())
    );
    assert!(DictationLanguage::Tag("zh-Hant".into()).validate().is_ok());
    assert!(DictationLanguage::Tag("fr=en".into()).validate().is_err());
    assert!(DictationLanguage::Tag(String::new()).validate().is_err());
    assert!(DictationLanguage::Automatic.validate().is_ok());
}

#[test]
fn a_device_name_may_not_break_the_file_it_is_written_into() {
    assert_eq!(
        DictationInput::parse("system-default"),
        DictationInput::SystemDefault
    );
    assert_eq!(
        DictationInput::parse("Yeti Nano"),
        DictationInput::Named("Yeti Nano".into())
    );
    assert!(DictationInput::Named("Yeti Nano".into()).validate().is_ok());
    assert!(
        DictationInput::Named("a\nb".into()).validate().is_err(),
        "a newline would end the record"
    );
    assert!(
        DictationInput::Named("a=b".into()).validate().is_err(),
        "an `=` would be read back as a second key"
    );
}

#[test]
fn an_endpoint_is_the_shape_the_ai_repository_already_enforces() {
    assert!(validate_endpoint("http://localhost:11434").is_ok());
    assert!(validate_endpoint("https://ollama.example/api").is_ok());
    assert!(validate_endpoint("ftp://localhost").is_err());
    assert!(validate_endpoint("http://").is_err());
    assert!(validate_endpoint("http://user:pw@host").is_err());
    assert!(validate_endpoint("http://host?x=1").is_err());
    assert!(validate_endpoint("http://host#frag").is_err());
}

#[test]
fn a_shortcut_is_not_empty_and_carries_no_control_characters() {
    assert_eq!(GlobalShortcut::default().as_str(), "Ctrl+Shift+K");
    assert!(GlobalShortcut::default().validate().is_ok());
    assert!(GlobalShortcut::new("  ").validate().is_err());
    assert!(GlobalShortcut::new("Ctrl+\n").validate().is_err());
}

#[test]
fn every_picker_dictation_chord_is_valid_and_invalid_text_falls_back() {
    for modifier in DictationShortcut::WINDOWS_MODIFIERS
        .iter()
        .chain(DictationShortcut::MACOS_MODIFIERS)
    {
        for key in DictationShortcut::KEYS {
            let chord = DictationShortcut::try_new(format!("{modifier}+{key}")).unwrap();
            assert_eq!(DictationShortcut::parse_or_default(chord.as_str()), chord);
        }
    }
    for invalid in [
        "",
        "Ctrl+Shift",
        "Ctrl+D",
        "Ctrl+Command+D",
        "Win+Alt+D",
        "Ctrl+Shift+-",
        "Ctrl+Alt+;",
        "Ctrl+Shift+d",
        "Ctrl+Shift+D+E",
    ] {
        assert!(DictationShortcut::try_new(invalid).is_err(), "{invalid}");
        assert_eq!(
            DictationShortcut::parse_or_default(invalid),
            DictationShortcut::default()
        );
    }
}

#[test]
fn a_patch_field_tells_absent_from_null_from_a_value() {
    #[derive(serde::Deserialize, PartialEq, Debug, Default)]
    #[serde(default)]
    struct Body {
        model: Field<String>,
    }

    assert_eq!(
        serde_json::from_str::<Body>("{}").unwrap().model,
        Field::Unchanged,
        "a body that does not mention a field leaves it alone"
    );
    assert_eq!(
        serde_json::from_str::<Body>(r#"{"model":null}"#)
            .unwrap()
            .model,
        Field::Clear,
        "null clears it"
    );
    assert_eq!(
        serde_json::from_str::<Body>(r#"{"model":"gpt"}"#)
            .unwrap()
            .model,
        Field::Set("gpt".into())
    );
}

#[test]
fn a_patch_field_folds_onto_a_nullable_value() {
    let mut current = Some("gpt".to_owned());
    Field::<String>::Unchanged.apply(&mut current);
    assert_eq!(current.as_deref(), Some("gpt"));
    Field::Set("llama".to_owned()).apply(&mut current);
    assert_eq!(current.as_deref(), Some("llama"));
    Field::<String>::Clear.apply(&mut current);
    assert_eq!(current, None);
}
