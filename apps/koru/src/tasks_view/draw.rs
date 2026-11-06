// SPDX-License-Identifier: MPL-2.0

//! What the Tasks tab puts on screen.
//!
//! Separate from the geometry for the reason the Trash tab splits them: none of
//! it decides anything, and every rectangle it draws into comes from
//! [`TasksView::parts`] in `view.rs`, so a control and its hit test cannot
//! drift.
//!
//! **Every chip is the one chip component**, `koru_ui::label_chip`
//! (KoruLabelChip) — the row's attribute chips, the filter chips under the
//! heading and the *Updating…* status alike. A row's chip is a **reading**:
//! compact, and with no remove lane, because the control that changes it is
//! the row's menu. A filter chip is a **control**: regular, inverted while
//! its dimension is set, and wearing the caret or the `×` that pressing it
//! answers to.

use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use koru_ui::table::RowState;
use vello::Scene;
use vello::kurbo::{Affine, Circle};

use super::pickers::Picker;
use super::query::TaskEntry;
use super::view::{CONTROL_W, Hit, Line, PAD, TasksView};

/// The page heading. **Medium, not semibold** — the size already says it is the
/// title, and weight on top of that is a second answer to a question the type
/// scale has answered. The Trash tab's, so the two pages read as one app. It is
/// `role::subtitle()`, which the heading's elements name as `roles::SUBTITLE`.
pub(super) const TITLE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::LG, theme::FontWeight::Medium);

/// What the freshness chip says. **A state, not a verb with a subject**: which
/// workspace is behind is not a thing a reader of this page can act on, and
/// naming one would turn an ambient note into a report.
const UPDATING: &str = "Updating\u{2026}";
/// The empty state's dot — the same mark the rail's own quiet indicators use.
const DOT_D: f32 = 5.0;
const DOT_GAP: f32 = 6.0;
/// Between the freshness chip and the `…` beside it.
const FRESHNESS_GAP: f32 = 10.0;

impl TasksView {
    /// The whole page, with no note — what the benchmarks draw.
    #[cfg(test)]
    pub(super) fn paint(&mut self, scene: &mut Scene, root: Affine, caret_on: bool) {
        self.paint_under(scene, root);
        self.paint_over(scene, root, caret_on);
    }

