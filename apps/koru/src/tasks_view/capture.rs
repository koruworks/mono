// SPDX-License-Identifier: MPL-2.0

//! **The task input, the same on every page.**
//!
//! The *Add a task…* row at the head of every queue and every list: one text
//! field, a trailing date word that previews as a chip, and — round nine —
//! the three things a typed `/` can ask for: `/due` opens the calendar,
//! `/priority` the four levels, `/task_list` the workspace's lists. What is
//! picked rides on the row as a chip beside the words, and leaves with the
//! request when Enter commits. The page owns the pickers (they are the same
//! three it hangs from a row's `…`); this component owns the words, the picks
//! and the `/` menu, and says what the request would be.
//!
//! A second host — a column with an *Add a task* line of its own — uses this
//! and not a second field, which is the whole reason it is a module.

use koru_core::attributes::Priority;
use koru_core::ids::ListId;
use koru_core::time::Temporal;
use koru_draw::chip::{self, ChipKind};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_editor::components::text_input::{InputStyle, TextInput};
use koru_editor::utils::when::Parsed;
use koru_ui::menu::{Anchor, Menu, MenuItem};
use koru_ui::table::{RowState, Table};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::row::paint_chip;
use super::view::{CHIP_GAP, ListFace, RowChip, RowParts, split_date, temporal_of};

/// What the capture row says before anybody types in it.
pub(super) const PLACEHOLDER: &str = "Add a task\u{2026}";

/// The three things `/` offers here — the row's own pickers, by name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum SlashRow {
    Due,
    Priority,
    TaskList,
}

impl SlashRow {
    pub const ALL: [Self; 3] = [Self::Due, Self::Priority, Self::TaskList];

    /// What you type after the `/` — the editor's own names for the same
    /// three commands, so a hand that knows the document knows this row.
    pub fn name(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::Priority => "priority",
            Self::TaskList => "task_list",
        }
    }
}

impl MenuItem for SlashRow {
    fn label(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(match self {
            Self::Due => "Due\u{2026}",
            Self::Priority => "Priority\u{2026}",
            Self::TaskList => "Task list\u{2026}",
        })
    }

    fn icon(&self) -> Option<char> {
        Some(match self {
            Self::Due => icons::DATE,
            Self::Priority => icons::FLAG,
            Self::TaskList => icons::LIST,
        })
    }
}

/// What a key in the row meant.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Typed {
    /// Taken; nothing for the page to do.
    Consumed,
    /// Enter on the words: make the task.
    Commit,
    /// Escape: nothing typed is nothing meant.
    Cancel,
    /// A `/` row was chosen: the page opens that picker over this row.
    Open(SlashRow),
}

pub(super) struct Capture {
    pub input: TextInput,
    /// The trailing date word, re-read after every keystroke: the title
    /// before it, and what it parsed to. `None` while a due was picked, since
    /// a pick outranks a word and the word is then part of the title.
    word: Option<(String, Parsed)>,
    /// What `/due` picked.
    pub due: Option<Temporal>,
    /// What `/priority` picked.
    pub priority: Option<Priority>,
    /// What `/task_list` picked — on a list's own page it is that list unless
    /// another was chosen.
    pub list: Option<ListId>,
    /// The `/` menu, while a `/word` is being typed at the end of the field.
    pub slash: Option<Menu<SlashRow>>,
    /// Where the pickers hang from: the row's `…`, as a row's do.
    anchor: (f32, f32),
    /// Where the field was last drawn — the `/` menu opens **at the caret**,
    /// under the word being typed, and the caret's place is the field's
    /// origin plus the width of the words before it.
    field: Rect,
}

