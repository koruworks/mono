// SPDX-License-Identifier: MPL-2.0

//! The three pickers the Tasks page hosts over a row, and the one adapter
//! from what a widget answered to what the page asks for.
//!
//! **One field, three panels.** The page holds `Option<Picker>` rather than
//! three `Option`s, so *two pickers open at once* is not a state the code can
//! be in and no site that reads the field has to decide what it would mean.
//! Every one of them is anchored on the row's `…`, closes on a choice, and
//! leaves as a [`Request`] — the page writes nothing itself; the container
//! turns the request into the ordinary task command, which is the one door a
//! Tasks surface has to the store.
//!
//! **The calendar is the shared widget.** [`DateGrid`] is the same month,
//! phrase box and Clear row the editor's `/due` shows and the search surface
//! hosts; what this module adds is the four-arm adapter from its [`Pick`] to a
//! request, and the seeds — which day it opens on, whether Clear is offered —
//! read off the row, by the editor's own rule for its picker. The priority
//! rows are the editor's tables, imported: the semantic order is a decision
//! (D6) and a second list of four strings would be a second place to get it
//! wrong.

use std::borrow::Cow;

use chrono::NaiveDate;
use koru_core::attributes::Priority;
use koru_core::time::{DateOnly, Temporal};
use koru_core::{DocumentId, NodeId};
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_editor::components::priority_picker::{LABELS, LEVELS};
use koru_editor::utils::when;
use koru_ui::date_grid::{DateGrid, Pick};
use koru_ui::menu::{Menu, MenuItem};

use super::menu::ListPick;
use super::view::Request;

/// The panel that is up over a row, if any. Exactly one.
pub(super) enum Picker {
    List(ListPicker),
    Priority(PriorityPick),
    Due(DuePick),
    Color(ColorPicker),
}

/// The colour picker, and the rows it decides for.
pub(super) struct ColorPicker {
    pub target: Target,
    pub menu: Menu<super::menu::ColorPick>,
}

/// The membership picker, and the row it belongs to.
///
/// The ids ride with the panel rather than being looked up again when a row is
/// chosen: the page underneath refreshes every half minute, and a picker that
/// resolved its task at *choice* time could write into whichever row had slid
/// into that position. The same rule holds for the other two.
pub(super) struct ListPicker {
    pub target: Target,
    pub menu: Menu<ListPick>,
}

/// What a picker is deciding for: rows on the page, whose change leaves as
/// a request, or the capture row, whose pick rides on the words until Enter.
///
/// **Rows, not a row.** One task is a set of one; a Ctrl/Shift selection is
/// the same picker deciding for every row in it, so the one-row and the
/// many-row paths are one path and cannot come to differ.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Target {
    Rows(Vec<(DocumentId, NodeId)>),
    Capture,
}

impl Target {
    #[cfg(test)]
    pub fn row(document: DocumentId, node: NodeId) -> Self {
        Self::Rows(vec![(document, node)])
    }

    /// The request a pick makes for these rows: `each` for one row, or one
    /// for every row, left as one [`Request::Many`] so the gesture is one.
    /// `None` for the capture row, whose pick is worn rather than sent.
    pub fn request(&self, each: impl Fn(DocumentId, NodeId) -> Request) -> Option<Request> {
        match self {
            Self::Rows(rows) if rows.len() == 1 => Some(each(rows[0].0, rows[0].1)),
            Self::Rows(rows) => Some(Request::Many(
                rows.iter()
                    .map(|&(document, node)| each(document, node))
                    .collect(),
            )),
            Self::Capture => None,
        }
    }
}

/// A pick made for the capture row, handed back to the page to wear.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Picked {
    Due(Option<Temporal>),
    Priority(Option<Priority>),
}

/// The priority picker: four levels in the scale's own order, the current one
/// checked, and Clear while one is set.
pub(super) struct PriorityPick {
    pub target: Target,
    pub menu: Menu<PriorityRow>,
}

/// One row of the priority picker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum PriorityRow {
    Level {
        level: Priority,
        checked: bool,
    },
    Divider,
    /// Take the priority off. Offered only while one is set — clearing nothing
    /// is not an offer, the rule the editor's picker and the Clear row on the
    /// calendar both follow.
    Clear,
}

/// The rows, from the editor's tables: `LEVELS` in the semantic order the
/// scale is read in, then a divider and Clear when there is something to clear.
pub(super) fn priority_rows(current: Option<Priority>) -> Vec<PriorityRow> {
    let mut rows: Vec<PriorityRow> = LEVELS
        .iter()
        .map(|level| PriorityRow::Level {
            level: *level,
            checked: current == Some(*level),
        })
        .collect();
    if current.is_some() {
        rows.push(PriorityRow::Divider);
        rows.push(PriorityRow::Clear);
    }
    rows
}

impl MenuItem for PriorityRow {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Level { level, .. } => Cow::Borrowed(
                LEVELS
                    .iter()
                    .position(|l| l == level)
                    .map(|i| LABELS[i])
                    .unwrap_or(""),
            ),
            Self::Divider => Cow::Borrowed(""),
            Self::Clear => Cow::Borrowed("Clear"),
        }
    }

    fn checked(&self) -> Option<bool> {
        match self {
            Self::Level { checked, .. } => Some(*checked),
            _ => None,
        }
    }

    /// The editor's priority picker's glyphs: a flag per level, Clear's
    /// eraser — so the two pickers are one picker in two places.
    fn icon(&self) -> Option<char> {
        match self {
            Self::Level { .. } => Some(koru_draw::icons::FLAG),
            Self::Clear => Some(koru_draw::icons::CLEAR),
            Self::Divider => None,
        }
    }

    /// The flag in its level's colour, as on the chip.
    fn icon_accent(&self) -> Option<&str> {
        match self {
            Self::Level { level, .. } => Some(koru_editor::chips::priority_accent(*level)),
            _ => None,
        }
    }

    fn divider(&self) -> bool {
        matches!(self, Self::Divider)
    }

    fn selectable(&self) -> bool {
        !matches!(self, Self::Divider)
    }

    fn height(&self) -> f32 {
        match self {
            Self::Divider => 8.0,
            _ => theme::ROW_H,
        }
    }
}

