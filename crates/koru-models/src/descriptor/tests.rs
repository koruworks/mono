// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::{self, Signer};
use crate::trust::TrustedKeys;

/// **The signed bytes, spelled out** — the same golden string
/// `koru-voice-models` pins, produced here from tokens. The two tests are the
/// two halves of one promise: the extraction changed what holds the fields and
/// not one byte of what is signed.
#[test]
fn the_canonical_form_is_exactly_what_voice_signed() {
    let bytes = fixtures::descriptor().canonical_bytes().unwrap();
    let expected = concat!(
        "koru-model-descriptor/1\n",
        "id\tparakeet-tdt-v3\n",
        "version\t1.2.0\n",
        "engine\tsherpa\n",
        "tier\tcompact\n",
        "languages\ten,zh-Hans\n",
        "size\t3\n",
        "digest\tba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
        "working_set_bytes\t1073741824\n",
        "licence\tApache-2.0\n",
        "attribution\tk2-fsa\n",
    );
    assert_eq!(String::from_utf8(bytes).unwrap(), expected);
}

/// A public key and two signatures made by `koru-voice-models`' generated-key
/// fixture signer **before the extraction**, over its typed fixture descriptor
/// and over a second one carrying every interface language, the other tier,
/// the other engine and every escape. Checked in on purpose — regenerating
/// them would prove nothing.
pub(crate) const BEFORE_PUBLIC_KEY: &str = "untrusted comment: minisign public key: 3EF16044D44BE0C6\nRWTG4EvURGDxPjcBbqDkol2WOWJ7/y5X1GwNaxQgGCqRnBN5xK2DxBAm\n";
pub(crate) const BEFORE_SIGNATURE_FIXTURE: &str = "untrusted comment: koru model fixture\nRUTG4EvURGDxPi40Ryu1tjteK0Rmy/+ztEEQi76kJr5lxrYHIy9FzGt772CDx2nbBRPt3+AZie1pu4qRJsQvecpeJrKrABz/eQc=\ntrusted comment: koru model fixture\nyKIuNSGdm5lXer28IejGWdYxHaJq8ROEg3iACXek0ayg3asnPUeRcH8L++RQ+Fv/4tqE9UqUS5RiO1eGzxQjAg==\n";
pub(crate) const BEFORE_SIGNATURE_WIDE: &str = "untrusted comment: koru model fixture\nRUTG4EvURGDxPiYXbyWuM8JFHEY7ROpJT9aUIGDSsP2cksuvVkVhgREO7wW3KyGPYixann6TnOVU9ONN03yTQDSrnSd+1DHBYAY=\ntrusted comment: koru model fixture\nM/mrDMqevqfRZPIAZcGWDPrpIpPYHIW4fjfnsN6vFpZrOBg2ZcEom18CwBsw8D1jICYw+19XOcgdpJ9ffvaGBw==\n";

/// The second vector, built from tokens alone.
fn wide() -> ModelDescriptor {
    ModelDescriptor {
        engine: Token::parse("whisper").unwrap(),
        tier: Token::parse("high_accuracy").unwrap(),
        languages: ["en", "es", "pt-BR", "de", "fr", "ar", "zh-Hans", "ja"]
            .into_iter()
            .map(|tag| LanguageTag::parse(tag).unwrap())
            .collect(),
        attribution: "k2-fsa\\nvia\ttab\r\nend".to_string(),
        ..fixtures::descriptor()
    }
}

/// **A descriptor signed before the extraction verifies after it**, built here
/// with no speech type anywhere — which is the whole of what a second
/// consumer needs to be true.
#[test]
fn a_signature_made_before_the_extraction_still_verifies() {
    let keys = TrustedKeys::from_public_keys(&[BEFORE_PUBLIC_KEY]).unwrap();
    keys.verify(
        &fixtures::descriptor(),
        &ModelSignature::parse(BEFORE_SIGNATURE_FIXTURE).unwrap(),
    )
    .unwrap();
    keys.verify(
        &wide(),
        &ModelSignature::parse(BEFORE_SIGNATURE_WIDE).unwrap(),
    )
    .unwrap();
    // And it is the bytes that carry it: the wide signature does not verify
    // over the narrow descriptor.
    assert_eq!(
        keys.verify(
            &fixtures::descriptor(),
            &ModelSignature::parse(BEFORE_SIGNATURE_WIDE).unwrap(),
        )
        .unwrap_err()
        .code,
        ModelErrorCode::SignatureInvalid
    );
}

#[test]
fn the_wide_vector_is_spelled_as_it_was_signed() {
    let text = String::from_utf8(wide().canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        text,
        "koru-model-descriptor/1\nid\tparakeet-tdt-v3\nversion\t1.2.0\nengine\twhisper\n\
         tier\thigh_accuracy\nlanguages\ten,es,pt-BR,de,fr,ar,zh-Hans,ja\nsize\t3\n\
         digest\tba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n\
         working_set_bytes\t1073741824\nlicence\tApache-2.0\n\
         attribution\tk2-fsa\\\\nvia\\ttab\\r\\nend\n"
    );
}

/// A reader model is describable — the reason the fields became tokens — and it
/// signs and verifies like any other.
#[test]
fn a_model_that_is_not_a_speech_model_can_be_described_and_signed() {
    let reader = ModelDescriptor {
        id: ModelId::parse("text-reader").unwrap(),
        engine: Token::parse("onnx-ocr").unwrap(),
        tier: Token::parse("mobile").unwrap(),
        languages: ["en", "ko", "zh-Hant"]
            .into_iter()
            .map(|tag| LanguageTag::parse(tag).unwrap())
            .collect(),
        ..fixtures::descriptor()
    };
    reader.validate().unwrap();
    let signer = Signer::new();
    signer
        .trusted
        .verify(&reader, &signer.sign(&reader))
        .unwrap();
}

#[test]
fn validate_refuses_what_no_single_field_can_see() {
    let en = LanguageTag::parse("en").unwrap();
    for (what, descriptor) in [
        (
            "no languages",
            ModelDescriptor {
                languages: Vec::new(),
                ..fixtures::descriptor()
            },
        ),
        (
            "a duplicated language",
            ModelDescriptor {
                languages: vec![en.clone(), en],
                ..fixtures::descriptor()
            },
        ),
        (
            "a zero-byte archive",
            ModelDescriptor {
                size: 0,
                ..fixtures::descriptor()
            },
        ),
        (
            "a control character",
            ModelDescriptor {
                attribution: "k2\u{0}fsa".into(),
                ..fixtures::descriptor()
            },
        ),
    ] {
        assert_eq!(
            descriptor.validate().unwrap_err().code,
            ModelErrorCode::DescriptorInvalid,
            "{what} was accepted"
        );
    }
    let future = ModelDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION + 1,
        ..fixtures::descriptor()
    };
    assert_eq!(
        future.canonical_bytes().unwrap_err().code,
        ModelErrorCode::SchemaUnsupported
    );
}

/// The JSON is the same shape the speech descriptor wrote, so a catalog and a
/// `.koru-model.json` written before the extraction read unchanged.
#[test]
fn the_json_is_the_shape_voice_wrote() {
    let json: serde_json::Value = serde_json::to_value(fixtures::descriptor()).unwrap();
    assert_eq!(json["engine"], "sherpa");
    assert_eq!(json["tier"], "compact");
    assert_eq!(json["languages"], serde_json::json!(["en", "zh-Hans"]));
    let back: ModelDescriptor = serde_json::from_value(json).unwrap();
    assert_eq!(back, fixtures::descriptor());
}
