// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn routine_retry_notice_keeps_manual_access_and_next_occurrence_guidance() {
    let key = Key::BotsErrRoutineRetryUnsupported;
    assert_eq!(
        t(Lang::En, key),
        "Routine retries aren’t available. Send a new manual message or wait for the next occurrence. New messages use Bot access."
    );
    for lang in Lang::ALL {
        assert!(!t(lang, key).trim().is_empty());
    }
}

// ------------------------------------------------------- the table is honest

/// The lookup is `ROWS[key as usize]`, so the table has to *be* in key order.
/// Everything else in this file rests on it.
#[test]
fn every_row_sits_at_its_own_key() {
    assert_eq!(
        ROWS.len(),
        Key::ALL.len(),
        "a key with no row, or the reverse"
    );
    for (i, key) in Key::ALL.into_iter().enumerate() {
        assert_eq!(ROWS[i].key, key, "row {i} is out of order");
        assert_eq!(key as usize, i, "{key:?} does not index its own row");
    }
}

/// The compiler already refuses a row with a missing field. What it cannot see
/// is an empty string standing in for one.
#[test]
fn no_string_is_empty_in_any_language() {
    for key in Key::ALL {
        for lang in Lang::ALL {
            let value = t(lang, key);
            assert!(
                !value.trim().is_empty(),
                "{key:?} is blank in {}",
                lang.tag()
            );
        }
    }
}

#[test]
fn terminal_cursor_menu_vocabulary_exists_in_every_language() {
    let keys = [
        Key::TerminalCursor,
        Key::TerminalCursorBlink,
        Key::TerminalCursorColor,
        Key::TerminalCursorWidth,
        Key::TerminalCursorHeight,
        Key::TerminalCursorApplication,
        Key::TerminalCursorOn,
        Key::TerminalCursorOff,
        Key::TerminalCursorDefault,
        Key::TerminalCursorBlue,
        Key::TerminalCursorAmber,
        Key::TerminalCursorViolet,
        Key::TerminalCursorThin,
        Key::TerminalCursorMedium,
        Key::TerminalCursorThick,
        Key::TerminalCursor4x,
        Key::TerminalCursor8x,
        Key::TerminalCursorShort,
        Key::TerminalCursorTall,
    ];
    for lang in Lang::ALL {
        assert!(
            keys.into_iter().all(|key| !t(lang, key).trim().is_empty()),
            "{} lacks terminal cursor menu copy",
            lang.tag()
        );
    }
}

/// The Details column's field labels and its closed capability vocabulary, in
/// every language — the surface draws a label beside every value it shows, so a
/// missing one is a value with nothing naming it.
#[test]
fn terminal_details_vocabulary_exists_in_every_language() {
    let keys = [
        Key::TerminalDetails,
        Key::TerminalDetailKind,
        Key::TerminalKindShell,
        Key::TerminalKindAgent,
        Key::TerminalKindCustom,
        Key::TerminalDetailProject,
        Key::TerminalDetailWorkspace,
        Key::TerminalDetailFolder,
        Key::TerminalDetailStatus,
        Key::TerminalDetailUptime,
        Key::TerminalDetailCapabilities,
        Key::TerminalWorkspaceUnknown,
        Key::TerminalCapabilityInput,
        Key::TerminalCapabilityResize,
        Key::TerminalCapabilityInterrupt,
        Key::TerminalCapabilityAttachments,
        Key::TerminalCapabilityScrollback,
        Key::TerminalCapabilityApprovals,
    ];
    for lang in Lang::ALL {
        assert!(
            keys.into_iter().all(|key| !t(lang, key).trim().is_empty()),
            "{} lacks Details column copy",
            lang.tag()
        );
    }
}

/// A placeholder dropped in translation is a name that never appears; one
/// invented is a `{}` on screen. English is the reference because it is what
/// every call site was written against.
#[test]
fn every_language_carries_the_same_placeholders() {
    for key in Key::ALL {
        let expected = t(Lang::En, key).matches("{}").count();
        for lang in Lang::ALL {
            assert_eq!(
                t(lang, key).matches("{}").count(),
                expected,
                "{key:?} in {} has the wrong number of placeholders",
                lang.tag()
            );
        }
    }
}

/// A language nobody can find in a list they cannot read is not offered at all.
#[test]
fn every_language_names_itself() {
    for lang in Lang::ALL {
        assert!(!lang.endonym().is_empty());
        assert!(!lang.tag().is_empty());
    }
    assert_eq!(Lang::Ja.endonym(), "日本語");
    // Eight distinct tags, and eight distinct names.
    let mut tags: Vec<&str> = Lang::ALL.iter().map(|l| l.tag()).collect();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), 8);
}

