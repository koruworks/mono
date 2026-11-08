// SPDX-License-Identifier: MPL-2.0

//! **What a Koru window offers a screen reader, and which control a request is
//! about.**
//!
//! One sentence, and what it excludes: *this module owns Koru's semantic
//! identity, its projection into an AccessKit tree, and the routing of a
//! request back to the behaviour that already owns it.* It excludes the
//! platform — when an adapter exists, what a `WM_GETOBJECT` is, which UIA
//! property a locator lands on — because that is a window trap and lives in
//! [`koru_window::accessibility`]. It excludes what a control *is*: a tab, a
//! document row, a paragraph and a caret belong to the views that draw them,
//! which contribute through [`View::semantics`](crate::View::semantics) and
//! keep their own knowledge.
//!
//! Between those two exclusions there is exactly one subject left, and it is
//! the one the roadmap named: identity, updates, and routing between a view
//! and its platform adapter. It is not a place for anything else — a helper
//! that is not about identity, an update or a route does not belong here, and
//! `app.rs` gets calls into this module rather than another subsystem grown
//! inside it.
//!
//! ## The three pieces
//!
//! - [`identity`] — an [`Occurrence`] is *window, view, object, control part*,
//!   and its [`locator`](Occurrence::locator) is the stable string an external
//!   UIA client reads out of `AutomationId`.
//! - [`projection`] — a prepared in-memory [`WindowSemantics`] snapshot turned
//!   into one `accesskit::TreeUpdate`, with the map that says what each
//!   published id meant.
//! - [`lifecycle`] — one window's state from before it is shown until after it
//!   is gone: when a tree is worth building, and which requests are still
//!   about something that exists.
//! - [`dump`] — the one-shot description of what was actually published, so
//!   somebody can look. Structure by default and never a word a person wrote.
//!
//! ## The window's own controls
//!
//! [`window_commands`] is the only projection this module produces itself, and
//! it is here rather than in a view for one reason: minimise, maximise and
//! close are operations on the *window*, and the host is what performs them.
//! A view draws the buttons, so their rectangles come *from* the view, through
//! [`CaptionGeometry`] — the host never reconstructs the caption's layout in a
//! second place. ADR-336 stopped at the identity and left the geometry
//! unpublished; ADR-338 is where the view fills it in.

pub mod dump;
pub mod identity;
pub mod lifecycle;
pub mod projection;

pub use dump::Redaction;
pub use identity::{ControlPart, ObjectKey, Occurrence, Slots, ViewSlot, WindowSlot, part};
pub use lifecycle::{Counters, PaneAccessibility, Poll, Resolved, Stage};
pub use projection::Projection;
pub use projection::{Runs, Scope, SemanticNode, TextBody, WindowSemantics};

/// How loudly a live node speaks when its label changes, re-exported so a view
/// publishing an announcement names one type rather than reaching past this
/// module into `accesskit` for the sake of a single argument. See
/// [`SemanticNode::live`].
pub use accesskit::Live;

/// A rectangle in a window's **logical** coordinates, re-exported so a view
/// building semantics names one type rather than reaching into `koru-draw`
/// for the sake of a signature.
pub use koru_draw::geometry::Rect as LogicalRect;

use accesskit::{Action, Role};
use koru_i18n::{Key, Lang, t};

/// **Where the view draws the window's own commands**, in this window's
/// logical coordinates.
///
/// The split it encodes: minimise, maximise and close are operations on the
/// *window*, so their identity and their routing are the host's — but a Koru
/// window has no OS caption, so their rectangles are wherever the view chose
/// to paint them. The host asks the view rather than reconstructing the
/// caption's layout in a second place, which is the thing ADR-336 stopped
/// short of and ADR-338 finishes.
///
/// `None` for a window whose view draws no caption; AccessKit answers that as
/// an empty rectangle, which is legal.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CaptionGeometry {
    pub minimize: Option<LogicalRect>,
    pub maximize: Option<LogicalRect>,
    pub close: Option<LogicalRect>,
}

