// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_i18n::Lang;

/// A rooted acceptance stack must neither read nor write the real machine's
/// consent. Agent and the window meet on this exact sibling path.
#[test]
fn a_rooted_consent_record_lives_beside_the_rooted_agent() {
    let root = std::path::Path::new("C:/isolated-koru");
    assert_eq!(
        consent_path(Some(root)),
        Some(root.join("agent").join(koru_voice_consent::CONSENT_FILE))
    );
}

fn at(stage: InstallStage, received: u64, total: u64) -> InstallProgress {
    InstallProgress {
        stage,
        received,
        total,
    }
}

/// **Every stage says something, and only one of them shows a bar.**
///
/// The bar is the download's alone: a determinate bar sitting still at 100%
/// through a two-minute re-hash reads as a hang, which is the reason
/// `InstallStage::Verifying` exists at all.
#[test]
fn each_stage_has_a_sentence_and_only_the_download_has_a_bar() {
    let (preparing, bar) = progress_line(Lang::En, at(InstallStage::Resolving, 0, 0));
    assert!(!preparing.is_empty());
    assert_eq!(bar, None);

    let (downloading, bar) = progress_line(Lang::En, at(InstallStage::Downloading, 50, 200));
    assert!(downloading.contains("25"), "{downloading}");
    assert_eq!(bar, Some(0.25));

    for stage in [
        InstallStage::Verifying,
        InstallStage::Unpacking,
        InstallStage::Publishing,
    ] {
        let (line, bar) = progress_line(Lang::En, at(stage, 0, 0));
        assert!(!line.is_empty(), "{stage:?} says nothing");
        assert_eq!(bar, None, "{stage:?} drew a bar");
    }
}

/// A download whose total is not known yet is indeterminate rather than a
/// division by zero.
#[test]
fn a_download_with_no_total_is_indeterminate() {
    let (line, bar) = progress_line(Lang::En, at(InstallStage::Downloading, 900, 0));
    assert!(!line.is_empty());
    assert_eq!(bar, None);
}

/// More received than the total — a server that lied, a resume that
/// over-counted — clamps rather than drawing a bar past its own end.
#[test]
fn a_bar_cannot_pass_its_own_end() {
    let (_, bar) = progress_line(Lang::En, at(InstallStage::Downloading, 500, 200));
    assert_eq!(bar, Some(1.0));
}

/// **Every sentence resolves in all eight languages**, which is the house rule
/// and is easy to break by adding a row in English and moving on.
#[test]
fn every_stage_speaks_all_eight_languages() {
    for lang in [
        Lang::En,
        Lang::Es,
        Lang::PtBr,
        Lang::De,
        Lang::Fr,
        Lang::Ar,
        Lang::ZhHans,
        Lang::Ja,
    ] {
        for stage in [
            InstallStage::Resolving,
            InstallStage::Downloading,
            InstallStage::Verifying,
            InstallStage::Unpacking,
            InstallStage::Publishing,
        ] {
            let (line, _) = progress_line(lang, at(stage, 1, 2));
            assert!(
                !line.trim().is_empty(),
                "{lang:?} says nothing at {stage:?}"
            );
        }
    }
}

/// Somebody pressing **Stop** is not a failure, and the toast must not follow
/// *Canceling* with a sentence saying the model could not be installed.
#[test]
fn a_cancellation_is_told_apart_from_a_failure() {
    assert!(was_cancelled(&ModelErrorCode::Cancelled.with("stopped")));
    for other in [
        ModelErrorCode::DownloadFailed,
        ModelErrorCode::NoTrustRoot,
        ModelErrorCode::ConsentRequired,
        ModelErrorCode::Io,
    ] {
        assert!(!was_cancelled(&other.with("x")), "{other} is not a Stop");
    }
}

// ----------------------------------------------------------- trust policy

