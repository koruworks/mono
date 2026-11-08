// SPDX-License-Identifier: MPL-2.0

//! The document text size: the scaled ramp, the metrics that follow it, and
//! the global a document reads.

use super::preview::{Preview, PreviewSlot};
use super::*;
use crate::geometry::{checkbox_x, marker_right_x, marker_right_x_in, text_x, text_x_in};

const LEVELS: [Option<u8>; 7] = [None, Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)];

#[test]
fn at_the_base_size_every_step_and_metric_is_todays() {
    for level in LEVELS {
        assert_eq!(type_step_at(level, 1.0), type_step(level), "{level:?}");
        assert_eq!(DocMetrics::BASE.step(level), type_step(level));
    }
    let base = DocMetrics::BASE;
    assert_eq!(base.checkbox, CHECKBOX_SIZE);
    assert_eq!(base.gutter, GUTTER);
    assert_eq!(base.bullet_r, BULLET_R);
    assert_eq!(base.marker_gap, MARKER_GAP);
    assert_eq!(base.code_pad, CODE_PAD);
    assert_eq!(base.code_pad_x, CODE_PAD_X);
    assert_eq!(base.line, FONT_SIZE * LINE_HEIGHT);
    assert_eq!(base.title_gap, TITLE_GAP_BELOW);
}

/// One factor: H1 stays 1.875 × the body, the space above keeps its rhythm,
/// and leading is a ratio that does not move.
#[test]
fn the_ramp_scales_as_one_and_keeps_its_leading() {
    let scale = 20.0 / 16.0;
    assert_eq!(type_step_at(None, scale).size, 20.0);
    let h1 = type_step_at(Some(1), scale);
    assert_eq!(h1.size, 37.5);
    assert_eq!(h1.line_height, type_step(Some(1)).line_height);
    assert_eq!(h1.space_above, 35.0);
    for level in LEVELS {
        let (base, scaled) = (type_step(level), type_step_at(level, scale));
        assert!((scaled.size / base.size - scale).abs() < 1e-6);
        assert_eq!(scaled.line_height, base.line_height);
    }
}

/// The checkbox column stays where the indent puts it; the gutter after it
/// and the gap before a marker grow with the text.
#[test]
fn the_gutter_and_the_marker_gap_grow_and_the_column_does_not() {
    assert_eq!(text_x(2u16, true), text_x_in(&DocMetrics::BASE, 2u16, true));
    assert_eq!(
        marker_right_x(2u16),
        marker_right_x_in(&DocMetrics::BASE, 2u16)
    );
    for px in [14.0f32, 16.0, 22.0] {
        let m = DocMetrics::at(px / FONT_SIZE);
        for depth in [0u16, 3] {
            assert_eq!(text_x_in(&m, depth, true) - checkbox_x(depth), m.gutter);
            assert_eq!(text_x_in(&m, depth, false), checkbox_x(depth));
            assert_eq!(
                text_x_in(&m, depth, true) - marker_right_x_in(&m, depth),
                m.marker_gap
            );
        }
    }
}

/// The global a document reads: a preview stands in for the choice, saving
/// reads the choice alone, and following the default is no choice at all.
#[test]
fn a_preview_stands_in_for_the_chosen_size_and_saving_never_sees_it() {
    let _serial = super::preview::SERIAL
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    set_doc_text_size(None);
    assert_eq!(chosen_doc_text_size(), None);
    assert_eq!(doc_text_size(), FONT_SIZE);
    set_doc_text_size(Some(20));
    assert_eq!(doc_scale(), 1.25);
    let preview = Preview::new(PreviewSlot::DocTextSize, 22);
    assert_eq!(doc_text_size(), 22.0);
    assert_eq!(chosen_doc_text_size(), Some(20));
    drop(preview);
    assert_eq!(doc_text_size(), 20.0);
    set_doc_text_size(None);
    assert_eq!(doc_text_size(), FONT_SIZE);
}

/// **A role keeps what it was set to, apart from what it draws.** An
/// installed family missing here draws the default and keeps its name; a
/// face chosen from a list records the matching setting; a preview of an
/// installed family round-trips through the preview value.
#[test]
fn a_font_role_keeps_its_setting_apart_from_its_face() {
    use koru_settings::{FamilyName, FontSetting};
    let _serial = super::preview::SERIAL
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    let was = font_setting(FontSlot::Headings);
    let missing = FontSetting::Installed(FamilyName::new("No Such Face 9000").unwrap());
    set_font_setting(FontSlot::Headings, missing.clone());
    assert_eq!(font_setting(FontSlot::Headings), missing);
    assert_eq!(chosen_font(FontSlot::Headings), default_headings_font());
    assert_eq!(
        missing_font(FontSlot::Headings).as_deref(),
        Some("No Such Face 9000")
    );

    set_headings_font(FontChoice::Lora);
    assert_eq!(
        font_setting(FontSlot::Headings),
        FontSetting::Chosen(koru_settings::FontFamily::Lora)
    );
    assert_eq!(missing_font(FontSlot::Headings), None);

    let installed = crate::installed_fonts::resolve("Inter").expect("bundled");
    set_font_setting(
        FontSlot::Headings,
        FontSetting::Installed(FamilyName::new("Inter").unwrap()),
    );
    assert_eq!(
        chosen_font(FontSlot::Headings),
        FontChoice::Installed(installed)
    );
    assert_eq!(
        font_of_value(font_preview_value(FontChoice::Installed(installed))),
        Some(FontChoice::Installed(installed))
    );
    set_font_setting(FontSlot::Headings, FontSetting::FollowDefault);
    assert_eq!(chosen_font(FontSlot::Headings), default_headings_font());
    set_font_setting(FontSlot::Headings, was);
}

