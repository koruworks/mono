// SPDX-License-Identifier: MPL-2.0

//! A month grid, a phrase box over it, and a Clear row under it.
//!
//! Its geometry lives here and is shared by the drawing and by [`cell_at`], so
//! a day is clickable exactly where it is drawn — the rule in `DESIGN.md`. The
//! test that walks every cell back to its own date exists because the grid once
//! had two copies of this arithmetic.
//!
//! **It answers a day, and nothing else.** [`Pick`] carries a `NaiveDate`, a
//! phrase the grid could not show back to you, or the fact that Clear was
//! asked for; what a day *means* — a `koru.due` attribute, an inline date
//! pill, a search bound — belongs to whoever opened the panel
//! ([ADR-196]). That is why this crate can hold it at all: it names no node,
//! no attribute and no outcome, and its only dependency beyond `koru-draw`
//! is [`koru_when`], itself a leaf over `chrono`.
//!
//! **The box and the grid are one instrument** ([ADR-214]). A phrase naming a
//! *day* does not commit: it **moves the grid's selection** and leaves the
//! panel open, so `2w`, then `+3d`, then `-1w` compose against what is on
//! screen — the sign in `koru-when`'s grammar binds to the selection, which is
//! only meaningful if the selection is the thing the box moves. A phrase naming
//! a *time* is handed up verbatim, because the grid has no cell for an hour and
//! refining to its day would throw away the clock that was just typed.
//!
//! [ADR-214]: ../../../../docs/decisions/ADR-201-225.md#adr-214-whenparser--natural-language-dates-are-their-own-crate-with-two-reference-points-and-a-slash-order-that-is-a-parameter
//! [ADR-196]: ../../../../docs/decisions/ADR-201-225.md#adr-219-dategrid--the-calendar-is-a-widget-in-koru-ui-what-a-day-means-stays-with-the-editor

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::scene::{draw_layout, draw_row_bg, fill, rrect, stroke};
use koru_draw::text::TextEngine;
use koru_draw::theme;

use crate::dates;
use crate::text_field::{self, TextField};

pub const CELL: f32 = 30.0;
pub const PANEL_W: f32 = CELL * 7.0 + theme::PANEL_PAD * 2.0;
pub const HEADER_H: f32 = 26.0;
pub const WEEKDAY_H: f32 = 20.0;
pub const GRID_H: f32 = WEEKDAY_H + CELL * 6.0;

/// The phrase field's rect inside a panel `panel_w` wide — the box every other
/// panel wears, reused rather than measured again, so the two boxes are one
/// shape and one set of numbers.
pub fn field_rect(panel_w: f32) -> Rect {
    text_field::search_rect(panel_w)
}

/// What the field's band costs above the grid, for a grid that has one.
pub fn field_band() -> f32 {
    text_field::search_band()
}

/// What the grid has to say back to whoever opened it.
///
/// Four answers, and the split is by **what the grid can hold**: a day it can
/// show, a clock it cannot, a clear, and nothing at all.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pick {
    /// Nothing for the host to do. The panel stays open — a refine landed, a
    /// phrase the grammar declined was left in the box, or the key was stray
    /// typing that must not reach whatever is behind the panel.
    None,
    /// A day was named, by a cell click or by Enter on the selection.
    Day(NaiveDate),
    /// A phrase carrying a **time**, handed up as typed.
    ///
    /// The grid has no cell for an hour, so refining to its day would throw
    /// away the clock that was just typed — a silent loss of the one thing the
    /// box exists to say. It travels as text rather than as a parsed value
    /// because the zone seam that turns a wall clock into a shared deadline
    /// belongs to the host, not to a widget.
    Timed(String),
    /// The Clear row was asked for — by a click, by `Alt+C`, or by
    /// Delete/Backspace in an empty box.
    Clear,
}

/// A month grid, optionally wearing a phrase box and a Clear row.
pub struct DateGrid {
    /// The day the grid is highlighting, and the month it is showing.
    pub selected: NaiveDate,
    /// The phrase box above the grid. `None` for a bare grid, whose whole
    /// vocabulary *is* a day.
    pub query: Option<TextField>,
    /// The clock a typed phrase is resolved against, **injected** — the parser
    /// never reads system time, and neither does this. It is the same clock
    /// the caller used to decide `selected`.
    pub now: NaiveDateTime,
    /// Whether the panel ends in a Clear row. Clearing nothing is not an offer,
    /// so the host sets this from whether there is anything to clear.
    pub offers_clear: bool,
}