/// **A pinned key wins, and there is no way to ask for anything else.**
///
/// The MVP's digest-only arm is reachable only while `MODEL_SIGNING_KEYS` is
/// empty; the day it is not, this function answers `Signed` and the other arm
/// is dead code somebody deletes. That is the whole deletion plan, asserted.
#[test]
fn a_pinned_key_forces_the_signed_policy() {
    if koru_voice_models::MODEL_SIGNING_KEYS.is_empty() {
        // The state of the tree today: the guarded arm, or `Signed` on stable.
        let chosen = trust_policy();
        let expected = TrustPolicy::digest_only()
            .map(|p| p.is_digest_only())
            .unwrap_or(false);
        assert_eq!(chosen.is_digest_only(), expected);
    } else {
        assert_eq!(trust_policy(), TrustPolicy::SIGNED);
    }
}

/// **A stable build never chooses the MVP arm**, however this window is built:
/// `digest_only` refuses there and the fall-back is the policy that refuses the
/// install outright, which is the failure a public release must have.
#[test]
fn a_stable_build_falls_back_to_signed() {
    assert!(TrustPolicy::digest_only_on(Some("stable")).is_err());
}

// ------------------------------------------------- the sentences a person gets

/// **Every catalog failure gets its own sentence**, and the sentences are
/// asserted rather than merely counted.
///
/// This is the whole of the bug that prompted the pass: a person set
/// `KORU_VOICE_CATALOG` to a host with nothing published on it, pressed
/// Install, and got *"No voice model is available to download yet"* — which is
/// true of exactly one of these four and misleading about the other three.
/// Asserting the *codes are distinct* would have passed before the fix, so
/// what is asserted is what somebody reads.
#[test]
fn every_catalog_failure_says_something_a_person_can_act_on() {
    let unreachable = catalog_failure(&ModelErrorCode::Offline.with("dns"));
    assert!(
        unreachable.contains("Could not reach") && unreachable.contains("connection"),
        "{unreachable}"
    );

    let missing = catalog_failure(&ModelErrorCode::CatalogUnavailable.with("404"));
    assert!(
        missing.contains("not published") && missing.contains("KORU_VOICE_CATALOG"),
        "a 404 has to name the thing to check: {missing}"
    );

    let malformed = catalog_failure(&ModelErrorCode::CatalogInvalid.with("bad json"));
    assert!(malformed.contains("could not be read"), "{malformed}");

    let nothing_for_us = catalog_failure(&ModelErrorCode::LanguageUnsupported.with("no compact"));
    assert!(
        nothing_for_us.contains("no speech model this build can install"),
        "{nothing_for_us}"
    );

    let all = [&unreachable, &missing, &malformed, &nothing_for_us];
    for (i, one) in all.iter().enumerate() {
        for (j, other) in all.iter().enumerate() {
            if i != j {
                assert_ne!(one, other, "two catalog failures say the same thing");
            }
        }
    }
}

/// **No code reaches a person as silence, or as a token.**
///
/// Every arm answers a non-empty sentence that reads as English rather than as
/// a code — the fall-through arm names its code *inside* a sentence, which is
/// the honest answer for a failure nobody wrote copy for and is still not the
/// same as printing `catalog_invalid` at somebody.
#[test]
fn no_model_failure_is_ever_wordless() {
    for code in ModelErrorCode::ALL {
        for line in [
            catalog_failure(&code.with("detail")),
            install_failure(&code.with("detail")),
        ] {
            assert!(!line.trim().is_empty(), "{code:?} says nothing");
            assert!(
                line.contains(' '),
                "{code:?} answers a token rather than a sentence: {line}"
            );
            assert!(
                line.ends_with('.') || line.ends_with(".)"),
                "{code:?} is not a sentence: {line}"
            );
        }
    }
}

/// **A download failure and a verification failure are not the same news.**
///
/// One is *try again*; the other is *nothing was installed and something is
/// wrong with the bytes*. The old single sentence said neither.
#[test]
fn an_install_failure_says_which_kind_it_was() {
    let network = install_failure(&ModelErrorCode::DownloadFailed.with("reset"));
    assert!(network.contains("connection"), "{network}");
    assert!(
        network.contains("carry on"),
        "a resume is worth saying: {network}"
    );

    let signature = install_failure(&ModelErrorCode::SignatureInvalid.with("bad sig"));
    assert!(signature.contains("signature"), "{signature}");
    assert!(signature.contains("Nothing was installed"), "{signature}");

    let disk = install_failure(&ModelErrorCode::Io.with("no space"));
    assert!(disk.contains("room on the drive"), "{disk}");

    assert_ne!(network, signature);
    assert_ne!(network, disk);
    assert_ne!(signature, disk);
}

