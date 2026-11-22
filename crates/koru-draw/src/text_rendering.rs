// SPDX-License-Identifier: MPL-2.0

//! One live raster preference shared by native windows and the Settings rows.
//! Read once per composed frame, never per glyph or while laying out text.
use crate::theme::preview::{self, PreviewSlot};
use koru_settings::{TextRendering, TextRenderingPreferences};
use std::sync::RwLock;

static CURRENT: RwLock<TextRenderingPreferences> = RwLock::new(TextRenderingPreferences::DEFAULT);

pub fn current() -> TextRendering {
    let mut shown = saved();
    if let Some(aa) = preview::previewed(PreviewSlot::TextAntialiasing(crate::theme::is_dark()))
        .and_then(|value| {
            koru_settings::TextAntialiasing::ALL
                .get(value as usize)
                .copied()
        })
    {
        shown.antialiasing = aa;
    }
    for (slot, value) in [
        (
            PreviewSlot::TextOffset(crate::theme::is_dark()),
            &mut shown.offset,
        ),
        (
            PreviewSlot::TextGamma(crate::theme::is_dark()),
            &mut shown.gamma,
        ),
        (
            PreviewSlot::TextCutoff(crate::theme::is_dark()),
            &mut shown.cutoff,
        ),
    ] {
        if let Some(bits) = preview::previewed(slot) {
            *value = f32::from_bits(bits);
        }
    }
    if shown.patch().validate().is_ok() {
        shown
    } else {
        saved()
    }
}

/// Committed preferences only: used for persistence and selected menu rows.
pub fn saved() -> TextRendering {
    preferences().for_theme(crate::theme::is_dark())
}

pub fn preferences() -> TextRenderingPreferences {
    *CURRENT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn set(value: TextRendering) {
    // Also protects direct callers from sending NaN into the GPU. Authority
    // patches reject invalid values before saving them.
    if value.patch().validate().is_ok() {
        let mut preferences = CURRENT
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if crate::theme::is_dark() {
            preferences.dark = value;
        } else {
            preferences.light = value;
        }
        for slot in [
            PreviewSlot::TextAntialiasing(crate::theme::is_dark()),
            PreviewSlot::TextOffset(crate::theme::is_dark()),
            PreviewSlot::TextGamma(crate::theme::is_dark()),
            PreviewSlot::TextCutoff(crate::theme::is_dark()),
        ] {
            preview::clear(slot);
        }
    }
}

pub fn set_preferences(value: TextRenderingPreferences) {
    if value.light.patch().validate().is_ok() && value.dark.patch().validate().is_ok() {
        *CURRENT
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
    }
}
