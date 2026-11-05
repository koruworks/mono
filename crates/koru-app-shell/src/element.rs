// SPDX-License-Identifier: MPL-2.0

//! What a tab has to be able to answer for itself.
//!
//! [`TabStrip`](crate::TabStrip) is generic over its element, and the four
//! questions it asks are the four places the container it came from branched on
//! a `TabView` or a `document` field. They are here rather than in the strip
//! because every one of them is a **host** rule: what may be pinned, what a
//! placeholder is, what may be overwritten in place, and which scope a pin
//! belongs to are decisions about a product, and this crate may not make one.
//!
//! **Scopes are sixteen bytes.** A pin belongs to a workspace in Koru and could
//! belong to a project in the Terminal; either way the shell compares uuids and
//! learns nothing from them, the `koru-link` precedent ADR-262 names. There is
//! no `WorkspaceId` here and no type parameter for one.

use uuid::Uuid;

/// One tab, as the strip sees it.
///
/// Every method has a default, and the defaults describe the simplest possible
/// tab: not pinnable, not a placeholder, not reusable, belonging to no scope. A
/// host that only wants an ordered list of tabs implements the trait with an
/// empty body and gets exactly that.
pub trait TabElement {
    /// The scope this tab is **pinned** into, or `None` when it is not pinned.
    ///
    /// Pin membership is this and nothing else, so a strip never needs a second
    /// flag that could disagree with it.
    fn pinned_scope(&self) -> Option<Uuid> {
        None
    }

    /// Stamp or clear the pin. Called by [`TabStrip::pin`](crate::TabStrip::pin)
    /// and [`unpin`](crate::TabStrip::unpin), never by anything else.
    fn set_pinned_scope(&mut self, _scope: Option<Uuid>) {}

    /// Where this tab's content lives, whether or not it is pinned. Pinning
    /// keeps the two in step; unpinning leaves the home behind.
    fn home_scope(&self) -> Option<Uuid> {
        None
    }

    fn set_home_scope(&mut self, _scope: Option<Uuid>) {}

    /// Whether this tab may be pinned at all.
    ///
    /// The desktop app pins every tab; the strip only needs the answer.
    fn pinnable(&self) -> bool {
        false
    }

    /// Whether the strip draws this pinned tab's title beside its icon.
    fn show_title(&self) -> bool {
        false
    }

    fn set_show_title(&mut self, _show: bool) {}

    /// A slot new content may overwrite rather than open beside.
    fn is_reusable(&self) -> bool {
        false
    }

    /// The floor: a tab holding nothing of its own, which real content replaces
    /// rather than joins.
    ///
    /// This is what [`TabStrip::push`](crate::TabStrip::push) consults before it
    /// leaves a lone placeholder sitting under a row of pins.
    fn is_placeholder(&self) -> bool {
        false
    }

    /// Convenience: pinned is "has a pin scope", in one place.
    fn is_pinned(&self) -> bool {
        self.pinned_scope().is_some()
    }
}
