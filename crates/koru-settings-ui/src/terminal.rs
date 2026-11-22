// SPDX-License-Identifier: MPL-2.0

//! **The Terminal page — the terminal's window menu, as cards.**
//!
//! The cursor rows reuse the deleted Terminal app's `TerminalCursor*` keys;
//! card labels, hints and appearance choices also use the shared catalog in
//! all eight languages. Zoom is an existing `TerminalSettings` field.
//!
//! **The font list is mono-restricted**, which is a refusal rather than a
//! preference: `TerminalPatch::validate` rejects a proportional face outright,
//! so offering one would be offering a choice the authority will not take.
//!
//! **"Match the editor caret" is not a new setting.** Width and height each
//! already have an `Application` value — *follow the app's caret* — and the
//! switch is on exactly when both hold it, which is the default. Turning it
//! off gives each the concrete value the editor caret has now, so nothing
//! visibly changes until the person picks something else. Blink and color
//! stay rows of their own: the editor caret has no blink setting to follow,
//! the registry gives color no follow value, and blink's product default is
//! `On` rather than `Application` (see `TerminalSettings::default`).
//!
//! This is the one page the surface patches entirely by itself — nothing else
//! mirrors `TerminalSettings`, so there is no window copy to keep in step.

use koru_draw::theme::{self, FontChoice};
use koru_i18n::{Key, Lang, t};
use koru_settings::{
    CaretColorChoice, FontFamily, TerminalCursorBlink, TerminalCursorHeight, TerminalCursorWidth,
    TerminalSettings, ThemeChoice,
};
use koru_ui::DropdownOption;

use crate::action::Action;
use crate::page::Builder;

#[cfg(test)]
mod tests;

/// The zoom factors the row offers, as the terminal's chord already steps
/// through them. Every one is inside [`Zoom`]'s accepted range, so none of them
/// can be refused by the authority for being out of it.
pub(crate) const ZOOM_STEPS: [f32; 6] = [0.75, 0.9, 1.0, 1.1, 1.25, 1.5];

pub(crate) fn lay_out(b: &mut Builder<'_>, lang: Lang, section: &TerminalSettings) {
    b.card(Some(koru_i18n::t(lang, Key::SettingsAppearance)));
    b.row_choice(
        koru_i18n::t(lang, Key::SettingsTheme),
        None,
        ThemeChoice::ALL
            .iter()
            .map(|choice| {
                // *System* means the terminal follows the app's theme, and
                // says so.
                let label = match choice {
                    ThemeChoice::System => koru_i18n::t(lang, Key::SettingsFollowApp),
                    other => crate::global::theme_label(lang, *other),
                };
                (
                    Action::TerminalTheme(*choice),
                    label.to_owned(),
                    *choice == section.theme,
                )
            })
            .collect(),
    );
    font_row(b, lang, section);
    zoom_row(b, lang, section);

    cursor_card(b, lang, section);
}

/// The mono families, each set in its own face.
fn font_row(b: &mut Builder<'_>, lang: Lang, section: &TerminalSettings) {
    // *Follow the default* is a real state that persists as no key at all;
    // the field shows the family it resolves to — the code role's default,
    // JetBrains Mono — rather than a blank.
    let current = Some(section.font.family().unwrap_or(FontFamily::JetBrainsMono));
    b.row_dropdown(
        koru_i18n::t(lang, Key::SettingsFont),
        None,
        mono_families()
            .map(|family| {
                let face = FontChoice::parse(family.token());
                (
                    Action::TerminalFont(family),
                    DropdownOption::new(face.label(), current == Some(family))
                        .in_face(face.stack()),
                )
            })
            .collect(),
    );
}

/// Every family a terminal may use, in the registry's own order.
pub(crate) fn mono_families() -> impl Iterator<Item = FontFamily> {
    FontFamily::ALL
        .iter()
        .copied()
        .filter(|family| family.is_mono())
}

fn zoom_row(b: &mut Builder<'_>, lang: Lang, section: &TerminalSettings) {
    let now = zoom_token(section.zoom.get());
    // Compared as it persists, never as it floats: `0.1 * 9` and the `0.90`
    // read back out of the file are different `f32`s.
    let index = ZOOM_STEPS.iter().position(|step| zoom_token(*step) == now);
    let value = zoom_label(section.zoom.get());
    let (less, more) = match index {
        Some(i) => (
            i.checked_sub(1).map(|j| ZOOM_STEPS[j]),
            ZOOM_STEPS.get(i + 1).copied(),
        ),
        // A factor off the scale (set by the chord's finer steps, or read
        // from an older file) steps to its nearest neighbours on it.
        None => {
            let current = section.zoom.get();
            (
                ZOOM_STEPS.iter().rev().copied().find(|s| *s < current),
                ZOOM_STEPS.iter().copied().find(|s| *s > current),
            )
        }
    };
    b.row_stepper(
        t(lang, Key::SettingsZoom),
        None,
        value,
        "150%",
        less.map(Action::TerminalZoom),
        more.map(Action::TerminalZoom),
    );
}

