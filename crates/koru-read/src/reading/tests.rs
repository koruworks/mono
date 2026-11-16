// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **One spelling per token, on the wire and in the document.** The macro makes
/// serde's name and `as_str` one table; this proves it, so a hand edit to
/// either side fails here rather than as a reading one build writes and
/// another cannot parse.
#[test]
fn every_token_is_spelled_the_same_on_the_wire_and_in_the_document() {
    fn check<T>(all: &[T], as_str: fn(T) -> &'static str, parse: fn(&str) -> Option<T>)
    where
        T: Copy + PartialEq + std::fmt::Debug + Serialize + serde::de::DeserializeOwned,
    {
        for &value in all {
            let token = as_str(value);
            assert_eq!(
                serde_json::to_value(value).unwrap(),
                serde_json::Value::String(token.to_string())
            );
            assert_eq!(
                serde_json::from_value::<T>(serde_json::json!(token)).unwrap(),
                value
            );
            assert_eq!(parse(token), Some(value));
        }
    }
    check(
        ReadingStatus::ALL,
        ReadingStatus::as_str,
        ReadingStatus::parse,
    );
    check(Reason::ALL, Reason::as_str, Reason::parse);
    check(Source::ALL, Source::as_str, Source::parse);
}

/// The tokens are the design's (TECHNICAL §7 "Content"), and they are stored
/// on every device: pinned literally, because changing one orphans every
/// reading already written with it.
#[test]
fn the_stored_tokens_are_the_designs() {
    let spell = |all: &[&'static str]| all.join(" ");
    assert_eq!(
        spell(
            &ReadingStatus::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        ),
        "read empty partial failed"
    );
    assert_eq!(
        spell(&Reason::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
        "password damaged page-cap"
    );
    assert_eq!(
        spell(&Source::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
        "text-layer recognized mixed plain"
    );
    assert_eq!(READER_TEXT, "text");
    assert_eq!((READ_PAGE_CAP, TEXT_PAGE_CAP), (200, 2_000));
}

/// A token a newer build wrote is not an error, and it is not a guess.
#[test]
fn a_token_this_build_does_not_know_parses_as_none() {
    assert_eq!(ReadingStatus::parse("skipped"), None);
    assert_eq!(Reason::parse("Password"), None, "tokens are case-sensitive");
    assert_eq!(Source::parse(""), None);
}

/// Only device-independent outcomes have a reason, and each status takes only
/// the reasons that mean something with it.
#[test]
fn a_status_admits_only_the_reasons_the_design_pairs_it_with() {
    use ReadingStatus::*;
    for status in ReadingStatus::ALL {
        for reason in std::iter::once(None).chain(Reason::ALL.iter().map(|r| Some(*r))) {
            let expected = matches!(
                (status, reason),
                (Read | Empty, None)
                    | (Failed, Some(Reason::Password | Reason::Damaged))
                    | (Partial, None | Some(Reason::PageCap))
            );
            assert_eq!(
                status.admits(reason),
                expected,
                "{status:?} with {reason:?}"
            );
        }
    }
}

/// A `partial` stopped at a cap is finished at its generation; a `partial`
/// still being read is not — which is what lets a restart resume it.
#[test]
fn only_a_capped_partial_counts_as_finished() {
    assert!(ReadingStatus::Read.is_finished(None));
    assert!(ReadingStatus::Empty.is_finished(None));
    assert!(ReadingStatus::Failed.is_finished(Some(Reason::Damaged)));
    assert!(ReadingStatus::Partial.is_finished(Some(Reason::PageCap)));
    assert!(!ReadingStatus::Partial.is_finished(None));
}

/// The coordinator's queue condition, in one place: an entry at a newer
/// generation is never this build's to read again.
#[test]
fn a_reading_is_settled_when_finished_here_or_written_by_a_newer_generation() {
    let at = |generation, status| Reading {
        generation,
        status,
        ..Reading::default()
    };
    assert!(at(GENERATION, Some(ReadingStatus::Read)).is_settled_for_this_build());
    assert!(!at(GENERATION, Some(ReadingStatus::Partial)).is_settled_for_this_build());
    assert!(!at(GENERATION, None).is_settled_for_this_build());
    assert!(at(GENERATION + 1, Some(ReadingStatus::Partial)).is_settled_for_this_build());
    assert!(at(GENERATION + 1, None).is_settled_for_this_build());
    assert!(!at(GENERATION - 1, Some(ReadingStatus::Read)).is_settled_for_this_build());
}

#[test]
fn a_reader_id_is_a_short_lowercase_token() {
    assert_eq!(ReaderId::text().as_str(), "text");
    assert!(ReaderId::parse("pp-ocr").is_some());
    assert!(ReaderId::parse("v2-reader-9").is_some());
    for bad in ["", "Text", "a b", "a/b", "a\nb", "é", &"x".repeat(33)] {
        assert!(ReaderId::parse(bad).is_none(), "{bad:?} was accepted");
    }
    assert!(serde_json::from_str::<ReaderId>("\"Bad Token\"").is_err());
    assert_eq!(
        serde_json::to_string(&ReaderId::text()).unwrap(),
        "\"text\""
    );
}

/// Text layer and recognition together are mixed; nothing else is invented.
#[test]
fn sources_combine_to_mixed_only_when_they_differ() {
    assert_eq!(
        Source::TextLayer.combine(Source::TextLayer),
        Source::TextLayer
    );
    assert_eq!(
        Source::Recognized.combine(Source::Recognized),
        Source::Recognized
    );
    assert_eq!(Source::TextLayer.combine(Source::Recognized), Source::Mixed);
    assert_eq!(Source::Recognized.combine(Source::TextLayer), Source::Mixed);
    assert_eq!(Source::Mixed.combine(Source::TextLayer), Source::Mixed);
}

/// **A page's words survive being printed at any level, as a length.** The
/// enclosing types derive `Debug`, so this is the only defence that holds.
#[test]
fn a_page_prints_as_its_length_and_never_its_words() {
    let secret = "zorblax quintessential アイウ";
    let page = PageText::new(secret);
    let reading = Reading {
        pages: [(1, page.clone())].into_iter().collect(),
        ..Reading::default()
    };
    for printed in [
        format!("{page:?}"),
        format!("{page:#?}"),
        format!("{reading:?}"),
        format!("{reading:#?}"),
    ] {
        assert!(!printed.contains("zorblax"), "{printed}");
        assert!(!printed.contains("アイウ"), "{printed}");
    }
    assert_eq!(format!("{page:?}"), "PageText([REDACTED] chars=26)");
    // And the wire carries it whole: redaction is a `Debug` property only.
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        serde_json::json!(secret)
    );
}