impl Capture {
    pub fn new(engine: &mut TextEngine) -> Self {
        Self {
            input: TextInput::new(engine, theme::role::row()),
            word: None,
            due: None,
            priority: None,
            list: None,
            slash: None,
            anchor: (0.0, 0.0),
            field: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Where the `/` menu opens: at the caret, when the field has been drawn;
    /// else the row's anchor.
    fn menu_anchor(&self, engine: &mut TextEngine) -> (f32, f32) {
        if self.field.w <= 0.0 {
            return self.anchor;
        }
        let value = self.value();
        let caret = self.input.caret().min(value.len());
        let before = engine
            .simple_layout(&value[..caret], theme::role::row())
            .width();
        (self.field.x + before, self.field.y + self.field.h)
    }

    /// The row with words already in it — a refused capture given back.
    pub fn with_text(engine: &mut TextEngine, text: &str, today: chrono::NaiveDate) -> Self {
        let mut capture = Self::new(engine);
        capture.input.set_text(engine, text);
        capture.input.move_to_end(engine);
        capture.reparse(today);
        capture
    }

    pub fn set_anchor(&mut self, at: (f32, f32)) {
        self.anchor = at;
    }

    pub fn value(&self) -> String {
        self.input.value()
    }

    /// The title the request would carry: what is left after the date word,
    /// or the whole line trimmed.
    pub fn title(&self) -> String {
        match &self.word {
            Some((head, _)) => head.clone(),
            None => self.value().trim().to_owned(),
        }
    }

    /// The head of the line while a date word is on it — the preview's text.
    #[cfg(test)]
    pub fn word_head(&self) -> Option<String> {
        self.word.as_ref().map(|(head, _)| head.clone())
    }

    /// The due the words say, before any page default: the pick, else the
    /// typed word.
    pub fn typed_due(&self) -> Option<Temporal> {
        self.due
            .clone()
            .or_else(|| self.word.as_ref().map(|(_, parsed)| temporal_of(*parsed)))
    }

    /// Re-read the trailing date word.
    ///
    /// **Cheap by construction**: at most three attempts at a phrase of at
    /// most three words, over a grammar that is a pure function of a string
    /// and two reference points. Not read at all under a picked due.
    pub fn reparse(&mut self, today: chrono::NaiveDate) {
        self.word = if self.due.is_some() {
            None
        } else {
            split_date(&self.value(), today)
        };
    }

    /// Whether the `/` menu is up.
    pub fn menu_open(&self) -> bool {
        self.slash.is_some()
    }

    pub fn menu_enabled(&self) -> Vec<bool> {
        self.slash
            .as_ref()
            .map(|menu| vec![true; menu.items().len()])
            .unwrap_or_default()
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        today: chrono::NaiveDate,
    ) -> Typed {
        if self.slash.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.slash = None;
                    return Typed::Consumed;
                }
                Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                    let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                        1
                    } else {
                        -1
                    };
                    let enabled = self.menu_enabled();
                    if let Some(menu) = self.slash.as_mut() {
                        menu.step(&enabled, delta);
                    }
                    return Typed::Consumed;
                }
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                    let chosen = self
                        .slash
                        .as_ref()
                        .and_then(|menu| menu.hover.and_then(|i| menu.items().get(i).copied()));
                    return match chosen {
                        Some(row) => self.choose(engine, row, today),
                        None => Typed::Consumed,
                    };
                }
                _ => {}
            }
        }
        match key {
            Key::Named(NamedKey::Enter) => Typed::Commit,
            Key::Named(NamedKey::Escape) => Typed::Cancel,
            _ => {
                self.input.key(engine, key, text, mods);
                self.reparse(today);
                self.refresh_menu(engine);
                Typed::Consumed
            }
        }
    }

    /// A press while the `/` menu is up: a row chooses, anywhere else closes
    /// the menu and does nothing more — every menu in this app.
    pub fn menu_press(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
        today: chrono::NaiveDate,
    ) -> Option<Typed> {
        let menu = self.slash.as_ref()?;
        match menu
            .item_at(viewport, x, y)
            .and_then(|i| menu.items().get(i).copied())
        {
            Some(row) => Some(self.choose(engine, row, today)),
            None => {
                self.slash = None;
                Some(Typed::Consumed)
            }
        }
    }

    pub fn menu_hover(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        let Some(menu) = self.slash.as_mut() else {
            return false;
        };
        let was = menu.hover;
        menu.hover = menu.item_at(viewport, x, y);
        was != menu.hover
    }

    /// The `/word` comes out of the field, and the page is asked to open the
    /// row's picker.
    fn choose(
        &mut self,
        engine: &mut TextEngine,
        row: SlashRow,
        today: chrono::NaiveDate,
    ) -> Typed {
        self.slash = None;
        let value = self.value();
        if let Some((start, _)) = slash_query(&value) {
            let head = value[..start].trim_end().to_owned();
            self.input.set_text(engine, &head);
            self.input.move_to_end(engine);
            self.reparse(today);
        }
        Typed::Open(row)
    }

    /// Open, narrow or close the `/` menu from what the field says now.
    fn refresh_menu(&mut self, engine: &mut TextEngine) {
        let Some((_, query)) = slash_query(&self.value()) else {
            self.slash = None;
            return;
        };
        let items: Vec<SlashRow> = SlashRow::ALL
            .into_iter()
            .filter(|row| matches(*row, &query))
            .collect();
        if items.is_empty() {
            self.slash = None;
            return;
        }
        let hover = self
            .slash
            .as_ref()
            .and_then(|menu| menu.hover.and_then(|i| menu.items().get(i).copied()))
            .and_then(|was| items.iter().position(|row| *row == was))
            .or(Some(0));
        let at = self.menu_anchor(engine);
        let mut menu = Menu::aligned(engine, at, items, Anchor::Cursor);
        menu.hover = hover;
        self.slash = Some(menu);
    }

    /// The chips the row wears: the due it would carry, the level, the list.
    pub fn chips(&self, today: chrono::NaiveDate, lists: &[ListFace]) -> Vec<RowChip> {
        let mut chips = Vec::new();
        if let Some(due) = self.typed_due() {
            chips.push(RowChip {
                kind: ChipKind::Due,
                label: koru_editor::chips::due_label(&due, today),
                accent: None,
                list: None,
            });
        }
        if let Some(level) = self.priority {
            chips.push(RowChip {
                kind: ChipKind::Priority,
                label: koru_editor::chips::priority_label(level).to_owned(),
                accent: None,
                list: None,
            });
        }
        if let Some(face) = self
            .list
            .and_then(|id| lists.iter().find(|face| face.id == id))
        {
            chips.push(RowChip {
                kind: ChipKind::Tag,
                label: face.name.clone(),
                accent: face.color.clone(),
                list: None,
            });
        }
        chips
    }

    /// Draw the row: the ghost control, the field, and the chips it wears
    /// laid in from the right so the caret never runs under them.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        table: &Table,
        r: Rect,
        parts: RowParts,
        caret_on: bool,
        today: chrono::NaiveDate,
        lists: &[ListFace],
    ) {
        let p = theme::p();
        let _ = table;
        koru_draw::checkbox::outline(scene, root, parts.checkbox, p.placeholder);
        let chips = self.chips(today, lists);
        let widths: Vec<f32> = chips
            .iter()
            .map(|chip| {
                engine.measure_attr_chip(
                    chip.kind,
                    &chip.label,
                    chip.accent.as_deref(),
                    None,
                    false,
                ) + chip::extra_w(chip.kind)
            })
            .collect();
        let total: f32 =
            widths.iter().sum::<f32>() + chip::CHIP_GAP * (widths.len().saturating_sub(1)) as f32;
        let field = Rect::new(
            parts.text.x,
            r.y,
            (parts.text.w - total - if total > 0.0 { CHIP_GAP } else { 0.0 }).max(0.0),
            r.h,
        );
        self.field = field;
        self.input.draw(
            scene,
            root,
            engine,
            field,
            InputStyle::inline(),
            Some(PLACEHOLDER),
            caret_on,
        );
        let mut x = parts.text.x + parts.text.w - total;
        let y = r.y + (r.h - chip::CHIP_H) * 0.5;
        for (chip, w) in chips.iter().zip(&widths) {
            paint_chip(
                scene,
                root,
                engine,
                chip.kind,
                &chip.label,
                chip.accent.as_deref(),
                x,
                y,
            );
            x += w + chip::CHIP_GAP;
        }
    }

    /// The `/` menu, over everything else on the row.
    pub fn paint_menu(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let enabled = self.menu_enabled();
        if let Some(menu) = &self.slash {
            menu.draw(scene, root, engine, viewport, &enabled);
        }
    }
}

