// SPDX-License-Identifier: MPL-2.0

//! **One task row, the same on every page.**
//!
//! Today, Next, Upcoming, Later, Completed and every list draw their rows
//! through this module and nothing else: one line, the complete control at
//! its head, the kind glyph, the title with its context folded in after it,
//! the chips laid in from the right, and the `…` lane at the end. Today used
//! to give its rows a second line for the context — one page reading
//! differently from the five beside it, for a reason nobody could say at a
//! glance — and the owner asked for one line everywhere (round nine).
//!
//! **The geometry and the painting are one component.** [`parts`] and
//! [`chip_rects`] are what `paint` draws into *and* what the page's `hit_at`
//! resolves a press against, so a control cannot be clickable where it is
//! not drawn; the page's `row_geometry_hits_back` test is the proof. A second
//! host — a column, a picker — draws the same row by building a [`RowFace`]
//! and calling [`paint`]; there is no second recipe to drift from this one.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{chip, icons, scene, theme};
use koru_ui::table::{RowState, Table};
use vello::Scene;
use vello::kurbo::Affine;

use super::view::{CHIP_GAP, CONTROL_GAP, CONTROL_W, DOTS_W, LANE_W, PAD, RowChip, RowParts};

/// Between the title and its context.
pub(super) const DOT: &str = " \u{00B7} ";

/// The accent token an overdue reading wears — `accent_tone`, not a palette
/// field: a derived state asks the shared accent set by name.
pub(super) const WARM: &str = "red";

/// What one row shows. Built by the page from its [`TaskEntry`] and its own
/// state, and by nothing this module needs to know about.
///
/// [`TaskEntry`]: super::query::TaskEntry
pub(super) struct RowFace<'a> {
    pub completed: bool,
    /// Completed, and still holding unfinished work (E-2).
    pub residue: bool,
    /// The glyph before the title: the kind's own, or the page glyph for a
    /// paragraph that lives in a document. `None` draws `emoji` if there is
    /// one, else nothing.
    pub glyph: Option<char>,
    pub emoji: Option<&'a str>,
    pub title: &'a str,
    /// What follows the title, quieter — the document path. Empty draws
    /// nothing.
    pub context: &'a str,
    /// The chips and where they go, in the same order.
    pub chips: &'a [RowChip],
    pub chip_rects: &'a [Rect],
    pub state: RowState,
    /// `Some(hot)` inks the `…` lane, `hot` when the pointer is on it.
    pub dots: Option<bool>,
}

/// The height of every row, on every page.
pub(super) fn row_h() -> f32 {
    theme::NAV_ROW_H + 6.0
}

/// Where the pieces of one row are, from its rectangle and nothing else.
///
/// **The one place this arithmetic exists.** The lane at the right end is
/// reserved on every row; only the `…` inside it comes and goes, which is
/// what stops the chips shuffling under the pointer.
pub(super) fn parts(r: Rect) -> RowParts {
    let control = Rect::new(
        r.x + PAD,
        r.y + (r.h - CONTROL_W) * 0.5,
        CONTROL_W,
        CONTROL_W,
    );
    let dots = Rect::new(
        r.x + r.w - PAD - LANE_W + (LANE_W - DOTS_W) * 0.5,
        r.y + (r.h - DOTS_W) * 0.5,
        DOTS_W,
        DOTS_W,
    );
    let text_x = control.x + CONTROL_W + CONTROL_GAP;
    let side = theme::CHECKBOX_SIZE;
    let checkbox = Rect::new(
        control.x + (CONTROL_W - side) * 0.5,
        control.y + (CONTROL_W - side) * 0.5,
        side,
        side,
    );
    RowParts {
        control,
        checkbox,
        dots,
        text: Rect::new(text_x, r.y, (dots.x - CHIP_GAP - text_x).max(0.0), r.h),
    }
}

/// Where a row's chips are, laid out from the right so the title's cell
/// takes what is left. `widths` are the chips' measured widths, in order.
pub(super) fn chip_rects(r: Rect, widths: &[f32]) -> Vec<Rect> {
    let total: f32 =
        widths.iter().sum::<f32>() + chip::CHIP_GAP * (widths.len().saturating_sub(1)) as f32;
    let mut x = parts(r).dots.x - CHIP_GAP - total;
    let y = r.y + (r.h - chip::CHIP_H) * 0.5;
    widths
        .iter()
        .map(|w| {
            let rect = Rect::new(x, y, *w, chip::CHIP_H);
            x += w + chip::CHIP_GAP;
            rect
        })
        .collect()
}

