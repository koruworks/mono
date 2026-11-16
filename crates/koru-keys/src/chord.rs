// SPDX-License-Identifier: MPL-2.0

//! A chord: one key, plus the modifiers held with it.
//!
//! This is the shape every resolver in the tree already has, written once. The
//! editor's `input::keymap` has carried its own copy — with a `ctrl: bool` that
//! secretly meant *primary* — and the other three resolvers spell the same
//! thing out inline in `if` chains.
//!
//! ## Two platform traps that live in [`On::Letter`]
//!
//! Both were real bugs, and both are why a chord is matched through a function
//! rather than compared to a `Key`:
//!
//! - **Ctrl+I arrives as `NamedKey::Tab`** on Windows. [`letter`] maps it back,
//!   so `Ctrl+I` can mean italic while a bare `Tab` still indents.
//! - **Ctrl+letter arrives as an ASCII control code**, not a character: Ctrl+K
//!   is `0x0B`. [`letter`] maps both encodings to the letter.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::modifiers::has_foreign_primary;

/// The key half of a chord.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum On {
    /// A named key: Enter, Tab, an arrow.
    Named(NamedKey),
    /// A character, matched through [`letter`] so both encodings hit. Not only
    /// letters: punctuation and digits pass through, which is what lets a chord
    /// sit on `\` or `=`.
    Letter(char),
}

/// A key and the modifiers held with it.
///
/// `mods` is literal — whatever is written here is what must be held. Use
/// [`crate::PRIMARY`] for "the app's modifier" so the chord follows the
/// platform, and `ModifiersState::CONTROL` only where the **Control key
/// itself** is meant on every platform (tab cycling, which cannot be ⌘+Tab on
/// macOS because that is the OS app switcher).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    pub on: On,
    pub mods: ModifiersState,
}

impl Chord {
    pub const fn new(on: On, mods: ModifiersState) -> Self {
        Self { on, mods }
    }

    /// Does this key press match?
    ///
    /// The modifier comparison is exact — a chord written without Shift does
    /// not fire with Shift held — and it additionally refuses the **foreign**
    /// primary, so ⌘+B does nothing on Windows and Ctrl+B does nothing on
    /// macOS. That refusal is not implied by the exact comparison: a chord on
    /// `PRIMARY` would otherwise match `PRIMARY | SUPER` on Windows only if
    /// SUPER were in the mask, and it is not.
    pub fn matches(&self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        // The foreign primary is never part of a chord, so its presence is
        // always a reason to decline — including when `self.mods` is empty.
        if has_foreign_primary(mods) && !has_foreign_primary(self.mods) {
            return false;
        }
        if mods != self.mods {
            return false;
        }
        match self.on {
            On::Named(n) => matches!(key, Key::Named(k) if *k == n),
            On::Letter(c) => letter(key, text) == Some(c),
        }
    }
}

/// Shorthand for a named key, so a table reads as one.
pub const fn named(n: NamedKey) -> On {
    On::Named(n)
}

/// The character a chord is on, normalised across both encodings.
///
/// Ctrl+K reaches us as `Character("k")` on some platforms and as the ASCII
/// control character `0x0B` on others — Windows in particular delivers the
/// control code in `text`. Matching only on `"k"` is why Ctrl+K did nothing.
///
/// Punctuation has the same split and a smaller alphabet of it: Ctrl+`\` is
/// `0x1C`, and the three codes after it belong to `]`, `^` and `_`. Anything
/// else printable is simply itself, which is what lets a chord be written on a
/// key that is not a letter at all — and what lets digits arrive verbatim, so
/// `Ctrl+1` can select a workspace.
pub fn letter(key: &Key, text: Option<&str>) -> Option<char> {
    fn from_char(ch: char) -> Option<char> {
        if ch.is_ascii_alphabetic() {
            return Some(ch.to_ascii_lowercase());
        }
        // 0x01..=0x1A are Ctrl+A..Ctrl+Z.
        let code = ch as u32;
        if (1..=26).contains(&code) {
            return Some((b'a' + (code as u8 - 1)) as char);
        }
        match ch {
            '\u{1c}' => Some('\\'),
            '\u{1d}' => Some(']'),
            '\u{1e}' => Some('^'),
            '\u{1f}' => Some('_'),
            c if !c.is_control() => Some(c),
            _ => None,
        }
    }

    if let Key::Character(c) = key
        && let Some(found) = c.chars().next().and_then(from_char)
    {
        return Some(found);
    }
    // Ctrl+I is indistinguishable from Tab (0x09) on Windows, and arrives as a
    // named key with no text. Map it back so Ctrl+I can mean italic.
    if matches!(key, Key::Named(NamedKey::Tab)) {
        return Some('i');
    }
    text.and_then(|t| t.chars().next()).and_then(from_char)
}

#[cfg(test)]
mod tests;