/// What a press or a key on a picker decided.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Choice {
    /// Something to ask the container for; the panel closes.
    Request(Request),
    /// Something for the capture row to wear; the panel closes.
    Picked(Picked),
    /// Nothing to ask; the panel closes.
    Close,
    /// Nothing to ask; the panel stays — a refine landed, a phrase was left
    /// in the box, a key was swallowed.
    Stay,
}

impl PriorityPick {
    /// Choose row `row`. Choosing the level it already has is a no-op, not a
    /// second write — the guarded set would answer the same thing, and not
    /// asking is cheaper than being told.
    pub fn choose(&self, row: usize) -> Choice {
        let priority = match self.menu.items().get(row) {
            Some(PriorityRow::Level { checked: true, .. }) => return Choice::Close,
            Some(PriorityRow::Level { level, .. }) => Some(*level),
            Some(PriorityRow::Clear) => None,
            Some(PriorityRow::Divider) | None => return Choice::Stay,
        };
        match self.target.request(|document, node| Request::SetPriority {
            document,
            node,
            priority,
        }) {
            Some(request) => Choice::Request(request),
            None => Choice::Picked(Picked::Priority(priority)),
        }
    }
}

/// The calendar over a row: the shared grid, and where the page put it.
pub(super) struct DuePick {
    pub target: Target,
    pub grid: DateGrid,
    /// The panel's rectangle in the view's coordinates — what the press and
    /// the drawing both read, so a cell is clickable exactly where it is
    /// drawn.
    pub panel: Rect,
    /// Whether the pointer is over the Clear row, for the row's hover fill.
    pub clear_hovered: bool,
}

/// What the calendar opens on, by the editor's own rule for its picker
/// (`open_due_chooser_seeded`): the row's due day when it has one — which is
/// also what puts the Clear row on — else today. A task due in November opens
/// on November, and `+2w` in the phrase box counts from the day the panel
/// shows.
pub(super) fn due_seeds(due_day: Option<NaiveDate>, today: NaiveDate) -> (NaiveDate, bool) {
    match due_day {
        Some(day) => (day, true),
        None => (today, false),
    }
}

impl DuePick {
    /// Where the panel goes: hanging under the row's `…`, right edge aligned
    /// to it, the way the two menus are — and kept inside the viewport, above
    /// the row when there is no room below it.
    pub fn place(dots: Rect, size: (f32, f32), viewport: (f32, f32)) -> Rect {
        Self::fit(dots.x + dots.w - size.0, dots, size, viewport)
    }

    /// Where the panel goes under the words that opened it — the capture
    /// row's `/due`: its **left** edge at `at`, the `/` that was typed, so the
    /// calendar opens where the eye already is rather than at the far end of
    /// the row. Kept inside the viewport the same way.
    pub fn place_under(at: Rect, size: (f32, f32), viewport: (f32, f32)) -> Rect {
        Self::fit(at.x, at, size, viewport)
    }

    /// A panel of `size` starting at `x`, below `anchor` when it fits and
    /// above it otherwise, inside the viewport.
    fn fit(x: f32, anchor: Rect, (w, h): (f32, f32), viewport: (f32, f32)) -> Rect {
        const GAP: f32 = 4.0;
        let pad = theme::PANEL_PAD;
        let dots = anchor;
        let mut x = x;
        if x + w > viewport.0 - pad {
            x = viewport.0 - pad - w;
        }
        if x < pad {
            x = pad;
        }
        let below = dots.y + dots.h + GAP;
        let y = if below + h <= viewport.1 - pad {
            below
        } else {
            let above = dots.y - GAP - h;
            if above >= pad {
                above
            } else {
                (viewport.1 - pad - h).max(pad)
            }
        };
        Rect::new(x, y, w, h)
    }

    /// The grid's answer, in the page's vocabulary.
    pub fn resolve(&self, pick: Pick) -> Choice {
        resolve_pick(&self.target, &self.grid, pick)
    }
}

/// **The whole adapter: four arms.** A day stores a calendar date, as the
/// document's own picker stores one — a task due "Thursday 17:00" picked to
/// Friday is due Friday with no time (D8). A timed phrase is parsed against
/// **this grid's** reference, selected day included, and converted by the one
/// function every surface uses, so the same words in the same panel store
/// what the editor stores. Clear is the same request with no due — one
/// dimension, both directions. Nothing is nothing, and the panel stays.
pub(super) fn resolve_pick(target: &Target, grid: &DateGrid, pick: Pick) -> Choice {
    let due = match pick {
        Pick::None => return Choice::Stay,
        Pick::Day(day) => Some(Temporal::Date(DateOnly::new(day))),
        Pick::Timed(phrase) => {
            match when::parse(&phrase, grid.reference(), when::DEFAULT_DATE_ORDER) {
                Some(parsed) => Some(when::to_temporal(parsed)),
                // The grid only hands up a phrase it has parsed against this
                // same reference, so this arm is unreachable in practice; if
                // it were reached, losing the typed words would be the wrong
                // answer, and the panel keeps them.
                None => return Choice::Stay,
            }
        }
        Pick::Clear => None,
    };
    match target.request(|document, node| Request::SetDue {
        document,
        node,
        due: due.clone(),
    }) {
        Some(request) => Choice::Request(request),
        None => Choice::Picked(Picked::Due(due)),
    }
}