/// Draw one row into `r`.
pub(super) fn paint(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    table: &Table,
    r: Rect,
    face: &RowFace<'_>,
) {
    let p = theme::p();
    table.paint_row_fill(scene, root, r, face.state);
    let parts = parts(r);

    // ---- the complete control. **The canonical command for every kind** —
    // this is the same tick a paragraph, an image and a document all get,
    // because SPEC §4 makes all five task-capable.
    // **The same box a document row draws** — `koru_draw::checkbox`, at the
    // document's own size inside this page's larger target. It was a circle
    // with a `CHECK` glyph in it, which made one task ticked in two places
    // look like two different things; the glyph also meant a row could not
    // say *done* at all before the icon font resolved. It was then drawn at
    // the full 18 px target, which read as too large beside the row's text.
    if face.completed {
        koru_draw::checkbox::ticked(scene, root, parts.checkbox);
    } else {
        koru_draw::checkbox::outline(scene, root, parts.checkbox, p.checkbox);
    }

    // ---- the chips, into the rectangles a press is resolved against.
    let chips_x = face
        .chip_rects
        .first()
        .map_or(parts.dots.x - CHIP_GAP, |c| c.x);
    for (chip, c) in face.chips.iter().zip(face.chip_rects) {
        paint_chip(
            scene,
            root,
            engine,
            chip.kind,
            &chip.label,
            chip.accent.as_deref(),
            c.x,
            c.y,
        );
    }

    // ---- the kind glyph, then the title.
    let mut text_x = parts.text.x;
    let text_w = (chips_x - CHIP_GAP - parts.text.x).max(0.0);
    let slot = Rect::new(text_x, r.y, koru_shell_chrome::PAGE_ICON_SLOT, r.h);
    if let Some(glyph) = face.glyph {
        let layout = engine.icon_layout(glyph, theme::CAPTION_ICON);
        scene::draw_layout(
            scene,
            root,
            &layout,
            slot.x + (slot.w - layout.width()) * 0.5,
            slot.y + (slot.h - layout.height()) * 0.5,
            Some(p.text_dim),
        );
        text_x += koru_shell_chrome::PAGE_ICON_SLOT + 4.0;
    } else if let Some(emoji) = face.emoji {
        koru_shell_chrome::draw_page_emoji(scene, root, engine, emoji, slot.x, slot, None);
        text_x += koru_shell_chrome::PAGE_ICON_SLOT + 4.0;
    }

    let title_cell = Rect::new(
        text_x,
        r.y,
        (text_w - (text_x - parts.text.x)).max(0.0),
        r.h,
    );
    // Completed is dimmed and struck — an archive reading, not a deletion.
    let ink = if face.completed {
        p.placeholder
    } else {
        p.text
    };
    let drawn = table.paint_cell(
        scene,
        root,
        engine,
        title_cell,
        face.title,
        theme::role::row(),
        ink,
    );
    if face.completed {
        let y = (title_cell.y + title_cell.h * 0.5).round();
        scene::fill(scene, root, ink, &scene::rect(title_cell.x, y, drawn, 1.0));
    }
    let mut after = title_cell.x + drawn;
    // E-2's glyph: **completed, and still holding unfinished work.** It
    // trails the strike rather than sitting in the chip lane, because it is
    // a note about this row's own text and not an attribute of it.
    if face.residue {
        let warn = engine.icon_layout(icons::WARNING, theme::CAPTION_ICON);
        scene::draw_layout(
            scene,
            root,
            &warn,
            after + 6.0,
            title_cell.y + (title_cell.h - warn.height()) * 0.5,
            Some(warm()),
        );
        after += 6.0 + warn.width();
    }

    // ---- the context, folded in after the title, quieter — one line.
    if !face.context.is_empty() {
        let cell = Rect::new(
            after,
            r.y,
            (title_cell.x + title_cell.w - after).max(0.0),
            title_cell.h,
        );
        table.paint_cell(
            scene,
            root,
            engine,
            cell,
            &format!("{DOT}{}", face.context),
            theme::role::meta(),
            p.text_dim,
        );
    }

    // ---- the trailing lane, inked only on the hovered row. Its space is
    // reserved on every row, which is what stops the chips reflowing as the
    // pointer travels.
    if let Some(hot) = face.dots {
        table.paint_trailing_dots(scene, root, parts.dots, hot);
    }
}

/// The warm ink an overdue reading wears.
pub(super) fn warm() -> vello::peniko::Color {
    theme::accent_tone(WARM)
        .map(|tone| tone.ink)
        .unwrap_or(theme::p().text)
}

/// One attribute chip, from the engine's chip cache.
pub(super) fn paint_chip(
    scene: &mut Scene,
    root: Affine,
    engine: &TextEngine,
    kind: chip::ChipKind,
    label: &str,
    accent: Option<&str>,
    x: f32,
    y: f32,
) {
    let Some(visual) = engine.attr_chips.get(kind, label, accent, None) else {
        return;
    };
    chip::draw(scene, root, visual, accent, x, y, false);
}
