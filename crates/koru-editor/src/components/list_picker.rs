// SPDX-License-Identifier: MPL-2.0

//! The `/task_list` picker: put this row in one of the workspace's lists —
//! making it a task first if it is not one — or make a new list, with its
//! colour, and put it there.
//!
//! On the [`TagPicker`] template: a search box over the table, the row the
//! node is already in checked, a **Clear** row while it is in one, and, when
//! what was typed names no list, a last row that offers to make one. Choosing
//! that row does not commit yet — a list wears a colour on the rail, so the
//! panel turns into the swatch list with the first unworn colour preselected,
//! and Enter there is what creates. The list table lives in the workspace's
//! system document, which this crate cannot see, so creating leaves as
//! [`Outcome::CreateList`] for the host; joining an existing list is
//! [`Outcome::JoinList`], which the editor commits in place.
//!
//! [`TagPicker`]: super::tag_picker::TagPicker

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::Outcome;
use super::command_menu::{MAX_ROWS, rows_top, visible_rows};
use super::text_field::TextField;
use crate::trigger::Trigger;
use koru_core::NodeId;
use koru_core::attributes::{AccentColor, KEY_LIST};
use koru_core::ids::ListId;
use koru_core::tags;
use koru_draw::text::TextEngine;
use koru_draw::theme;

/// One row's worth of the list table, resolved for display.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListChoice {
    pub id: ListId,
    /// The list's own name: what the search box filters on, and what the
    /// create row compares against.
    pub name: String,
    /// The list's accent token, for the row's swatch.
    pub color: Option<String>,
    /// The lists it is inside, ready to be shown ahead of the name — only
    /// when another list shares the name, so the row says which one it is.
    pub path: Option<String>,
    /// Whether it sits at the top level, which is where *New list* creates.
    pub top_level: bool,
}

