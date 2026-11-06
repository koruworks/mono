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
//! **The geometry and the painting are one element tree.** The row is
//! written as elements (`koru_draw::el`, `docs/features/koruui/ELEMENTS-GUIDE.md`)
//! and laid out by the one solver, so the F12 inspector shows it as what it
//! is rather than as a run of fills and text lines:
//!
//! ```text
//! Task list                      the page's container (`draw.rs`)
//!   Task row                     one row, the full width of the page
//!     Complete control           the box
//!     Task content
//!       Task label
//!         Task icon              the kind glyph or the emoji, when there is one
//!         Task title             the title, its strike and its context
//!       Task tags
//!         Task tag …             one per chip
//!       Row menu                 the `…` lane, reserved on every row
//! ```
//!
//! [`parts`] and [`chip_rects`] **read their rectangles back off that
//! layout**, and they are what the page's `hit_at` resolves a press against
//! and what the accessibility tree reports, so a control cannot be clickable
//! where it is not drawn; the page's `row_geometry_hits_back` test is the
//! proof. Nothing here repeats a row's arithmetic: every gap is a spacing
//! step, and the only raw lengths are the page's control and chip sizes.
//!
//! **The row's plate is the table's.** A hovered or selected row is lit by
//! `Table::paint_row_fill` under the element row — the KoruTable row state
//! every list page shares, at a strength no surface role carries — and the
//! element row draws what sits on it.
//!
//! **Accessibility stays the page's** (`accessibility.rs`), which already
//! publishes each row and its complete control from [`parts`]; the elements
//! are decorative to the frame, so the two trees cannot disagree.

use koru_draw::el::{Align, El, ElState, Frame, FrameCx, Leaf, Length, TextCache, leaf, row};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Space;
use koru_draw::{chip, icons, scene, theme};
use koru_ui::table::{Metrics, RowState, Table};
use vello::Scene;
use vello::kurbo::Affine;

use super::view::{CHIP_GAP, CONTROL_W, DOTS_W, LANE_W, RowChip, RowParts};

/// Between the title and its context.
pub(super) const DOT: &str = " \u{00B7} ";

/// The accent token an overdue reading wears — `accent_tone`, not a palette
/// field: a derived state asks the shared accent set by name.
pub(super) const WARM: &str = "red";

/// The developer names the inspector shows, and the geometry is read back by.
pub(super) const LIST: &str = "Task list";
pub(super) const ROW: &str = "Task row";
pub(super) const CONTROL: &str = "Complete control";
pub(super) const CONTENT: &str = "Task content";
pub(super) const LABEL: &str = "Task label";
pub(super) const ICON: &str = "Task icon";
pub(super) const TITLE: &str = "Task title";
pub(super) const TAGS: &str = "Task tags";
pub(super) const TAG: &str = "Task tag";
pub(super) const MENU: &str = "Row menu";

/// The page's numbers, as the spacing steps they are. Held equal to the
/// `view.rs` constants the header and the capture row still use by
/// `the_rows_steps_are_the_pages_numbers`.
pub(super) const PAD_STEP: Space = Space::S10;
pub(super) const CONTROL_GAP_STEP: Space = Space::S2_5;
pub(super) const CHIP_GAP_STEP: Space = Space::S3_5;
/// The lane the `…` sits in: `LANE_W`.
pub(super) const LANE_STEP: Space = Space::S6;
/// Between two chips: `chip::CHIP_GAP`.
pub(super) const TAG_GAP: Space = Space::S2;
/// Between the kind glyph's slot and the title.
const ICON_GAP: Space = Space::S1;

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
    /// The chips, and their measured widths in the same order.
    pub chips: &'a [RowChip],
    pub chip_widths: &'a [f32],
    pub state: RowState,
    /// `Some(hot)` inks the `…` lane, `hot` when the pointer is on it.
    pub dots: Option<bool>,
}

impl RowFace<'_> {
    /// A row that says nothing, with chips of these widths: what the
    /// geometry is laid out from. Where the control, the text and the `…`
    /// lane are does not depend on what a row says, and where its chips are
    /// depends only on their widths.
    fn blank(chip_widths: &[f32]) -> RowFace<'_> {
        RowFace {
            completed: false,
            residue: false,
            glyph: None,
            emoji: None,
            title: "",
            context: "",
            chips: &[],
            chip_widths,
            state: RowState::Idle,
            dots: None,
        }
    }
}

/// The height of every row, on every page.
pub(super) fn row_h() -> f32 {
    theme::NAV_ROW_H + 6.0
}

