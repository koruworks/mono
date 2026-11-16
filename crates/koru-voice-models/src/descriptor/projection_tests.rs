// SPDX-License-Identifier: MPL-2.0

//! **The signed bytes did not change.**
//!
//! Before the extraction, a speech descriptor's canonical form was written by
//! this crate from `EngineId`, `ModelTier` and `VoiceLanguage`. Now it is
//! written by `koru-models` from tokens. These tests hold the two together in
//! three ways:
//!
//! 1. [`canonical_before_the_extraction`] is the function as it was, copied
//!    verbatim from the typed implementation it replaced. Every vector below is
//!    canonicalised by it, by the typed descriptor (which now goes through the
//!    tokens), and by a generic descriptor built **directly from strings** with
//!    no speech type involved — and all three must agree byte for byte.
//! 2. A public key and two signatures made by this crate's generated-key
//!    fixture signer *before* the change, checked in, still verify.
//! 3. `koru_voice::ModelId` and `koru_models::ModelId` restate one charset rule
//!    in two crates; a table of values pins that they agree.

use super::*;
use crate::fixtures;
use crate::trust::TrustedKeys;

/// **The pre-extraction canonicalisation, verbatim.** Only the signature line
/// changed: it was `ModelDescriptor::canonical_bytes(&self)`. Kept here as the
/// reference the new path is measured against; it is not called by anything
/// that ships.
fn canonical_before_the_extraction(this: &ModelDescriptor) -> Result<Vec<u8>, ModelError> {
    const CANONICAL_MAGIC: &str = "koru-model-descriptor";
    if this.schema_version != DESCRIPTOR_SCHEMA_VERSION {
        return Err(ModelErrorCode::SchemaUnsupported.with(format!(
            "cannot canonicalise descriptor schema {}; this build knows {DESCRIPTOR_SCHEMA_VERSION}",
            this.schema_version
        )));
    }
    let languages = this
        .languages
        .iter()
        .map(|l| l.tag())
        .collect::<Vec<_>>()
        .join(",");
    let mut out = String::new();
    out.push_str(CANONICAL_MAGIC);
    out.push('/');
    out.push_str(&this.schema_version.to_string());
    out.push('\n');
    let mut line = |name: &str, value: &str| {
        out.push_str(name);
        out.push('\t');
        out.push_str(value);
        out.push('\n');
    };
    line("id", this.id.as_str());
    line("version", this.version.as_str());
    line("engine", this.engine.as_str());
    line("tier", this.tier.as_str());
    line("languages", &languages);
    line("size", &this.size.to_string());
    line("digest", this.digest.as_str());
    line("working_set_bytes", &this.working_set_bytes.to_string());
    line("licence", this.licence.as_str());
    line(
        "attribution",
        &escape_before_the_extraction(&this.attribution),
    );
    Ok(out.into_bytes())
}

/// The escape as it was, verbatim.
fn escape_before_the_extraction(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// One vector, as plain strings, so it can be built both ways without either
/// way borrowing from the other.
struct Vector {
    id: &'static str,
    version: &'static str,
    engine: &'static str,
    tier: &'static str,
    languages: &'static [&'static str],
    size: u64,
    digest: &'static str,
    working_set_bytes: u64,
    licence: &'static str,
    attribution: &'static str,
}

const ALL_EIGHT: &[&str] = &["en", "es", "pt-BR", "de", "fr", "ar", "zh-Hans", "ja"];
const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The fixture every suite in this crate builds from.
const FIXTURE: Vector = Vector {
    id: "parakeet-tdt-v3",
    version: "1.2.0",
    engine: "sherpa",
    tier: "compact",
    languages: &["en", "zh-Hans"],
    size: 3,
    digest: fixtures::ABC_SHA256,
    working_set_bytes: 1_073_741_824,
    licence: "Apache-2.0",
    attribution: "k2-fsa",
};

