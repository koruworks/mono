// SPDX-License-Identifier: MPL-2.0

//! The shortcut reference reads the same registry as the input layers.
use crate::page::Builder;
use koru_i18n::{Key, Lang};
use koru_keys::{Layer, REGISTRY, Section, ShortcutId, show_hint};

fn chord(entry: &koru_keys::Shortcut) -> String {
    show_hint(entry.id, Section::Reference).unwrap_or_default()
}

fn entry_matches(entry: &koru_keys::Shortcut, query: &str) -> bool {
    let words = format!("{} {} {}", entry.layer.name(), entry.label, chord(entry)).to_lowercase();
    query
        .split_whitespace()
        .all(|word| words.contains(&word.to_lowercase()))
}

pub(crate) fn matches(query: &str) -> bool {
    REGISTRY.iter().any(|entry| entry_matches(entry, query))
}

pub(crate) fn lay_out(b: &mut Builder<'_>, lang: Lang, query: &str) {
    b.search(""); // Rows are filtered individually below, rather than by whole group.
    let page_match = koru_i18n::t(lang, Key::SettingsShortcuts)
        .to_lowercase()
        .contains(&query.to_lowercase());
    for layer in [
        Layer::Shell,
        Layer::Container,
        Layer::View,
        Layer::Editor,
        Layer::Terminal,
        Layer::Chat,
    ] {
        b.card(Some(layer.name()));
        for entry in REGISTRY.iter().filter(|entry| {
            !entry.internal_only
                && entry.layer == layer
                && (page_match || entry_matches(entry, query))
        }) {
            b.next_row_chip(&chord(entry));
            b.row_text(entry.label, None);
        }
    }
    b.card(Some(koru_i18n::t(lang, Key::SettingsInternalShortcut)));
    for entry in REGISTRY
        .iter()
        .filter(|entry| entry.internal_only && (page_match || entry_matches(entry, query)))
    {
        b.next_row_chip(&chord(entry));
        b.row_text(entry.label, None);
    }
}

pub(crate) fn zoom_hint(lang: Lang) -> String {
    let hint = |id| show_hint(id, Section::Guide).unwrap_or_default();
    // Scrolling is a pointer gesture, not a keyboard chord. The key labels
    // above remain sourced from the shortcut registry.
    let modifier = if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl"
    };
    koru_i18n::fill(
        koru_i18n::t(lang, Key::SettingsZoomHint),
        &[
            &hint(ShortcutId::ZoomIn),
            &hint(ShortcutId::ZoomOut),
            &hint(ShortcutId::ZoomReset),
            modifier,
        ],
    )
}
