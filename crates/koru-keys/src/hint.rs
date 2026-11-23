// SPDX-License-Identifier: MPL-2.0

//! Advertising a chord — **the one seam every printed shortcut goes through**.
//!
//! A hint is a chord written down where somebody can read it: the trailing
//! column of a menu row, the second line of a tooltip, the heading of the panel
//! a chord opens. It is not the binding. [`Chord`](crate::Chord) is what a key
//! press is matched against; [`Hinted`] is what a surface *prints*, and the two
//! are different jobs with different failure modes.
//!
//! ## Why one function
//!
//! Two reasons, and the second is the one this module exists for.
//!
//! **A printed chord must be the bound chord.** The literals came first — 77 of
//! them in the document's menus alone, every one wrong on macOS — and
//! `registry::label_of` fixed those by rendering from the table. The remaining
//! literals were in tooltips, which nothing had swept: four in the container's
//! caption said `Ctrl+…` on a Mac. Going through [`show_hint`] with a
//! [`ShortcutId`] means the label cannot drift from the binding, because there
//! is only one of them.
//!
//! **And hints have to be switchable.** Somebody who knows the app does not
//! need every menu row teaching them a chord they typed to open the menu; a
//! screenshot does not want them; a demo may want only the tooltips. That is a
//! decision about a *class* of hint rather than about one row, so it cannot live
//! at a call site — which is why every call site asks here first, and why the
//! answer is `Option<String>` rather than a `String` a caller could draw anyway.
//!
//! ```
//! # use koru_keys::{Section, ShortcutId, show_hint};
//! if let Some(text) = show_hint(ShortcutId::NewTab, Section::Tooltip) {
//!     // draw `text` — and nothing at all when hints are off
//! }
//! ```
//!
//! ## Registering
//!
//! **Every chord this app binds belongs in [`REGISTRY`](crate::REGISTRY)**, and
//! anything new — written by a person or by an agent — goes in the table rather
//! than into a string at a call site. The collision test only sees what is in
//! the table, so an unregistered binding is one nothing checks, and a
//! hand-written label is one nothing keeps in step with the platform.
//!
//! [`Hinted::Literal`] is for a chord the *platform* owns. A handful of app
//! bindings still use it because registering them needs table changes rather
//! than table rows — they are named in `docs/contributing/techdebt/README.md`.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::registry::{self, ShortcutId};

/// Where a hint is about to be drawn.
///
/// Sections rather than one switch, because they are turned off for different
/// reasons: a menu that stops teaching chords is a preference, and a tooltip
/// that stops is usually a screenshot.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Section {
    /// A menu row's trailing chord column.
    Menu,
    /// The second line of a hover tooltip.
    Tooltip,
    /// A section heading that teaches the chord opening the panel it heads.
    Heading,
    /// Ambient teaching text that is not attached to a control — the welcome
    /// screen's *Getting around* list, the status bar's standing line. The kind
    /// somebody turns off first, because it is the kind you stop reading.
    Guide,
    /// The complete shortcut reference, including feature-gated bindings.
    Reference,
}

impl Section {
    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A chord about to be **printed**.
///
/// `Copy`, so a menu `Row` stays `Copy` and the menus stay `const`-built.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hinted {
    /// A binding this app owns. Rendered from the registry at draw time, so the
    /// chord a surface teaches and the chord that fires are one value.
    Id(ShortcutId),
    /// A chord with no registry row, printed as written.
    ///
    /// **Two kinds of thing, and only one of them is permanent.** A chord the
    /// *platform* owns — `Esc`, `Win+Down`, an arrow — is not ours to rebind and
    /// will never have a row. A chord this app binds but has not registered yet
    /// is a debt, not a category: the collision test cannot see it and it is
    /// hand-written per platform, which is the bug `Id` exists to stop. The ones
    /// that are left are named in `docs/contributing/techdebt/README.md`; anything **new** goes in
    /// the table.
    Literal(&'static str),
}

impl From<ShortcutId> for Hinted {
    fn from(id: ShortcutId) -> Self {
        Self::Id(id)
    }
}

impl Hinted {
    /// A chord the platform owns, if *this* platform has it.
    ///
    /// `None` off Windows for the `Win+…` family: those are Windows' own
    /// window-management chords, and a row advertising one on macOS teaches a
    /// key combination that does nothing.
    pub fn windows_only(text: &'static str) -> Option<Self> {
        cfg!(windows).then_some(Self::Literal(text))
    }

    /// How it is written on this platform, **whether or not it would be shown**.
    ///
    /// [`show_hint`] is what drawing calls. This is for a test asserting what a
    /// row would say, and for a caller that has already asked.
    pub fn text(self) -> String {
        match self {
            Self::Id(id) => registry::label_of(id),
            Self::Literal(s) => s.to_string(),
        }
    }
}

/// Every section on. A bitmask rather than a `bool` per section so the switch is
/// one atomic, the way the theme's is.
static SHOWN: AtomicU32 = AtomicU32::new(u32::MAX);

/// **Whether to print this chord here, and how it is written on this platform.**
///
/// `None` means draw nothing — not an empty string, which most callers would
/// lay out and leave a gap for.
///
/// Takes anything that becomes a [`Hinted`], so the common case reads
/// `show_hint(ShortcutId::NewTab, Section::Tooltip)`.
pub fn show_hint(chord: impl Into<Hinted>, section: Section) -> Option<String> {
    let chord = chord.into();
    // A chord whose surface this build hides is not taught anywhere
    // (`ShortcutId::shown_in_product`).
    if let Hinted::Id(id) = chord
        && section != Section::Reference
        && !id.shown_in_product()
    {
        return None;
    }
    shown(section).then(|| chord.text())
}

/// Whether `section` currently shows its hints.
pub fn shown(section: Section) -> bool {
    SHOWN.load(Ordering::Relaxed) & section.bit() != 0
}

/// Turn one section's hints on or off, for this process.
///
/// A process global, like the theme and the font roles — and with the same
/// caveat: a test that flips it races every other test in the binary unless they
/// share a mutex.
pub fn set_shown(section: Section, on: bool) {
    let bit = section.bit();
    let mut current = SHOWN.load(Ordering::Relaxed);
    loop {
        let next = if on { current | bit } else { current & !bit };
        match SHOWN.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(seen) => current = seen,
        }
    }
}

#[cfg(test)]
mod tests;
