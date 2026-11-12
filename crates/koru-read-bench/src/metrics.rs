// SPDX-License-Identifier: MPL-2.0

//! **How close a reading is to the text it should be**, measured the way Plan
//! 0.1 measured it (`docs/features/ocr/MEASUREMENTS.md` §4.2):
//!
//! - **character error rate**: the edit distance between the two texts with
//!   every white-space character removed, over the expected text's length —
//!   so a line break where the page had a space costs nothing, and a word
//!   read wrong costs its letters;
//! - **word recall**: the share of the expected text's words of three or more
//!   letters that appear among the reading's words — what search would miss.
//!   It counts what character error rate cannot: two words run together read
//!   with no character wrong, and are not found.

use std::collections::HashSet;

/// Edits and length for one comparison, so a file's rate is the sum of its
/// pages' edits over the sum of their lengths, not a mean of rates.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Errors {
    pub edits: usize,
    pub expected_chars: usize,
}

impl Errors {
    pub fn plus(self, other: Errors) -> Errors {
        Errors {
            edits: self.edits + other.edits,
            expected_chars: self.expected_chars + other.expected_chars,
        }
    }

    /// The rate, or `None` when nothing was expected (then any character read
    /// is an error with nothing to divide it by).
    pub fn rate(self) -> Option<f64> {
        (self.expected_chars > 0).then(|| self.edits as f64 / self.expected_chars as f64)
    }
}

/// Compare `got` with `expected`, white space ignored.
pub fn errors(expected: &str, got: &str) -> Errors {
    let expected: Vec<char> = expected.chars().filter(|c| !c.is_whitespace()).collect();
    let got: Vec<char> = got.chars().filter(|c| !c.is_whitespace()).collect();
    Errors {
        edits: levenshtein(&expected, &got),
        expected_chars: expected.len(),
    }
}

/// The words of three or more letters or digits in `expected` that `got`
/// holds, and how many there were.
pub fn recall(expected: &str, got: &str) -> (usize, usize) {
    let got: HashSet<String> = words(got).collect();
    let wanted: Vec<String> = words(expected)
        .filter(|word| word.chars().count() >= 3)
        .collect();
    let found = wanted.iter().filter(|word| got.contains(*word)).count();
    (found, wanted.len())
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Edit distance over characters: insertions, deletions and substitutions,
/// one each.
pub fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// A rate as Plan 0.1 printed it: three decimals.
pub fn rate_text(rate: Option<f64>) -> String {
    match rate {
        Some(rate) => format!("{rate:.3}"),
        None => "-".into(),
    }
}

/// Pages per second, or `-` when no time was spent.
pub fn per_second(count: usize, seconds: f64) -> String {
    if seconds > 0.0 {
        format!("{:.2}", count as f64 / seconds)
    } else {
        "-".into()
    }
}

#[cfg(test)]
mod tests;
