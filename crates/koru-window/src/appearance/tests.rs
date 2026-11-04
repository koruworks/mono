// SPDX-License-Identifier: MPL-2.0

use super::*;

use koru_settings::{
    CaretColorChoice, CaretWidthChoice, DensityChoice, FontFamily, FontSetting, ThemeChoice, Zoom,
};

/// **What an independent window shows before it has heard anything.** These are
/// the values `AppearancePreferences::default()` gave when `preferences.conf`
/// could not be read, and they have to stay exactly that: the search overlay
/// opens on them, on the main thread, before any window exists.
#[test]
fn the_fallback_appearance_is_unchanged_from_the_parser_it_replaces() {
    let appearance = Appearance::default();
    assert_eq!(appearance.theme, ThemePreference::System);
    assert_eq!(appearance.caret.color, CaretColor::Default);
    assert_eq!(appearance.caret.width, CaretWidth::Medium);
    assert_eq!(appearance.caret.height, CaretHeight::Medium);
    assert_eq!(appearance.ui_font, theme::default_ui_font());
    assert_eq!(appearance.body_font, theme::default_body_font());
    assert_eq!(appearance.headings_font, theme::default_headings_font());
    assert_eq!(appearance.code_font, theme::default_code_font());
    assert_eq!(appearance.density, Density::Normal);
    assert_eq!(appearance.zoom, 1.0);
    assert!(
        appearance.follows_system(),
        "a window that has heard nothing still follows the OS, so a light/dark \
         change still reaches it"
    );
}

/// Every value the section carries reaches the projection. One test rather than
/// eight, because the failure mode is a *field* forgotten in `from_section` —
/// and a forgotten field is a window that quietly ignores a preference.
#[test]
fn every_appearance_value_comes_from_the_section() {
    let section = GlobalSettings {
        theme: ThemeChoice::Dark,
        icon_pack: Some(koru_settings::IconPackChoice::Lucide),
        caret_color: CaretColorChoice::Amber,
        caret_width: CaretWidthChoice::Thick,
        caret_height: koru_settings::CaretHeightChoice::Short,
        ui_font: FontSetting::Chosen(FontFamily::Poppins),
        body_font: FontSetting::Chosen(FontFamily::SourceSerif4),
        headings_font: FontSetting::Chosen(FontFamily::Lora),
        code_font: FontSetting::Chosen(FontFamily::SourceCodePro),
        density: DensityChoice::Compact,
        zoom: Zoom::new(1.25).unwrap(),
        ..GlobalSettings::default()
    };
    let appearance = Appearance::from_section(&section);
    assert_eq!(appearance.theme, ThemePreference::Dark);
    assert_eq!(
        appearance.icon_pack,
        Some(koru_draw::icons::IconPack::Lucide)
    );
    assert_eq!(appearance.caret.color, CaretColor::Amber);
    assert_eq!(appearance.caret.width, CaretWidth::Thick);
    assert_eq!(appearance.caret.height, CaretHeight::Short);
    assert_eq!(appearance.ui_font, FontChoice::Poppins);
    assert_eq!(appearance.body_font, FontChoice::SourceSerif4);
    assert_eq!(appearance.headings_font, FontChoice::Lora);
    assert_eq!(appearance.code_font, FontChoice::SourceCodePro);
    assert_eq!(appearance.density, Density::Compact);
    assert_eq!(appearance.zoom, 1.25);
    assert!(!appearance.follows_system());
    assert!(appearance.effective_dark());
}

/// **A role that follows the app follows *this build*.** The reason the
/// authority keeps `FollowDefault` as a state: a default serialized into a file
/// froze that default for everyone who had ever pressed save.
#[test]
fn a_role_that_follows_the_app_resolves_to_this_builds_default() {
    let section = GlobalSettings {
        headings_font: FontSetting::FollowDefault,
        ..GlobalSettings::default()
    };
    assert_eq!(
        Appearance::from_section(&section).headings_font,
        theme::default_headings_font()
    );
}

/// The caret projection is the same values, for a host that has nothing else —
/// the Agent's overlay, which draws one text field.
#[test]
fn the_caret_projection_is_the_appearances_own() {
    let section = GlobalSettings {
        caret_color: CaretColorChoice::Violet,
        caret_width: CaretWidthChoice::Thin,
        ..GlobalSettings::default()
    };
    assert_eq!(
        CaretAppearance::from_section(&section),
        Appearance::from_section(&section).caret
    );
}

/// A caret width a file may carry but no build writes still arrives intact —
/// the tolerant-read asymmetry the authority inherited, seen from this side.
#[test]
fn the_read_only_caret_widths_reach_the_window() {
    for (choice, width) in [
        (CaretWidthChoice::X4, CaretWidth::X4),
        (CaretWidthChoice::X8, CaretWidth::X8),
    ] {
        let section = GlobalSettings {
            caret_width: choice,
            ..GlobalSettings::default()
        };
        assert_eq!(CaretAppearance::from_section(&section).width, width);
    }
}
