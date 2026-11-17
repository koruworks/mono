// SPDX-License-Identifier: MPL-2.0

//! The open tabs, and which of them is showing.
//!
//! ## Why this is a type and not two fields
//!
//! `active` is an index into `tabs`, so every operation that removes or reorders
//! a tab has to keep it valid. That was five separate clamps, written five
//! times — in `close`, `move_tab`, `restore`, `drop_dead_tabs` and `select` —
//! and each one had to remember the same two rules: the index stays in range,
//! and the list is never empty. Here the fields are private, so the only code
//! that can break the invariant is the code that states it.
//!
//! ## The list is never empty, and the floor mints through a host factory
//!
//! There used to be a **permanent** tab — the outline — that `close` refused,
//! because closing it would have left the `outline/` realm with no way back.
//! What replaced it is weaker and simpler: closing the last tab leaves a fresh
//! tab rather than nothing. Every caller still gets "there is always an active
//! tab", and no tab is special.
//!
//! **What that fresh tab *is* is the host's**, so it arrives as a factory
//! closure rather than a constant ([ADR-262](../../../docs/decisions/README.md)):
//! a stored prototype would be payload the shell persists, which the minimal
//! contract forbids. A set built with [`TabSet::empty`] has no factory and is
//! allowed to be empty — that is the pinned group, which legitimately has no
//! tabs at all.
//!
//! ## What it deliberately does not own
//!
//! **Geometry.** `select` moves the index; it does not lay the tab out, because
//! what size a tab should be is a question about the window, which this knows
//! nothing about. The container calls its own resize afterwards. Keeping that
//! out is what lets every method here be tested without a viewport.
//!
//! **Meaning.** It is generic over its element and never asks one a question:
//! finding "the tab over this document" is [`position`](TabSet::position) with a
//! predicate the host wrote, because a document is a thing this crate may not
//! name.

/// How a set that must never be empty gets its replacement tab.
///
/// A boxed closure rather than a stored value, so the floor is minted when it is
/// needed and the set holds no tab-shaped payload of its own.
type Floor<T> = Box<dyn Fn() -> T>;

pub struct TabSet<T> {
    tabs: Vec<T>,
    /// Valid when `tabs` is not empty. Closing the last tab of a floored set
    /// mints a replacement; a pin group may be empty.
    active: usize,
    /// When each tab was last shown, on this set's own logical clock. Parallel
    /// to `tabs` — every add, remove and reorder below keeps them in step,
    /// which only works because the fields are private. Closing the active tab
    /// walks this trail back to the most recently shown survivor instead of
    /// falling on whichever neighbour the indices left.
    visits: Vec<u64>,
    clock: u64,
    /// `Some` when closing the last tab mints a replacement rather than going
    /// empty. The host's factory, not a prototype.
    floor: Option<Floor<T>>,
}

impl<T> TabSet<T> {
    /// A set holding one tab, which is what a window opens on.
    ///
    /// `floor` is what the set mints when its last tab closes. It is called at
    /// most once per operation and never stored as a value.
    pub fn new(first: T, floor: impl Fn() -> T + 'static) -> Self {
        Self {
            tabs: vec![first],
            active: 0,
            visits: vec![1],
            clock: 1,
            floor: Some(Box::new(floor)),
        }
    }