/// The tags are the store's fixture tags. Two lists of eight that have to agree
/// and live in different crates: this is the assertion that keeps them doing so.
#[test]
fn the_tags_are_the_ones_the_store_fixture_uses() {
    let tags: Vec<&str> = Lang::ALL.iter().map(|l| l.tag()).collect();
    assert_eq!(
        tags,
        vec!["en", "es", "pt-BR", "de", "fr", "ar", "zh-Hans", "ja"]
    );
}

// ------------------------------------------------------------------ matching

#[test]
fn an_exact_tag_matches_whatever_its_case() {
    assert_eq!(Lang::matching("ja"), Some(Lang::Ja));
    assert_eq!(Lang::matching("pt-BR"), Some(Lang::PtBr));
    assert_eq!(Lang::matching("pt-br"), Some(Lang::PtBr));
    assert_eq!(Lang::matching("ZH-HANS"), Some(Lang::ZhHans));
}

/// A region is dropped; the primary subtag is what decides.
#[test]
fn a_region_falls_back_to_the_language() {
    assert_eq!(Lang::matching("en-GB"), Some(Lang::En));
    assert_eq!(Lang::matching("es-MX"), Some(Lang::Es));
    assert_eq!(Lang::matching("de-AT"), Some(Lang::De));
    assert_eq!(Lang::matching("fr-CA"), Some(Lang::Fr));
    assert_eq!(Lang::matching("ar-EG"), Some(Lang::Ar));
    assert_eq!(Lang::matching("ja-JP"), Some(Lang::Ja));
}

/// European Portuguese gets the Brazilian strings. Not perfect, and much closer
/// than English.
#[test]
fn european_portuguese_gets_the_brazilian_strings() {
    assert_eq!(Lang::matching("pt-PT"), Some(Lang::PtBr));
    assert_eq!(Lang::matching("pt"), Some(Lang::PtBr));
}

/// **Two scripts are not two dialects.** `zh-Hant` is Traditional and there are
/// no Traditional strings, so it must not quietly become Simplified — while
/// `zh-CN`, which names no script, may.
#[test]
fn a_script_subtag_has_to_match() {
    assert_eq!(Lang::matching("zh-Hans"), Some(Lang::ZhHans));
    assert_eq!(Lang::matching("zh-Hans-CN"), Some(Lang::ZhHans));
    assert_eq!(Lang::matching("zh-CN"), Some(Lang::ZhHans));
    assert_eq!(Lang::matching("zh"), Some(Lang::ZhHans));

    assert_eq!(
        Lang::matching("zh-Hant"),
        None,
        "Traditional is not Simplified"
    );
    assert_eq!(Lang::matching("zh-Hant-TW"), None);
}

/// A POSIX `$LANG` is not a bare BCP 47 tag, and it is what most of the world's
/// shells hand over.
#[test]
fn a_posix_lang_variable_is_understood() {
    assert_eq!(Lang::matching("ja_JP.UTF-8"), Some(Lang::Ja));
    assert_eq!(Lang::matching("pt_BR.utf8"), Some(Lang::PtBr));
    assert_eq!(Lang::matching("de_DE@euro"), Some(Lang::De));
}

/// **The tags a Mac actually reports.** `NSLocale.preferredLanguages` answers
/// in BCP 47 and almost always carries a region, and for Chinese a script *and*
/// a region — so these are the eight strings `koru_window::system_locale` hands
/// over from a Mac set to each supported language (ADR-175). Written out rather
/// than derived, because the point is that a real platform string resolves and
/// not that this table agrees with itself.
#[test]
fn every_language_a_mac_can_report_resolves_to_its_own() {
    let from_a_mac = [
        ("en-US", Lang::En),
        ("en-GB", Lang::En),
        ("es-ES", Lang::Es),
        ("es-419", Lang::Es),
        ("pt-BR", Lang::PtBr),
        ("pt-PT", Lang::PtBr),
        ("de-DE", Lang::De),
        ("fr-FR", Lang::Fr),
        ("ar-SA", Lang::Ar),
        ("ar-AE", Lang::Ar),
        ("zh-Hans-CN", Lang::ZhHans),
        ("ja-JP", Lang::Ja),
    ];
    for (tag, expected) in from_a_mac {
        assert_eq!(Lang::matching(tag), Some(expected), "{tag}");
        assert_eq!(Lang::for_system(Some(tag)), expected, "{tag}");
    }
    // Every one of the eight is covered by the list above.
    let covered: Vec<Lang> = Lang::ALL
        .into_iter()
        .filter(|lang| from_a_mac.iter().any(|(_, got)| got == lang))
        .collect();
    assert_eq!(covered.len(), 8, "a supported language is untested here");
}

/// A Mac set to a language Koru has no strings for still reports its own tag,
/// and the *nearest supported* is English by the path ADR-87 describes — not by
/// a `None` that never reached the matcher. `zh-Hant-TW` is the one that would
/// be wrong twice over if the script rule were dropped: Traditional strings do
/// not exist, so English is the honest answer and Simplified is not.
#[test]
fn an_unsupported_mac_language_draws_english_by_the_documented_path() {
    for tag in [
        "ko-KR",
        "hi-IN",
        "ru-RU",
        "zh-Hant-TW",
        "zh-Hant-HK",
        "th-TH",
    ] {
        assert_eq!(Lang::matching(tag), None, "{tag}");
        assert_eq!(Lang::for_system(Some(tag)), Lang::En, "{tag}");
    }
}