impl ListChoice {
    /// What the row reads: the name, behind its path when it has one.
    pub fn label(&self) -> String {
        match &self.path {
            Some(path) => format!("{path}{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Which of the picker's two faces is showing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Stage {
    /// The lists, filtered by the search box.
    Lists,
    /// The swatches, for the list about to be made.
    Color { name: String, sel: usize },
}

pub struct ListPicker {
    pub trigger: Trigger,
    /// The nodes the choice applies to — one for `/task_list`.
    pub nodes: Vec<NodeId>,
    /// The table snapshot this picker filters, in the rail's order.
    pub table: Vec<ListChoice>,
    /// The list the (first) node is in now: checked, and what puts the Clear
    /// row on.
    pub current: Option<ListId>,
    pub query: TextField,
    /// Indices into [`table`](Self::table).
    pub hits: Vec<usize>,
    pub sel: usize,
    /// First visible row, when the list is longer than the window.
    pub first: usize,
    pub stage: Stage,
}

impl ListPicker {
    pub fn opened(
        engine: &mut TextEngine,
        trigger: Trigger,
        nodes: Vec<NodeId>,
        table: Vec<ListChoice>,
        current: Option<ListId>,
    ) -> Self {
        let hits = (0..table.len()).collect();
        let mut picker = Self {
            trigger,
            nodes,
            table,
            current,
            query: TextField::new(engine, theme::role::ui()),
            hits,
            sel: 0,
            first: 0,
            stage: Stage::Lists,
        };
        // The list the row is in, preselected — past the Clear row.
        if let Some(current) = current
            && let Some(row) = picker
                .hits
                .iter()
                .position(|&i| picker.table[i].id == current)
        {
            picker.sel = 1 + row;
        }
        picker
    }

    /// Whether the search box is drawn: only over the lists.
    pub fn has_search(&self) -> bool {
        matches!(self.stage, Stage::Lists)
    }

    pub fn query_text(&self) -> String {
        self.query.value()
    }

    /// Whether row 0 is Clear: the node is in a list and nothing is typed.
    pub fn offers_clear(&self) -> bool {
        self.current.is_some() && self.query_text().trim().is_empty()
    }

    /// The name the last row offers to make a list from, when what was typed
    /// names no list yet.
    pub fn offer_create(&self) -> Option<String> {
        offer_create_for(&self.table, &self.query_text())
    }

    pub fn row_count(&self) -> usize {
        match &self.stage {
            Stage::Lists => {
                usize::from(self.offers_clear())
                    + self.hits.len()
                    + usize::from(self.offer_create().is_some())
            }
            Stage::Color { .. } => AccentColor::ALL.len(),
        }
    }

    /// What sits on row `i` of the lists face.
    pub fn row(&self, i: usize) -> Option<ListRow> {
        let mut i = i;
        if self.offers_clear() {
            if i == 0 {
                return Some(ListRow::Clear);
            }
            i -= 1;
        }
        if let Some(&t) = self.hits.get(i) {
            return Some(ListRow::List(t));
        }
        if i == self.hits.len()
            && let Some(name) = self.offer_create()
        {
            return Some(ListRow::Create(name));
        }
        None
    }

    pub fn first_visible(&self) -> usize {
        self.first.min(self.max_first())
    }

    fn max_first(&self) -> usize {
        self.row_count().saturating_sub(MAX_ROWS)
    }

    fn scroll_selection_into_view(&mut self) {
        let rows = visible_rows(self.row_count());
        if self.sel < self.first {
            self.first = self.sel;
        } else if self.sel >= self.first + rows {
            self.first = self.sel + 1 - rows;
        }
    }

    pub fn height(&self) -> f32 {
        let rows = visible_rows(self.row_count()) as f32 * theme::ROW_H;
        if self.has_search() {
            theme::PANEL_PAD + rows_top() + rows + 8.0
        } else {
            theme::PANEL_PAD * 2.0 + rows
        }
    }

    fn refilter_from_field(&mut self) {
        let q = self.query_text();
        self.hits = filter_table(&self.table, &q);
        self.sel = self.sel.min(self.row_count().saturating_sub(1));
        self.first = 0;
    }

    /// The first swatch no list is wearing — the rail's own suggestion.
    pub fn suggested_color(&self) -> AccentColor {
        AccentColor::ALL
            .into_iter()
            .find(|token| {
                !self
                    .table
                    .iter()
                    .any(|list| list.color.as_deref() == Some(token.as_str()))
            })
            .unwrap_or(AccentColor::ALL[0])
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        let rows = self.row_count().max(1);
        match named {
            Some(NamedKey::ArrowDown) => {
                self.move_sel(|s| (s + 1) % rows);
                Outcome::Consumed
            }
            Some(NamedKey::ArrowUp) => {
                self.move_sel(|s| (s + rows - 1) % rows);
                Outcome::Consumed
            }
            Some(NamedKey::Enter) | Some(NamedKey::Tab) => {
                let sel = match &self.stage {
                    Stage::Lists => self.sel,
                    Stage::Color { sel, .. } => *sel,
                };
                self.choose(sel)
            }
            _ => match &self.stage {
                // Backspace on the swatches goes back to the name.
                Stage::Color { .. } if named == Some(NamedKey::Backspace) => {
                    self.stage = Stage::Lists;
                    Outcome::Consumed
                }
                Stage::Color { .. } => Outcome::Consumed,
                Stage::Lists => {
                    if named == Some(NamedKey::Backspace) && self.query.is_empty() {
                        return Outcome::Close;
                    }
                    self.query.key(engine, key, text, mods);
                    self.refilter_from_field();
                    Outcome::Consumed
                }
            },
        }
    }

    fn move_sel(&mut self, next: impl Fn(usize) -> usize) {
        match &mut self.stage {
            Stage::Lists => {
                self.sel = next(self.sel);
                self.scroll_selection_into_view();
            }
            Stage::Color { sel, .. } => *sel = next(*sel),
        }
    }

    /// Which row a panel-local point is over, in list indices.
    pub fn item_at(&self, local_y: f32) -> Option<usize> {
        let top = if self.has_search() { rows_top() } else { 0.0 };
        let row = ((local_y - top) / theme::ROW_H).floor();
        if row < 0.0 || row as usize >= visible_rows(self.row_count()) {
            return None;
        }
        let i = self.first_visible() + row as usize;
        (i < self.row_count()).then_some(i)
    }

    pub fn hover(&mut self, item: usize) {
        if item >= self.row_count() {
            return;
        }
        match &mut self.stage {
            Stage::Lists => self.sel = item,
            Stage::Color { sel, .. } => *sel = item,
        }
    }

    pub fn click(&mut self, local_y: f32) -> Outcome {
        match self.item_at(local_y) {
            Some(row) => self.choose(row),
            None => Outcome::Consumed,
        }
    }

    pub fn scroll_rows(&mut self, delta: isize) {
        if self.row_count() == 0 {
            return;
        }
        self.first = (self.first as isize + delta).clamp(0, self.max_first() as isize) as usize;
    }

    pub fn scrolls(&self) -> bool {
        self.has_search() && self.row_count() > MAX_ROWS
    }

    fn choose(&mut self, i: usize) -> Outcome {
        match self.stage.clone() {
            Stage::Color { name, .. } => match AccentColor::ALL.get(i) {
                Some(token) => {
                    Outcome::CreateList(self.nodes.clone(), name, Some(token.as_str().to_owned()))
                }
                None => Outcome::Close,
            },
            Stage::Lists => match self.row(i) {
                Some(ListRow::Clear) => Outcome::ClearAttr(self.nodes.clone(), KEY_LIST.to_owned()),
                Some(ListRow::List(t)) => Outcome::JoinList(self.nodes.clone(), self.table[t].id),
                Some(ListRow::Create(name)) => {
                    // Not a commit: the colour is chosen next, on this panel.
                    let sel = AccentColor::ALL
                        .iter()
                        .position(|t| *t == self.suggested_color())
                        .unwrap_or(0);
                    self.stage = Stage::Color { name, sel };
                    Outcome::Consumed
                }
                None => Outcome::Close,
            },
        }
    }
}

/// What one row of the lists face is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ListRow {
    Clear,
    /// An index into the table.
    List(usize),
    /// *New list "…"*, with the name it would make.
    Create(String),
}

/// The table rows whose name contains the folded query — the rail's own
/// folding, so `birthday` finds *Birthday Tasks*.
fn filter_table(table: &[ListChoice], q: &str) -> Vec<usize> {
    let query = tags::fold_key(q);
    table
        .iter()
        .enumerate()
        .filter(|(_, list)| query.is_empty() || tags::fold_key(&list.name).contains(&query))
        .map(|(i, _)| i)
        .collect()
}

/// The name the create row offers, or `None` when nothing is typed or the
/// name already names a **top-level** list — the table's own fold, so
/// *birthday tasks* and *Birthday Tasks* are one list.
///
/// The picker creates at the top level and has no parent chooser, and a name
/// is unique among siblings only. So a namesake inside another list does not
/// take the name: it is listed by its path **and** the create row is offered.
fn offer_create_for(table: &[ListChoice], q: &str) -> Option<String> {
    let name = q.trim();
    if name.is_empty() {
        return None;
    }
    let key = tags::fold_key(name);
    let exists = table
        .iter()
        .any(|list| list.top_level && tags::fold_key(&list.name) == key);
    (!exists).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Vec<ListChoice> {
        vec![
            ListChoice {
                id: ListId::new(),
                name: "Birthday Tasks".to_owned(),
                color: Some("red".to_owned()),
                path: None,
                top_level: true,
            },
            ListChoice {
                id: ListId::new(),
                name: "Bike Project".to_owned(),
                color: Some("orange".to_owned()),
                path: None,
                top_level: true,
            },
        ]
    }

    fn opened(current: Option<ListId>) -> (TextEngine, ListPicker) {
        let mut engine = TextEngine::new();
        let picker = ListPicker::opened(
            &mut engine,
            Trigger::at_start(),
            vec![NodeId::new()],
            table(),
            current,
        );
        (engine, picker)
    }

    fn type_into(engine: &mut TextEngine, picker: &mut ListPicker, s: &str) {
        for ch in s.chars() {
            let text = ch.to_string();
            picker.key(
                engine,
                &Key::Character(text.clone().into()),
                Some(&text),
                ModifiersState::empty(),
            );
        }
    }

    /// Choosing a list joins it; the row the node is already in is checked
    /// and preselected, and Clear leads while it is.
    #[test]
    fn a_list_is_joined_and_the_current_one_is_clearable() {
        let (_, mut fresh) = opened(None);
        assert!(!fresh.offers_clear());
        assert_eq!(fresh.row_count(), 2, "two lists, no Clear, nothing to make");
        let chosen = fresh.choose(1);
        assert!(
            matches!(chosen, Outcome::JoinList(_, id) if id == fresh.table[1].id),
            "row 1 is Bike Project, got {chosen:?}"
        );

        let current = table()[0].id;
        let (_, mut picker) = opened(Some(current));
        picker.table[0].id = current;
        picker.current = Some(current);
        assert!(picker.offers_clear());
        assert_eq!(picker.row_count(), 3);
        assert!(matches!(
            picker.choose(0),
            Outcome::ClearAttr(_, key) if key == KEY_LIST
        ));
    }

    /// Typing filters the table; a name nothing matches offers to make it, the
    /// swatches come next with the first unworn colour preselected, and Enter
    /// there is the creation — with that colour.
    #[test]
    fn a_new_name_makes_a_list_after_its_colour_is_chosen() {
        let (mut engine, mut picker) = opened(None);
        type_into(&mut engine, &mut picker, "Bir");
        assert_eq!(picker.hits, vec![0], "Birthday Tasks alone");
        assert!(picker.offer_create().is_some(), "no list is called Bir");
        type_into(&mut engine, &mut picker, "thday tasks");
        assert!(
            picker.offer_create().is_none(),
            "the fold makes it the list that exists"
        );

        let (mut engine, mut picker) = opened(None);
        type_into(&mut engine, &mut picker, "Wedding");
        assert!(picker.hits.is_empty());
        assert_eq!(picker.row_count(), 1, "the create row alone");
        assert!(matches!(picker.choose(0), Outcome::Consumed));
        assert!(
            matches!(&picker.stage, Stage::Color { name, sel }
            if name == "Wedding" && *sel == 2),
            "red and orange are worn; yellow is the suggestion: {:?}",
            picker.stage
        );
        assert!(!picker.has_search(), "the swatches have no search box");
        assert_eq!(picker.row_count(), AccentColor::ALL.len());
        let created = picker.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(
            matches!(&created, Outcome::CreateList(_, name, Some(color))
                if name == "Wedding" && color == "yellow"),
            "got {created:?}"
        );

        // Backspace on the swatches is the way back to the name.
        picker.stage = Stage::Color {
            name: "Wedding".to_owned(),
            sel: 0,
        };
        picker.key(
            &mut engine,
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert!(matches!(picker.stage, Stage::Lists));
    }

    /// Rows hit-test below the search box, and an empty field's Backspace
    /// closes the panel.
    #[test]
    fn rows_sit_below_the_search_box_and_backspace_on_nothing_closes() {
        let (mut engine, mut picker) = opened(None);
        assert_eq!(picker.item_at(rows_top() + theme::ROW_H * 0.5), Some(0));
        assert_eq!(picker.item_at(1.0), None, "the box is not a row");
        assert!(matches!(
            picker.key(
                &mut engine,
                &Key::Named(NamedKey::Backspace),
                None,
                ModifiersState::empty()
            ),
            Outcome::Close
        ));
    }

    fn choice(name: &str, path: Option<&str>) -> ListChoice {
        ListChoice {
            id: ListId::new(),
            name: name.to_owned(),
            color: None,
            path: path.map(str::to_owned),
            top_level: path.is_none(),
        }
    }

    /// The picker creates at the top level, so only a top-level namesake
    /// takes the name: one inside another list is shown by its path **and**
    /// the create row is still offered.
    #[test]
    fn a_namesake_inside_another_list_does_not_take_the_name() {
        let nested = vec![choice("Backlog", Some("Top 3 / "))];
        assert_eq!(
            offer_create_for(&nested, "backlog"),
            Some("backlog".to_owned())
        );
        assert_eq!(nested[0].label(), "Top 3 / Backlog");
        assert_eq!(
            filter_table(&nested, "back"),
            vec![0],
            "and it is still found by its own name"
        );
        assert!(
            filter_table(&nested, "top").is_empty(),
            "the path is shown, never searched"
        );

        let both = vec![choice("Backlog", Some("Top 3 / ")), choice("Backlog", None)];
        assert_eq!(
            offer_create_for(&both, "Backlog"),
            None,
            "a top-level list already has the name"
        );
        assert_eq!(both[1].label(), "Backlog");
        assert_eq!(
            filter_table(&both, "backlog").len(),
            2,
            "both are selectable"
        );
    }
}
