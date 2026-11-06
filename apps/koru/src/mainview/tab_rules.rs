// SPDX-License-Identifier: MPL-2.0

//! **Koru's answers to the four questions the shell's tab strip asks**, and the
//! domain searches that used to live inside it.
//!
//! `TabSet`, `TabStrip`, `TabSlides` and `Panel` moved to `koru-app-shell`
//! (SP-9), generic over the tab element. Everything they lost on the way out is
//! here, beside [`Tab`] where it belongs: what may be pinned, what a placeholder
//! is, which workspace a pin sits in, and the two lookups — *the tab over this
//! node*, *the tab over this document* — that name a type the shell may not.
//!
//! ## Why an extension trait rather than free functions
//!
//! Every call site in the container reads `self.tabs().showing(node)`, and the
//! rule it states is about the strip. A free function would have turned three
//! hundred of them into `showing(&self.tabs, node)` for no gain; a trait keeps
//! the sentence and puts the domain exactly where the compiler can see it is
//! Koru's. The generic machinery carries the differently-named half —
//! `position`, `retain_main`, `retain_pins`, `reset_to_floor`,
//! `has_placeholder` — so nothing here shadows an inherent method.

use koru_app_shell::{TabElement, TabStrip};
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use uuid::Uuid;

use super::tab::{Tab, TabView};

impl TabElement for Tab {
    fn pinned_scope(&self) -> Option<Uuid> {
        self.pinned.map(|id| id.as_uuid())
    }

    fn set_pinned_scope(&mut self, scope: Option<Uuid>) {
        self.pinned = scope.map(WorkspaceId::from_uuid);
    }

    fn home_scope(&self) -> Option<Uuid> {
        self.home.map(|id| id.as_uuid())
    }

    fn set_home_scope(&mut self, scope: Option<Uuid>) {
        self.home = scope.map(WorkspaceId::from_uuid);
    }

    /// **Koru's pin rule, in one place: every tab may be pinned.** A pin is a
    /// tab kept at the left of the strip across workspace switches, and which
    /// tab is worth keeping there is the person's call — a task list, the
    /// Trash or a terminal as much as a document. It was documents and chats
    /// only, written out again in the tab menu's build and its enable check,
    /// which is why a task list could not be pinned; both now ask this.
    ///
    /// A pinned tab comes back after a restart when its kind has a saved form
    /// (`TabRef`). Settings, Feedback, a version comparison, Socials and a
    /// run's pages have none: they stay pinned for the session, and do not
    /// come back.
    fn pinnable(&self) -> bool {
        true
    }

    fn show_title(&self) -> bool {
        self.show_title
    }

    fn set_show_title(&mut self, show: bool) {
        self.show_title = show;
    }

    fn is_reusable(&self) -> bool {
        Tab::is_reusable(self)
    }

    /// The welcome tab, and only when it is genuinely empty — and never a
    /// pinned one.
    ///
    /// A welcome tab never names a document, so a document-bearing tab is
    /// never a placeholder whatever view it currently holds. The `draft` check
    /// is the same rule from the other side: a draft is content somebody has
    /// started typing. **A pin is never a placeholder** — the strip drops a
    /// placeholder and lets opened content replace it, and a Welcome tab
    /// somebody pinned is one they asked to keep.
    fn is_placeholder(&self) -> bool {
        matches!(self.view, TabView::Welcome(_))
            && self.pinned.is_none()
            && self.draft.is_none()
            && self.document.is_none()
    }
}

/// The lookups and the lifecycle rules that name a Koru type.
pub trait KoruTabs {
    /// The active tab's view.
    ///
    /// A `TabView` is a welcome screen, a trash listing, a workspace manager or
    /// a `Leaf` — four things the shell may not name, which is why this is here
    /// and not on the strip.
    fn active_view(&mut self) -> &mut TabView;

    /// The tab already rooted at `node`, if there is one. What stops a second
    /// view of one document appearing.
    fn showing(&self, node: NodeId) -> Option<usize>;

    /// The tab over this **document**, if one is open.
    ///
    /// **Prefer this to [`showing`](Self::showing) whenever a document is what
    /// is being opened.** That one compares a `NodeId` inside a tab's document,
    /// and a tab whose body was rebuilt against a *different* document reports
    /// that one's root — so it could answer "yes, that tab" about a document it
    /// has never shown, which is how clicking one sometimes reused a tab instead
    /// of opening it.
    fn showing_document(&self, document: DocumentId) -> Option<usize>;

    /// Close every Main tab whose node has gone — deleted here, or by another
    /// peer.
    ///
    /// **Only tabs over the document `ws` is.** A `NodeId` is meaningful only
    /// inside the document that minted it ([ADR-90]), so asking the active
    /// document about a tab over a *different* one answers "gone" for every
    /// single row — and this closed them all. Opening a second document reset
    /// the window to one tab, and the strip that reached disk had one tab in it,
    /// so the next launch reopened one. Two reports, one line.
    ///
    /// `active` is `None` for a workspace with no manifest — every workspace in
    /// the editor's own suite — and so is `Tab::document` there, which is what
    /// makes the comparison the old unconditional behaviour in that world.
    ///
    /// [ADR-90]: ../../../../docs/decisions/ADR-076-100.md
    fn retain_live(&mut self, ws: &WorkspaceDoc, active: Option<DocumentId>) -> bool;

    /// Drop every pinned tab whose document is no longer in the library.
    fn retain_pinned(&mut self, live: impl Fn(DocumentId) -> bool) -> bool;

    /// Replace Main with one fresh welcome tab, keeping the pins.
    fn reset_to_welcome(&mut self);

    /// Whether the strip holds an empty welcome tab.
    fn has_welcome(&self) -> bool;

    /// With pins holding the strip up, a lone welcome is only a floor.
    fn drop_lone_welcome_if_pinned(&mut self);
}

impl KoruTabs for TabStrip<Tab> {
    fn active_view(&mut self) -> &mut TabView {
        &mut self.active_mut().view
    }

    fn showing(&self, node: NodeId) -> Option<usize> {
        self.position(|t| t.view.root() == Some(node))
    }

    fn showing_document(&self, document: DocumentId) -> Option<usize> {
        // `edits`, not `document`: the Tasks tab whose page document was just
        // activated is showing it, and a second tab over it would be two.
        self.position(|t| t.edits() == Some(document) && t.draft.is_none())
    }

    fn retain_live(&mut self, ws: &WorkspaceDoc, active: Option<DocumentId>) -> bool {
        self.retain_main(|t| {
            // **A draft is never dead.** Its root is a node in its own private
            // document, which this one has never heard of — so asking `ws` about
            // it answers "gone" and would close the tab the instant it opened.
            !(t.draft.is_none()
                && t.document == active
                && t.view.root().is_some_and(|r| ws.read(r).is_err()))
        })
    }

    fn retain_pinned(&mut self, live: impl Fn(DocumentId) -> bool) -> bool {
        self.retain_pins(|t| t.document.map_or(true, |id| live(id)))
    }

    fn reset_to_welcome(&mut self) {
        self.reset_to_floor();
    }

    fn has_welcome(&self) -> bool {
        self.has_placeholder()
    }

    fn drop_lone_welcome_if_pinned(&mut self) {
        self.drop_lone_placeholder_if_pinned();
    }
}

#[cfg(test)]
mod tests;
