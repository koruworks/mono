// SPDX-License-Identifier: MPL-2.0

//! Back and forward between the panel's tabs, as a browser walks its pages:
//! the mouse's thumb buttons, or Alt+Left and Alt+Right.
use super::*;

/// The most steps kept each way; older ones are let go.
const DEPTH: usize = 64;

/// One place the panel has been: a tab, and what was selected on it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct View {
    hierarchy: bool,
    selected: Option<usize>,
    /// The capture `selected` indexes. A record index means nothing in
    /// another frame, so a step into a newer capture keeps today's selection.
    revision: u64,
}

#[derive(Default)]
pub(super) struct History {
    back: Vec<View>,
    forward: Vec<View>,
}

impl History {
    /// A new step from `from`: what was ahead of it is gone, as in a browser.
    fn record(&mut self, from: View) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
            if self.back.len() > DEPTH {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// The view `back` (or forward) of `current`, leaving `current` on the
    /// other side so the opposite step returns to it.
    fn step(&mut self, current: View, back: bool) -> Option<View> {
        let (from, to) = if back {
            (&mut self.back, &mut self.forward)
        } else {
            (&mut self.forward, &mut self.back)
        };
        let next = from.pop()?;
        to.push(current);
        Some(next)
    }
}

impl Inspector {
    fn view(&self) -> View {
        View {
            hierarchy: self.show_hierarchy,
            selected: self.selected,
            revision: self.snapshot.revision,
        }
    }

    /// Every tab change made by the person goes through here, so it can be
    /// walked back.
    pub(super) fn show_tab(&mut self, hierarchy: bool) {
        if self.show_hierarchy != hierarchy {
            let from = self.view();
            self.history.record(from);
            self.show_hierarchy = hierarchy;
        }
    }

    /// Select `index` as a step of its own, so Back returns to what was
    /// selected before: an ancestor chosen from Details.
    pub(super) fn go_to(&mut self, index: usize) {
        if self.selected != Some(index) {
            let from = self.view();
            self.history.record(from);
            self.pause_refresh();
            self.select(Some(index));
        }
    }

    /// One step back or forward; false when there is nowhere to go.
    pub(super) fn go(&mut self, back: bool) -> bool {
        let current = self.view();
        let Some(next) = self.history.step(current, back) else {
            return false;
        };
        if next.revision == self.snapshot.revision
            && next
                .selected
                .is_none_or(|index| index < self.snapshot.records.len())
        {
            // Keep the restored selection from being replaced by the pointer.
            self.pause_refresh();
            self.select(next.selected);
        }
        // As a click on the tab does, but without recording a new step.
        let was_hierarchy = self.show_hierarchy;
        self.show_hierarchy = next.hierarchy;
        self.tab_shown(was_hierarchy);
        self.dirty_semantics = true;
        true
    }
}