/// **Every descriptor this crate's suites, voice's worker suites and the
/// release table build**, field for field: the fixture and each mutation the
/// envelope test makes of it, the attributions the escape tests forge, the
/// second digests the catalog, store and disk suites publish, the worker's
/// scripted and provisioned packages, and the rows `koru-release` publishes.
fn vectors() -> Vec<Vector> {
    vec![
        FIXTURE,
        // `every_behaviour_bearing_field_is_inside_the_envelope`, one field at
        // a time.
        Vector {
            id: "other-model",
            ..FIXTURE
        },
        Vector {
            version: "1.2.1",
            ..FIXTURE
        },
        Vector {
            engine: "whisper",
            ..FIXTURE
        },
        Vector {
            tier: "high_accuracy",
            ..FIXTURE
        },
        Vector {
            languages: &["en"],
            ..FIXTURE
        },
        Vector { size: 4, ..FIXTURE },
        Vector {
            digest: ZEROS,
            ..FIXTURE
        },
        Vector {
            working_set_bytes: 2_147_483_648,
            ..FIXTURE
        },
        Vector {
            licence: "GPL-3.0-only",
            ..FIXTURE
        },
        Vector {
            attribution: "somebody else",
            ..FIXTURE
        },
        // The escape tests: a forged field boundary, and the pair the escape
        // must keep apart.
        Vector {
            attribution: "k2-fsa\nlicence\tGPL-3.0-only",
            ..FIXTURE
        },
        Vector {
            attribution: "a\\nb",
            ..FIXTURE
        },
        Vector {
            attribution: "a\nb",
            ..FIXTURE
        },
        Vector {
            attribution: "back\\slash, tab\there, cr\r, and ümlauts — 中文",
            ..FIXTURE
        },
        // The catalog suite's preferred build and the store suite's accurate
        // one.
        Vector {
            version: "2.0.0",
            digest: "1111111111111111111111111111111111111111111111111111111111111111",
            ..FIXTURE
        },
        Vector {
            tier: "high_accuracy",
            digest: "2222222222222222222222222222222222222222222222222222222222222222",
            ..FIXTURE
        },
        // The disk suite's builds.
        Vector {
            version: "1.0.0",
            size: 128,
            ..FIXTURE
        },
        Vector {
            version: "1.1.0",
            size: 4096,
            ..FIXTURE
        },
        // `koru-voice`'s scripted engine, and its two provisioned packages
        // declaring every language.
        Vector {
            engine: "scripted",
            languages: &["en", "zh-Hans"],
            ..FIXTURE
        },
        Vector {
            engine: "sherpa",
            tier: "high_accuracy",
            languages: ALL_EIGHT,
            ..FIXTURE
        },
        Vector {
            engine: "whisper",
            tier: "compact",
            languages: ALL_EIGHT,
            ..FIXTURE
        },
        Vector {
            languages: &["en", "ja"],
            tier: "high_accuracy",
            ..FIXTURE
        },
        // The rows `koru-release` publishes, with the digests it pins.
        Vector {
            id: "nemotron-3.5-streaming-0.6b-160ms",
            version: "int8-2026-06-11",
            engine: "sherpa",
            tier: "high_accuracy",
            languages: ALL_EIGHT,
            size: 475_273_363,
            digest: "8e3ca0895ed78a04a3a86cd16530d41dbf7003ec2eec198bd3b25ed6ca33f229",
            working_set_bytes: 2 * 1024 * 1024 * 1024,
            licence: "OpenMDW-1.1",
            attribution: "NVIDIA Corporation, Nemotron Speech Streaming (OpenMDW-1.1)",
        },
        Vector {
            id: "parakeet-tdt-0.6b-v3",
            version: "int8",
            engine: "sherpa",
            tier: "high_accuracy",
            languages: &["en", "es", "pt-BR", "de", "fr"],
            size: 487_170_055,
            digest: ZEROS,
            working_set_bytes: 2 * 1024 * 1024 * 1024,
            licence: "CC-BY-4.0",
            attribution: "NVIDIA Corporation, Parakeet TDT 0.6B v3 (CC-BY-4.0); \
                          converted by k2-fsa (Apache-2.0)",
        },
        Vector {
            id: "whisper-base",
            version: "q5_1",
            engine: "whisper",
            tier: "compact",
            languages: ALL_EIGHT,
            size: 59_707_625,
            digest: ZEROS,
            working_set_bytes: 512 * 1024 * 1024,
            licence: "MIT",
            attribution: "OpenAI, Whisper (MIT); ggml conversion by ggerganov (MIT)",
        },
    ]
}