/// The row as elements, inked through `table`.
pub(super) fn tree(face: &RowFace<'_>, table: Table) -> El {
    let icon = (face.glyph.is_some() || face.emoji.is_some()).then(|| {
        leaf(
            ICON,
            IconLeaf {
                glyph: face.glyph,
                emoji: face.emoji.map(str::to_owned),
            },
        )
        .decorative()
        .w(Length::Px(koru_shell_chrome::PAGE_ICON_SLOT))
    });
    let tags = face.chip_widths.iter().enumerate().map(|(i, &width)| {
        let chip = face.chips.get(i);
        leaf(
            TAG,
            TagLeaf {
                kind: chip.map_or(chip::ChipKind::Tag, |c| c.kind),
                label: chip.map(|c| c.label.clone()),
                accent: chip.and_then(|c| c.accent.clone()),
            },
        )
        .decorative()
        .w(Length::Runtime(width, "the chip's measured width"))
        .h(Length::Px(chip::CHIP_H))
        .align_self(Align::Centre)
    });
    let title = leaf(
        TITLE,
        TitleLeaf {
            title: face.title.to_owned(),
            context: face.context.to_owned(),
            completed: face.completed,
            residue: face.residue,
            table,
        },
    )
    .decorative()
    .fill();
    let menu = leaf(
        MENU,
        MenuLeaf {
            dots: face.dots,
            table,
        },
    )
    .decorative()
    .w(LANE_STEP)
    .h(Length::Px(DOTS_W))
    .align_self(Align::Centre);
    let control = leaf(
        CONTROL,
        CheckLeaf {
            completed: face.completed,
        },
    )
    .decorative()
    .w(Length::Px(CONTROL_W))
    .h(Length::Px(CONTROL_W))
    .align_self(Align::Centre);
    row(ROW).pad_x(PAD_STEP).child(control).child(
        row(CONTENT)
            .fill()
            .pad_start(CONTROL_GAP_STEP)
            .gap(CHIP_GAP_STEP)
            .child(row(LABEL).fill().gap(ICON_GAP).children(icon).child(title))
            .child(row(TAGS).gap(TAG_GAP).children(tags))
            .child(menu),
    )
}

/// `tree` laid out in `r`.
pub(super) fn layout(tree: &El, engine: &mut TextEngine, r: Rect) -> Frame {
    // No element of the row shapes text — its words are painted by leaves —
    // so the cache only satisfies the signature.
    tree.layout(engine, &mut TextCache::default(), r, FrameCx::root())
}

/// A row's geometry, read back off its layout: its parts, and where chips
/// of these widths go.
fn geometry(r: Rect, chip_widths: &[f32]) -> (RowParts, Vec<Rect>) {
    // A table only inks what a leaf paints, and a layout read for its
    // rectangles paints nothing, so any table will do.
    let table = Table::new(r, Metrics::manager(), 0.0);
    let tree = tree(&RowFace::blank(chip_widths), table);
    let frame = crate::input_engine::with(|engine| layout(&tree, engine, r));
    let at = |name| frame.rect_of(name).unwrap_or(r);
    let control = at(CONTROL);
    let dots = at(MENU);
    let label = at(LABEL);
    let parts = RowParts {
        control,
        checkbox: checkbox_in(control),
        dots,
        text: Rect::new(label.x, r.y, (dots.x - CHIP_GAP - label.x).max(0.0), r.h),
    };
    let chips = frame.named(TAG).map(|node| node.rect).collect();
    (parts, chips)
}

/// **Where a rename field stands on a row**: the title's own box, laid out
/// as the row lays it out — after the kind glyph when the row wears one,
/// before the chips — so the words being edited sit where they were drawn.
pub(super) fn title_field(r: Rect, has_icon: bool, chip_widths: &[f32]) -> Rect {
    let table = Table::new(r, Metrics::manager(), 0.0);
    let mut face = RowFace::blank(chip_widths);
    if has_icon {
        face.glyph = Some(icons::DOCUMENT);
    }
    let tree = tree(&face, table);
    let frame = crate::input_engine::with(|engine| layout(&tree, engine, r));
    let title = frame.rect_of(TITLE).unwrap_or(r);
    Rect::new(title.x, r.y, title.w, r.h)
}

/// Where the pieces of one row are, from its rectangle and nothing else.
pub(super) fn parts(r: Rect) -> RowParts {
    geometry(r, &[]).0
}

/// Where a row's chips are: laid in from the right, so the title takes what
/// is left. `widths` are the chips' measured widths, in order.
pub(super) fn chip_rects(r: Rect, widths: &[f32]) -> Vec<Rect> {
    geometry(r, widths).1
}

/// The box drawn inside the complete control's target: the document's own
/// size, centred.
fn checkbox_in(control: Rect) -> Rect {
    let side = theme::CHECKBOX_SIZE;
    Rect::new(
        control.x + (control.w - side) * 0.5,
        control.y + (control.h - side) * 0.5,
        side,
        side,
    )
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
    table.paint_row_fill(scene, root, r, face.state);
    let tree = tree(face, *table);
    let frame = layout(&tree, engine, r);
    frame.paint(&tree, scene, root, engine);
}

