// SPDX-License-Identifier: MPL-2.0

//! Raster controls below the theme selection, using the existing Settings controls.
use super::*;
use koru_settings::{TextAntialiasing, TextRendering};

pub(super) fn rows(b: &mut Builder<'_>, lang: Lang) {
    let current = koru_draw::text_rendering::saved();
    let defaults = TextRendering::defaults(theme::is_dark());
    b.caption_with_link(
        koru_i18n::t(lang, Key::SettingsTextRendering),
        koru_i18n::t(lang, Key::SettingsRestoreDefaults),
        Action::TextRendering(defaults),
    );
    b.card(None);
    b.row_toggle(
        koru_i18n::t(lang, Key::SettingsTextHinting),
        Some(koru_i18n::t(lang, Key::SettingsTextRenderingHint)),
        current.hinting,
        Action::TextRendering(TextRendering {
            hinting: !current.hinting,
            ..current
        }),
    );
    b.row_dropdown(
        koru_i18n::t(lang, Key::SettingsTextAntialiasing),
        None,
        TextAntialiasing::ALL
            .into_iter()
            .map(|aa| {
                let label = match aa {
                    TextAntialiasing::Area => "Area",
                    TextAntialiasing::Msaa8 => "MSAA8",
                    TextAntialiasing::Msaa16 => "MSAA16",
                };
                (
                    Action::TextAntialiasing(aa),
                    DropdownOption::new(
                        default_label(lang, label.to_owned(), aa == defaults.antialiasing),
                        aa == current.antialiasing,
                    ),
                )
            })
            .collect(),
    );
    for (key, selected, default, options) in [
        (
            Key::SettingsTextOffset,
            current.offset,
            defaults.offset,
            &[0.0, 0.25, 0.5, 0.75, 1.0][..],
        ),
        (
            Key::SettingsTextGamma,
            current.gamma,
            defaults.gamma,
            &[0.5, 0.75, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0][..],
        ),
        (
            Key::SettingsTextCutoff,
            current.cutoff,
            defaults.cutoff,
            &[0.0, 0.02, 0.05, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5][..],
        ),
    ] {
        let mut values = options.to_vec();
        if !values.contains(&selected) {
            values.push(selected);
            values.sort_by(f32::total_cmp);
        }
        b.row_dropdown(
            koru_i18n::t(lang, key),
            None,
            values
                .into_iter()
                .map(|value| {
                    let (action, label) = match key {
                        Key::SettingsTextOffset => (
                            Action::TextOffset(value),
                            koru_i18n::number::fraction(lang, value),
                        ),
                        Key::SettingsTextGamma => (
                            Action::TextGamma(value),
                            koru_i18n::number::fraction(lang, value),
                        ),
                        _ => (
                            Action::TextCutoff(value),
                            format!("{}%", koru_i18n::number::fraction(lang, value * 100.0)),
                        ),
                    };
                    (
                        action,
                        DropdownOption::new(
                            default_label(lang, label, value == default),
                            value == selected,
                        ),
                    )
                })
                .collect(),
        );
    }
}

fn default_label(lang: Lang, label: String, is_default: bool) -> String {
    if is_default {
        koru_i18n::fill(koru_i18n::t(lang, Key::SettingsTextDefaultValue), &[&label])
    } else {
        label
    }
}