    /// An empty group — pinned tabs, which may have none.
    pub fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            visits: Vec::new(),
            clock: 0,
            floor: None,
        }
    }

    /// Record that the active tab is showing now. Every path that changes
    /// which tab shows ends here, so the active tab always carries the
    /// newest stamp and a zero means "never shown" — a restored session.
    fn stamp(&mut self) {
        self.clock += 1;
        self.visits[self.active] = self.clock;
    }

    /// The tabs by when each last showed, most recent first: the tab in
    /// front, then the one before it, and so on. Tabs never shown (a restored
    /// session) follow in strip order.
    pub fn by_recency(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.tabs.len()).collect();
        order.sort_by(|a, b| self.visits[*b].cmp(&self.visits[*a]).then(a.cmp(b)));
        if let Some(at) = order.iter().position(|i| *i == self.active) {
            let active = order.remove(at);
            order.insert(0, active);
        }
        order
    }

    /// Mint the floor, if this set has one.
    fn mint(&self) -> Option<T> {
        self.floor.as_ref().map(|floor| floor())
    }

    #[cfg(test)]
    pub(crate) fn visits_len(&self) -> usize {
        self.visits.len()
    }

    // -------------------------------------------------------------- reading

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_tab(&self) -> &T {
        &self.tabs[self.active]
    }

    pub fn active_mut(&mut self) -> &mut T {
        &mut self.tabs[self.active]
    }

    pub fn force_select(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        self.tabs.get_mut(i)
    }

    /// The tab at `i`, if it is in range. Reading-only counterpart of
    /// [`get_mut`](Self::get_mut); the drag and pop-out paths ask about a tab
    /// without changing it.
    pub fn get(&self, i: usize) -> Option<&T> {
        self.tabs.get(i)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.tabs.iter_mut()
    }

    /// The first tab the host's predicate accepts.
    ///
    /// **This is where `showing(node)` and `showing_document(id)` went.** Both
    /// were the same search over a domain field, and a crate that may not name a
    /// `NodeId` cannot hold either — so the search is here and the question is
    /// the host's. See `apps/koru/src/mainview/tab_rules.rs` for Koru's two.
    pub fn position(&self, keep: impl Fn(&T) -> bool) -> Option<usize> {
        self.tabs.iter().position(keep)
    }

    // -------------------------------------------------------------- writing

    /// Show tab `i`. `false` if there is no such tab, or it is already showing.
    pub fn select(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() || i == self.active {
            return false;
        }
        self.active = i;
        self.stamp();
        true
    }

    /// Move `delta` tabs from the active one, wrapping.
    pub fn cycle(&mut self, delta: i32) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let n = self.tabs.len() as i32;
        let next = (self.active as i32 + delta).rem_euclid(n) as usize;
        self.select(next)
    }

    /// Append a tab and show it. Returns its index.
    pub fn push(&mut self, tab: T) -> usize {
        self.tabs.push(tab);
        self.visits.push(0);
        self.active = self.tabs.len() - 1;
        self.stamp();
        self.active
    }

    /// Close tab `i`.
    ///
    /// Closing the **last** tab mints a fresh one through the host's factory
    /// rather than leaving an empty window: every caller here is written against
    /// "there is always an active tab", and a window with no tab at all has
    /// nothing to draw and nowhere to put a keystroke.
    pub fn close(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() {
            return false;
        }
        let was_active = self.active == i;
        self.tabs.remove(i);
        self.visits.remove(i);
        if self.tabs.is_empty() {
            if let Some(fresh) = self.mint() {
                self.tabs.push(fresh);
                self.visits.push(0);
                self.active = 0;
                self.stamp();
            } else {
                self.active = 0;
            }
            return true;
        }
        if was_active {
            // Closing what you are looking at goes *back along the trail* —
            // to the most recently shown survivor — not to whichever
            // neighbour the indices left. A tab never shown carries stamp 0
            // (a restored session), so with no trail the tie falls to the
            // old rule: the neighbour on the left.
            let mut target = i.saturating_sub(1).min(self.tabs.len() - 1);
            for (j, &v) in self.visits.iter().enumerate() {
                if v > self.visits[target] {
                    target = j;
                }
            }
            self.active = target;
            self.stamp();
        } else {
            // A closed tab below the active one shifts it down a slot; the
            // same tab stays showing.
            if self.active > i {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
        true
    }

    /// Close every tab except `i` — the context menu's "close other tabs".
    pub fn close_others(&mut self, i: usize) -> bool {
        if self.tabs.get(i).is_none() {
            return false;
        }
        let remove: Vec<usize> = (0..self.tabs.len()).filter(|&j| j != i).collect();
        self.drop_tabs(remove, i)
    }

    /// Close every tab to the left of `i` — "close tabs to the left".
    pub fn close_left(&mut self, i: usize) -> bool {
        if self.tabs.get(i).is_none() {
            return false;
        }
        self.drop_tabs((0..i).collect(), i)
    }

    /// Close every tab to the right of `i` — "close tabs to the right".
    pub fn close_right(&mut self, i: usize) -> bool {
        if self.tabs.get(i).is_none() {
            return false;
        }
        self.drop_tabs((i + 1..self.tabs.len()).collect(), i)
    }

    /// Close every tab the host's predicate rejects, back to front.
    ///
    /// **Where `retain_live` went.** The rule it enforced — a tab whose node
    /// another peer deleted must not survive — is a question about a document,
    /// so the predicate is the host's and only the walk is here. Back to front,
    /// so an earlier removal cannot shift a later index out from under the loop.
    pub fn retain(&mut self, keep: impl Fn(&T) -> bool) -> bool {
        let dead: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| !keep(tab))
            .map(|(i, _)| i)
            .collect();
        let mut closed = false;
        for i in dead.into_iter().rev() {
            closed |= self.close(i);
        }
        closed
    }

    /// Remove the tabs at `remove` (back-to-front) and resolve `active` by a
    /// single rule shared by all three bulk closes: the active tab stays active
    /// if it survives, otherwise the selection lands on `fallback` — the
    /// right-clicked tab `i`, which is never itself in the removal set.
    /// `fallback` shifts with everything below a removed index, the same
    /// adjustment `close` makes one tab at a time.
    ///
    /// All three keep `i`, so this can never empty the list.
    fn drop_tabs(&mut self, mut remove: Vec<usize>, fallback: usize) -> bool {
        remove.sort_unstable();
        remove.dedup();
        remove.retain(|&j| j < self.tabs.len() && j != fallback);
        if remove.is_empty() {
            return false;
        }
        let mut target = if remove.contains(&self.active) {
            fallback
        } else {
            self.active
        };
        for &j in remove.iter().rev() {
            self.tabs.remove(j);
            self.visits.remove(j);
        }
        // The target shifts down by one for every removed tab that was to its
        // left — counted against its *original* index, since the decremented
        // value would otherwise hide a second removed tab below it.
        let original = target;
        target -= remove.iter().filter(|&&j| j < original).count();
        if self.tabs.is_empty() {
            if let Some(fresh) = self.mint() {
                self.tabs.push(fresh);
                self.visits.push(0);
            }
            self.active = 0;
            return true;
        }
        self.active = target.min(self.tabs.len() - 1);
        self.stamp();
        true
    }

    /// Move a tab along the strip, carrying the selection with it — dragging
    /// the tab you are looking at must not switch you to a different one.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return false;
        }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        let v = self.visits.remove(from);
        self.visits.insert(to, v);
        if self.active == from {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
        }
        true
    }

    /// Lift tab `i` out without minting a floor. Used when moving a tab between groups.
    pub fn take(&mut self, i: usize) -> Option<T> {
        if i >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(i);
        self.visits.remove(i);
        if self.tabs.is_empty() {
            self.active = 0;
            return Some(tab);
        }
        if self.active >= i {
            self.active = self.active.saturating_sub(1);
        }
        self.active = self.active.min(self.tabs.len() - 1);
        Some(tab)
    }

    /// Insert `tab` at `i`, clamped to the end. Does not change which tab is showing
    /// except when this was empty.
    pub fn insert(&mut self, i: usize, tab: T) -> usize {
        let i = i.min(self.tabs.len());
        let was_empty = self.tabs.is_empty();
        self.tabs.insert(i, tab);
        // Arrives unshown; selecting it is what stamps it.
        self.visits.insert(i, 0);
        if was_empty {
            self.active = 0;
        } else if i <= self.active {
            self.active += 1;
        }
        i
    }

    /// Replace the whole list with one fresh floor tab.
    ///
    /// What a **workspace switch** leaves in Koru: every tab named a node in the
    /// document that has just closed, and a `NodeId` is only meaningful inside
    /// the document that minted it, so there is nothing in the new one for any
    /// of them to point at. A set with no floor is simply cleared.
    pub fn reset_to_floor(&mut self) {
        match self.mint() {
            Some(fresh) => {
                self.tabs = vec![fresh];
                self.visits = vec![0];
                self.active = 0;
                self.stamp();
            }
            None => self.clear(),
        }
    }

    pub fn clear(&mut self) {
        self.tabs.clear();
        self.visits.clear();
        self.active = 0;
    }

    /// Set the list wholesale — what restoring a session needs.
    ///
    /// **An empty `tabs` becomes one floor tab**, not an empty window: the
    /// file is hand-editable and came off disk, so it is not trusted to contain
    /// anything at all. `active` is clamped rather than asserted for the same
    /// reason.
    pub fn restore(&mut self, tabs: Vec<T>, active: usize) {
        self.tabs = tabs;
        if self.tabs.is_empty() {
            match self.mint() {
                Some(fresh) => self.tabs.push(fresh),
                None => {
                    self.visits.clear();
                    self.active = 0;
                    return;
                }
            }
        }
        // A restored session has no trail: nothing but the active tab has
        // been shown, so every other stamp starts at zero.
        self.visits = vec![0; self.tabs.len()];
        self.active = active.min(self.tabs.len() - 1);
        self.stamp();
    }
}

#[cfg(test)]
mod tests;