/// **The chat font follows the app font until chosen** (koru-ai D79) — live,
/// not as a copy taken when it was set: an App font change, and a preview of
/// one, carry an unchosen chat with them; a chosen one stays put; and a
/// chosen family missing here draws the app font, keeping its name.
#[test]
fn the_chat_font_follows_the_app_font_until_chosen() {
    use koru_settings::{FamilyName, FontFamily, FontSetting};
    let _serial = super::preview::SERIAL
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    let (was_ui, was_chat) = (font_setting(FontSlot::Ui), font_setting(FontSlot::Chat));

    set_font_setting(FontSlot::Chat, FontSetting::FollowDefault);
    set_ui_font(FontChoice::Poppins);
    assert_eq!(chat_font(), FontChoice::Poppins);
    set_ui_font(FontChoice::Nunito);
    assert_eq!(chat_font(), FontChoice::Nunito, "followed, not copied");
    assert_eq!(chosen_font(FontSlot::Chat), FontChoice::Nunito);
    assert_eq!(chat_stack(), FontChoice::Nunito.stack());
    {
        let _preview = Preview::new(
            PreviewSlot::Font(FontSlot::Ui),
            font_preview_value(FontChoice::Lora),
        );
        assert_eq!(chat_font(), FontChoice::Lora, "an App font preview too");
    }

    set_chat_font(FontChoice::SourceSerif4);
    assert_eq!(
        font_setting(FontSlot::Chat),
        FontSetting::Chosen(FontFamily::SourceSerif4)
    );
    set_ui_font(FontChoice::Inter);
    assert_eq!(chat_font(), FontChoice::SourceSerif4, "a choice stays put");
    assert_eq!(ui_font(), FontChoice::Inter, "and the app font is its own");
    {
        let _preview = Preview::new(
            PreviewSlot::Font(FontSlot::Chat),
            font_preview_value(FontChoice::Manrope),
        );
        assert_eq!(chat_font(), FontChoice::Manrope);
        assert_eq!(chosen_font(FontSlot::Chat), FontChoice::SourceSerif4);
        assert_eq!(ui_font(), FontChoice::Inter, "a chat preview is the chat's");
    }

    let missing = FontSetting::Installed(FamilyName::new("No Such Face 9000").unwrap());
    set_font_setting(FontSlot::Chat, missing.clone());
    assert_eq!(font_setting(FontSlot::Chat), missing, "the name is kept");
    set_ui_font(FontChoice::Poppins);
    assert_eq!(chat_font(), FontChoice::Poppins);
    assert_eq!(
        missing_font(FontSlot::Chat).as_deref(),
        Some("No Such Face 9000")
    );

    set_font_setting(FontSlot::Ui, was_ui);
    set_font_setting(FontSlot::Chat, was_chat);
}

/// Each theme keeps its own base colour; neither is derived from the other,
/// and no choice is the theme's own ground.
#[test]
fn each_theme_keeps_its_own_base_colour() {
    set_base_color(true, Some(0x18202E));
    assert_eq!(base_color(true), Some(0x18202E));
    assert_eq!(base_color(false), None, "the light base is untouched");
    set_base_color(false, Some(0xDDE7F6));
    assert_eq!(base_color(false), Some(0xDDE7F6));
    assert_eq!(base_color(true), Some(0x18202E));
    // An element's ground is the base colour of its own theme's
    // appearance, whatever the root theme is (QUESTIONS M2).
    for (theme, base) in [
        (&themes::LIGHT_THEME, 0xDDE7F6),
        (&themes::DARK_THEME, 0x18202E),
    ] {
        for contrast in themes::Contrast::ALL {
            assert_eq!(Surface::Ground.resolve(theme, contrast), rgb_hex(base));
        }
    }
    set_base_color(true, None);
    set_base_color(false, None);
    assert_eq!(base_fill(), p().ground);
    for theme in themes::registered() {
        assert_eq!(
            Surface::Ground.resolve(theme, themes::Contrast::Standard),
            theme.standard.ground,
            "{}: no choice is the theme's own ground",
            theme.key
        );
    }
}