/// **A press that starts nothing still says why.** Both of these used to be a
/// silent `return`, which makes *Install…* indistinguishable from a broken
/// button.
#[test]
fn a_setup_that_cannot_start_is_not_silent() {
    assert_eq!(
        setup_refused(false, false),
        None,
        "the ordinary case proceeds"
    );

    let busy = setup_refused(true, false).expect("a busy press says something");
    assert!(busy.contains("already being set up"), "{busy}");

    let off = setup_refused(false, true).expect("a switched-off press says something");
    assert!(off.contains("KORU_VOICE_DISABLED"), "{off}");

    // The kill switch outranks being busy: an installation that offers no
    // dictation has nothing in flight worth reporting.
    assert_eq!(setup_refused(true, true), setup_refused(false, true));
}

// ------------------------------------------------------- what a tier weighs

/// **The size is a descriptor's, and an unread one is no size at all.**
///
/// A literal in the UI drifts from what actually downloads the first time a
/// model is repackaged, so the only two sources are the installed cache and a
/// catalog that was actually fetched — and before either has happened the
/// honest answer is `None`.
#[test]
fn a_tier_weighs_what_a_descriptor_said_or_nothing() {
    // Bytes to megabytes is `koru_voice_consent::megabytes`, rounded once here
    // rather than at each surface.
    note_tier_size(ModelTier::HighAccuracy, 475 * 1024 * 1024);
    assert_eq!(tier_megabytes(ModelTier::HighAccuracy), Some(475));

    // A descriptor claiming nothing records nothing: zero is not a size, and
    // storing it would make "unknown" and "empty" the same answer.
    let was = tier_megabytes(ModelTier::Compact);
    note_tier_size(ModelTier::Compact, 0);
    assert_eq!(tier_megabytes(ModelTier::Compact), was);
}

#[test]
fn the_product_offer_combines_both_downloads_and_both_licences() {
    let offer = VoiceOffer {
        terms: vec![
            Terms {
                model: "whisper-base".into(),
                version: "q5_1".into(),
                licence: "MIT".into(),
                attribution: "whisper.cpp".into(),
                download_bytes: 57 * 1024 * 1024,
            },
            Terms {
                model: "nemotron".into(),
                version: "int8".into(),
                licence: "OpenMDW-1.1".into(),
                attribution: "NVIDIA".into(),
                download_bytes: 475 * 1024 * 1024,
            },
        ],
    };
    assert_eq!(offer.megabytes(), 532);
}

fn shipped_offer() -> VoiceOffer {
    VoiceOffer {
        terms: vec![
            Terms {
                model: "whisper-base".into(),
                version: "q5_1".into(),
                licence: "MIT".into(),
                attribution: "OpenAI, Whisper base (MIT); GGML quantisation published by the \
                              whisper.cpp project"
                    .into(),
                download_bytes: 57 * 1024 * 1024,
            },
            Terms {
                model: "nemotron-3.5-streaming-0.6b-160ms".into(),
                version: "int8".into(),
                licence: "OpenMDW-1.1".into(),
                attribution: "NVIDIA Corporation, Nemotron 3.5 ASR Streaming 0.6B (OpenMDW-1.1); \
                              ONNX export by the k2-fsa sherpa-onnx project (Apache-2.0)"
                    .into(),
                download_bytes: 475 * 1024 * 1024,
            },
        ],
    }
}

