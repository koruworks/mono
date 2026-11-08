// SPDX-License-Identifier: MPL-2.0

//! **Every exported token resolves in every registered theme**, at both
//! contrasts and in every profile — so a fourth theme is checked the day it
//! is registered, without a test of its own.

use super::*;
use crate::theme::{Accent, IconSize, Ink, Line, Profile, Radius, Space, Surface, metrics, roles};

fn finite(c: Color) -> bool {
    c.components
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

#[test]
fn every_exported_token_resolves_in_every_theme_contrast_and_profile() {
    assert!(registered().len() >= 3, "light, dark and the fixture");
    for theme in registered() {
        for contrast in Contrast::ALL {
            let palette = theme.palette(contrast);
            let at = |what: &str| format!("{} / {} / {what}", theme.key, contrast.key());
            for role in Surface::ALL {
                assert!(finite(role.of(palette)), "{}", at(role.name()));
            }
            for role in Ink::ALL {
                assert!(finite(role.of(palette)), "{}", at(role.name()));
            }
            for role in Line::ALL {
                assert!(finite(role.of(palette)), "{}", at(role.name()));
            }
            for role in Accent::ALL {
                assert!(finite(role.of(palette)), "{}", at(role.name()));
            }
            for colour in theme.terminal.ansi {
                assert!(finite(colour), "{}", at("terminal"));
            }
            for (colour, wash) in theme.carets.colors.iter().zip(&theme.carets.selections) {
                assert!(finite(*colour) && finite(*wash), "{}", at("carets"));
            }
        }
    }
    for profile in Profile::ALL {
        for space in Space::ALL {
            let v = space.px(profile);
            assert!(v.is_finite() && v >= 0.0, "{}", space.name());
        }
        for radius in Radius::ALL {
            let v = radius.px(profile, 20.0, 26.0);
            assert!(v.is_finite() && v >= 0.0, "{}", radius.name());
        }
        for size in IconSize::ALL {
            assert!(size.px(profile) > 0.0, "{}", size.name());
        }
    }
    for metric in metrics::ALL {
        assert!(metric.value > 0.0, "{}", metric.name);
    }
    for role in roles::ALL {
        let style = role.style();
        assert!(
            style.metrics.size > 0.0 && style.metrics.line_height >= style.metrics.size,
            "{}",
            role.name
        );
    }
}

/// Keys are unique and stable, and the built-ins are what they say they are.
#[test]
fn the_registry_holds_light_dark_and_the_fixture_once_each() {
    let keys: Vec<_> = registered().iter().map(|t| t.key).collect();
    assert_eq!(keys, ["light", "dark", "fixture"]);
    assert_eq!(by_key("dark").map(|t| t.appearance), Some(Appearance::Dark));
    assert!(by_key("sepia").is_none());
    assert!(std::ptr::eq(LIGHT_THEME.standard, &LIGHT));
    assert!(std::ptr::eq(DARK_THEME.standard, &DARK));
}

/// **The built-ins' high contrast is a placeholder** (S5), stated rather
/// than hidden: each names its own standard table. The day a designed table
/// lands, this test is the one that changes.
#[test]
fn built_in_high_contrast_is_the_standard_table_for_now() {
    for theme in [&LIGHT_THEME, &DARK_THEME] {
        assert!(
            std::ptr::eq(theme.high_contrast, theme.standard),
            "{}",
            theme.key
        );
    }
}

/// The fixture's two contrasts differ in every exported colour role, so a
/// surface that resolves through the wrong table, or a cache that ignores
/// contrast, shows.
#[test]
fn the_fixture_themes_contrasts_differ_in_every_exported_role() {
    let standard = FIXTURE_THEME.palette(Contrast::Standard);
    let high = FIXTURE_THEME.palette(Contrast::High);
    let light = LIGHT_THEME.palette(Contrast::Standard);
    let pairs: Vec<(&str, Color, Color, Color)> = Surface::ALL
        .iter()
        .map(|r| (r.name(), r.of(standard), r.of(high), r.of(light)))
        .chain(
            Ink::ALL
                .iter()
                .map(|r| (r.name(), r.of(standard), r.of(high), r.of(light))),
        )
        .chain(
            Line::ALL
                .iter()
                .map(|r| (r.name(), r.of(standard), r.of(high), r.of(light))),
        )
        .chain(
            Accent::ALL
                .iter()
                .map(|r| (r.name(), r.of(standard), r.of(high), r.of(light))),
        )
        .collect();
    for (name, s, h, l) in pairs {
        assert_ne!(s, h, "{name}: the fixture's contrasts must differ");
        assert_ne!(s, l, "{name}: the fixture must not be the light table");
    }
}

/// The appearance is derived from the theme: the caret, the terminal and the
/// canvas presets each read the active theme's set, and the built-ins hold
/// today's values.
#[test]
fn each_theme_names_the_sets_it_supplies() {
    assert!(std::ptr::eq(LIGHT_THEME.terminal, &LIGHT_TERMINAL));
    assert!(std::ptr::eq(DARK_THEME.terminal, &DARK_TERMINAL));
    assert!(std::ptr::eq(LIGHT_THEME.carets, &LIGHT_CARETS));
    assert!(std::ptr::eq(DARK_THEME.carets, &DARK_CARETS));
    assert_eq!(DARK_THEME.canvas_presets, &CANVAS_PRESETS_DARK);
    assert!(DARK_THEME.is_dark() && !LIGHT_THEME.is_dark() && !FIXTURE_THEME.is_dark());
    assert_eq!(
        crate::theme::CaretColor::Violet.color_for(true),
        rgb(0x8B, 0x5C, 0xF6)
    );
    assert_eq!(
        crate::theme::CaretColor::Blue.color_for(false),
        rgb(0x25, 0x63, 0xEB)
    );
}