impl DateGrid {
    /// A bare grid: no field, no Clear row. Enter and a cell click are the only
    /// two gestures, and both answer [`Pick::Day`].
    pub fn bare(selected: NaiveDate) -> Self {
        Self {
            selected,
            query: None,
            now: selected.and_time(chrono::NaiveTime::MIN),
            offers_clear: false,
        }
    }

    /// A grid with the phrase box above it, resolved against `now`.
    pub fn with_phrase_field(
        engine: &mut TextEngine,
        selected: NaiveDate,
        now: NaiveDateTime,
        offers_clear: bool,
    ) -> Self {
        Self {
            selected,
            query: Some(TextField::new(engine, theme::role::ui())),
            now,
            offers_clear,
        }
    }

    /// Whether the panel carries the phrase field.
    pub fn has_field(&self) -> bool {
        self.query.is_some()
    }

    /// The band the field costs above the grid — zero without one, so every
    /// number below is the same arithmetic in both modes.
    pub fn band(&self) -> f32 {
        if self.has_field() { field_band() } else { 0.0 }
    }

    pub fn size(&self) -> (f32, f32) {
        let clear = if self.offers_clear { theme::ROW_H } else { 0.0 };
        (
            PANEL_W,
            theme::PANEL_PAD * 2.0 + self.band() + HEADER_H + GRID_H + clear,
        )
    }

    /// The Clear row's top, in panel-local y — one number for drawing and
    /// hit-testing.
    pub fn clear_row_top(&self) -> Option<f32> {
        self.offers_clear
            .then(|| theme::PANEL_PAD + self.band() + HEADER_H + GRID_H)
    }

    /// The day under a panel-local point, with the field's band taken off —
    /// so a cell is clickable exactly where it is drawn whether or not the
    /// panel wears a field.
    pub fn cell_at_local(&self, local_x: f32, local_y: f32) -> Option<NaiveDate> {
        cell_at(self.selected, local_x, local_y - self.band())
    }

