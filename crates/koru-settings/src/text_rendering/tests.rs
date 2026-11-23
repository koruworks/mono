// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{GlobalPatch, GlobalSettings, storage};

#[test]
fn theme_overrides_round_trip_independently_and_reset_only_one_theme() {
    let mut settings = GlobalSettings::default();
    let patch = GlobalPatch {
        text_gamma: Some(2.0),
        text_gamma_dark: Some(0.5),
        text_offset_dark: Some(0.25),
        ..Default::default()
    };
    assert!(patch.validate().is_ok());
    patch.apply(&mut settings);
    let written = storage::render_preferences(&settings, &storage::PreferencesFile::default());
    let mut loaded = GlobalSettings::default();
    storage::apply_preferences(&written, &mut loaded);
    assert_eq!(settings, loaded);
    TextRendering::DEFAULT.patch().apply(&mut loaded);
    assert_eq!(loaded.text_gamma_dark, 0.5);
    assert_eq!(loaded.text_offset_dark, 0.25);
    let mut invalid = GlobalPatch::default();
    invalid.text_gamma_dark = Some(f32::NAN);
    assert!(invalid.validate().is_err());
    let mut legacy = GlobalSettings::default();
    storage::apply_preferences("version=2\ntext_gamma=1.25\n", &mut legacy);
    let both = TextRenderingPreferences::from_section(&legacy);
    assert_eq!(both.light.gamma, 1.25);
    assert_eq!(both.dark, TextRendering::DARK_DEFAULT);
}

#[test]
fn rendering_preferences_round_trip_and_old_readers_have_defaults() {
    let selected = TextRendering {
        hinting: false,
        antialiasing: TextAntialiasing::Msaa8,
        offset: 0.25,
        gamma: 1.1,
        cutoff: 0.02,
    };
    let mut settings = GlobalSettings::default();
    settings.heading_weight = 500;
    let patch = selected.patch();
    assert!(patch.touches_roaming());
    assert!(patch.validate().is_ok());
    patch.apply(&mut settings);
    let text = storage::render_preferences(&settings, &storage::PreferencesFile::default());
    let mut loaded = GlobalSettings::default();
    storage::apply_preferences(&text, &mut loaded);
    assert_eq!(loaded, settings);
    let mut old = serde_json::to_value(&settings).unwrap();
    for name in [
        "text_hinting",
        "text_antialiasing",
        "text_offset",
        "text_gamma",
        "text_cutoff",
        "heading_weight",
    ] {
        old.as_object_mut().unwrap().remove(name);
    }
    let old: GlobalSettings = serde_json::from_value(old).unwrap();
    assert_eq!(old.heading_weight, 600);
    assert_eq!(TextRendering::from_section(&old), TextRendering::default());
    TextRendering::default().patch().apply(&mut loaded);
    GlobalPatch {
        heading_weight: Some(600),
        ..Default::default()
    }
    .apply(&mut loaded);
    assert_eq!(
        TextRendering::from_section(&loaded),
        TextRendering::default()
    );
    let reset = storage::render_preferences(&loaded, &storage::PreferencesFile::default());
    assert!(!reset.contains("heading_weight="));
    assert!(
        !reset.lines().any(|line| line.starts_with("text_")),
        "restored values follow platform defaults instead of becoming roaming overrides"
    );
}

#[test]
fn invalid_parameters_are_rejected_and_bad_file_values_keep_defaults() {
    for weight in [0, 400, 550, 700, u16::MAX] {
        assert!(
            GlobalPatch {
                heading_weight: Some(weight),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for value in [f32::NAN, f32::INFINITY, -0.1, 3.1] {
        assert!(
            GlobalPatch {
                text_gamma: Some(value),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for value in [-0.1, 0.51, f32::NAN] {
        assert!(
            GlobalPatch {
                text_cutoff: Some(value),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        GlobalPatch {
            text_offset: Some(1.1),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    let mut settings = GlobalSettings::default();
    storage::apply_preferences(
        "version=2\ntext_gamma=NaN\ntext_cutoff=0.9\ntext_offset=-1\ntext_antialiasing=typo\ntext_hinting=typo\n",
        &mut settings,
    );
    assert_eq!(
        TextRendering::from_section(&settings),
        TextRendering::default()
    );
    GlobalPatch {
        text_gamma: Some(1.25),
        ..Default::default()
    }
    .apply(&mut settings);
    assert_eq!(settings.text_gamma, 1.25);
    assert_eq!(settings.text_offset, TextRendering::DEFAULT.offset);
}
