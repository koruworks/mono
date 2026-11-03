// SPDX-License-Identifier: MPL-2.0

//! The effort control on the chip: what a model's reasoning control offers
//! as levels, what the chat's setting resolves to under it, and the words
//! for each — the full word a readout and a screen reader say, and the
//! short one the model picker's effort strip shows (D76). Nothing here
//! knows a provider; the control comes from the service with the transcript.

use koru_ai_core::{ReasoningChoice, ReasoningControl, ReasoningLevel};
use koru_i18n::{Key, Lang, t};

/// The levels a control offers, in the strip's order. `Default` is never
/// one: the chip shows what the default resolves to instead.
pub(crate) fn stops(control: &ReasoningControl) -> Vec<ReasoningChoice> {
    match control {
        ReasoningControl::Unsupported => Vec::new(),
        ReasoningControl::Toggle { .. } => vec![ReasoningChoice::Off, ReasoningChoice::On],
        ReasoningControl::Levels {
            values,
            supports_off,
            ..
        } => {
            let mut stops = Vec::with_capacity(values.len() + 1);
            if *supports_off {
                stops.push(ReasoningChoice::Off);
            }
            stops.extend(values.iter().map(|level| ReasoningChoice::Level(*level)));
            stops
        }
    }
}

/// What the chat's setting means under the control: the level the chip
/// reads and the strip raises. `None` where the model has no control. A
/// model that states no default level reads *Default* while the chat
/// chooses none, and the strip raises no level: the agent decides.
pub(crate) fn resolve(
    control: &ReasoningControl,
    choice: ReasoningChoice,
) -> Option<ReasoningChoice> {
    match control {
        ReasoningControl::Unsupported => None,
        ReasoningControl::Toggle { default_on } => Some(match choice {
            ReasoningChoice::Default => {
                if *default_on {
                    ReasoningChoice::On
                } else {
                    ReasoningChoice::Off
                }
            }
            ReasoningChoice::Off => ReasoningChoice::Off,
            ReasoningChoice::On | ReasoningChoice::Level(_) => ReasoningChoice::On,
        }),
        ReasoningControl::Levels {
            values,
            default,
            supports_off,
        } => Some(match choice {
            ReasoningChoice::Off if *supports_off => ReasoningChoice::Off,
            ReasoningChoice::Level(level) if values.contains(&level) => {
                ReasoningChoice::Level(level)
            }
            _ => default.map_or(ReasoningChoice::Default, ReasoningChoice::Level),
        }),
    }
}

/// **The model's own level** (D76): what the chat's *Default* resolves to,
/// when the model states one. A press on it in the strip is the chat's
/// *Default* again; `None` for a model that states none.
pub(crate) fn default_level(control: &ReasoningControl) -> Option<ReasoningChoice> {
    resolve(control, ReasoningChoice::Default).filter(|level| *level != ReasoningChoice::Default)
}

/// The key of the full word a level reads as: the readout's, and the
/// accessible name of its segment in the strip.
pub(crate) fn word_key(choice: ReasoningChoice) -> Key {
    match choice {
        ReasoningChoice::Default => Key::AiEffortDefault,
        ReasoningChoice::Off => Key::AiEffortOff,
        ReasoningChoice::On => Key::AiEffortOn,
        ReasoningChoice::Level(ReasoningLevel::Minimal) => Key::AiEffortMinimal,
        ReasoningChoice::Level(ReasoningLevel::Low) => Key::AiEffortLow,
        ReasoningChoice::Level(ReasoningLevel::Medium) => Key::AiEffortMedium,
        ReasoningChoice::Level(ReasoningLevel::High) => Key::AiEffortHigh,
        ReasoningChoice::Level(ReasoningLevel::XHigh) => Key::AiEffortXHigh,
        ReasoningChoice::Level(ReasoningLevel::Max) => Key::AiEffortMax,
    }
}

/// The word a level reads as, in the person's language.
pub(crate) fn word(lang: Lang, choice: ReasoningChoice) -> String {
    t(lang, word_key(choice)).to_owned()
}

/// **A level's short word** (D76, the design's *Low · Med · High · XH ·
/// Max*): what its segment in the effort strip shows, where six share the
/// picker's width. Off and On are short already.
pub(crate) fn short_word(lang: Lang, choice: ReasoningChoice) -> String {
    let key = match choice {
        ReasoningChoice::Level(ReasoningLevel::Minimal) => Key::AiEffortShortMinimal,
        ReasoningChoice::Level(ReasoningLevel::Low) => Key::AiEffortShortLow,
        ReasoningChoice::Level(ReasoningLevel::Medium) => Key::AiEffortShortMedium,
        ReasoningChoice::Level(ReasoningLevel::High) => Key::AiEffortShortHigh,
        ReasoningChoice::Level(ReasoningLevel::XHigh) => Key::AiEffortShortXHigh,
        ReasoningChoice::Level(ReasoningLevel::Max) => Key::AiEffortShortMax,
        other => word_key(other),
    };
    t(lang, key).to_owned()
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
