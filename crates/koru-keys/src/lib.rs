// SPDX-License-Identifier: MPL-2.0

//! Chords, modifiers and the labels they are written as.
//!
//! ## Why this is a crate and not a module
//!
//! Four separate places in this tree resolve a key press — the shell
//! (`koru-workspace-host`'s `app.rs`), the container (`apps/koru`'s `mainview::keys`),
//! the document view (`apps/koru-document`), and the editor
//! (`koru-editor`'s `input::keymap`) — and a fifth, the menu bar, is coming.
//! They need one answer to "what is the primary modifier here" and one answer to
//! "how is this chord written". They cannot get it from each other:
//! **`koru-workspace-host` must not depend on `koru-editor`**, which is load-bearing
//! and enforced by the manifests.
//!
//! `koru-core` is the common ancestor but has no `winit` dependency and should
//! not gain one — its description is "No UI, no rendering". So this sits below
//! all of them, the same way `koru-blocks` and `koru-ui` were carved out
//! ([ADR-45](../../../docs/decisions/README.md),
//! [ADR-73](../../../docs/decisions/ADR-051-075.md#adr-73--koru-ui-holds-composed-widgets-koru-draw-holds-primitives)).
//!
//! ## The registry, and what is owed
//!
//! [`REGISTRY`] is the table: **every chord this app binds belongs in it**, and
//! anything new — written by a person or by an agent — is registered there
//! rather than spelled out at a call site. It is what makes
//! `no_shortcut_shadows_a_lower_layer` able to see a collision at all, and what
//! `reference/SHORTCUTS.md` is generated from.
//!
//! It is not yet what every resolver resolves *from*, which was the original
//! objection to having a table: four places still match chords by hand and are
//! checked against the table by a test per layer rather than driven by it
//! (`every_container_shortcut_in_the_registry_resolves_here` is the pattern).
//! Driving all four *from* the table is the rest, and it is the standing
//! reason this module doc says "not yet".
//!
//! Printing a chord is a separate job with its own seam — see [`hint`].

mod chord;
mod format;
pub mod hint;
mod modifiers;
pub mod registry;

pub use chord::{Chord, On, letter, named};
pub use format::{label, label_for};
pub use hint::{Hinted, Section, show_hint};
pub use modifiers::{
    NONE, PRIMARY, PRIMARY_ALT, PRIMARY_SHIFT, SHIFT, VIEW, has_foreign_primary, has_primary,
    is_chord, primary_only, without_primary,
};
pub use registry::{Layer, RAIL_SLOTS, REGISTRY, Shortcut, ShortcutId, label_of, rail_slot};
