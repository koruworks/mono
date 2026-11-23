// SPDX-License-Identifier: MPL-2.0

//! **The navigator's list**: the gallery's catalog as three groups that fold,
//! each entry a row, in one scrolling viewport.
//!
//! ```text
//! scroll  Navigator list              the viewport; its key presses the scrollbar
//!   Rows above                        the height of the rows scrolled past
//!   row   Navigator group             chevron, heading, count
//!   row   Navigator entry             a record's name
//!   Rows below                        the height of the rows not reached yet
//! ```
//!
//! **What it lists is [`registry::desktop`]'s**, and nothing else reads the
//! catalog for it: a record that exists only on a phone is not a row, is not
//! counted beside its group, is not found by the search and cannot be
//! reached by the keyboard.
//!
//! **The search narrows the same list.** What is typed in the field over it
//! is matched against each record's name in the display language, its public
//! name, its catalog ID and its recorded symbols; the groups keep their
//! headings and count what was found. A search that finds nothing keeps the
//! query in the field, says so, and offers a button that empties it. The
//! record on show stays on show whatever is searched for.
//!
//! **Only the rows in view are elements.** Every row is one height
//! ([`NAVIGATOR_ROW`]), so which rows the viewport shows is arithmetic on the
//! scroll offset. The tree holds those rows and two spacers that stand for
//! the rest, so three hundred records cost a screenful to lay out, paint and
//! publish. The viewport itself is the element layer's scroll element with
//! the shared scrollbar, which is what clamps the offset and measures the
//! thumb.
//!
//! **The list is one keyboard stop.** Tab reaches the row the keyboard was
//! last on, the arrows move through the rows, Left and Right fold and unfold
//! a group, and Enter or Space shows an entry. A row is identified by what it
//! stands for, never by its position, so folding a group cannot move the
//! keyboard to whatever took a row's place.

use koru_draw::icons;
use koru_draw::theme::IconSize;

use super::*;

/// The scrolling viewport. Its key is what a press in the scrollbar's gutter
/// and the wheel are about.
pub(super) const LIST: Key = Key::surface("ui-lab", "navigator");
/// The search field over the list.
pub(super) const SEARCH: Key = Key::surface("ui-lab", "search");
/// The line that says a search found nothing.
pub(super) const NOTHING_FOUND: Key = Key::surface("ui-lab", "no-results");
/// The button under that line that empties the search.
pub(super) const RESET_SEARCH: Key = Key::surface("ui-lab", "reset-search");

/// How much of a query the nothing-found line quotes. A query can be any
/// length and need not have a space to wrap at.
const QUOTED: usize = 32;

/// One row of the list.
#[derive(Copy, Clone, Debug)]
pub(super) enum Row {
    /// A group's heading: pressing it folds or unfolds the group.
    Group(Family),
    /// A record: pressing it shows the record's page.
    Entry(&'static Entry),
}

/// Two rows are the same row when they stand for the same thing: a record is
/// its catalog ID, which the registry keeps unique.
impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Group(a), Self::Group(b)) => a == b,
            (Self::Entry(a), Self::Entry(b)) => a.catalog_id == b.catalog_id,
            _ => false,
        }
    }
}

impl Row {
    /// The row's identity in the window: the group's keyword or the record's
    /// catalog ID, the same in every language.
    pub(super) fn key(self) -> Key {
        match self {
            Self::Group(family) => Key::surface("ui-lab.group", family.part()),
            Self::Entry(entry) => Key::surface("ui-lab.navigator", entry.catalog_id),
        }
    }
}

