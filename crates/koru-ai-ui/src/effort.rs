// SPDX-License-Identifier: MPL-2.0

//! The effort control on the chip: what a model's reasoning control offers
//! as levels, what the chat's setting resolves to under it, and the words
//! for each — the full word a readout and a screen reader say, and the
//! short one the model picker's effort strip shows (D76). Nothing here
//! knows a provider; the control comes from the service with the transcript.
//!
//! **The rules are the agent-configuration control's** (Bots P1.8,
//! `koru_agent_config_ui::effort`), which speaks its own plain vocabulary so
//! that it links nothing of the AI stack. This module maps the AI service's
//! reasoning types onto it and back, one for one and by exhaustive matches —
//! a level added to the service fails to compile here until the control has
//! it too — and keeps the chat's own signatures over them.

use koru_agent_config_ui::effort as cfg;
use koru_ai_core::{ReasoningChoice, ReasoningControl, ReasoningLevel};
use koru_i18n::{Key, Lang};

/// A level, in the control's words.
pub(crate) fn level(level: ReasoningLevel) -> cfg::Level {
    match level {
        ReasoningLevel::Minimal => cfg::Level::Minimal,
        ReasoningLevel::Low => cfg::Level::Low,
        ReasoningLevel::Medium => cfg::Level::Medium,
        ReasoningLevel::High => cfg::Level::High,
        ReasoningLevel::XHigh => cfg::Level::XHigh,
        ReasoningLevel::Max => cfg::Level::Max,
    }
}

/// The control's level, in the service's words.
pub(crate) fn reasoning_level(level: cfg::Level) -> ReasoningLevel {
    match level {
        cfg::Level::Minimal => ReasoningLevel::Minimal,
        cfg::Level::Low => ReasoningLevel::Low,
        cfg::Level::Medium => ReasoningLevel::Medium,
        cfg::Level::High => ReasoningLevel::High,
        cfg::Level::XHigh => ReasoningLevel::XHigh,
        cfg::Level::Max => ReasoningLevel::Max,
    }
}

/// A choice, in the control's words.
pub(crate) fn effort(choice: ReasoningChoice) -> cfg::Effort {
    match choice {
        ReasoningChoice::Default => cfg::Effort::Default,
        ReasoningChoice::Off => cfg::Effort::Off,
        ReasoningChoice::On => cfg::Effort::On,
        ReasoningChoice::Level(value) => cfg::Effort::Level(level(value)),
    }
}

/// The control's choice, in the service's words.
pub(crate) fn reasoning(effort: cfg::Effort) -> ReasoningChoice {
    match effort {
        cfg::Effort::Default => ReasoningChoice::Default,
        cfg::Effort::Off => ReasoningChoice::Off,
        cfg::Effort::On => ReasoningChoice::On,
        cfg::Effort::Level(value) => ReasoningChoice::Level(reasoning_level(value)),
    }
}

/// What a model offers, in the control's words; `None` for a model that
/// offers no effort at all.
pub(crate) fn levels(control: &ReasoningControl) -> Option<cfg::Levels> {
    match control {
        ReasoningControl::Unsupported => None,
        ReasoningControl::Toggle { default_on } => Some(cfg::Levels::Toggle {
            default_on: *default_on,
        }),
        ReasoningControl::Levels {
            values,
            default,
            supports_off,
        } => Some(cfg::Levels::Levels {
            values: values.iter().copied().map(level).collect(),
            default: default.map(level),
            supports_off: *supports_off,
        }),
    }
}

/// The levels a control offers, in the strip's order. `Default` is never
/// one: the chip shows what the default resolves to instead.
pub(crate) fn stops(control: &ReasoningControl) -> Vec<ReasoningChoice> {
    levels(control)
        .map(|levels| cfg::stops(&levels).into_iter().map(reasoning).collect())
        .unwrap_or_default()
}

/// What the chat's setting means under the control: the level the chip
/// reads and the strip raises. `None` where the model has no control. A
/// model that states no default level reads *Default* while the chat
/// chooses none, and the strip raises no level: the agent decides.
pub(crate) fn resolve(
    control: &ReasoningControl,
    choice: ReasoningChoice,
) -> Option<ReasoningChoice> {
    levels(control).map(|levels| reasoning(cfg::resolve(&levels, effort(choice))))
}