/// `1.25` → `125%`. Digits and a sign, so it reads the same in all eight.
pub(crate) fn zoom_label(factor: f32) -> String {
    format!("{}%", (factor * 100.0).round() as i32)
}

fn zoom_token(factor: f32) -> String {
    format!("{factor:.2}")
}

/// Whether the terminal cursor's shape follows the editor caret.
pub(crate) fn matches_caret(section: &TerminalSettings) -> bool {
    section.cursor_width == TerminalCursorWidth::Application
        && section.cursor_height == TerminalCursorHeight::Application
}

fn cursor_card(b: &mut Builder<'_>, lang: Lang, section: &TerminalSettings) {
    b.card(Some(t(lang, Key::TerminalCursor)));
    let matching = matches_caret(section);
    b.row_toggle(
        koru_i18n::t(lang, Key::SettingsMatchTheEditorCaret),
        Some(if matching {
            koru_i18n::t(lang, Key::SettingsCaretFollowsEditor)
        } else {
            koru_i18n::t(lang, Key::SettingsCaretIndependent)
        }),
        matching,
        Action::TerminalMatchCaret(!matching),
    );
    // Blink is the terminal's own whatever it matches — the editor caret has
    // no blink to follow. *Follow the app* blinks, so it reads as on.
    let blinking = section.cursor_blink != TerminalCursorBlink::Off;
    b.row_toggle(
        t(lang, Key::TerminalCursorBlink),
        None,
        blinking,
        Action::TerminalCursorBlink(if blinking {
            TerminalCursorBlink::Off
        } else {
            TerminalCursorBlink::On
        }),
    );
    // Matching covers colour as well as shape, so both fold away with it.
    if matching {
        return;
    }
    b.row_swatches(
        t(lang, Key::TerminalCursorColor),
        None,
        CaretColorChoice::ALL
            .iter()
            .map(|choice| {
                (
                    Action::TerminalCursorColor(*choice),
                    Some(crate::caret_color(*choice).color()),
                    *choice == section.cursor_color,
                )
            })
            .collect(),
    );
    b.row_pair(
        koru_i18n::t(lang, Key::SettingsShape),
        None,
        TerminalCursorWidth::ALL
            .iter()
            .filter(|w| **w != TerminalCursorWidth::Application)
            .map(|choice| {
                (
                    Action::TerminalCursorWidth(*choice),
                    DropdownOption::new(
                        width_label(lang, *choice),
                        *choice == section.cursor_width,
                    ),
                )
            })
            .collect(),
        TerminalCursorHeight::ALL
            .iter()
            .filter(|h| **h != TerminalCursorHeight::Application)
            .map(|choice| {
                (
                    Action::TerminalCursorHeight(*choice),
                    DropdownOption::new(
                        height_label(lang, *choice),
                        *choice == section.cursor_height,
                    ),
                )
            })
            .collect(),
    );
}

/// A width's name, in the words the cursor flyout used — shared with the
/// editor caret's row so the two shapes are named alike.
pub(crate) fn width_label(lang: Lang, width: TerminalCursorWidth) -> &'static str {
    t(
        lang,
        match width {
            TerminalCursorWidth::Application => Key::TerminalCursorApplication,
            TerminalCursorWidth::Thin => Key::TerminalCursorThin,
            TerminalCursorWidth::Medium => Key::TerminalCursorMedium,
            TerminalCursorWidth::Thick => Key::TerminalCursorThick,
            TerminalCursorWidth::X4 => Key::TerminalCursor4x,
            TerminalCursorWidth::X8 => Key::TerminalCursor8x,
        },
    )
}

pub(crate) fn height_label(lang: Lang, height: TerminalCursorHeight) -> &'static str {
    t(
        lang,
        match height {
            TerminalCursorHeight::Application => Key::TerminalCursorApplication,
            TerminalCursorHeight::Short => Key::TerminalCursorShort,
            TerminalCursorHeight::Medium => Key::TerminalCursorMedium,
            TerminalCursorHeight::Tall => Key::TerminalCursorTall,
        },
    )
}

/// The concrete cursor the editor caret has now — what "stop matching" sets,
/// so turning the switch off changes nothing on screen by itself.
pub(crate) fn caret_now() -> (TerminalCursorWidth, TerminalCursorHeight) {
    (
        TerminalCursorWidth::parse(theme::caret_width().key()),
        TerminalCursorHeight::parse(theme::caret_height().key()),
    )
}

/// The zoom factors, as the authority would accept them. Named so the test can
/// assert the row can never offer one the registry refuses.
#[cfg(test)]
pub(crate) fn accepted_zooms() -> impl Iterator<Item = koru_settings::Zoom> {
    ZOOM_STEPS.into_iter().filter_map(koru_settings::Zoom::new)
}
