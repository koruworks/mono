// SPDX-License-Identifier: MPL-2.0

//! **The Canvases section** — Explore's seventh mode (CANVAS P1.3).
//!
//! Every canvas in the workspace, as **one flat list**: the manifest's
//! [`KIND_CANVAS`] entries in manifest order, each a row at depth zero with no
//! caret. A canvas holds no pages, so there is nothing to nest under one; and a
//! canvas somebody nested under a page elsewhere is still listed here, flat,
//! because this section is the one place a person looks for canvases.
//!
//! A row is a `Kind::Document` row in every way the pane's machinery asks: a
//! click opens it (the container builds a canvas tab from its kind), F2 and the
//! row menu rename it, a drag carries it to a pane or to the Trash. What it is
//! **never** is reordered or nested — the drop a drag inside the list would
//! make is refused, so the carried row eases back to where it was. The order is
//! the manifest's, and nobody arranges it.
//!
//! A child module, like `rail` and `actions`, so the section's rules are one
//! file rather than a few more arms in the pane's 7,000 lines.

use koru_core::explore::KIND_CANVAS;
use koru_core::{Kind, NodeId};

use koru_shell_chrome::title_icon::split_leading_emoji;

use super::{ExploreView, NavigatorMode, Row};
use crate::panes::Outcome;

#[cfg(test)]
mod tests;

impl ExploreView {
    /// Show the Canvases section.
    pub fn show_canvases(&mut self) {
        self.set_navigator_mode(NavigatorMode::Canvases);
    }

    /// Whether the Canvases section is what the column is showing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn showing_canvases(&self) -> bool {
        self.mode == NavigatorMode::Canvases
    }

    /// Build the section's rows from the manifest cache: every canvas, flat,
    /// in manifest order.
    ///
    /// **The fold set is not touched.** It belongs to the document tree —
    /// pruning it here against a list of canvases would forget every page's
    /// fold on the next trip to Documents, the Files lesson
    /// `rebuild_manifest_rows` records.
    pub(super) fn rebuild_canvas_rows(&mut self) {
        let span = koru_core::timing::Span::start("explore rebuild (canvases)");
        self.rows.clear();
        self.slides.clear();
        let keep = self.focus.take();
        let untitled = koru_i18n::t(crate::format::lang(), koru_i18n::Key::DocUntitled);
        for entry in self
            .manifest_entries
            .iter()
            .filter(|entry| entry.kind == KIND_CANVAS)
        {
            let node = NodeId::from_uuid(entry.id.as_uuid());
            let raw = if entry.title.is_empty() {
                untitled.to_owned()
            } else {
                entry.title.clone()
            };
            // A leading emoji is the row's icon, as a page's is, and is not
            // drawn twice.
            let (leading, rest) = split_leading_emoji(&raw);
            let icon = leading.map(str::to_owned);
            let label = if leading.is_some() {
                rest.to_owned()
            } else {
                raw
            };
            self.rows.push(Row {
                node,
                depth: 0,
                label,
                // A document to everything that asks — the click that opens,
                // the drag that carries — and drawn with the canvas glyph by
                // the mode, not by a kind of its own.
                kind: Kind::Document,
                mime: None,
                icon,
                has_children: false,
                expanded: true,
                file_state: self.file_state_of(node),
                note: None,
            });
        }
        self.focus = keep.filter(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection
            .retain(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection_anchor = self
            .selection_anchor
            .filter(|id| self.rows.iter().any(|r| r.node == *id));
        let n = self.rows.len();
        span.stop_with(|| format!("{n} rows"));
    }

    /// **A drag inside the list lands nowhere.** The carried row follows the
    /// pointer — out of the column onto a pane, or down onto the Trash row,
    /// both of which are answered before the list is asked — but no slot in
    /// the list opens for it, and releasing over the list moves nothing.
    pub(super) fn hold_canvas_drop(&mut self) {
        if let Some(d) = self.drag.as_mut() {
            d.last = None;
            d.spring.clear();
        }
    }
}

/// The empty section: what would put something in it, and the button that
/// does.
pub(super) fn empty_state() -> (&'static str, &'static str, Outcome) {
    let lang = crate::format::lang();
    (
        koru_i18n::t(lang, koru_i18n::Key::CanvasesEmpty),
        koru_i18n::t(lang, koru_i18n::Key::CanvasNew),
        Outcome::NewCanvas,
    )
}