/// **The complete control** — the canonical command for every kind, the
/// same tick a paragraph, an image and a document all get, because SPEC §4
/// makes all five task-capable.
///
/// **The same box a document row draws** — `koru_draw::checkbox`, at the
/// document's own size ([`theme::CHECKBOX_SIZE`]) centred in this page's
/// larger target. It was a circle with a `CHECK` glyph in it, which made one
/// task ticked in two places look like two different things (and could not
/// say *done* before the icon font resolved); then the box at the full
/// target, which read as too large beside the row's text.
struct CheckLeaf {
    completed: bool,
}

impl Leaf for CheckLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (CONTROL_W, CONTROL_W)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let r = checkbox_in(at);
        if self.completed {
            koru_draw::checkbox::ticked(scene, root, r);
        } else {
            koru_draw::checkbox::outline(scene, root, r, cx.palette().checkbox);
        }
    }
}

/// The kind glyph, or the row's emoji, centred in the page-icon slot.
struct IconLeaf {
    glyph: Option<char>,
    emoji: Option<String>,
}

impl Leaf for IconLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (koru_shell_chrome::PAGE_ICON_SLOT, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(glyph) = self.glyph {
            let layout = engine.icon_layout(glyph, theme::CAPTION_ICON);
            scene::draw_layout(
                scene,
                root,
                &layout,
                at.x + (at.w - layout.width()) * 0.5,
                at.y + (at.h - layout.height()) * 0.5,
                Some(cx.palette().text_dim),
            );
        } else if let Some(emoji) = &self.emoji {
            koru_shell_chrome::draw_page_emoji(scene, root, engine, emoji, at.x, at, None);
        }
    }
}

/// The title, struck when completed, with E-2's glyph after it and the
/// context folded in after that — one line, elided to the cell.
struct TitleLeaf {
    title: String,
    context: String,
    completed: bool,
    residue: bool,
    table: Table,
}

impl Leaf for TitleLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        cell: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        // Completed is dimmed and struck — an archive reading, not a deletion.
        let ink = if self.completed {
            p.placeholder
        } else {
            p.text
        };
        let drawn = self.table.paint_cell(
            scene,
            root,
            engine,
            cell,
            &self.title,
            theme::role::row(),
            ink,
        );
        if self.completed {
            let y = (cell.y + cell.h * 0.5).round();
            scene::fill(scene, root, ink, &scene::rect(cell.x, y, drawn, 1.0));
        }
        let mut after = cell.x + drawn;
        // E-2's glyph: **completed, and still holding unfinished work.** It
        // trails the strike rather than sitting in the chip lane, because it
        // is a note about this row's own text and not an attribute of it.
        if self.residue {
            let warn = engine.icon_layout(icons::WARNING, theme::CAPTION_ICON);
            scene::draw_layout(
                scene,
                root,
                &warn,
                after + 6.0,
                cell.y + (cell.h - warn.height()) * 0.5,
                Some(warm()),
            );
            after += 6.0 + warn.width();
        }
        // The context, folded in after the title, quieter — one line.
        if !self.context.is_empty() {
            let rest = Rect::new(after, cell.y, (cell.x + cell.w - after).max(0.0), cell.h);
            self.table.paint_cell(
                scene,
                root,
                engine,
                rest,
                &format!("{DOT}{}", self.context),
                theme::role::meta(),
                p.text_dim,
            );
        }
    }
}

/// One attribute chip — a reading of the row, never a control of its own:
/// the list chip's press is the page's `hit_at`, against this rectangle.
struct TagLeaf {
    kind: chip::ChipKind,
    /// `None` on a layout read only for its geometry.
    label: Option<String>,
    accent: Option<String>,
}

impl Leaf for TagLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, chip::CHIP_H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(label) = &self.label {
            koru_ui::LabelChip::attribute(self.kind, label, self.accent.as_deref()).draw(
                scene,
                root,
                engine,
                at,
                koru_ui::label_chip::State::default(),
            );
        }
    }
}

/// The trailing lane, inked only on the hovered row. Its space is reserved
/// on every row, which is what stops the chips reflowing as the pointer
/// travels.
struct MenuLeaf {
    dots: Option<bool>,
    table: Table,
}

impl Leaf for MenuLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (LANE_W, DOTS_W)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        if let Some(hot) = self.dots {
            self.table.paint_trailing_dots(scene, root, engine, at, hot);
        }
    }
}

/// The warm ink an overdue reading wears.
pub(super) fn warm() -> vello::peniko::Color {
    theme::accent_tone(WARM)
        .map(|tone| tone.ink)
        .unwrap_or(theme::p().text)
}