/// The window's own commands, as a screen reader sees them.
///
/// Labels come from `koru-i18n` in all eight languages; identity comes from
/// [`part`] and is byte-identical whichever language is drawn — which is the
/// property `identity_is_the_same_in_every_language` pins.
///
/// Only [`Action::Click`] is advertised. A caption button has no focus ring to
/// light: Koru's visual focus lives inside the view, and advertising `Focus`
/// here would promise to move a state this control does not own. Windows sends
/// a focus request before every invoke regardless, and the host ignores it —
/// see `guide/contributing/deps/ACCESSKIT.md`.
pub fn window_commands(
    window: WindowSlot,
    maximized: bool,
    lang: Lang,
    geometry: CaptionGeometry,
) -> Vec<SemanticNode> {
    let command = |part: ControlPart, label: &'static str, bounds: Option<LogicalRect>| {
        let node = SemanticNode::new(Occurrence::window_command(window, part), Role::Button)
            .with_label(label)
            .with_action(Action::Click);
        match bounds {
            Some(bounds) => node.with_bounds(bounds),
            None => node,
        }
    };
    vec![
        command(
            part::MINIMIZE,
            t(lang, Key::WindowMinimize),
            geometry.minimize,
        ),
        command(
            part::MAXIMIZE,
            if maximized {
                t(lang, Key::WindowRestore)
            } else {
                t(lang, Key::WindowMaximize)
            },
            geometry.maximize,
        ),
        command(part::CLOSE, t(lang, Key::WindowCloseWindow), geometry.close),
    ]
}

/// **A number that changes when the state behind a tree changes**, built by
/// feeding it that state.
///
/// The counterpart of [`PaneAccessibility::observe`]: a view answers
/// `View::semantics_revision` by folding in every input its projection reads,
/// and *not* folding in anything that moves per frame — a scroll offset, an
/// animation's progress, a caret's phase. That asymmetry is the whole point.
/// A counter somebody has to remember to bump is a counter that is one day
/// forgotten in the one path that mattered; a fingerprint over the inputs
/// cannot be.
///
/// FNV-1a, written out rather than taken from `DefaultHasher` for
/// [`Occurrence::node_id`]'s reason: that one is explicitly allowed to change
/// between Rust releases, and a revision that moved under a running client
/// would rebuild the tree for nothing on the first frame after an upgrade.
#[derive(Copy, Clone, Debug)]
pub struct Revision(u64);

impl Default for Revision {
    fn default() -> Self {
        Self::new()
    }
}

impl Revision {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub const fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub fn byte(&mut self, byte: u8) -> &mut Self {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(Self::PRIME);
        self
    }

    pub fn u64(&mut self, value: u64) -> &mut Self {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
        self
    }

    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.byte(u8::from(value))
    }

    /// A float by its bits, so `-0.0` and `0.0` are two revisions rather than
    /// a comparison nobody wrote correctly.
    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.u64(u64::from(value.to_bits()))
    }

    /// A string, followed by a separator — without one, `["ab", "c"]` and
    /// `["a", "bc"]` would be the same revision.
    pub fn str(&mut self, value: &str) -> &mut Self {
        for byte in value.as_bytes() {
            self.byte(*byte);
        }
        self.byte(0)
    }

    pub fn uuid(&mut self, value: uuid::Uuid) -> &mut Self {
        for byte in value.as_bytes() {
            self.byte(*byte);
        }
        self
    }

    pub fn opt_uuid(&mut self, value: Option<uuid::Uuid>) -> &mut Self {
        match value {
            Some(id) => {
                self.byte(1);
                self.uuid(id)
            }
            None => self.byte(0),
        }
    }

    pub fn finish(self) -> u64 {
        self.0
    }
}

/// **Which language a window speaks**, asked of the OS once.
///
/// The same answer `voice_install` needs and for the same reason: a locale
/// cannot change under a running process, and asking on every semantic
/// publish would be a round trip to the OS on a path that must not make one.
/// Public because a view builds its own accessible names and must speak the
/// same language the window commands do — one answer, asked once, shared.
pub fn ui_lang() -> Lang {
    static LANG: std::sync::OnceLock<Lang> = std::sync::OnceLock::new();
    *LANG.get_or_init(|| Lang::for_system(koru_window::system_locale::system_locale().as_deref()))
}

#[cfg(test)]
mod tests;