    /// The trimmed phrase in the field, when there is one and it is not blank.
    pub fn phrase(&self) -> Option<String> {
        let text = self.query.as_ref()?.value();
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// The two reference points a typed phrase is read against: the injected
    /// clock, and **the day the grid is highlighting**.
    ///
    /// That second one is the whole reason `+2w` is worth saying — it means
    /// *two weeks from what I am looking at*, and any other reading would let
    /// the box and the grid beside it disagree about what is on screen
    /// ([ADR-214] §3). [`Reference::on_day`] reads the selected day at the
    /// current time of day, so `+4h` on a selection sitting on today is exactly
    /// what `4h` means.
    ///
    /// [ADR-214]: ../../../../docs/decisions/ADR-201-225.md#adr-214-whenparser--natural-language-dates-are-their-own-crate-with-two-reference-points-and-a-slash-order-that-is-a-parameter
    /// [`Reference::on_day`]: koru_when::Reference::on_day
    pub fn reference(&self) -> koru_when::Reference {
        koru_when::Reference::on_day(self.now, self.selected)
    }

    /// Whether this press is the Clear row's accelerator, **Alt+C**.
    ///
    /// It exists because the phrase field swallows plain keys: Delete and
    /// Backspace clear only while the box is *empty*, so a person halfway
    /// through typing `friday 17:` had no key that reached Clear at all, and
    /// that row is the last thing in the panel a caret can never get to. `C` is
    /// the row's own first letter, drawn underlined, which is the only mnemonic
    /// this app advertises anywhere.
    ///
    /// **Deliberately not in `koru_keys::REGISTRY`.** That table is what the
    /// four *resolvers* bind — shell, container, view, editor — and its
    /// collision test reads an entry as a chord live for a whole layer. This
    /// one is live only while this panel owns the keyboard and is gone the
    /// instant it closes; registering it would assert a layer claims Alt+C
    /// everywhere, which is exactly the shadowing the table exists to catch.
    /// No panel-local key is in there — not Escape, not Enter, not the grid's
    /// arrows — and this is one of those.
    ///
    /// The modifiers are checked exactly: `Ctrl+Alt` is AltGr on Windows and
    /// means something else entirely, and `Alt+Shift+C` is nobody's chord.
    ///
    /// **Recognised whether or not there is anything to clear**, so that the
    /// caller can swallow it either way: without this a layout that delivers
    /// `text` for `Alt+C` would type a `c` into the phrase box on a grid with
    /// no Clear row, which is the same key doing two unrelated things.
    fn is_alt_c(key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        mods.alt_key()
            && !mods.control_key()
            && !mods.super_key()
            && !mods.shift_key()
            && crate::keys::ctrl_letter(key, text) == Some('c')
    }

    /// What Enter means while something is typed.
    ///
    /// - a phrase naming a **day** moves the selection there and the panel
    ///   stays open, with the box emptied. Nothing is picked yet: the person
    ///   is aiming, and the next phrase reads against where they just landed,
    ///   which is what makes `2w` → `+3d` → `-1w` a calculation rather than
    ///   three unrelated guesses. Emptying the box is what hands Enter straight
    ///   back to the grid, so the pick is a second Enter.
    /// - a phrase naming a **time** is handed up as [`Pick::Timed`].
    /// - **a phrase the grammar does not know picks nothing, clears nothing
    ///   and moves nothing.** The panel stays open with the text still in it
    ///   and the grid still usable, which is the only answer that does not
    ///   silently set a date the person did not say.
    fn enter_phrase(&mut self, engine: &mut TextEngine, phrase: String) -> Pick {
        match koru_when::parse(&phrase, self.reference(), koru_when::DEFAULT_DATE_ORDER) {
            Some(koru_when::When::Date(d)) => {
                self.selected = d;
                if let Some(field) = &mut self.query {
                    field.set_text(engine, "");
                }
                Pick::None
            }
            Some(koru_when::When::Wall(_)) => Pick::Timed(phrase),
            None => Pick::None,
        }
    }

    /// One key press.
    ///
    /// **The field and the grid share the keyboard, and the split is by what
    /// the field would lose.** Up/Down (a week) and PgUp/PgDn (a month) are
    /// always the grid's: a one-line box has no use for them. Left/Right move
    /// the grid a day *only while the field is empty* — with a caret in text
    /// they have to be the caret's, and an empty box has no caret motion to
    /// give up. Backspace and Delete are the same shape: they ask for Clear
    /// while the box is empty and edit the text otherwise, which is the rule
    /// the tag chooser's own field already follows.
    ///
    /// **Which left Clear with no keyboard route at all while a phrase was
    /// typed**, and that is what [`is_alt_c`](Self::is_alt_c) fixes — the
    /// accelerator the Clear row advertises with the underline under its `C`.
    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Pick {
        // Asked before anything else, because with a phrase in the box every
        // other route to Clear belongs to the caret.
        if Self::is_alt_c(key, text, mods) {
            // No Clear row: nothing to do, but still swallowed rather than
            // typed — see [`is_alt_c`](Self::is_alt_c).
            return if self.offers_clear {
                Pick::Clear
            } else {
                Pick::None
            };
        }
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        // No field at all reads as empty, which is what keeps every branch
        // below byte-for-byte the behaviour it had.
        let empty = self.query.as_ref().is_none_or(TextField::is_empty);
        let shift = |d, by| dates::shift_days(d, by);
        match named {
            Some(NamedKey::ArrowLeft) if empty => self.selected = shift(self.selected, -1),
            Some(NamedKey::ArrowRight) if empty => self.selected = shift(self.selected, 1),
            Some(NamedKey::ArrowUp) => self.selected = shift(self.selected, -7),
            Some(NamedKey::ArrowDown) => self.selected = shift(self.selected, 7),
            Some(NamedKey::PageUp) => self.selected = dates::shift_month(self.selected, -1),
            Some(NamedKey::PageDown) => self.selected = dates::shift_month(self.selected, 1),
            Some(NamedKey::Enter) => {
                return match self.phrase() {
                    // Something is typed: that is what Enter means, whether or
                    // not the grammar knows it. Falling back to the grid's
                    // selection would pick a day nobody named.
                    Some(phrase) => self.enter_phrase(engine, phrase),
                    // An empty box hands Enter to the grid — which is also how
                    // a refined selection is committed, since a successful
                    // refine empties the box.
                    None => Pick::Day(self.selected),
                };
            }
            // The Clear row's keyboard reach, only while it exists.
            Some(NamedKey::Delete) | Some(NamedKey::Backspace) if empty && self.offers_clear => {
                return Pick::Clear;
            }
            // Everything else goes into the field when there is one, and is
            // swallowed rather than ignored when there is not — so stray typing
            // cannot extend the text sitting behind the panel.
            _ => {
                if let Some(field) = &mut self.query {
                    field.key(engine, key, text, mods);
                }
            }
        }
        Pick::None
    }

    /// A click in panel-local coordinates.
    pub fn click(&self, local_x: f32, local_y: f32) -> Pick {
        if let Some(top) = self.clear_row_top()
            && local_y >= top
            && local_y < top + theme::ROW_H
        {
            return Pick::Clear;
        }
        match self.cell_at_local(local_x, local_y) {
            Some(picked) => Pick::Day(picked),
            None => Pick::None,
        }
    }

    /// Put the caret in the phrase field at a panel-local x.
    pub fn click_field(&mut self, engine: &mut TextEngine, x: f32, panel_w: f32) {
        let rect = field_rect(panel_w);
        if let Some(field) = &mut self.query {
            field.click(engine, x - rect.x + field.scroll());
        }
    }

    pub fn drag_field(&mut self, engine: &mut TextEngine, x: f32, panel_w: f32) {
        let rect = field_rect(panel_w);
        if let Some(field) = &mut self.query {
            field.drag_to(engine, x - rect.x + field.scroll());
        }
    }
}

/// The cell `day` occupies, in coordinates measured from the top of the
/// **grid** — the same origin [`cell_at`] reads.
///
/// One definition, used by the renderer and by the tests that walk a cell back
/// to its own date. The grid once carried two copies of this arithmetic, which
/// is why the round-trip test exists at all.
pub fn cell_rect(selected: NaiveDate, day: u32) -> Rect {
    let cell = dates::first_weekday_col(selected) + day - 1;
    let (row, col) = (cell / 7, cell % 7);
    Rect::new(
        theme::PANEL_PAD + col as f32 * CELL,
        theme::PANEL_PAD + HEADER_H + WEEKDAY_H + row as f32 * CELL,
        CELL,
        CELL,
    )
}

/// The date under a point in panel-local coordinates, if it is a real day of the
/// displayed month.
///
/// `local_y` is measured from the top of the *grid*, so a caller whose panel
/// wears a phrase field takes its band off first — which is what
/// [`DateGrid::cell_at_local`] does.
pub fn cell_at(selected: NaiveDate, local_x: f32, local_y: f32) -> Option<NaiveDate> {
    let gx = local_x - theme::PANEL_PAD;
    let gy = local_y - theme::PANEL_PAD - HEADER_H - WEEKDAY_H;
    if gx < 0.0 || gy < 0.0 {
        return None;
    }
    let col = (gx / CELL).floor() as i64;
    let row = (gy / CELL).floor() as i64;
    if !(0..7).contains(&col) || !(0..6).contains(&row) {
        return None;
    }
    let cell = row * 7 + col;
    let day = cell - dates::first_weekday_col(selected) as i64 + 1;
    if day < 1 || day > dates::days_in_month(selected) as i64 {
        return None;
    }
    dates::month_start(selected).with_day(day as u32)
}

// ----------------------------------------------------------------- drawing

/// Draw the whole panel's contents at `(px, py)` — the phrase field when there
/// is one, the grid, and the Clear row when there is one.
///
/// The panel's own frame is the host's: every overlay in the document editor
/// gets one `scene::floating_panel` call before this, so a widget that drew its
/// own would be the second recipe (`DESIGN.md`).
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    grid: &mut DateGrid,
    px: f32,
    py: f32,
    pw: f32,
    clear_hovered: bool,
) {
    let band = grid.band();
    if band > 0.0 {
        draw_phrase_field(scene, root, engine, grid, px, py, pw);
    }
    draw_month(scene, root, engine, grid.selected, px, py + band);
    if grid.clear_row_top().is_some() {
        draw_clear_row(scene, root, engine, grid, px, py, pw, clear_hovered);
    }
}