#[test]
fn an_unsupported_or_malformed_tag_matches_nothing() {
    for tag in ["ko", "ko-KR", "", "-", "!!", "und", "C", "POSIX"] {
        assert_eq!(Lang::matching(tag), None, "{tag:?}");
    }
}

/// The fall-back is a **rendering** decision. What gets stored is the person's
/// own tag, which this function never sees — see the module note.
#[test]
fn an_unmatched_system_draws_english() {
    assert_eq!(Lang::for_system(Some("ko-KR")), Lang::En);
    assert_eq!(Lang::for_system(None), Lang::En);
    assert_eq!(Lang::for_system(Some("ja-JP")), Lang::Ja);
}

#[test]
fn arabic_is_the_one_right_to_left_language() {
    let rtl: Vec<Lang> = Lang::ALL.into_iter().filter(|l| l.is_rtl()).collect();
    assert_eq!(rtl, vec![Lang::Ar]);
}

// ------------------------------------------------------------------- filling

#[test]
fn a_placeholder_takes_its_value_wherever_it_sits() {
    // English puts the name last; Japanese puts it first. The same call has to
    // produce both, which is the whole reason these are templates.
    assert_eq!(
        fill(t(Lang::En, Key::Greeting), &["Steven"]),
        "Good to see you, Steven."
    );
    assert_eq!(
        fill(t(Lang::Ja, Key::Greeting), &["Steven"]),
        "Steven さん、ようこそ。"
    );
    assert_eq!(
        fill(t(Lang::Ar, Key::WorkspaceReady), &["الخطة"]),
        "الخطة جاهزة."
    );
}

/// A mismatched call renders something legible rather than panicking in front
/// of a person. The placeholder-count test is what actually catches the bug.
#[test]
fn a_mismatched_fill_is_legible_rather_than_a_panic() {
    assert_eq!(fill("{} and {}", &["one"]), "one and {}");
    assert_eq!(fill("no slots", &["one"]), "no slots");
    assert_eq!(fill("{}", &[]), "{}");
}

/// **Every step of a run's history has a sentence, in every language.**
///
/// A run's history is a closed vocabulary of ids, counts and step kinds
/// (D45) — it holds no words at all — so the words are entirely this
/// table's. A missing one is a step a person cannot read, on a surface
/// whose whole purpose is understanding a run they never watched (UC3).
#[test]
fn the_run_views_vocabulary_exists_in_every_language() {
    let first = Key::RunTabOverview as usize;
    let last = Key::RunStageTurn as usize;
    for lang in Lang::ALL {
        for index in first..=last {
            let key = Key::ALL[index];
            assert!(
                !t(lang, key).trim().is_empty(),
                "{} lacks run-view copy for {key:?}",
                lang.tag()
            );
        }
    }
}

/// The two takeovers and the one ending never read the same (D36): one is
/// recoverable and says the run keeps going, the other is not and says the
/// run stops for good. A translation that let them collapse into each
/// other would undo the distinction the surface exists to draw.
#[test]
fn ending_a_step_and_ending_a_run_are_different_sentences_in_every_language() {
    for lang in Lang::ALL {
        let step = t(lang, Key::RunTakeOverNowNote);
        let run = t(lang, Key::RunEndRunNote);
        assert_ne!(step, run, "{}", lang.tag());
        assert_ne!(
            t(lang, Key::RunTakeOverNow),
            t(lang, Key::RunEndRun),
            "{}",
            lang.tag()
        );
        assert_ne!(
            t(lang, Key::RunTakeControl),
            t(lang, Key::RunEndRun),
            "{}",
            lang.tag()
        );
    }
}

/// O128: a Bot turn's "Needed you" closing line has both sentences in every
/// language, in sentence case, and neither reads as an error.
#[test]
fn needed_you_closing_lines_exist_in_every_language() {
    assert_eq!(
        t(Lang::En, Key::BotsNeededYouDeclined),
        "You declined this, so the Bot stopped."
    );
    assert_eq!(
        t(Lang::En, Key::BotsNeededYouUnanswered),
        "The Bot stopped because it needed your decision."
    );
    for key in [Key::BotsNeededYouDeclined, Key::BotsNeededYouUnanswered] {
        for lang in Lang::ALL {
            let value = t(lang, key);
            assert!(!value.trim().is_empty(), "{key:?} in {}", lang.tag());
            assert_ne!(
                value,
                t(lang, Key::AiFailureGeneric),
                "{key:?} in {}",
                lang.tag()
            );
        }
    }
}