    /// The sheet and the heading: everything under the note's band.
    pub(super) fn paint_under(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        // `bg`, not `canvas`: this is a **sheet** you read a list on, the same
        // surface the Trash tab and the workspace manager use.
        scene::fill(
            scene,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        self.paint_header(scene, root);
    }

    /// The rows and every panel over them — over the note's band, so a menu
    /// opened from a row is above the note too.
    pub(super) fn paint_over(&mut self, scene: &mut Scene, root: Affine, caret_on: bool) {
        self.paint_lines(scene, root, caret_on);
        self.paint_heading_rename(scene, root, caret_on);
        self.paint_overflow(scene, root);
        self.paint_picker(scene, root);
        // The capture row's `/` menu, over everything but a picker it opened.
        if self.picker.is_none()
            && let Some(capture) = self.capture.as_mut()
        {
            capture.paint_menu(scene, root, &mut self.engine, self.viewport);
        }
    }

    /// The overflow menu, last and therefore over everything.
    fn paint_overflow(&mut self, scene: &mut Scene, root: Affine) {
        let enabled = self.overflow_enabled();
        let viewport = self.viewport;
        let engine = &mut self.engine;
        if let Some(menu) = &self.overflow {
            menu.draw(scene, root, engine, viewport, &enabled);
        }
    }

    /// The picker over a row, whichever of the three it is. Only one panel is
    /// ever up, so the order between it and the overflow is bookkeeping rather
    /// than a decision.
    ///
    /// The calendar's frame is the one `scene::floating_panel` call every
    /// panel at that elevation gets, and the widget draws its three parts
    /// inside it — a widget that drew its own frame would be the second
    /// recipe (`DESIGN.md`).
    fn paint_picker(&mut self, scene: &mut Scene, root: Affine) {
        let enabled = self.picker_enabled();
        let viewport = self.viewport;
        let engine = &mut self.engine;
        match &mut self.picker {
            Some(Picker::List(picker)) => {
                picker.menu.draw(scene, root, engine, viewport, &enabled);
            }
            Some(Picker::Priority(picker)) => {
                picker.menu.draw(scene, root, engine, viewport, &enabled);
            }
            Some(Picker::Color(picker)) => {
                picker.menu.draw(scene, root, engine, viewport, &enabled);
            }
            Some(Picker::Due(pick)) => {
                let r = pick.panel;
                scene::floating_panel(scene, root, r.x, r.y, r.w, r.h, &scene::Panel::popover(8.0));
                koru_ui::date_grid::draw(
                    scene,
                    root,
                    engine,
                    &mut pick.grid,
                    r.x,
                    r.y,
                    r.w,
                    pick.clear_hovered,
                );
            }
            None => {}
        }
    }

    fn paint_header(&mut self, scene: &mut Scene, root: Affine) {
        // **A list wears its colour square here too** (`1d`): same band, same
        // swatch, same accent token as the rail's row — a list is a bucket,
        // and the two surfaces are showing the one thing. The heading is one
        // element row (`heading.rs`): the swatch, the title, the date and the
        // count.
        self.paint_heading(scene, root);
        self.paint_freshness(scene, root);
        self.paint_header_dots(scene, root);
    }

    /// The **Updating…** chip.
    ///
    /// The one chip component, regular and neutral with the status dot —
    /// `chip_bg` and `text_dim`, at the filter chips' size and in their register:
    /// this is *ambient*, not an alarm. The store has answered — these are the
    /// rows — and it is saying that one of the workspaces behind them may not
    /// have finished indexing. So there is **no spinner and no veil**: the list
    /// underneath stays exactly as readable as it was, which is the design's own
    /// sentence about it. The inverted surface is reserved for the offline chip,
    /// where something is actually wrong.
    fn paint_freshness(&mut self, scene: &mut Scene, root: Affine) {
        if !self.panel.updating() {
            return;
        }
        use koru_ui::label_chip::{Lead, Size};
        let chip = koru_ui::LabelChip::new(UPDATING)
            .size(Size::Regular)
            .lead(Lead::Dot);
        let (w, h) = (chip.width(&mut self.engine), chip.height());
        // Left of the `…`, on the heading's own line.
        let r = Rect::new(
            self.header_dots().x - FRESHNESS_GAP - w,
            self.title_top() + (super::draw::TITLE.metrics.line_height - h) * 0.5,
            w,
            h,
        );
        chip.draw(
            scene,
            root,
            &mut self.engine,
            r,
            koru_ui::label_chip::State::default(),
        );
    }

    /// The `…`. Always there, unlike a row's — this is the page's own control.
    fn paint_header_dots(&mut self, scene: &mut Scene, root: Affine) {
        // **The icon button every `…` in the app is**, with `DOTS_THREE`. The
        // hover state is the whole affordance (ADR-215): the cursor does not
        // change over a control here, so it stays lit while its menu is open.
        let lit = self.hover == Some(Hit::HeaderDots) || self.overflow.is_some();
        let dots = self.header_dots();
        koru_ui::icon_button::draw_with(
            scene,
            root,
            &mut self.engine,
            dots,
            koru_draw::icons::DOTS_THREE,
            koru_ui::icon_button::State {
                hovered: lit,
                active: false,
                disabled: false,
            },
            koru_ui::icon_button::Metrics::INLINE,
        );
    }

    fn paint_lines(&mut self, scene: &mut Scene, root: Affine, caret_on: bool) {
        let t = self.table();
        let bands = self.bands();
        // **The list, as the inspector's column of rows.** Its rows are the
        // table's bands — the capture row, the rules and the task rows, each
        // a `Task row` element tree (`row.rs`) — so the list is recorded as
        // the container it is, over the band area it scrolls in.
        let _list = koru_draw::inspect::scope(
            super::row::LIST,
            "container",
            root,
            scene::rect(
                0.0,
                self.rows_top(),
                self.viewport.0,
                (self.viewport.1 - self.header_h).max(0.0),
            ),
            |record| {
                record.property("implementation", "koru_ui::table::Table", "row geometry");
                record.property("rows", self.lines.len(), "the page's lines");
            },
        );
        for i in t.visible(&bands) {
            let Some(line) = self.lines.get(i).cloned() else {
                continue;
            };
            let r = t.row_rect(&bands, i);
            match line {
                Line::Capture => self.paint_capture(scene, root, r, caret_on),
                Line::Rule(label) => {
                    // **A rule with a label, not a filled band** — the design's
                    // words: headers stay quieter than the rows they hold.
                    t.paint_section(scene, root, &mut self.engine, r, &label);
                    let y = (r.y + r.h - 1.0).round();
                    let x = r.x + PAD;
                    scene::fill(
                        scene,
                        root,
                        theme::p().panel_border,
                        &scene::rect(x, y, (r.w - PAD * 2.0).max(0.0), 1.0),
                    );
                }
                Line::Task(idx) => {
                    let Some(row) = self.rows.get(idx).cloned() else {
                        continue;
                    };
                    self.paint_row(scene, root, r, idx, &row, caret_on);
                }
            }
        }
        self.paint_drop_line(scene, root);
        // **Not gated on `known` here.** Which states may appear before the
        // first answer is `empty_state`'s decision — and it was the wrong
        // decision in this line: a query that *failed* leaves a notice with
        // `known` still false, so the one state a person could act on was the
        // one this gate hid.
        if self.rows.is_empty() {
            self.paint_empty(scene, root);
        }
    }

    /// Where a drag would put the rows: one accent line across the gap.
    fn paint_drop_line(&mut self, scene: &mut Scene, root: Affine) {
        let Some(gap) = self.drag.as_ref().filter(|d| d.active).and_then(|d| d.gap) else {
            return;
        };
        let Some(y) = self.drop_line_y(gap) else {
            return;
        };
        let m = self.table().metrics;
        let (x0, w) = (m.pad, (self.viewport.0 - m.pad * 2.0).max(0.0));
        let line = koru_draw::geometry::Rect::new(x0, (y - 1.0).round(), w, 2.0);
        let _record = koru_draw::inspect::scope(
            "Drop indicator",
            "part",
            root,
            scene::rect(line.x, line.y, line.w, line.h),
            |_| {},
        );
        scene::fill(
            scene,
            root,
            theme::p().accent,
            &scene::rrect(line.x, line.y, line.w, line.h, 1.0),
        );
    }

    /// The ghost row at the top of the list.
    ///
    /// Idle it is a hollow circle and a placeholder; active it is a field with
    /// the parsed date previewed as the due chip beside it, **before** the task
    /// is committed — which is the whole of what 1c asks for.
    fn paint_capture(&mut self, scene: &mut Scene, root: Affine, r: Rect, caret_on: bool) {
        let parts = self.parts(r);
        let table = self.table();
        let today = self.today;
        match self.capture.as_mut() {
            None => super::capture::paint_empty(
                scene,
                root,
                &mut self.engine,
                &mut self.idle_input,
                &table,
                r,
                parts,
                self.hover == Some(Hit::Capture),
            ),
            Some(capture) => capture.paint(
                scene,
                root,
                &mut self.engine,
                &table,
                r,
                parts,
                caret_on,
                today,
                &self.lists,
            ),
        }
    }

    fn paint_row(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        r: Rect,
        idx: usize,
        row: &TaskEntry,
        caret_on: bool,
    ) {
        let renaming = self
            .rename
            .as_ref()
            .is_some_and(|rename| (rename.document, rename.node) == (row.document, row.node));
        // The whole row counts as hovered when any of its controls is, or the
        // fill would drop away the moment the pointer reached the `…`.
        let on_row = matches!(
            self.hover,
            Some(
                Hit::Row(j) | Hit::Complete(j) | Hit::Dots(j) | Hit::ListChip(j) | Hit::ValueChip(j, _)
            ) if j == idx
        );
        // **Selected outranks hovered**, which is the table's own rule — one
        // fill, chosen here rather than two drawn over each other.
        let state = if self.is_selected(idx) {
            RowState::Selected
        } else if on_row {
            RowState::Hovered
        } else {
            RowState::Idle
        };
        // **What the row says is decided here; how it is drawn is `row`'s.**
        // The chips are `row_chips`, one list asserted in the suite, so a
        // chip cannot be drawn that the view does not admit to; where they
        // are is the row's layout, which `row_chip_rects` reads back for the
        // press to be resolved against.
        let chips = self.row_chips(row);
        let chip_widths = self.row_chip_ws.get(idx).cloned().unwrap_or_default();
        let context = self.context_of(row);
        let face = super::row::RowFace {
            completed: row.completed,
            residue: row.residue,
            glyph: self.row_glyph(row),
            emoji: row.icon.as_deref(),
            // The rename field stands where the words were; the row draws
            // none of its own under it.
            title: if renaming { "" } else { &row.title },
            context: if renaming { "" } else { &context },
            chips: &chips,
            chip_widths: &chip_widths,
            state,
            dots: on_row.then(|| self.hover == Some(Hit::Dots(idx))),
        };
        let table = self.table();
        super::row::paint(scene, root, &mut self.engine, &table, r, &face);
        if renaming
            && let Some(field) = self.rename_field()
            && let Some(rename) = self.rename.as_mut()
        {
            rename.input.draw(
                scene,
                root,
                &mut self.engine,
                super::capture::input_rect(field),
                koru_editor::components::text_input::InputStyle::bare(theme::role::row()),
                None,
                caret_on,
            );
        }
        // The landing light, over the row and fading: the document's search
        // flash, on the page — so *View task* says where the task is.
        if let Some(alpha) = self.flash_alpha(row.document, row.node) {
            let m = table.metrics;
            let inset = m.pad - m.row_pad_x;
            scene::fill(
                scene,
                root,
                theme::p().highlights[0].1.multiply_alpha(alpha),
                &scene::rrect(
                    r.x + inset,
                    r.y + m.row_pad_y,
                    (r.w - inset * 2.0).max(0.0),
                    (r.h - m.row_pad_y * 2.0).max(0.0),
                    m.row_radius,
                ),
            );
        }
    }

    /// What follows the row's title, quieter: the document path.
    ///
    /// **Nothing for a task typed into this page.** It used to say *added
    /// here, no document* — and the list it went into — which the owner asked
    /// to take off (round nine): the missing page glyph already says the row
    /// has no origin, the list is on its chip, and a sentence about an
    /// absence on every such row was the loudest thing on the page.
    pub(super) fn context_of(&self, row: &TaskEntry) -> String {
        if self.task_documents.contains(&row.document) {
            return String::new();
        }
        row.breadcrumb.clone()
    }

    /// The one thing the table cannot say: that there is nothing to say.
    ///
    /// **One row on the chip shape**, whichever of the two states it is: the
    /// fact at the text column, and — for an empty bucket alone — the thing to
    /// do about it at the right end. Nothing illustrated, nothing centred in
    /// dead space; the row sits where the first task would be, so the page still
    /// reads as a list.
    ///
    /// Which state this is comes from [`TasksView::empty_state`] and nowhere
    /// else, so what is *drawn* and what is *pressable* cannot disagree about
    /// whether there is an action.
    fn paint_empty(&mut self, scene: &mut Scene, root: Affine) {
        let Some(state) = self.empty_state() else {
            return;
        };
        let p = theme::p();
        let chip = self.empty_chip();
        scene::fill(
            scene,
            root,
            p.chip_bg,
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, self.empty_radius()),
        );

        // The text column, not the row's left edge: the sentence lines up with
        // the titles above it rather than sitting where a control would.
        let mut x = PAD + CONTROL_W + super::view::CONTROL_GAP;
        // The cold state alone wears the quiet dot every ambient indicator in
        // this window uses — it is the only one saying *still working*.
        if state.dotted() {
            let cy = (chip.y + chip.h * 0.5) as f64;
            scene::fill(
                scene,
                root,
                p.text_dim,
                &Circle::new(((x + DOT_D * 0.5) as f64, cy), (DOT_D * 0.5) as f64),
            );
            x += DOT_D + DOT_GAP;
        }

        // **One run.** The teaching state was the only one that needed more —
        // its middle run was `/task` in a lozenge — and it went with the state,
        // taking the run loop and the lozenge's own height with it.
        let layout = self.engine.simple_layout(&state.line(), theme::role::row());
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            x,
            scene::centre_y_in(&layout, chip),
            Some(p.text_dim),
        );
    }
}