/// The phrase box, above the grid — the same box the command menu and the tag
/// chooser draw, in the same place, because it is the same widget and a third
/// recipe would drift from both ([ADR-181], reversed).
///
/// [ADR-181]: ../../../../docs/decisions/ADR-201-225.md#adr-204-duepriority--due-and-priority-enter-through-the-pickers-they-already-imply-and-a-typed-argument-commits-without-one
fn draw_phrase_field(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    grid: &mut DateGrid,
    px: f32,
    py: f32,
    pw: f32,
) {
    // The placeholder is the grammar's shortest useful sample: it has to teach
    // that this box takes a phrase rather than a date, or it reads as a filter.
    if let Some(field) = &mut grid.query {
        text_field::draw_panel_search(scene, root, field, engine, "friday 17:30, +4h", px, py, pw);
    }
}

/// The Clear row, under the grid — placed by [`DateGrid::clear_row_top`] so the
/// click that clears lands exactly where the row is drawn.
fn draw_clear_row(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    grid: &DateGrid,
    px: f32,
    py: f32,
    pw: f32,
    hovered: bool,
) {
    let Some(top) = grid.clear_row_top() else {
        return;
    };
    let ry = py + top;
    draw_row_bg(scene, root, px, ry, pw, hovered);
    let glyph = engine.icon_layout(icons::CLEAR, 15.0);
    draw_layout(
        scene,
        root,
        &glyph,
        px + 10.0,
        ry + 7.0,
        Some(theme::p().row_hint),
    );
    let style = theme::role::ui().with_weight(if hovered {
        theme::FontWeight::SemiBold
    } else {
        theme::FontWeight::Medium
    });
    let label = engine.simple_layout("Clear", style);
    let (lx, ly) = (px + 32.0, ry + 4.0);
    let baseline = label
        .lines()
        .next()
        .map(|line| line.metrics().baseline)
        .unwrap_or_else(|| label.height());
    draw_layout(scene, root, &label, lx, ly, Some(theme::p().text));
    // **The access key, advertised.** Alt+C is the only keyboard route to this
    // row while a phrase is typed into the box above, and a shortcut nobody can
    // see is a shortcut nobody uses. A hairline under the `C` is the whole of
    // it: there is no mnemonic *system* in this app — this is its one
    // picker-local accelerator — so the mark is drawn here rather than modelled
    // as an access key on every label. The `C` is measured on its own in the
    // same style the label was laid out in, so the rule is exactly as wide as
    // the glyph above it at any weight, density or zoom.
    let c = engine.simple_layout("C", style);
    let uy = (ly + baseline + 1.5) as f64;
    stroke(
        scene,
        root,
        theme::p().text,
        1.0,
        &vello::kurbo::Line::new((lx as f64, uy), ((lx + c.width()) as f64, uy)),
    );
}

