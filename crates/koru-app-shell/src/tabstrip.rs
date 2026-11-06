// SPDX-License-Identifier: MPL-2.0

//! The caption strip: a sequence of tab groups, each a [`TabSet`].
//!
//! Pinned is a group stuck on the left; Main fills the rest. A later TabGroup
//! is another [`TabSet`] with a different placement — do not flatten pins into
//! indices `0..n` of one vec.
//!
//! ## What the strip asks its tabs, and what it does not
//!
//! Generic over the element, and every domain question it used to answer by
//! matching on a `TabView` or reading a `document` field is now one method on
//! [`TabElement`]: *may this be pinned*, *is this a placeholder*, *is this
//! reusable*, *which scope is it pinned into*. Those four are the whole of what
//! the container branched on, and moving them to the host is what makes the
//! machinery portable — the strip compares uuids and learns nothing from them.

use uuid::Uuid;

use crate::element::TabElement;
use crate::tabset::TabSet;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TabGroup {
    Pinned,
    Main,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TabLoc {
    pub group: TabGroup,
    pub index: usize,
}

pub struct TabStrip<T> {
    pinned: TabSet<T>,
    main: TabSet<T>,
    active_group: TabGroup,
}

impl<T: TabElement> TabStrip<T> {
    /// A strip holding one tab, and the factory its floor mints through.
    ///
    /// The factory is the host's ([ADR-262](../../../docs/decisions/README.md));
    /// only Main has one, because the pinned group is allowed to be empty.
    pub fn new(first: T, floor: impl Fn() -> T + 'static) -> Self {
        Self {
            pinned: TabSet::empty(),
            main: TabSet::new(first, floor),
            active_group: TabGroup::Main,
        }
    }

    pub fn len(&self) -> usize {
        self.pinned.len() + self.main.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn pinned_count(&self) -> usize {
        self.pinned.len()
    }

    pub fn main_active(&self) -> usize {
        self.main.active()
    }

    pub fn pinned_showing(&self) -> Option<usize> {
        match self.active_group {
            TabGroup::Pinned if !self.pinned.is_empty() => Some(self.pinned.active()),
            _ => None,
        }
    }

    pub fn main_count(&self) -> usize {
        self.main.len()
    }

    /// Flat index of Main's last-active tab when that tab is reusable.
    ///
    /// Main keeps its last-active index while a pin is showing, so this is the
    /// working slot new content should overwrite instead of the pin.
    pub fn reusable_main_flat_index(&self) -> Option<usize> {
        if self.main.is_empty() {
            return None;
        }
        let i = self.main.active();
        self.main
            .get(i)
            .is_some_and(TabElement::is_reusable)
            .then_some(self.pinned.len() + i)
    }

    pub fn loc(&self, flat: usize) -> Option<TabLoc> {
        let n = self.pinned.len();
        if flat < n {
            Some(TabLoc {
                group: TabGroup::Pinned,
                index: flat,
            })
        } else {
            let index = flat - n;
            if index < self.main.len() {
                Some(TabLoc {
                    group: TabGroup::Main,
                    index,
                })
            } else {
                None
            }
        }
    }

    pub fn flat(&self, loc: TabLoc) -> usize {
        match loc.group {
            TabGroup::Pinned => loc.index,
            TabGroup::Main => self.pinned.len() + loc.index,
        }
    }

    pub fn active_loc(&self) -> TabLoc {
        match self.active_group {
            TabGroup::Pinned if !self.pinned.is_empty() => TabLoc {
                group: TabGroup::Pinned,
                index: self.pinned.active(),
            },
            TabGroup::Main if !self.main.is_empty() => TabLoc {
                group: TabGroup::Main,
                index: self.main.active(),
            },
            _ if !self.pinned.is_empty() => TabLoc {
                group: TabGroup::Pinned,
                index: self.pinned.active(),
            },
            _ => TabLoc {
                group: TabGroup::Main,
                index: self.main.active(),
            },
        }
    }

    pub fn active(&self) -> usize {
        self.flat(self.active_loc())
    }

    pub fn active_tab(&self) -> &T {
        match self.active_loc().group {
            TabGroup::Pinned => self.pinned.active_tab(),
            TabGroup::Main => self.main.active_tab(),
        }
    }

    pub fn active_mut(&mut self) -> &mut T {
        match self.active_loc().group {
            TabGroup::Pinned => self.pinned.active_mut(),
            TabGroup::Main => self.main.active_mut(),
        }
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        let loc = self.loc(i)?;
        match loc.group {
            TabGroup::Pinned => self.pinned.get(loc.index),
            TabGroup::Main => self.main.get(loc.index),
        }
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        let loc = self.loc(i)?;
        match loc.group {
            TabGroup::Pinned => self.pinned.get_mut(loc.index),
            TabGroup::Main => self.main.get_mut(loc.index),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.pinned.iter().chain(self.main.iter())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.pinned.iter_mut().chain(self.main.iter_mut())
    }

    /// The flat index of the first tab the host's predicate accepts, pins first.
    ///
    /// **Where `showing` and `showing_document` went.** Both asked a domain
    /// question over the same walk; the walk is here and the question is the
    /// host's.
    pub fn position(&self, keep: impl Fn(&T) -> bool) -> Option<usize> {
        self.pinned
            .position(&keep)
            .or_else(|| self.main.position(&keep).map(|i| self.pinned.len() + i))
    }

    pub fn select(&mut self, i: usize) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        if self.active_loc() == loc {
            return false;
        }
        if self.active_group == loc.group {
            return match loc.group {
                TabGroup::Pinned => self.pinned.select(loc.index),
                TabGroup::Main => self.main.select(loc.index),
            };
        }
        self.active_group = loc.group;
        match loc.group {
            TabGroup::Pinned => self.pinned.force_select(loc.index),
            TabGroup::Main => self.main.force_select(loc.index),
        }
        true
    }

    pub fn cycle(&mut self, delta: i32) -> bool {
        if self.len() < 2 {
            return false;
        }
        if self.pinned.is_empty() {
            return self.main.cycle(delta);
        }
        if self.main.is_empty() {
            return self.pinned.cycle(delta);
        }
        let n = self.len() as i32;
        let next = (self.active() as i32 + delta).rem_euclid(n) as usize;
        self.select(next)
    }

    /// Insert `tab` in Main without selecting it.
    ///
    /// **Always Main.** Only [`pin`](Self::pin) puts a tab in the pin group.
    /// Ctrl+click (a background tab) used to insert into whichever group was
    /// showing, so a click from a pin placed an unpinned tab in the pin strip.
    /// `is_pinned()` was then false while `loc()` still treated it as a pin —
    /// close, SameTab and the keyboard all desynced from what was on screen.
    pub fn insert_inactive(&mut self, tab: T) -> usize {
        self.drop_lone_replaceable_placeholder_if_pinned();
        let i = if self.main.is_empty() {
            self.main.insert(0, tab)
        } else {
            self.main.insert(self.main.active() + 1, tab)
        };
        self.pinned.len() + i
    }

    /// Lift tab `i` out of its group.
    #[allow(dead_code)]
    pub fn take(&mut self, i: usize) -> Option<T> {
        let loc = self.loc(i)?;
        let tab = match loc.group {
            TabGroup::Pinned => self.pinned.take(loc.index),
            TabGroup::Main => self.main.take(loc.index),
        };
        if tab.is_some() {
            self.sanitize();
        }
        tab
    }

    /// Lift tab `i` out without adding the usual placeholder floor. Used when a
    /// pane group is being emptied so it can be collapsed.
    #[allow(dead_code)]
    pub fn take_no_floor(&mut self, i: usize) -> Option<T> {
        let loc = self.loc(i)?;
        match loc.group {
            TabGroup::Pinned => self.pinned.take(loc.index),
            TabGroup::Main => self.main.take(loc.index),
        }
    }

    /// Insert `tab` at flat index `i`, clamped to the end.
    #[allow(dead_code)]
    pub fn insert(&mut self, i: usize, tab: T) -> usize {
        let n = self.len();
        let i = i.min(n);
        if i < self.pinned.len() {
            self.pinned.insert(i, tab)
        } else {
            let main_i = i - self.pinned.len();
            self.main.insert(main_i, tab) + self.pinned.len()
        }
    }

    pub fn push(&mut self, tab: T) -> usize {
        // With pins holding the strip up, a lone placeholder is only a floor —
        // pushing real content over it replaces it rather than joining it.
        // Unless the host says the placeholder is not to be replaced: a
        // dedicated page (Koru's Welcome) stays beside what opens.
        self.drop_lone_replaceable_placeholder_if_pinned();
        let index = self.main.push(tab);
        self.active_group = TabGroup::Main;
        self.pinned.len() + index
    }

    pub fn close(&mut self, i: usize) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        match loc.group {
            TabGroup::Pinned => {
                if !self.pinned.close(loc.index) {
                    return false;
                }
            }
            TabGroup::Main => {
                if self.main.len() == 1 && !self.pinned.is_empty() {
                    let _ = self.main.take(0);
                } else if !self.main.close(loc.index) {
                    return false;
                }
            }
        }
        self.sanitize();
        true
    }

    pub fn close_others(&mut self, i: usize) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        let changed = match loc.group {
            TabGroup::Pinned => {
                if self.main.is_empty() {
                    false
                } else {
                    self.main.clear();
                    true
                }
            }
            TabGroup::Main => self.main.close_others(loc.index),
        };
        if changed {
            self.sanitize();
        }
        changed
    }

    pub fn close_left(&mut self, i: usize) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        if loc.group != TabGroup::Main {
            return false;
        }
        if !self.main.close_left(loc.index) {
            return false;
        }
        self.sanitize();
        true
    }

    pub fn close_right(&mut self, i: usize) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        if loc.group != TabGroup::Main {
            return false;
        }
        if !self.main.close_right(loc.index) {
            return false;
        }
        self.sanitize();
        true
    }

    pub fn close_all_unpinned(&mut self) -> bool {
        if self.pinned.is_empty() {
            if self.main.len() <= 1 {
                return false;
            }
            self.main.reset_to_floor();
            self.active_group = TabGroup::Main;
            return true;
        }
        if self.main.is_empty() {
            return false;
        }
        self.main.clear();
        self.sanitize();
        true
    }

    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        let (Some(a), Some(b)) = (self.loc(from), self.loc(to)) else {
            return false;
        };
        if a.group != b.group {
            return false;
        }
        match a.group {
            TabGroup::Pinned => self.pinned.move_tab(a.index, b.index),
            TabGroup::Main => self.main.move_tab(a.index, b.index),
        }
    }

    /// Move a Main tab into the pinned group, stamped with the scope it belongs
    /// to.
    ///
    /// **Whether it may be pinned is the tab's own answer**
    /// ([`TabElement::pinnable`]) — Koru's is *every tab* — and the strip only
    /// needs a bool. A refusal puts the tab back where it was.
    pub fn pin(&mut self, i: usize, home: Uuid, show_title: bool) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        if loc.group != TabGroup::Main {
            return false;
        }
        let Some(mut tab) = self.main.take(loc.index) else {
            return false;
        };
        if !tab.pinnable() {
            self.main.insert(loc.index, tab);
            return false;
        }
        tab.set_home_scope(Some(home));
        tab.set_pinned_scope(Some(home));
        tab.set_show_title(show_title);
        let index = self.pinned.push(tab);
        self.active_group = TabGroup::Pinned;
        self.pinned.force_select(index);
        self.sanitize();
        true
    }

    /// Take a tab out of the pinned group. It returns to Main when its pin scope
    /// is the one showing, and is dropped when it belongs somewhere else.
    pub fn unpin(&mut self, i: usize, current: Option<Uuid>) -> bool {
        let Some(loc) = self.loc(i) else {
            return false;
        };
        if loc.group != TabGroup::Pinned {
            return false;
        }
        let Some(mut tab) = self.pinned.take(loc.index) else {
            return false;
        };
        let home = tab.pinned_scope();
        tab.set_pinned_scope(None);
        if tab.home_scope().is_none() {
            tab.set_home_scope(home);
        }
        tab.set_show_title(false);
        if home == current {
            let index = self.main.insert(0, tab);
            self.active_group = TabGroup::Main;
            self.main.force_select(index);
        }
        self.sanitize();
        true
    }

    pub fn set_show_title(&mut self, i: usize, show: bool) -> bool {
        let Some(tab) = self.get_mut(i) else {
            return false;
        };
        if !tab.is_pinned() || tab.show_title() == show {
            return false;
        }
        tab.set_show_title(show);
        true
    }

    /// Close every Main tab the host's predicate rejects.
    ///
    /// **Where `retain_live` went.** Whether a tab's content still exists is a
    /// question about a document, so the predicate is the host's.
    pub fn retain_main(&mut self, keep: impl Fn(&T) -> bool) -> bool {
        let changed = self.main.retain(keep);
        if changed {
            self.sanitize();
        }
        changed
    }

    /// Drop every pinned tab the host's predicate rejects. Pins are **taken**
    /// rather than closed: a pin group has no floor and must be allowed to
    /// empty.
    pub fn retain_pins(&mut self, keep: impl Fn(&T) -> bool) -> bool {
        let dead: Vec<usize> = self
            .pinned
            .iter()
            .enumerate()
            .filter(|(_, tab)| !keep(tab))
            .map(|(i, _)| i)
            .collect();
        if dead.is_empty() {
            return false;
        }
        for i in dead.into_iter().rev() {
            let _ = self.pinned.take(i);
        }
        self.sanitize();
        true
    }

    /// Replace Main with one fresh floor tab, keeping the pins.
    pub fn reset_to_floor(&mut self) {
        if !self.pinned.is_empty() {
            self.main.clear();
            self.sanitize();
            return;
        }
        self.main.reset_to_floor();
        self.active_group = TabGroup::Main;
        self.sanitize();
    }

    /// [`drop_lone_placeholder_if_pinned`](Self::drop_lone_placeholder_if_pinned),
    /// but only for a placeholder that is also [`is_reusable`](TabElement::is_reusable)
    /// — the floor real content may take the place of. What `push` consults.
    fn drop_lone_replaceable_placeholder_if_pinned(&mut self) {
        if self
            .main
            .get(0)
            .is_some_and(|tab| self.main.len() == 1 && tab.is_reusable())
        {
            self.drop_lone_placeholder_if_pinned();
        }
    }

    pub fn drop_lone_placeholder_if_pinned(&mut self) {
        if self.pinned.is_empty() {
            return;
        }
        if self.main.len() == 1 && self.main.get(0).is_some_and(TabElement::is_placeholder) {
            let _ = self.main.take(0);
            self.sanitize();
        }
    }

    /// Whether any tab in the strip is a placeholder.
    ///
    /// A pin is never a placeholder whatever it currently draws: the element
    /// answers [`TabElement::is_placeholder`] `false` for a pinned tab, since
    /// a placeholder is what the strip drops or lets content replace.
    pub fn has_placeholder(&self) -> bool {
        self.iter().any(TabElement::is_placeholder)
    }

    pub fn restore_pinned(&mut self, tabs: Vec<T>, showing: Option<usize>) {
        self.pinned.restore(tabs, showing.unwrap_or(0));
        if let Some(i) = showing.filter(|&i| i < self.pinned.len()) {
            self.active_group = TabGroup::Pinned;
            self.pinned.force_select(i);
        }
        self.sanitize();
    }

    pub fn restore_main(&mut self, tabs: Vec<T>, active: usize) {
        self.main.restore(tabs, active);
        if self.active_group != TabGroup::Pinned || self.pinned.is_empty() {
            self.active_group = TabGroup::Main;
        }
        self.sanitize();
    }

    fn sanitize(&mut self) {
        match self.active_group {
            TabGroup::Pinned if self.pinned.is_empty() => {
                if self.main.is_empty() {
                    self.main.reset_to_floor();
                }
                self.active_group = TabGroup::Main;
            }
            TabGroup::Main if self.main.is_empty() => {
                if self.pinned.is_empty() {
                    self.main.reset_to_floor();
                    self.active_group = TabGroup::Main;
                } else {
                    self.active_group = TabGroup::Pinned;
                }
            }
            _ => {
                if self.pinned.is_empty() && self.main.is_empty() {
                    self.main.reset_to_floor();
                    self.active_group = TabGroup::Main;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