/// The empty row's ghost: the outline of a control and the placeholder.
pub(super) fn paint_empty(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    table: &Table,
    r: Rect,
    parts: RowParts,
    hovered: bool,
) {
    let p = theme::p();
    if hovered {
        table.paint_row_fill(scene, root, r, RowState::Hovered);
    }
    // A dashed-looking ghost: the same box, drawn in the placeholder tone
    // so it reads as an outline of a control rather than as one.
    koru_draw::checkbox::outline(scene, root, parts.checkbox, p.placeholder);
    let layout = engine.simple_layout(PLACEHOLDER, theme::role::row());
    scene::draw_layout_first_line(
        scene,
        root,
        &layout,
        parts.text.x,
        parts.text.y + (r.h - scene::first_line_height(&layout)) * 0.5,
        Some(p.placeholder),
    );
}

/// The `/word` at the end of the field, if there is one: where the `/` is and
/// what follows it. A `/` counts only at the start or after a space, and the
/// word has no spaces — a `/` mid-sentence is punctuation, not a command.
pub(super) fn slash_query(value: &str) -> Option<(usize, String)> {
    let start = value.rfind('/')?;
    if start > 0 && !value[..start].ends_with(char::is_whitespace) {
        return None;
    }
    let query = &value[start + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some((start, query.to_owned()))
}

/// Whether a row answers to what was typed: its name or its label, by
/// prefix, case-blind — `du` finds Due, `task` finds Task list.
fn matches(row: SlashRow, query: &str) -> bool {
    let q = query.to_lowercase();
    q.is_empty() || row.name().starts_with(&q) || row.label().to_lowercase().starts_with(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_word_is_read_only_at_a_word_boundary() {
        assert_eq!(slash_query("/"), Some((0, String::new())));
        assert_eq!(slash_query("Chase it /du"), Some((9, "du".to_owned())));
        assert_eq!(slash_query("a/b"), None, "punctuation, not a command");
        assert_eq!(slash_query("/due friday"), None, "the word ended");
        assert_eq!(slash_query("Chase it"), None);
    }

    #[test]
    fn the_rows_answer_to_their_names_and_their_labels() {
        assert!(matches(SlashRow::Due, ""));
        assert!(matches(SlashRow::Due, "du"));
        assert!(matches(SlashRow::TaskList, "task"));
        assert!(matches(SlashRow::TaskList, "task_l"));
        assert!(!matches(SlashRow::Priority, "task"));
    }

    fn typed(capture: &mut Capture, engine: &mut TextEngine, s: &str) -> Typed {
        let mut last = Typed::Consumed;
        for ch in s.chars() {
            let text = ch.to_string();
            last = capture.key(
                engine,
                &Key::Character(text.clone().into()),
                Some(&text),
                ModifiersState::empty(),
                chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap(),
            );
        }
        last
    }

    /// Typing `/` opens the menu with the three rows, letters narrow it,
    /// Enter chooses and takes the `/word` out of the field.
    #[test]
    fn a_slash_opens_the_menu_and_enter_chooses_and_strips_the_word() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let mut engine = TextEngine::new();
        let mut capture = Capture::new(&mut engine);
        typed(&mut capture, &mut engine, "Chase it /");
        assert!(capture.menu_open());
        assert_eq!(capture.slash.as_ref().unwrap().items().len(), 3);
        typed(&mut capture, &mut engine, "pr");
        assert_eq!(
            capture.slash.as_ref().unwrap().items(),
            &[SlashRow::Priority]
        );
        let chosen = capture.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
            today,
        );
        assert_eq!(chosen, Typed::Open(SlashRow::Priority));
        assert!(!capture.menu_open());
        assert_eq!(capture.value(), "Chase it", "the word came out");
        assert_eq!(capture.title(), "Chase it");

        // A `/` nothing answers to is just a `/`: no menu, and Enter commits.
        typed(&mut capture, &mut engine, " /zzz");
        assert!(!capture.menu_open());
        assert_eq!(
            capture.key(
                &mut engine,
                &Key::Named(NamedKey::Enter),
                None,
                ModifiersState::empty(),
                today
            ),
            Typed::Commit
        );
    }

    /// A picked due outranks the typed word, and the word is then title.
    #[test]
    fn a_picked_due_outranks_the_typed_word() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let mut engine = TextEngine::new();
        let mut capture = Capture::new(&mut engine);
        typed(&mut capture, &mut engine, "Chase it tomorrow");
        assert_eq!(capture.title(), "Chase it");
        assert!(capture.typed_due().is_some());
        capture.due = Some(Temporal::Date(
            koru_core::time::DateOnly::parse("2026-09-20").unwrap(),
        ));
        capture.reparse(today);
        assert_eq!(capture.title(), "Chase it tomorrow");
        assert_eq!(
            capture.typed_due(),
            Some(Temporal::Date(
                koru_core::time::DateOnly::parse("2026-09-20").unwrap()
            ))
        );
        assert_eq!(capture.chips(today, &[]).len(), 1);
        capture.priority = Some(Priority::High);
        assert_eq!(
            capture.chips(today, &[]).len(),
            2,
            "the level rides beside it"
        );
    }
}
