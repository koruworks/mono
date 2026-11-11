// SPDX-License-Identifier: MPL-2.0

//! **Effort, in the control's own words**: the levels a model offers, what a
//! choice resolves to under them, and the words for each — the full word a
//! readout and a screen reader say, and the short one the effort strip shows
//! (koru-ai D76). A host maps its own reasoning types onto these one for one
//! (`koru-ai-ui` does, exhaustively, so a level added there fails to compile
//! until it is added here); nothing here knows a provider.

use koru_i18n::{Key, Lang, t};

/// One level of effort a model may offer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// **What is chosen**: the model's own default, reasoning off or on (a model
/// with a toggle), or a level.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Effort {
    #[default]
    Default,
    Off,
    On,
    Level(Level),
}

/// **What a model offers**: reasoning on or off, or levels — in the order the
/// model gives them — with the one it uses by default, if it says, and
/// whether reasoning can be turned off. A model that offers neither has no
/// `Levels` at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Levels {
    Toggle {
        default_on: bool,
    },
    Levels {
        values: Vec<Level>,
        default: Option<Level>,
        supports_off: bool,
    },
}

/// The levels a model offers, in the strip's order. `Default` is never one:
/// the chip shows what the default resolves to instead.
pub fn stops(levels: &Levels) -> Vec<Effort> {
    match levels {
        Levels::Toggle { .. } => vec![Effort::Off, Effort::On],
        Levels::Levels {
            values,
            supports_off,
            ..
        } => {
            let mut stops = Vec::with_capacity(values.len() + 1);
            if *supports_off {
                stops.push(Effort::Off);
            }
            stops.extend(values.iter().map(|level| Effort::Level(*level)));
            stops
        }
    }
}

/// What `choice` means under `levels`: the level the chip reads and the
/// strip raises. A model that states no default level reads *Default* while
/// nothing is chosen, and the strip raises no level: the agent decides.
pub fn resolve(levels: &Levels, choice: Effort) -> Effort {
    match levels {
        Levels::Toggle { default_on } => match choice {
            Effort::Default => {
                if *default_on {
                    Effort::On
                } else {
                    Effort::Off
                }
            }
            Effort::Off => Effort::Off,
            Effort::On | Effort::Level(_) => Effort::On,
        },
        Levels::Levels {
            values,
            default,
            supports_off,
        } => match choice {
            Effort::Off if *supports_off => Effort::Off,
            Effort::Level(level) if values.contains(&level) => Effort::Level(level),
            _ => default.map_or(Effort::Default, Effort::Level),
        },
    }
}

/// **The model's own level** (D76): what *Default* resolves to, when the
/// model states one. A press on it in the strip is *Default* again; `None`
/// for a model that states none.
pub fn default_level(levels: &Levels) -> Option<Effort> {
    Some(resolve(levels, Effort::Default)).filter(|level| *level != Effort::Default)
}

/// The key of the full word a choice reads as: the readout's, and the
/// accessible name of its segment in the strip.
pub fn word_key(choice: Effort) -> Key {
    match choice {
        Effort::Default => Key::AiEffortDefault,
        Effort::Off => Key::AiEffortOff,
        Effort::On => Key::AiEffortOn,
        Effort::Level(Level::Minimal) => Key::AiEffortMinimal,
        Effort::Level(Level::Low) => Key::AiEffortLow,
        Effort::Level(Level::Medium) => Key::AiEffortMedium,
        Effort::Level(Level::High) => Key::AiEffortHigh,
        Effort::Level(Level::XHigh) => Key::AiEffortXHigh,
        Effort::Level(Level::Max) => Key::AiEffortMax,
    }
}

/// The word a choice reads as, in the person's language.
pub fn word(lang: Lang, choice: Effort) -> String {
    t(lang, word_key(choice)).to_owned()
}

/// **A level's short word** (D76, the design's *Low · Med · High · XH ·
/// Max*): what its segment in the effort strip shows, where six share the
/// picker's width. Off and On are short already.
pub fn short_word(lang: Lang, choice: Effort) -> String {
    let key = match choice {
        Effort::Level(Level::Minimal) => Key::AiEffortShortMinimal,
        Effort::Level(Level::Low) => Key::AiEffortShortLow,
        Effort::Level(Level::Medium) => Key::AiEffortShortMedium,
        Effort::Level(Level::High) => Key::AiEffortShortHigh,
        Effort::Level(Level::XHigh) => Key::AiEffortShortXHigh,
        Effort::Level(Level::Max) => Key::AiEffortShortMax,
        other => word_key(other),
    };
    t(lang, key).to_owned()
}