/// The glyph in a chip's trailing lane: a caret on a selector, an `×` on a
/// filter that is switched on, nothing on one that is not.
///
/// A list's colour square.
///
/// **The rail's swatch, at this surface's size** — a square rather than a dot,
/// because a list is a bucket and the round swatch already means *this node's
/// colour*, and an accent *token* rather than an RGB value, so a colour chosen
/// in one theme is read in the other. An unknown or absent token draws the
/// neutral square the rail draws, which is the honest reading of a list whose
/// colour the table has forgotten.
///
/// **The recipe is shared and the eight lines are not.** What could drift is
/// the *colour*, and that comes from one place — `theme::accent_tone`, keyed by
/// the token both surfaces are handed. The rail's own body is private to
/// `panes::explore::rail`, at a size fixed to a nav row's icon lane; making it
/// public would spend a name in that module so this page could pass a different number
/// to it.
pub(super) fn draw_swatch(scene: &mut Scene, root: Affine, r: Rect, token: Option<&str>) {
    let colour = token
        .and_then(theme::accent_tone)
        .map(|tone| tone.swatch)
        .unwrap_or(theme::p().text_dim);
    scene::fill(
        scene,
        root,
        colour,
        &scene::rrect(r.x, r.y, r.w, r.h, r.w * 0.25),
    );
}
