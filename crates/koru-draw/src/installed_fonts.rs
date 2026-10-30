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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use parley::style::{FontFamily, LineHeight, StyleProperty};

use crate::text::TextEngine;
use crate::theme::Brush;

/// What follows a chosen terminal family in its stack: the platform's
/// monospaced faces, then the symbol and script fallbacks every other stack
/// carries, so a glyph the family lacks still draws.
const MONO_TAIL: &str = "ui-monospace, Cascadia Mono, Menlo, Consolas, Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2, monospace, Microsoft YaHei UI, Microsoft YaHei, PingFang SC, Noto Sans CJK SC, Noto Sans SC, Yu Gothic UI, Hiragino Sans, Noto Sans KR, Noto Sans Arabic, Segoe UI, Geeza Pro";

/// Whether `name` is safe to store in the profile file and to splice into a
/// font stack. The rule is `koru-settings`' — there is one copy of it.
pub fn is_valid_family_name(name: &str) -> bool {
    koru_settings::is_storable_family_name(name)
}

/// One installed family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledFont {
    pub family: String,
    /// Every character the test string uses is the same width.
    pub monospace: bool,
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
    start(|| {});
}

/// The stack a terminal draws with when its font is the installed family
/// `family`, or `None` when the name is not one this build may use or no such
/// family is installed (a profile carried from another machine reads as
/// *follow Settings*, never as a failure).
///
/// Answered from the interner after the first look, because the terminal asks
/// every frame. Always `MONO_TAIL` behind the family, whatever the family is:
/// a terminal draws a grid.
pub fn terminal_stack_for(family: &str) -> Option<&'static str> {
    resolve(family).map(|id| interned(id).terminal_stack)
}

// --------------------------------------------------- one installed family
//
// A font role or a terminal names one installed family. Resolving it is one
// lookup in the text engine's collection and three short layouts to learn
// whether it is monospaced — never the scan above — so a family stored in
// the settings is drawn from the first frame without waiting for the list a
// picker needs. Each name is interned once per process: a few bytes for
// every family the person ever chose.

/// An interned installed family. Small and `Copy`, so a [`FontChoice`] can
/// carry one and stay `Copy` itself.
///
/// [`FontChoice`]: crate::theme::FontChoice
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstalledId(u16);

impl InstalledId {
    pub(crate) fn index(self) -> u16 {
        self.0
    }

    pub(crate) fn from_index(index: u16) -> Option<Self> {
        let interned = INTERNED.lock().unwrap_or_else(|held| held.into_inner());
        (usize::from(index) < interned.len()).then_some(Self(index))
    }
}

/// What an interned family is drawn with.
#[derive(Copy, Clone, Debug)]
pub struct Interned {
    pub name: &'static str,
    pub monospace: bool,
    /// A font role's stack: the family, then the monospaced faces for a
    /// monospaced family or the sans tail for any other, then the scripts.
    pub role_stack: &'static str,
    /// A terminal's: the family, then always [`MONO_TAIL`] — exactly what the
    /// terminal's per-agent font has always drawn with.
    pub terminal_stack: &'static str,
}

/// What follows any proportional family: the platform's sans, then the
/// script faces every bundled stack carries. Held equal to
/// `FontChoice::System`'s stack by a test.
pub(crate) const SANS_TAIL: &str = "system-ui, sans-serif, Microsoft YaHei UI, Microsoft YaHei, PingFang SC, Noto Sans CJK SC, Noto Sans SC, Yu Gothic UI, Hiragino Sans, Noto Sans KR, Noto Sans Arabic, Segoe UI, Geeza Pro";