/// **The model's own level** (D76): what the chat's *Default* resolves to,
/// when the model states one. A press on it in the strip is the chat's
/// *Default* again; `None` for a model that states none.
pub(crate) fn default_level(control: &ReasoningControl) -> Option<ReasoningChoice> {
    levels(control)
        .as_ref()
        .and_then(cfg::default_level)
        .map(reasoning)
}

/// The key of the full word a level reads as: the readout's, and the
/// accessible name of its segment in the strip.
pub(crate) fn word_key(choice: ReasoningChoice) -> Key {
    cfg::word_key(effort(choice))
}

/// The word a level reads as, in the person's language.
pub(crate) fn word(lang: Lang, choice: ReasoningChoice) -> String {
    cfg::word(lang, effort(choice))
}

/// **A level's short word** (D76, the design's *Low · Med · High · XH ·
/// Max*): what its segment in the effort strip shows, where six share the
/// picker's width. Off and On are short already.
pub(crate) fn short_word(lang: Lang, choice: ReasoningChoice) -> String {
    cfg::short_word(lang, effort(choice))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels() -> ReasoningControl {
        ReasoningControl::Levels {
            values: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
            ],
            default: Some(ReasoningLevel::Medium),
            supports_off: false,
        }
    }

    #[test]
    fn stops_follow_the_control_and_default_resolves_to_its_level() {
        assert_eq!(
            stops(&levels()),
            vec![
                ReasoningChoice::Level(ReasoningLevel::Low),
                ReasoningChoice::Level(ReasoningLevel::Medium),
                ReasoningChoice::Level(ReasoningLevel::High),
            ]
        );
        assert_eq!(
            resolve(&levels(), ReasoningChoice::Default),
            Some(ReasoningChoice::Level(ReasoningLevel::Medium))
        );
        assert_eq!(
            default_level(&levels()),
            Some(ReasoningChoice::Level(ReasoningLevel::Medium))
        );
        assert_eq!(
            resolve(&levels(), ReasoningChoice::Off),
            Some(ReasoningChoice::Level(ReasoningLevel::Medium)),
            "off is not offered, so it reads as the default"
        );
        assert_eq!(
            resolve(&levels(), ReasoningChoice::Level(ReasoningLevel::Max)),
            Some(ReasoningChoice::Level(ReasoningLevel::Medium)),
            "a level the model lacks reads as the default"
        );
        let toggle = ReasoningControl::Toggle { default_on: true };
        assert_eq!(
            stops(&toggle),
            vec![ReasoningChoice::Off, ReasoningChoice::On]
        );
        assert_eq!(
            resolve(&toggle, ReasoningChoice::Default),
            Some(ReasoningChoice::On)
        );
        assert_eq!(default_level(&toggle), Some(ReasoningChoice::On));
        let unstated = ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low],
            default: None,
            supports_off: false,
        };
        assert_eq!(default_level(&unstated), None, "the agent decides");
        assert!(stops(&ReasoningControl::Unsupported).is_empty());
        assert!(resolve(&ReasoningControl::Unsupported, ReasoningChoice::Default).is_none());
    }

    /// D76: the strip's words are the design's short ones; the full word
    /// stays the readout's and the screen reader's.
    #[test]
    fn every_level_has_a_short_and_a_full_word_in_every_language() {
        let all = stops(&ReasoningControl::Levels {
            values: vec![
                ReasoningLevel::Minimal,
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            default: Some(ReasoningLevel::Medium),
            supports_off: true,
        });
        for lang in koru_i18n::Lang::ALL {
            for choice in all.iter().copied().chain([ReasoningChoice::On]) {
                assert!(!word(lang, choice).is_empty());
                assert!(!short_word(lang, choice).is_empty());
            }
        }
        let short: Vec<String> = all
            .iter()
            .skip(2)
            .map(|choice| short_word(Lang::En, *choice))
            .collect();
        assert_eq!(short, ["Low", "Med", "High", "XH", "Max"]);
        assert_eq!(
            word(Lang::En, ReasoningChoice::Level(ReasoningLevel::XHigh)),
            "Extra high"
        );
    }
}
