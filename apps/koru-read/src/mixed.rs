// SPDX-License-Identifier: MPL-2.0

//! **One page, two sets of words: its text layer and what recognition read
//! in its pictures.**
//!
//! A page with a usable text layer whose pictures cover more than half of it
//! is recognized as well ([`crate::accept::Ask::AsWell`]): a screenshot or a
//! scan inside a typed document holds words the layer does not. Both are
//! kept, as the page's one string (`Source::Mixed`), and that string must not
//! hold every word twice — recognition also reads the typed text, which is on
//! the same rendered page.
//!
//! **The rule: the text layer, whole and first; then each recognized line
//! that is not already there**, in recognition's reading order. A recognized
//! line is already there when
//!
//! - with case and white space ignored, it appears inside the layer — the
//!   same words, perhaps wrapped or spaced differently; or
//! - at least [`COVERED`] of its words are words of the layer — the same line
//!   with a character misread, which an exact comparison would add again; or
//! - it repeats a recognized line already added.
//!
//! The layer goes first because it is exact (it is the file's own text) and
//! recognition is not; a line is the unit because it is what recognition
//! returns and what a person reads. Words that only recognition found arrive
//! as whole lines, so a search for any of them finds the page; a typed word
//! is never present twice unless a picture really repeats it in a line of
//! otherwise new words.

use std::collections::HashSet;

/// A recognized line most of whose words the layer already holds is the
/// layer's line, read again.
pub const COVERED: f64 = 0.8;

/// The page's text: `layer`, then the lines of `recognized` it lacks.
pub fn combine(layer: &str, recognized: &str) -> String {
    let layer_squashed = squash(layer);
    let layer_words: HashSet<String> = words(layer).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = layer.to_string();
    for line in recognized.lines() {
        let line = line.trim();
        let squashed = squash(line);
        if squashed.is_empty() || !seen.insert(squashed.clone()) {
            continue;
        }
        if layer_squashed.contains(&squashed) {
            continue;
        }
        let line_words: Vec<String> = words(line).collect();
        if !line_words.is_empty() {
            let known = line_words
                .iter()
                .filter(|word| layer_words.contains(*word))
                .count();
            if known as f64 >= COVERED * line_words.len() as f64 {
                continue;
            }
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// Lower case, with every white-space character removed.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The lower-cased runs of letters and digits.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

#[cfg(test)]
mod tests;
