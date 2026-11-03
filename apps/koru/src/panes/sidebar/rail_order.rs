// SPDX-License-Identifier: MPL-2.0

//! The order the views sit in down the rail, which the person chooses by
//! dragging them (or `Ctrl+Shift+↑`/`↓` on a focused one).
//!
//! **Every view this build knows, once each** — the ones it hides included.
//! A view hidden here (Home, for now, or one behind a feature switch) keeps
//! its place among the others, so the order reads the same on a build that
//! shows it. What the rail draws is [`RailOrder::visible`], and a move only
//! ever permutes the shown views among the slots they already hold
//! ([`RailOrder::set_visible`]).
//!
//! ## Where it is kept, and what an older build makes of it
//!
//! **Per device**, in the window's session file beside `tabs=` and
//! `chat_seen=` — the `rail_order=` line (`Settings::rail_order`), a comma
//! list of [`NavigatorMode::token`]s. Not a synced setting: how one machine's
//! rail is arranged is that machine's furniture. A rail in the default order
//! writes no line at all.
//!
//! Read tolerantly, in both directions. A token this build does not know (a
//! view from a newer build) is dropped, the known ones keep their saved order,
//! and a view the list does not name (one newer than the list) is appended in
//! its default place in [`NavigatorMode::ALL`]. A build older than this one
//! ignores the line entirely — its `from_text` passes over keys it does not
//! know — draws its default order, and drops the line on its next save.
//!
//! **Shortcuts stay with their view.** `Alt+4` is Chats wherever Chats sits;
//! the order is where a view is drawn and which way the arrows walk, nothing
//! else.

use crate::panes::explore::NavigatorMode;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RailOrder(Vec<NavigatorMode>);

impl Default for RailOrder {
    fn default() -> Self {
        Self(NavigatorMode::ALL.to_vec())
    }
}

impl RailOrder {
    /// The saved line, merged with what this build knows. Never fails: an
    /// empty or unreadable line is the default order.
    pub fn parse(line: &str) -> Self {
        let mut order: Vec<NavigatorMode> = Vec::with_capacity(NavigatorMode::ALL.len());
        for mode in line
            .split(',')
            .filter_map(|t| NavigatorMode::parse(t.trim()))
        {
            if !order.contains(&mode) {
                order.push(mode);
            }
        }
        for mode in NavigatorMode::ALL {
            if !order.contains(&mode) {
                order.push(mode);
            }
        }
        Self(order)
    }

    /// The line to save, or empty for the default order — which writes none.
    pub fn to_storage(&self) -> String {
        if *self == Self::default() {
            return String::new();
        }
        self.0
            .iter()
            .map(|mode| mode.token())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The views this build shows, in the person's order. What the rail's
    /// squares, its hit test, the arrows and the accessibility tree are built
    /// from.
    pub fn visible(&self) -> Vec<NavigatorMode> {
        self.0
            .iter()
            .copied()
            .filter(|mode| mode.shown_in_product())
            .collect()
    }

    /// Put the shown views in `visible`'s order. Each takes the next slot a
    /// shown view held, so the hidden ones stay exactly where they were. A
    /// list that is not a permutation of [`visible`](Self::visible) is
    /// refused — `false` — rather than allowed to lose or duplicate a view.
    pub fn set_visible(&mut self, visible: &[NavigatorMode]) -> bool {
        let mut now = self.visible();
        let mut wanted = visible.to_vec();
        now.sort_by_key(|m| m.token());
        wanted.sort_by_key(|m| m.token());
        if now != wanted {
            return false;
        }
        let mut next = visible.iter().copied();
        for slot in self.0.iter_mut() {
            if slot.shown_in_product() {
                if let Some(mode) = next.next() {
                    *slot = mode;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