/// **A group heading's states**: the hover plate under the pointer and the
/// focus ring while the keyboard is on it. A group is never the selected row.
static GROUP_STATES: StateRecipe = StateRecipe {
    name: "ui lab navigator group",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// What the list holds and where the person is in it.
pub(super) struct Navigator {
    /// What the search is looking for: [`registry::search_words`]'s. Empty
    /// while nothing is searched for.
    words: Vec<String>,
    /// The groups the person folded, by [`Family::slot`].
    folded: [bool; 3],
    /// The rows the list shows, top to bottom: each group that has a record,
    /// then its records unless it is folded.
    rows: Vec<Row>,
    /// How many records each group has that the search finds, folded or
    /// not.
    counts: [usize; 3],
    /// The row the keyboard is on, or was last on.
    cursor: Option<Row>,
    /// The viewport's offset, as the last layout clamped it.
    pub(super) scroll: f32,
}

impl Navigator {
    /// The whole gallery, every group unfolded, the keyboard on `shown`.
    pub(super) fn new(shown: &'static Entry) -> Self {
        let mut navigator = Self {
            words: Vec::new(),
            folded: [false; 3],
            rows: Vec::new(),
            counts: [0; 3],
            cursor: Some(Row::Entry(shown)),
            scroll: 0.0,
        };
        navigator.rebuild();
        navigator
    }

    /// Read the gallery again: after a group folds or unfolds, the search
    /// changes, or the display language does (a record is found by the name
    /// it has in that language).
    pub(super) fn rebuild(&mut self) {
        self.rows.clear();
        self.counts = [0; 3];
        for family in Family::ALL {
            let records: Vec<&'static Entry> = registry::desktop()
                .filter(|entry| entry.family == family)
                .filter(|entry| self.words.is_empty() || entry.matches(&self.words))
                .collect();
            // A group with nothing in it is not shown as an empty heading.
            if records.is_empty() {
                continue;
            }
            self.counts[family.slot()] = records.len();
            self.rows.push(Row::Group(family));
            if !self.folded[family.slot()] {
                self.rows.extend(records.into_iter().map(Row::Entry));
            }
        }
        // A row that folded away hands the keyboard to its group's heading;
        // one the search no longer finds leaves the keyboard nowhere in
        // particular, and Tab finds the list's first row.
        if let Some(Row::Entry(entry)) = self.cursor
            && self.position(Row::Entry(entry)).is_none()
        {
            self.cursor = Some(Row::Group(entry.family));
        }
        if self.cursor.is_some_and(|row| self.position(row).is_none()) {
            self.cursor = None;
        }
    }

    /// Search for `query`: the list becomes the records it finds, from the
    /// top. Returns whether what is searched for changed.
    pub(super) fn set_query(&mut self, query: &str) -> bool {
        let words = registry::search_words(query);
        if words == self.words {
            return false;
        }
        self.words = words;
        self.scroll = 0.0;
        self.rebuild();
        true
    }

    /// The first record the list shows.
    pub(super) fn first_entry(&self) -> Option<&'static Entry> {
        self.rows.iter().find_map(|row| match row {
            Row::Entry(entry) => Some(*entry),
            Row::Group(_) => None,
        })
    }

    pub(super) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// How many records of `family` the search finds, whether or not they
    /// are showing. All of them while nothing is searched for.
    pub(super) fn count(&self, family: Family) -> usize {
        self.counts[family.slot()]
    }

    pub(super) fn is_folded(&self, family: Family) -> bool {
        self.folded[family.slot()]
    }

    /// The row `key` identifies, when the list has it.
    pub(super) fn row_of(&self, key: Key) -> Option<Row> {
        self.rows.iter().copied().find(|row| row.key() == key)
    }

    fn position(&self, row: Row) -> Option<usize> {
        self.rows.iter().position(|candidate| *candidate == row)
    }

    #[cfg(test)]
    pub(super) fn cursor(&self) -> Option<Row> {
        self.cursor
    }

    /// Put the keyboard on `row`, when the list has it.
    pub(super) fn set_cursor(&mut self, row: Row) -> bool {
        if self.position(row).is_none() || self.cursor == Some(row) {
            return false;
        }
        self.cursor = Some(row);
        true
    }

    /// Fold or unfold `family`. Returns whether anything changed.
    pub(super) fn set_folded(&mut self, family: Family, folded: bool) -> bool {
        if self.folded[family.slot()] == folded || self.count(family) == 0 {
            return false;
        }
        self.folded[family.slot()] = folded;
        self.rebuild();
        true
    }

    /// The row `delta` rows from the keyboard's, stopping at either end.
    pub(super) fn stepped(&self, delta: isize) -> Option<Row> {
        let last = self.rows.len().checked_sub(1)?;
        let at = self.cursor.and_then(|row| self.position(row));
        let next = match at {
            Some(at) => at.saturating_add_signed(delta).min(last),
            None if delta < 0 => last,
            None => 0,
        };
        self.rows.get(next).copied()
    }

    pub(super) fn first(&self) -> Option<Row> {
        self.rows.first().copied()
    }

    pub(super) fn last(&self) -> Option<Row> {
        self.rows.last().copied()
    }

    /// The first record under `family`, when it is unfolded and has one.
    pub(super) fn first_in(&self, family: Family) -> Option<Row> {
        let at = self.position(Row::Group(family))?;
        self.rows
            .get(at + 1)
            .copied()
            .filter(|row| matches!(row, Row::Entry(_)))
    }

    /// The offset that brings `row` wholly into a viewport `viewport` high:
    /// unchanged when it already shows.
    pub(super) fn reveal(&mut self, row: Row, viewport: f32) -> bool {
        let Some(at) = self.position(row) else {
            return false;
        };
        let height = NAVIGATOR_ROW.value;
        let top = at as f32 * height;
        let scroll = if top < self.scroll {
            top
        } else if top + height > self.scroll + viewport {
            top + height - viewport
        } else {
            self.scroll
        };
        let changed = scroll != self.scroll;
        self.scroll = scroll.max(0.0);
        changed
    }

    /// The offset that puts `row` in the middle of a viewport `viewport`
    /// high, when it is not in view: where the list opens, so the record on
    /// show is seen with its neighbours rather than at an edge. The layout
    /// holds the offset to what the list can scroll.
    pub(super) fn centre(&mut self, row: Row, viewport: f32) -> bool {
        let Some(at) = self.position(row) else {
            return false;
        };
        let height = NAVIGATOR_ROW.value;
        let top = at as f32 * height;
        if top >= self.scroll && top + height <= self.scroll + viewport {
            return false;
        }
        let scroll = (top - (viewport - height) * 0.5).max(0.0);
        let changed = scroll != self.scroll;
        self.scroll = scroll;
        changed
    }

    /// The rows a viewport no taller than `height` can show at the current
    /// offset: the range of [`rows`](Self::rows) that become elements.
    fn window(&self, height: f32) -> std::ops::Range<usize> {
        let row = NAVIGATOR_ROW.value;
        let first = ((self.scroll / row).floor().max(0.0) as usize).min(self.rows.len());
        // One more than fits, for the row cut by the viewport's bottom edge.
        let count = (height / row).ceil().max(0.0) as usize + 1;
        first..first.saturating_add(count).min(self.rows.len())
    }

    /// The row Tab stops on: the keyboard's own while it is in view,
    /// otherwise the one showing `shown`, otherwise the first in view. There
    /// is always one, so the list can always be reached.
    fn stop(&self, window: std::ops::Range<usize>, shown: &'static Entry) -> Option<Row> {
        let visible = &self.rows[window];
        let in_view = |row: Row| visible.contains(&row).then_some(row);
        self.cursor
            .and_then(in_view)
            .or_else(|| in_view(Row::Entry(shown)))
            .or_else(|| visible.first().copied())
    }

    /// The list as elements, for a window `window_height` high. `shown` is
    /// the record on show, whose row is the selected one.
    pub(super) fn element(&self, shown: &'static Entry, window_height: f32) -> El {
        let window = self.window(window_height);
        let stop = self.stop(window.clone(), shown);
        let row = NAVIGATOR_ROW.value;
        let above = window.start as f32 * row;
        let below = (self.rows.len() - window.end) as f32 * row;
        let mut list = el::scroll_y("Navigator list")
            .id(LIST)
            .fill()
            .scrollbar()
            .offset(self.scroll)
            .semantics(Role::List)
            .name(koru_i18n::Key::UiLabNavigator);
        if above > 0.0 {
            list = list.child(el::boxed("Rows above").h(Length::Runtime(
                above,
                "navigator rows scrolled above the viewport",
            )));
        }
        list = list.children(self.rows[window].iter().map(|row| {
            let element = match *row {
                Row::Group(family) => self.group(family),
                Row::Entry(entry) => entry_row(entry, entry.catalog_id == shown.catalog_id),
            };
            if stop == Some(*row) {
                element.focusable()
            } else {
                element
            }
        }));
        if below > 0.0 {
            list = list.child(
                el::boxed("Rows below")
                    .h(Length::Runtime(below, "navigator rows below the viewport")),
            );
        }
        list
    }

    fn group(&self, family: Family) -> El {
        let chevron = if self.is_folded(family) {
            icons::CARET_RIGHT
        } else {
            icons::CARET_DOWN
        };
        let count = koru_i18n::number::decimal(koru_i18n::current(), self.count(family) as u64);
        el::row("Navigator group")
            .h(NAVIGATOR_ROW)
            .pad_x(alias::NAV_PAD)
            .gap(NAVIGATOR_GROUP_GAP)
            .radius(Radius::M)
            .items_center()
            .states(&GROUP_STATES)
            .id(Row::Group(family).key())
            .hit()
            .semantics(Role::Button)
            .name(family.heading())
            .action(el::Action::Click)
            .action(el::Action::Focus)
            .child(el::icon(chevron, IconSize::XS).decorative())
            .child(
                el::text(text(family.heading()))
                    .named("Family heading")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            )
            .child(
                el::text(count)
                    .named("Family count")
                    .role(roles::META)
                    .ink(Ink::Hint),
            )
    }
}

