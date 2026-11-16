// SPDX-License-Identifier: MPL-2.0

//! **The identifiers a descriptor carries, and the rule they share.**
//!
//! Three of a descriptor's fields used to be a speech crate's types —
//! `EngineId`, `ModelTier` and `VoiceLanguage` — and those types could only
//! say what speech knew: two engines, two tiers, eight interface languages. A
//! model that reads text out of pictures could not be described with them.
//! Here they are **validated tokens**, and a feature crate maps them to its own
//! types (`koru-voice-models` to `EngineId`, `ModelTier` and `VoiceLanguage`).
//!
//! **What is signed did not change.** The canonical form writes the same
//! string the old types wrote — `sherpa`, `high_accuracy`, `pt-BR` — so every
//! descriptor already signed verifies unchanged; the tokens validate *what can
//! be a token* rather than *which tokens this build knows*.
//!
//! ## Two rules, not one
//!
//! [`ModelId`], [`ModelVersion`](crate::ModelVersion) and [`Token`] share the
//! identifier rule below: lowercase ASCII letters, digits, `-`, `.` and `_`;
//! non-empty; at most 64 characters; never beginning with a dot. Lowercase,
//! because Windows and macOS fold case and Linux does not, and an id is a
//! directory name.
//!
//! A [`LanguageTag`] cannot be lowercase: the tags already signed are
//! `pt-BR` and `zh-Hans`, and lowercasing them would change the signed bytes.
//! So a tag is a BCP 47 tag in its **canonical case** (RFC 5646 §2.1.1:
//! language lowercase, script title case, region upper case), which gives the
//! property lowercase gives an id — one language has one spelling, so a
//! duplicate is visible as a duplicate.
//!
//! The identifier rule is the same text as `koru_voice`'s for `ModelId` and
//! `EngineId`. It is restated rather than borrowed because this crate may not
//! depend on a speech crate; `koru-voice-models` holds the test that the two
//! agree.

use crate::error::{ModelError, ModelErrorCode};
use serde::{Deserialize, Serialize};

/// The identifier rule — see the module note.
pub(crate) fn identifier(what: &str, value: &str) -> Result<(), ModelError> {
    let bad = |why: &str| Err(ModelErrorCode::DescriptorInvalid.with(format!("{what} {why}")));
    if value.is_empty() {
        return bad("is empty");
    }
    if value.chars().count() > 64 {
        return bad("is longer than 64 characters");
    }
    if value.starts_with('.') {
        return bad("begins with a dot");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
    {
        return bad("is not lowercase ASCII letters, digits, '-', '.' or '_'");
    }
    Ok(())
}

macro_rules! checked_string {
    ($(#[$doc:meta])* $type:ident, $check:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $type(String);

        impl $type {
            /// Check and wrap. The only constructor: an unchecked
            /// `From<String>` would be the shortest path and would therefore
            /// be the one used.
            pub fn parse(value: impl Into<String>) -> Result<$type, ModelError> {
                let value = value.into();
                let check: fn(&str) -> Result<(), ModelError> = $check;
                check(&value)?;
                Ok($type(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $type {
            type Error = ModelError;

            fn try_from(value: String) -> Result<$type, ModelError> {
                $type::parse(value)
            }
        }

        impl From<$type> for String {
            fn from(value: $type) -> String {
                value.0
            }
        }

        impl std::fmt::Display for $type {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

checked_string!(
    /// Which model — `parakeet-tdt-v3`, `whisper-base`.
    ///
    /// **A path component**: installed bytes live under
    /// `<id>/<version>/<digest>/`, and the identifier rule is what stops a
    /// catalog entry named `../../etc` from ever becoming a directory.
    ModelId,
    |value| identifier("a model id", value)
);

checked_string!(
    /// A signed word whose vocabulary belongs to a feature, not to this crate
    /// — a descriptor's `engine` (`sherpa`, `whisper`) and `tier` (`compact`,
    /// `high_accuracy`).
    ///
    /// The identifier rule and nothing more: whether `sherpa` is an engine
    /// this build can run is the feature's question, answered when it maps
    /// the token to its own type. The rule is what keeps a token out of a
    /// canonical-form field boundary and out of a log line's next line.
    Token,
    |value| identifier("a token", value)
);

checked_string!(
    /// A BCP 47 language tag in its canonical case — `en`, `pt-BR`,
    /// `zh-Hans`. What a model can read or transcribe.
    ///
    /// See the module note for why this one is not lowercase.
    LanguageTag,
    language_tag
);

/// A tag of one to eight subtags of one to eight ASCII letters or digits,
/// separated by single hyphens; the first subtag letters only and lowercase, a
/// four-letter subtag title case, a two-letter subtag after the first upper
/// case, and every other subtag lowercase.
fn language_tag(value: &str) -> Result<(), ModelError> {
    let bad = |why: &str| {
        Err(ModelErrorCode::DescriptorInvalid.with(format!("language tag {value:?} {why}")))
    };
    if value.is_empty() {
        return bad("is empty");
    }
    let subtags: Vec<&str> = value.split('-').collect();
    if subtags.len() > 8 {
        return bad("has more than eight subtags");
    }
    for (at, subtag) in subtags.iter().enumerate() {
        if subtag.is_empty() || subtag.len() > 8 {
            return bad("has a subtag that is empty or longer than eight characters");
        }
        if !subtag.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return bad("is not ASCII letters and digits separated by '-'");
        }
        let canonical = if at == 0 {
            if !subtag.bytes().all(|b| b.is_ascii_alphabetic()) {
                return bad("does not begin with a language subtag of letters");
            }
            subtag.to_ascii_lowercase()
        } else if subtag.len() == 4 && subtag.bytes().all(|b| b.is_ascii_alphabetic()) {
            let mut script = subtag.to_ascii_lowercase();
            script[..1].make_ascii_uppercase();
            script
        } else if subtag.len() == 2 && subtag.bytes().all(|b| b.is_ascii_alphabetic()) {
            subtag.to_ascii_uppercase()
        } else {
            subtag.to_ascii_lowercase()
        };
        if *subtag != canonical {
            return bad("is not in canonical case (en, pt-BR, zh-Hans)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