/// Through the typed path: speech's own types, exactly as a caller builds one.
fn typed(v: &Vector) -> ModelDescriptor {
    ModelDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION,
        id: ModelId::parse(v.id).unwrap(),
        version: ModelVersion::parse(v.version).unwrap(),
        engine: EngineId::parse(v.engine).unwrap(),
        tier: ModelTier::parse(v.tier).unwrap(),
        languages: v
            .languages
            .iter()
            .map(|tag| {
                VoiceLanguage::ALL
                    .into_iter()
                    .find(|language| language.tag() == *tag)
                    .unwrap()
            })
            .collect(),
        size: v.size,
        digest: Sha256Digest::parse(v.digest).unwrap(),
        working_set_bytes: v.working_set_bytes,
        licence: LicenceRef::parse(v.licence).unwrap(),
        attribution: v.attribution.to_string(),
    }
}

/// Through the token path: no speech type anywhere.
fn tokens(v: &Vector) -> koru_models::ModelDescriptor {
    koru_models::ModelDescriptor {
        schema_version: koru_models::DESCRIPTOR_SCHEMA_VERSION,
        id: koru_models::ModelId::parse(v.id).unwrap(),
        version: ModelVersion::parse(v.version).unwrap(),
        engine: Token::parse(v.engine).unwrap(),
        tier: Token::parse(v.tier).unwrap(),
        languages: v
            .languages
            .iter()
            .map(|tag| LanguageTag::parse(*tag).unwrap())
            .collect(),
        size: v.size,
        digest: Sha256Digest::parse(v.digest).unwrap(),
        working_set_bytes: v.working_set_bytes,
        licence: LicenceRef::parse(v.licence).unwrap(),
        attribution: v.attribution.to_string(),
    }
}

/// **The test the roadmap names.** For every vector: the bytes the code wrote
/// before the extraction, the bytes the typed descriptor writes now, and the
/// bytes a descriptor built from tokens alone writes, are one sequence.
#[test]
fn the_typed_path_and_the_token_path_sign_the_same_bytes_as_before() {
    for v in vectors() {
        let typed = typed(&v);
        let tokens = tokens(&v);
        let before = canonical_before_the_extraction(&typed).unwrap();
        assert_eq!(
            typed.canonical_bytes().unwrap(),
            before,
            "the typed path moved the bytes of {}/{}",
            v.id,
            v.version
        );
        assert_eq!(
            tokens.canonical_bytes().unwrap(),
            before,
            "the token path moved the bytes of {}/{}",
            v.id,
            v.version
        );
        // And the two descriptors are each other's projection, both ways.
        assert_eq!(typed.to_model().unwrap(), tokens);
        assert_eq!(ModelDescriptor::from_model(tokens.clone()).unwrap(), typed);
        // The JSON a catalog carries is the same document too.
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            serde_json::to_value(&tokens).unwrap()
        );
    }
}

/// The two descriptors refuse the same things, in the same order: a schema
/// from the future before anything else, then the rules no single field sees.
#[test]
fn both_paths_refuse_the_same_descriptors() {
    let future = ModelDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION + 1,
        ..fixtures::descriptor()
    };
    assert_eq!(
        canonical_before_the_extraction(&future).unwrap_err(),
        future.canonical_bytes().unwrap_err()
    );
    let twice = ModelDescriptor {
        languages: vec![VoiceLanguage::En, VoiceLanguage::En],
        ..fixtures::descriptor()
    };
    let error = twice.validate().unwrap_err();
    assert_eq!(error.code, ModelErrorCode::DescriptorInvalid);
    assert_eq!(error.message, "language en is declared twice");
}

const BEFORE_PUBLIC_KEY: &str = "untrusted comment: minisign public key: 3EF16044D44BE0C6\nRWTG4EvURGDxPjcBbqDkol2WOWJ7/y5X1GwNaxQgGCqRnBN5xK2DxBAm\n";
const BEFORE_SIGNATURE_FIXTURE: &str = "untrusted comment: koru model fixture\nRUTG4EvURGDxPi40Ryu1tjteK0Rmy/+ztEEQi76kJr5lxrYHIy9FzGt772CDx2nbBRPt3+AZie1pu4qRJsQvecpeJrKrABz/eQc=\ntrusted comment: koru model fixture\nyKIuNSGdm5lXer28IejGWdYxHaJq8ROEg3iACXek0ayg3asnPUeRcH8L++RQ+Fv/4tqE9UqUS5RiO1eGzxQjAg==\n";
const BEFORE_SIGNATURE_WIDE: &str = "untrusted comment: koru model fixture\nRUTG4EvURGDxPiYXbyWuM8JFHEY7ROpJT9aUIGDSsP2cksuvVkVhgREO7wW3KyGPYixann6TnOVU9ONN03yTQDSrnSd+1DHBYAY=\ntrusted comment: koru model fixture\nM/mrDMqevqfRZPIAZcGWDPrpIpPYHIW4fjfnsN6vFpZrOBg2ZcEom18CwBsw8D1jICYw+19XOcgdpJ9ffvaGBw==\n";