/// The catalog's own attributions read into publisher, model and the credits
/// after the first `;` — the shape the release table writes.
#[test]
fn a_catalog_attribution_reads_as_publisher_model_and_further_credit() {
    let offer = shipped_offer();
    assert_eq!(
        Credit::of(&offer.terms[0].attribution, "MIT"),
        Some(Credit {
            publisher: "OpenAI",
            model: "Whisper base",
            further: Some("GGML quantisation published by the whisper.cpp project"),
        })
    );
    assert_eq!(
        Credit::of(&offer.terms[1].attribution, "OpenMDW-1.1"),
        Some(Credit {
            publisher: "NVIDIA Corporation",
            model: "Nemotron 3.5 ASR Streaming 0.6B",
            further: Some("ONNX export by the k2-fsa sherpa-onnx project (Apache-2.0)"),
        })
    );
    assert_eq!(
        Credit::of("OpenAI, Whisper base (MIT)", "MIT"),
        Some(Credit {
            publisher: "OpenAI",
            model: "Whisper base",
            further: None,
        })
    );
}

/// **Anything off the shape is not guessed at**: the caller shows the whole
/// attribution instead.
#[test]
fn an_attribution_off_the_shape_is_not_guessed_at() {
    for (attribution, licence) in [
        ("whisper.cpp", "MIT"),
        ("NVIDIA", "OpenMDW-1.1"),
        // The parenthesised licence is not the offer's.
        ("OpenAI, Whisper base (Apache-2.0)", "MIT"),
        // No licence in parentheses at all.
        ("OpenAI, Whisper base", "MIT"),
        // A comma too many: which half is the publisher?
        ("Foo, Inc., Model (MIT)", "MIT"),
        // An empty half.
        (", Whisper base (MIT)", "MIT"),
        ("OpenAI, (MIT)", "MIT"),
    ] {
        assert_eq!(Credit::of(attribution, licence), None, "{attribution}");
    }
}

/// The consent step is a list: a lead line, one line per model with its name,
/// size, publisher and licence, the further credits under it, and a closing
/// line — never the old paragraph of joined licences and credits.
#[test]
fn the_consent_body_lists_one_line_per_model() {
    let body = shipped_offer().consent_body(koru_i18n::Lang::En);
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines,
        vec![
            "To support dictation, Koru will download:",
            "",
            "\u{2022} Whisper base, 57 MB, by OpenAI (MIT)",
            "   GGML quantisation published by the whisper.cpp project",
            "\u{2022} Nemotron 3.5 ASR Streaming 0.6B, 475 MB, by NVIDIA Corporation (OpenMDW-1.1)",
            "   ONNX export by the k2-fsa sherpa-onnx project (Apache-2.0)",
            "",
            "Koru uses your microphone; all models run locally and nothing leaves your computer.",
        ]
    );
    // Every language assembles the same shape from its own templates.
    for lang in koru_i18n::Lang::ALL {
        let body = shipped_offer().consent_body(lang);
        assert_eq!(body.lines().count(), 8, "{}", lang.tag());
        assert!(!body.contains("{}"), "{}", lang.tag());
        assert!(body.contains("NVIDIA Corporation"), "{}", lang.tag());
        assert!(body.contains("OpenMDW-1.1"), "{}", lang.tag());
    }
}

/// An attribution off the shape is the whole line, with the size — and the
/// licence, because the attribution did not name it.
#[test]
fn an_unparsed_attribution_is_shown_whole_with_size_and_licence() {
    let mut offer = shipped_offer();
    offer.terms.truncate(1);
    offer.terms[0].attribution = "whisper.cpp".into();
    let body = offer.consent_body(koru_i18n::Lang::En);
    assert!(
        body.lines()
            .any(|line| line == "\u{2022} whisper.cpp, 57 MB (MIT)"),
        "{body}"
    );
}

#[test]
fn installed_is_not_the_same_as_both_tiers_ready() {
    let partial = VoiceModels {
        builds: 1,
        bytes: 57,
        tiers: [true, false],
    };
    assert!(partial.installed());
    assert!(!partial.complete());

    assert!(
        VoiceModels {
            builds: 2,
            bytes: 532,
            tiers: [true, true],
        }
        .complete()
    );
}
