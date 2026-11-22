// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Every engine and tier voice has ever signed is a token, spelled exactly as
/// it was signed.
#[test]
fn every_speech_engine_and_tier_is_a_token() {
    for value in ["sherpa", "whisper", "scripted", "compact", "high_accuracy"] {
        assert_eq!(Token::parse(value).unwrap().as_str(), value);
    }
}

/// And a word voice never knew is one too — the point of the extraction.
#[test]
fn a_token_is_not_limited_to_speech() {
    for value in ["paddle-ocr", "mobile", "server_v5", "pp-ocrv5.mobile"] {
        Token::parse(value).unwrap();
    }
}

/// What a token may not hold is what could cross a field boundary in the
/// canonical form, name a directory it should not, or fold into another token
/// on a case-insensitive filesystem.
#[test]
fn a_token_refuses_what_could_break_out() {
    for bad in [
        "",
        ".hidden",
        "Sherpa",
        "high accuracy",
        "tab\there",
        "line\nbreak",
        "a,b",
        "../etc",
        "ünicode",
        &"x".repeat(65),
    ] {
        let error = Token::parse(bad).unwrap_err();
        assert_eq!(error.code, ModelErrorCode::DescriptorInvalid, "{bad:?}");
    }
}

/// The eight languages voice signs, in the spelling it signs them in — which
/// is the spelling the canonical form must keep.
#[test]
fn the_eight_interface_languages_are_tags_as_they_were_signed() {
    for tag in ["en", "es", "pt-BR", "de", "fr", "ar", "zh-Hans", "ja"] {
        assert_eq!(LanguageTag::parse(tag).unwrap().as_str(), tag);
    }
}

/// A reader reads more scripts than voice speaks, so a tag is not a closed
/// list.
#[test]
fn a_tag_is_not_limited_to_the_interface_languages() {
    for tag in ["ko", "zh-Hant", "sr-Latn", "hi", "es-419", "th"] {
        LanguageTag::parse(tag).unwrap();
    }
}

/// One language, one spelling: a tag out of canonical case is refused rather
/// than folded, so `en` and `EN` can never both be declared.
#[test]
fn a_tag_in_the_wrong_case_or_shape_is_refused() {
    for bad in [
        "",
        "EN",
        "pt-br",
        "zh-hans",
        "ZH-Hans",
        "pt_BR",
        "en-",
        "-en",
        "en--us",
        "en,ja",
        "e n",
        "1en",
        "toolongsubtag",
    ] {
        let error = LanguageTag::parse(bad).unwrap_err();
        assert_eq!(error.code, ModelErrorCode::DescriptorInvalid, "{bad:?}");
    }
}

/// A model id is a path component; the rule is what stops `..` and the store's
/// own dotted directories from being one.
#[test]
fn a_model_id_is_a_path_component() {
    ModelId::parse("parakeet-tdt-v3").unwrap();
    for bad in ["..", ".staging", "a/b", "C:\\models", "Upper", ""] {
        let error = ModelId::parse(bad).unwrap_err();
        assert_eq!(error.code, ModelErrorCode::DescriptorInvalid, "{bad:?}");
        assert!(error.message.starts_with("a model id"), "{}", error.message);
    }
}

/// Serde goes through the same constructor, so a catalog cannot smuggle in a
/// value `parse` would refuse.
#[test]
fn serde_cannot_smuggle_what_parse_refuses() {
    assert_eq!(
        serde_json::to_string(&Token::parse("sherpa").unwrap()).unwrap(),
        "\"sherpa\""
    );
    assert!(serde_json::from_str::<Token>("\"Sherpa\"").is_err());
    assert!(serde_json::from_str::<LanguageTag>("\"pt-br\"").is_err());
    assert!(serde_json::from_str::<ModelId>("\"../etc\"").is_err());
    assert_eq!(
        serde_json::from_str::<LanguageTag>("\"zh-Hans\"").unwrap(),
        LanguageTag::parse("zh-Hans").unwrap()
    );
}