/// **What stands in the list's place when a search finds nothing**: a line
/// that says so, quoting what was searched for, and a button that empties
/// the search. The query itself stays in the field.
pub(super) fn nothing_found(engine: &mut TextEngine, query: &str) -> El {
    let query = query.trim();
    let quoted = match query.char_indices().nth(QUOTED) {
        Some((end, _)) => format!("{}…", &query[..end]),
        None => query.to_owned(),
    };
    let reset = text(koru_i18n::Key::UiLabResetSearch);
    el::col("No results")
        .fill()
        .pad_top(NAVIGATOR_GAP)
        .gap(PANEL_GAP)
        .child(
            el::text(koru_i18n::fill(
                text(koru_i18n::Key::UiLabNoResults),
                &[&quoted],
            ))
            .named("No results message")
            .id(NOTHING_FOUND)
            .role(roles::ROW)
            .ink(Ink::Secondary)
            .wrap(),
        )
        .child(
            el::leaf(
                "Reset search",
                Slot {
                    width: ActionButton::new(reset, ActionStyle::Secondary, None).width(engine),
                    height: ActionButton::H,
                },
            )
            .semantic()
            .id(RESET_SEARCH)
            .align_self(Align::Start)
            .hit()
            .focusable()
            .semantics(Role::Button)
            .name(koru_i18n::Key::UiLabResetSearch)
            .action(el::Action::Click)
            .action(el::Action::Focus),
        )
}

/// One record's row. The selected one wears the selected plate and a heavier
/// name, so it is told apart by more than a color.
fn entry_row(entry: &'static Entry, selected: bool) -> El {
    el::row("Navigator entry")
        .h(NAVIGATOR_ROW)
        .pad_start(NAVIGATOR_INDENT)
        .pad_end(alias::NAV_PAD)
        .radius(Radius::M)
        .items_center()
        .states(&ENTRY_STATES)
        .selected(selected)
        .id(Row::Entry(entry).key())
        .hit()
        .semantics(Role::ListItem)
        .name(entry.name)
        .action(el::Action::Click)
        .action(el::Action::Focus)
        .child(
            el::text(text(entry.name))
                .named("Entry name")
                .role(roles::ROW)
                .emphasis(selected)
                .fill()
                .truncate(),
        )
}

#[cfg(test)]
mod tests;
