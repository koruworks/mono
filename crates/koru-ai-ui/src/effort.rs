// SPDX-License-Identifier: MPL-2.0

//! The effort control on the chip: what a model's reasoning control offers
//! as stops, what the chat's setting resolves to under it, the word for
//! each, and the slider's geometry and drawing. Nothing here knows a
//! provider; the control comes from the service with the transcript.

use koru_ai_core::{ReasoningChoice, ReasoningControl, ReasoningLevel};
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_i18n::{Key, Lang, t};
use vello::Scene;
use vello::kurbo::{Affine, Circle};

use crate::{role, theme};

/// The section's height inside the chip's popover: a heading line and the
/// track with its stop words.
pub(crate) const SECTION_H: f32 = 64.0;
const TRACK_PAD: f32 = 18.0;
const STOP_R: f64 = 5.0;
const CURRENT_R: f64 = 7.0;

/// The stops a control offers, in slider order. `Default` is never a stop:
/// the chip shows what the default resolves to instead.
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

/// What the chat's setting means under the control: the stop the chip
/// reads and the slider marks. `None` where the model has no control. A
/// model that states no default level reads *Default* while the chat
/// chooses none, and the slider marks no stop: the agent decides.
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

/// The word a stop reads as, in the person's language.
pub(crate) fn word(lang: Lang, choice: ReasoningChoice) -> String {
    let key = match choice {
        ReasoningChoice::Default => Key::AiEffortDefault,
        ReasoningChoice::Off => Key::AiEffortOff,
        ReasoningChoice::On => Key::AiEffortOn,
        ReasoningChoice::Level(ReasoningLevel::Minimal) => Key::AiEffortMinimal,
        ReasoningChoice::Level(ReasoningLevel::Low) => Key::AiEffortLow,
        ReasoningChoice::Level(ReasoningLevel::Medium) => Key::AiEffortMedium,
        ReasoningChoice::Level(ReasoningLevel::High) => Key::AiEffortHigh,
        ReasoningChoice::Level(ReasoningLevel::XHigh) => Key::AiEffortXHigh,
        ReasoningChoice::Level(ReasoningLevel::Max) => Key::AiEffortMax,
    };
    t(lang, key).to_owned()
}

/// Where the stops sit inside a section.
pub(crate) struct Slider {
    pub(crate) track_y: f32,
    pub(crate) stops: Vec<(f32, ReasoningChoice)>,
}

pub(crate) fn slider(section: Rect, control: &ReasoningControl) -> Slider {
    let stops = stops(control);
    let left = section.x + TRACK_PAD;
    let right = section.x + section.w - TRACK_PAD;
    let count = stops.len().max(1);
    let step = if count > 1 {
        (right - left) / (count - 1) as f32
    } else {
        0.0
    };
    Slider {
        track_y: section.y + 32.0,
        stops: stops
            .into_iter()
            .enumerate()
            .map(|(index, choice)| (left + index as f32 * step, choice))
            .collect(),
    }
}

/// The stop nearest to `x`, when `y` is on the track's band.
pub(crate) fn stop_at(slider: &Slider, x: f32, y: f32) -> Option<ReasoningChoice> {
    if (y - slider.track_y).abs() > 16.0 {
        return None;
    }
    nearest(slider, x)
}

/// The stop nearest to `x`, whatever the row: a drag stays on the slider.
pub(crate) fn nearest(slider: &Slider, x: f32) -> Option<ReasoningChoice> {
    slider
        .stops
        .iter()
        .min_by(|(a, _), (b, _)| {
            (a - x)
                .abs()
                .partial_cmp(&(b - x).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, choice)| *choice)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    lang: Lang,
    section: Rect,
    control: &ReasoningControl,
    current: ReasoningChoice,
    pointer: (f32, f32),
) {
    let p = theme::p();
    let heading = format!("{} · {}", t(lang, Key::AiReasoning), word(lang, current));
    let line = engine.simple_layout(&heading, role::meta());
    scene::draw_layout_first_line(
        canvas,
        root,
        &line,
        section.x + 8.0,
        section.y + 6.0,
        Some(p.text_dim),
    );
    let slider = slider(section, control);
    if let (Some((first, _)), Some((last, _))) = (slider.stops.first(), slider.stops.last()) {
        scene::fill(
            canvas,
            root,
            p.text_dim,
            &scene::rrect(
                *first,
                slider.track_y - 1.0,
                (last - first).max(1.0),
                2.0,
                1.0,
            ),
        );
    }
    let hovered = stop_at(&slider, pointer.0, pointer.1);
    for (x, choice) in &slider.stops {
        let is_current = *choice == current;
        let radius = if is_current { CURRENT_R } else { STOP_R };
        scene::fill(
            canvas,
            root,
            if is_current {
                p.accent
            } else if hovered == Some(*choice) {
                p.text
            } else {
                p.text_dim
            },
            &Circle::new((*x as f64, slider.track_y as f64), radius),
        );
        let label = engine.simple_layout(&word(lang, *choice), role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            x - label.width() * 0.5,
            slider.track_y + 10.0,
            Some(if is_current { p.text } else { p.text_dim }),
        );
    }
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
        assert!(stops(&ReasoningControl::Unsupported).is_empty());
        assert!(resolve(&ReasoningControl::Unsupported, ReasoningChoice::Default).is_none());
    }

    #[test]
    fn the_slider_spaces_stops_evenly_and_a_drag_snaps_to_the_nearest() {
        let section = Rect::new(100.0, 0.0, 300.0, SECTION_H);
        let slider = slider(section, &levels());
        let xs: Vec<f32> = slider.stops.iter().map(|(x, _)| *x).collect();
        assert_eq!(xs, vec![118.0, 250.0, 382.0]);
        assert_eq!(
            stop_at(&slider, 260.0, slider.track_y + 4.0),
            Some(ReasoningChoice::Level(ReasoningLevel::Medium))
        );
        assert_eq!(stop_at(&slider, 260.0, slider.track_y + 40.0), None);
        assert_eq!(
            nearest(&slider, 900.0),
            Some(ReasoningChoice::Level(ReasoningLevel::High))
        );
    }

    #[test]
    fn every_stop_has_a_word_in_every_language() {
        for lang in koru_i18n::Lang::ALL {
            for choice in stops(&ReasoningControl::Levels {
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
            }) {
                assert!(!word(lang, choice).is_empty());
            }
        }
    }
}
