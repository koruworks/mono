// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn identical_text_has_no_errors_whatever_its_white_space() {
    let errors = errors(
        "The quarterly invoice\nis due.",
        "The quarterly  invoice is due.",
    );
    assert_eq!(errors.edits, 0);
    assert_eq!(errors.rate(), Some(0.0));
}

#[test]
fn a_misread_character_is_one_edit_over_the_expected_length() {
    let errors = errors("Closed on Sundays", "Closed on Sundavs");
    assert_eq!(
        errors,
        Errors {
            edits: 1,
            expected_chars: 15
        }
    );
    assert_eq!(rate_text(errors.rate()), "0.067");
}

#[test]
fn a_file_rate_sums_edits_and_lengths() {
    let total = errors("abcd", "abcd").plus(errors("efgh", "efgx"));
    assert_eq!(
        total,
        Errors {
            edits: 1,
            expected_chars: 8
        }
    );
    assert_eq!(total.rate(), Some(0.125));
    assert_eq!(errors("", "anything").rate(), None, "nothing expected");
}

/// Two words read together have no character wrong and are not found: what
/// word recall is for (Plan 0.1's noisy scan).
#[test]
fn words_run_together_lose_recall_not_characters() {
    assert_eq!(errors("send the contract", "sendthe contract").edits, 0);
    assert_eq!(recall("send the contract", "sendthe contract"), (1, 3));
    assert_eq!(
        recall("an ox is in it", "an ox"),
        (0, 0),
        "short words do not count"
    );
}

#[test]
fn levenshtein_counts_insertions_deletions_and_substitutions() {
    let chars = |s: &str| s.chars().collect::<Vec<_>>();
    assert_eq!(levenshtein(&chars("kitten"), &chars("sitting")), 3);
    assert_eq!(levenshtein(&chars(""), &chars("abc")), 3);
    assert_eq!(levenshtein(&chars("日本語"), &chars("日本")), 1);
}

#[test]
fn rates_and_speeds_print_like_the_measurement() {
    assert_eq!(rate_text(None), "-");
    assert_eq!(per_second(7, 1.0), "7.00");
    assert_eq!(per_second(3, 0.0), "-");
}
