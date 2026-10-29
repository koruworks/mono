// SPDX-License-Identifier: MPL-2.0

//! The font families installed on this machine, for a person who wants to pick
//! one by name.
//!
//! Koru ships a handful of families, and the text engine already discovers the
//! system's (see `text::prototype`), so a family the person names resolves the
//! same way a bundled one does. This module adds the three things a picker
//! needs on top: the **list**, whether each family is **monospaced** (a
//! terminal draws a fixed grid, and a proportional face breaks its columns),
//! and a **stack** to draw with that keeps the usual fallbacks behind the
//! chosen family.
//!
//! The scan lays a few characters out in every family, which takes a moment on
//! a machine with hundreds of them, so it runs once and callers that can wait
//! should [`warm`] it from a thread first.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use parley::style::{FontFamily, LineHeight, StyleProperty};

use crate::text::TextEngine;
use crate::theme::Brush;

/// What follows a chosen terminal family in its stack: the platform's
/// monospaced faces, then the symbol and script fallbacks every other stack
/// carries, so a glyph the family lacks still draws.
const MONO_TAIL: &str = "ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace, Microsoft YaHei UI, Microsoft YaHei, PingFang SC, Noto Sans CJK SC, Noto Sans SC, Yu Gothic UI, Hiragino Sans, Noto Sans KR, Noto Sans Arabic, Segoe UI, Geeza Pro";

/// The longest family name Koru will store or put in a stack.
const MAX_NAME: usize = 64;

/// One installed family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledFont {
    pub family: String,
    /// Every character the test string uses is the same width.
    pub monospace: bool,
}

/// Whether `name` is safe to store in the profile file and to splice into a
/// font stack: no separator of either, no quote, no control character.
pub fn is_valid_family_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= MAX_NAME
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, ',' | ';' | '|' | '=' | '"' | '\'' | '`' | '\\' | '@')
        })
}

impl TextEngine {
    /// The width of `text` in one named family at `size`.
    fn advance_in_family(&mut self, text: &str, family: &str, size: f32) -> f32 {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(
            family.to_owned(),
        ))));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.0)));
        builder.push_default(StyleProperty::Brush(Brush(crate::theme::p().text)));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        layout.full_width()
    }

    /// Whether `family` sets a narrow letter, a wide one and a full stop at one
    /// width. A face that is not installed measures its fallback, so the caller
    /// only asks about families the collection named.
    fn is_monospace(&mut self, family: &str) -> bool {
        const SIZE: f32 = 16.0;
        let narrow = self.advance_in_family("iiiiiiiiii", family, SIZE);
        let wide = self.advance_in_family("WWWWWWWWWW", family, SIZE);
        let stop = self.advance_in_family("..........", family, SIZE);
        narrow > 0.0
            && (narrow - wide).abs() < narrow * 0.01
            && (narrow - stop).abs() < narrow * 0.01
    }
}

static INSTALLED: OnceLock<Vec<InstalledFont>> = OnceLock::new();

fn scan() -> Vec<InstalledFont> {
    let mut engine = TextEngine::new();
    let mut names: Vec<String> = engine
        .font_cx
        .collection
        .family_names()
        .filter(|name| name.chars().next().is_some_and(|c| c != '@') && is_valid_family_name(name))
        .map(str::to_owned)
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names
        .into_iter()
        .map(|family| {
            let monospace = engine.is_monospace(&family);
            InstalledFont { family, monospace }
        })
        .collect()
}

/// Every installed family, by name, with whether it is monospaced. The first
/// call scans, and the others wait for it.
pub fn installed_fonts() -> &'static [InstalledFont] {
    INSTALLED.get_or_init(scan)
}

/// Start the scan on a thread, so a picker opened a moment later finds it done.
pub fn warm() {
    if INSTALLED.get().is_none() {
        std::thread::spawn(|| {
            let _ = installed_fonts();
        });
    }
}

static STACKS: Mutex<BTreeMap<String, Option<&'static str>>> = Mutex::new(BTreeMap::new());

/// The stack a terminal draws with when its font is the installed family
/// `family`, or `None` when the name is not one this build may use or no such
/// family is installed (a profile carried from another machine reads as
/// *follow Settings*, never as a failure).
///
/// Answered from a cache after the first look, because the terminal asks every
/// frame. Each distinct name is leaked once: a few bytes per font the person
/// ever chose.
pub fn terminal_stack_for(family: &str) -> Option<&'static str> {
    let family = family.trim();
    if !is_valid_family_name(family) {
        return None;
    }
    let mut stacks = STACKS.lock().unwrap_or_else(|held| held.into_inner());
    if let Some(known) = stacks.get(family) {
        return *known;
    }
    let installed = crate::text::prototype()
        .clone()
        .collection
        .family_id(family)
        .is_some();
    let stack = installed.then(|| &*Box::leak(format!("{family}, {MONO_TAIL}").into_boxed_str()));
    stacks.insert(family.to_owned(), stack);
    stack
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_a_separator_or_a_quote_is_not_storable() {
        for bad in [
            "",
            "  ",
            "a,b",
            "a;b",
            "a|b",
            "a=b",
            "a\"b",
            "a'b",
            "a\\b",
            "@Vertical",
        ] {
            assert!(!is_valid_family_name(bad), "{bad:?}");
        }
        assert!(!is_valid_family_name(&"x".repeat(65)));
        for good in [
            "JetBrains Mono",
            "Cascadia Code NF",
            "Fira Code",
            "MS Gothic",
            "Iosevka-Term",
        ] {
            assert!(is_valid_family_name(good), "{good:?}");
        }
    }

    #[test]
    fn the_bundled_mono_is_found_and_measured_as_monospaced() {
        let fonts = installed_fonts();
        let jetbrains = fonts
            .iter()
            .find(|font| font.family == "JetBrains Mono")
            .expect("a bundled family is in the collection");
        assert!(jetbrains.monospace);
        let inter = fonts
            .iter()
            .find(|font| font.family == "Inter")
            .expect("a bundled family is in the collection");
        assert!(
            !inter.monospace,
            "a proportional face is not offered by default"
        );
    }

    #[test]
    fn a_family_that_is_not_installed_has_no_stack() {
        assert_eq!(terminal_stack_for("No Such Face 9000"), None);
        assert_eq!(terminal_stack_for("bad;name"), None);
        let stack = terminal_stack_for("JetBrains Mono").expect("bundled");
        assert!(stack.starts_with("JetBrains Mono, "));
        assert!(std::ptr::eq(
            stack,
            terminal_stack_for("JetBrains Mono").unwrap()
        ));
    }
}