/// **A signed descriptor produced before the change still verifies.**
///
/// The key pair is one this crate's `fixtures::Signer` generated, and the two
/// signatures are what it made, on the tree before the extraction, over
/// `fixtures::descriptor()` and over a second descriptor carrying the other
/// engine, the other tier, every language and every escape. The public half
/// and the signatures are checked in because regenerating them would prove
/// nothing; the secret half was never kept.
#[test]
fn a_signature_made_before_the_extraction_still_verifies() {
    let keys = TrustedKeys::from_public_keys(&[BEFORE_PUBLIC_KEY]).unwrap();
    let wide = ModelDescriptor {
        engine: EngineId::parse("whisper").unwrap(),
        tier: ModelTier::HighAccuracy,
        languages: VoiceLanguage::ALL.to_vec(),
        attribution: "k2-fsa\\nvia\ttab\r\nend".to_string(),
        ..fixtures::descriptor()
    };
    let fixture_signature = ModelSignature::parse(BEFORE_SIGNATURE_FIXTURE).unwrap();
    let wide_signature = ModelSignature::parse(BEFORE_SIGNATURE_WIDE).unwrap();

    keys.verify(&fixtures::descriptor(), &fixture_signature)
        .unwrap();
    keys.verify(&wide, &wide_signature).unwrap();
    // Through a catalog entry, which is how an install meets it.
    let entry = crate::CatalogEntry {
        signature: wide_signature.clone(),
        ..fixtures::entry(&fixtures::Signer::new(), wide.clone())
    };
    entry.verify(&keys).unwrap();
    // And through the generic descriptor the typed one projects.
    keys.verify(&wide.to_model().unwrap(), &wide_signature)
        .unwrap();
    // The signatures are bound to their bytes: swapped, neither verifies.
    assert_eq!(
        keys.verify(&wide, &fixture_signature).unwrap_err().code,
        ModelErrorCode::SignatureInvalid
    );
}

/// A token speech has no word for is refused rather than guessed, so a model
/// that is not a speech model is never one by accident.
#[test]
fn a_token_speech_has_no_word_for_is_refused() {
    let base = fixtures::descriptor().to_model().unwrap();
    let unknown_tier = koru_models::ModelDescriptor {
        tier: Token::parse("mobile").unwrap(),
        ..base.clone()
    };
    assert_eq!(
        ModelDescriptor::from_model(unknown_tier).unwrap_err().code,
        ModelErrorCode::DescriptorInvalid
    );
    let unknown_language = koru_models::ModelDescriptor {
        languages: vec![LanguageTag::parse("ko").unwrap()],
        ..base.clone()
    };
    assert_eq!(
        ModelDescriptor::from_model(unknown_language)
            .unwrap_err()
            .code,
        ModelErrorCode::DescriptorInvalid
    );
    // An engine voice has never run is still an engine id: whether a host can
    // run it is `LoadedModel::open`'s question, as it always was.
    let other_engine = koru_models::ModelDescriptor {
        engine: Token::parse("onnx-ocr").unwrap(),
        ..base
    };
    assert_eq!(
        ModelDescriptor::from_model(other_engine)
            .unwrap()
            .engine
            .as_str(),
        "onnx-ocr"
    );
}

/// `koru_voice::ModelId` and `koru_models::ModelId` are one rule written in two
/// crates, because the model store may not depend on a speech crate. This is
/// the table that keeps them one rule.
#[test]
fn the_two_model_id_rules_agree() {
    let long = "x".repeat(64);
    let longer = "x".repeat(65);
    for value in [
        "parakeet-tdt-v3",
        "whisper-base",
        "a",
        "1.2.0",
        "int8_2026",
        long.as_str(),
        longer.as_str(),
        "",
        ".",
        "..",
        ".staging",
        "Upper",
        "with space",
        "tab\t",
        "a/b",
        "a\\b",
        "C:",
        "ünicode",
        "a,b",
    ] {
        assert_eq!(
            ModelId::parse(value).is_ok(),
            koru_models::ModelId::parse(value).is_ok(),
            "{value:?}"
        );
        assert_eq!(
            EngineId::parse(value).is_ok(),
            Token::parse(value).is_ok(),
            "{value:?}"
        );
    }
}