/// The month itself: the header, the weekday strip and the day cells.
pub fn draw_month(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    selected: NaiveDate,
    px: f32,
    py: f32,
) {
    let title = engine.simple_layout(
        &dates::date_title(selected),
        theme::role::ui().with_weight(theme::FontWeight::SemiBold),
    );
    draw_layout(
        scene,
        root,
        &title,
        px + theme::PANEL_PAD + 4.0,
        py + theme::PANEL_PAD + 4.0,
        Some(theme::p().text),
    );
    let nav = engine.simple_layout("PgUp / PgDn", theme::role::hint());
    let nw = nav.width();
    draw_layout(
        scene,
        root,
        &nav,
        px + PANEL_W - theme::PANEL_PAD - 4.0 - nw,
        py + theme::PANEL_PAD + 6.0,
        Some(theme::p().row_hint),
    );

    let grid_x = px + theme::PANEL_PAD;
    let head_y = py + theme::PANEL_PAD + HEADER_H;

    for (i, d) in theme::WEEKDAYS.iter().enumerate() {
        let l = engine.simple_layout(d, theme::role::hint());
        let cx = grid_x + i as f32 * CELL + (CELL - l.width()) * 0.5;
        draw_layout(scene, root, &l, cx, head_y + 3.0, Some(theme::p().row_hint));
    }

    let days = dates::days_in_month(selected);

    for day in 1..=days {
        // The one definition of where a cell is — the same one `cell_at` reads
        // back, so a day is clickable exactly where it is drawn.
        let cell = cell_rect(selected, day);
        let (cx, cy) = (px + cell.x, py + cell.y);

        let is_sel = day == selected.day();
        if is_sel {
            fill(
                scene,
                root,
                theme::p().date.bg,
                &rrect(cx + 1.0, cy + 1.0, CELL - 2.0, CELL - 2.0, 5.0),
            );
            stroke(
                scene,
                root,
                theme::p().date.border,
                1.0,
                &rrect(cx + 1.0, cy + 1.0, CELL - 2.0, CELL - 2.0, 5.0),
            );
        }
        let l = engine.simple_layout(
            &day.to_string(),
            theme::role::meta().with_weight(if is_sel {
                theme::FontWeight::SemiBold
            } else {
                theme::FontWeight::Medium
            }),
        );
        let tx = cx + (CELL - l.width()) * 0.5;
        let ty = cy + (CELL - l.height()) * 0.5;
        let color = if is_sel {
            theme::p().date.fg
        } else {
            theme::p().text
        };
        draw_layout(scene, root, &l, tx, ty, Some(color));
    }
}

#[cfg(test)]
mod tests;