static INTERNED: Mutex<Vec<Interned>> = Mutex::new(Vec::new());
/// Names looked up and found missing, so a terminal asking every frame does
/// not ask the collection every frame.
static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The installed family `name`, interned, or `None` when the name is not
/// storable or no such family is installed here. Never the full scan.
pub fn resolve(name: &str) -> Option<InstalledId> {
    let name = name.trim();
    if !is_valid_family_name(name) {
        return None;
    }
    {
        let interned = INTERNED.lock().unwrap_or_else(|held| held.into_inner());
        if let Some(i) = interned.iter().position(|entry| entry.name == name) {
            return Some(InstalledId(i as u16));
        }
    }
    if MISSING
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .iter()
        .any(|missing| missing == name)
    {
        return None;
    }
    let installed = crate::text::prototype()
        .clone()
        .collection
        .family_id(name)
        .is_some();
    if !installed {
        MISSING
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(name.to_owned());
        return None;
    }
    let monospace = TextEngine::new().is_monospace(name);
    let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let entry = Interned {
        name: leak(name.to_owned()),
        monospace,
        role_stack: leak(if monospace {
            format!("{name}, {MONO_TAIL}")
        } else {
            format!("{name}, {SANS_TAIL}")
        }),
        terminal_stack: leak(format!("{name}, {MONO_TAIL}")),
    };
    let mut interned = INTERNED.lock().unwrap_or_else(|held| held.into_inner());
    if let Some(i) = interned.iter().position(|e| e.name == name) {
        return Some(InstalledId(i as u16));
    }
    interned.push(entry);
    Some(InstalledId((interned.len() - 1) as u16))
}

/// What an interned family is drawn with.
pub fn interned(id: InstalledId) -> Interned {
    INTERNED.lock().unwrap_or_else(|held| held.into_inner())[usize::from(id.0)]
}

// ------------------------------------------- the list, without waiting for it

static WAITING: Mutex<Vec<Box<dyn FnOnce() + Send>>> = Mutex::new(Vec::new());
static STARTED: AtomicBool = AtomicBool::new(false);

/// The installed families, if the scan has finished. Never waits — the
/// one accessor an event thread may use.
pub fn ready() -> Option<&'static [InstalledFont]> {
    INSTALLED.get().map(Vec::as_slice)
}

/// Make sure the scan runs, once, on a thread, and call `wake` when the list
/// is [`ready`] — at once if it already is. A second call while the scan runs
/// only adds its `wake`.
pub fn start(wake: impl FnOnce() + Send + 'static) {
    if ready().is_some() {
        wake();
        return;
    }
    WAITING
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .push(Box::new(wake));
    if !STARTED.swap(true, Ordering::AcqRel) {
        std::thread::spawn(|| {
            let _ = installed_fonts();
            wake_waiting();
        });
    } else if ready().is_some() {
        // The scan finished between the first check and the push.
        wake_waiting();
    }
}

fn wake_waiting() {
    let waiting = std::mem::take(&mut *WAITING.lock().unwrap_or_else(|held| held.into_inner()));
    for wake in waiting {
        wake();
    }
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

#[cfg(test)]
mod installed_tests {
    use super::*;
    use crate::theme::FontChoice;

    /// The sans tail an installed family takes is the one the bundled sans
    /// stacks carry, so a missing glyph falls back the same way.
    #[test]
    fn the_sans_tail_is_the_bundled_one() {
        assert_eq!(FontChoice::System.stack(), SANS_TAIL);
    }

    /// One interner for the terminal and the roles, and the terminal's
    /// stack is exactly what it always was.
    #[test]
    fn a_family_resolves_once_and_the_terminal_stack_is_unchanged() {
        let mono = resolve("JetBrains Mono").expect("a bundled family is in the collection");
        assert_eq!(resolve("JetBrains Mono"), Some(mono));
        let entry = interned(mono);
        assert!(entry.monospace);
        assert_eq!(entry.terminal_stack, format!("JetBrains Mono, {MONO_TAIL}"));
        assert_eq!(entry.role_stack, format!("JetBrains Mono, {MONO_TAIL}"));
        assert_eq!(
            terminal_stack_for("JetBrains Mono"),
            Some(entry.terminal_stack)
        );

        let sans = resolve("Inter").expect("a bundled family is in the collection");
        let entry = interned(sans);
        assert!(!entry.monospace);
        assert_eq!(entry.role_stack, format!("Inter, {SANS_TAIL}"));
        assert_eq!(
            entry.terminal_stack,
            format!("Inter, {MONO_TAIL}"),
            "a terminal draws a grid, whatever the family"
        );
        assert_eq!(resolve("No Such Face 9000"), None);
        assert_eq!(terminal_stack_for("a,b"), None);
    }

    /// The list is started once and every waiter hears when it is ready;
    /// `ready` never waits.
    #[test]
    fn the_list_starts_once_and_wakes_every_waiter() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let woken = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let woken = Arc::clone(&woken);
            start(move || {
                woken.fetch_add(1, Ordering::SeqCst);
            });
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while woken.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert!(ready().is_some());
    }
}
