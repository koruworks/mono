// SPDX-License-Identifier: MPL-2.0

//! **One page, two sets of words: its text layer and what recognition read
//! in its pictures.**
//!
//! A page with a usable text layer and pictures or painted paths
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
//! - its exact word sequence already occurs in the layer, ignoring case,
//!   punctuation and whitespace; or
//! - it repeats a recognized line already added.
//!
//! The layer goes first because it is exact (it is the file's own text) and
//! recognition is not; a line is the unit because it is what recognition
//! returns and what a person reads. Words that only recognition found arrive
//! as whole lines, so a search for any of them finds the page; a typed word
//! is never present twice unless a picture really repeats it in a line of
//! otherwise new words.

use std::collections::HashSet;

/// The page's text: `layer`, then the lines of `recognized` it lacks.
pub fn combine(layer: &str, recognized: &str) -> String {
    let layer_words: Vec<String> = words(layer).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = layer.to_string();
    for line in recognized.lines() {
        let line = line.trim();
        let line_words: Vec<String> = words(line).collect();
        if line_words.is_empty() || !seen.insert(line_words.join(" ")) {
            continue;
        }
        // Similarity is not duplication: one different name or number can be
        // the very word somebody needs to find. Do not suppress partial matches
        // or a short name that happens to be a substring of another word.
        if layer_words
            .windows(line_words.len())
            .any(|window| window == line_words)
        {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// The lower-cased runs of letters and digits.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

#[cfg(test)]
mod tests;
