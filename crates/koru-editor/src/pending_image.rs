// SPDX-License-Identifier: MPL-2.0

//! **An image that is still loading, drawn where it will land.**
//!
//! Attaching a picture — a paste, a drop, `/image` — reads, decodes and stores
//! it on a worker the shell owns, and only then makes the document node
//! (`koru_workspace_host::image_import`). Until then the view reserves the room
//! the picture will take and draws a skeleton block in it: the theme's
//! `chip_bg`, the colour an image whose pixels are not here already wears,
//! breathing on the shared `PULSE_LOOP_MS` loop, and still under Reduce
//! Motion.
//!
//! **The placeholder is view-local, not a node.** A node is synced: a
//! placeholder written into the document would appear on every other device
//! as a picture that has no bytes yet — and on a failure, as one that never
//! will — and it would have to be removed again by a second edit that undo
//! then has to know to skip. The node also cannot be written before the blob
//! has its store address, which is the slow part. So the view keeps a
//! [`PendingImage`] and the document is untouched until the image is ready;
//! then one edit, one undo step, lands it.
//!
//! **Its position is a node id, which already maps through concurrent
//! edits.** Typing before the anchor, Enter, a peer's insert: none of them
//! moves a node, so the band stays below (or, for a replaced selection,
//! above) the same row and the shell inserts at that same row
//! (`image_import::map_place`). Only the anchor's own deletion moves it, to
//! the sibling that preceded it — the same rule on both sides.
//!
//! **The room is reserved by the positioning pass itself**
//! (`rowlayout::position_rows`, through [`Body::reserved`]), opening exactly
//! where the picture's row will start, so a placeholder of the picture's size
//! is replaced by the picture without the page moving.

use std::time::Instant;

use koru_core::NodeId;
use koru_core::node::{Align, BlobHash};
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::scene::{fill, rect, rrect};
use koru_draw::text::TextEngine;
use koru_draw::{anim, icons, theme};
use vello::Scene;
use vello::kurbo::Affine;

use crate::body::Body;
use crate::paragraph::ImageRef;
use crate::rowlayout;
use crate::view_state::ViewState;

#[cfg(test)]
mod tests;

/// The block shown before an image's size is known — a clipboard bitmap
/// still being read. A modest 16:9 figure, so the guess is neither a sliver
/// nor a page; it takes the real shape as soon as the header is read.
pub const UNSIZED: (u32, u32) = (240, 135);

/// Where the band goes, relative to the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Below this row and anything nested under it: a paste below the caret's
    /// row, or `/image` on this line.
    After(NodeId),
    /// Above this row: a paste replacing a selection that starts here.
    Before(NodeId),
    /// After the last row: a dropped file, or a paste with no row to stand on.
    End,
}

/// Where the caret and selection were when the gesture was made. The image
/// takes the caret on arrival only if they are still there — somebody who
/// kept typing while it loaded keeps their place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaretMark {
    row: Option<NodeId>,
    caret: usize,
    selected: Vec<NodeId>,
    active: bool,
}

impl CaretMark {
    pub fn of(body: &Body, active: bool) -> Self {
        Self {
            row: body.focused_id(),
            caret: body.focused().map_or(0, |row| row.caret()),
            selected: body.selected_ids(),
            active,
        }
    }
}

/// One image on its way into this view's document.
#[derive(Clone, Debug)]
pub struct PendingImage {
    /// The shell's ticket for it.
    pub ticket: u64,
    pub place: Place,
    /// The sibling before the anchor at the gesture: where the band (and the
    /// image) go if the anchor is deleted while it loads.
    pub fallback: Option<NodeId>,
    /// Each image's pixel size, once the headers are read. Empty until then,
    /// which draws one [`UNSIZED`] block.
    pub sizes: Vec<(u32, u32)>,
    pub started: Instant,
    pub caret: CaretMark,
    /// A stable identity for the accessibility node that stands for it. Not a
    /// document node id, and never written anywhere.
    pub id: uuid::Uuid,
}

/// The room one pending image holds in the body this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Reserved {
    pub ticket: u64,
    /// The band opens before this row; `rows.len()` is the end of the body.
    pub before_row: usize,
    /// Everything the band adds to the page, its trailing gap included.
    pub height: f32,
    /// The gap after the last block, which is spacing rather than content.
    pub trailing: f32,
    /// Set by `rowlayout::position_rows`: the content y the band opens at.
    pub top: f32,
    /// Each block's rect, its `y` measured from [`top`](Self::top).
    blocks: Vec<Rect>,
}

impl Reserved {
    /// Each skeleton block in content coordinates — what is drawn, and what
    /// accessibility publishes.
    pub fn blocks(&self) -> impl Iterator<Item = Rect> + '_ {
        self.blocks
            .iter()
            .map(|b| Rect::new(b.x, self.top + b.y, b.w, b.h))
    }
}

/// Record an image on its way to `place`. Reads the document only to find
/// the anchor's previous sibling.
pub fn begin(
    pending: &mut Vec<PendingImage>,
    ws: &WorkspaceDoc,
    body: &Body,
    active: bool,
    ticket: u64,
    place: Place,
    now: Instant,
) {
    let fallback = match place {
        Place::After(anchor) | Place::Before(anchor) => previous_sibling(ws, anchor),
        Place::End => None,
    };
    pending.retain(|p| p.ticket != ticket);
    pending.push(PendingImage {
        ticket,
        place,
        fallback,
        sizes: Vec::new(),
        started: now,
        caret: CaretMark::of(body, active),
        id: uuid::Uuid::new_v4(),
    });
}

/// The headers are read: the blocks take the pictures' shapes. Returns
/// whether anything changed.
pub fn size(pending: &mut [PendingImage], ticket: u64, sizes: &[(u32, u32)]) -> bool {
    let Some(one) = pending.iter_mut().find(|p| p.ticket == ticket) else {
        return false;
    };
    if one.sizes == sizes {
        return false;
    }
    one.sizes = sizes.to_vec();
    true
}

/// Take a pending image away — it landed, or failed.
pub fn end(pending: &mut Vec<PendingImage>, ticket: u64) -> Option<PendingImage> {
    let at = pending.iter().position(|p| p.ticket == ticket)?;
    Some(pending.remove(at))
}

/// Whether a frame is owed for the pulse. Never under Reduce Motion, and
/// never once nothing is loading, so an idle window goes back to sleep.
pub fn animating(pending: &[PendingImage], reduced_motion: bool) -> bool {
    !pending.is_empty() && !reduced_motion
}

/// How this view would size a fresh image of `(width, height)` pixels, as
/// the device-pixel side its first draw asks the cache for — captured now, so
/// a worker can build that mip before the image is ever drawn.
pub fn mip_side(view: &ViewState) -> impl Fn(u32, u32) -> u32 + Send + Sync + 'static {
    let layout_w = view.view_mode.effective_w(view.viewport.0);
    let scale = view.drawn_scale;
    move |width, height| {
        let image = ImageRef {
            hash: BlobHash(String::new()),
            mime: String::new(),
            width: Some(width),
            height: Some(height),
            display: None,
        };
        let (dw, dh) = rowlayout::image_display_size(&image, layout_w);
        crate::draw::mip_wanted(dw, dh, scale)
    }
}

fn previous_sibling(ws: &WorkspaceDoc, id: NodeId) -> Option<NodeId> {
    let index = ws.sibling_index(id).ok()?;
    let siblings = ws.children(ws.parent(id).ok()?).ok()?;
    index.checked_sub(1).and_then(|i| siblings.get(i).copied())
}

/// The row a band opens before.
fn before_row(body: &Body, pending: &PendingImage) -> usize {
    let after = |id: NodeId| {
        let i = body.index_of(id)?;
        // `insert_after` puts the image after the anchor's whole subtree, so
        // the band goes below every row nested under it as well.
        let depth = body.rows[i].depth;
        let end = body.rows[i + 1..]
            .iter()
            .position(|row| row.depth <= depth)
            .map_or(body.rows.len(), |n| i + 1 + n);
        Some(end)
    };
    let found = match pending.place {
        Place::After(id) => after(id),
        Place::Before(id) => body.index_of(id),
        Place::End => Some(body.rows.len()),
    };
    found
        .or_else(|| pending.fallback.and_then(after))
        .unwrap_or(body.rows.len())
}

/// Resolve every pending image to the room it holds, before the rows are
/// positioned. Free when nothing is loading.
pub fn reserve(body: &mut Body, pending: &[PendingImage], layout_w: f32) {
    if pending.is_empty() {
        body.reserved.clear();
        return;
    }
    let line = theme::FONT_SIZE * theme::LINE_HEIGHT;
    let mut reserved: Vec<Reserved> = pending
        .iter()
        .map(|one| {
            let sizes: &[(u32, u32)] = if one.sizes.is_empty() {
                &[UNSIZED]
            } else {
                &one.sizes
            };
            let mut y = 0.0;
            let mut blocks = Vec::with_capacity(sizes.len());
            for &(width, height) in sizes {
                // The room an image row of this size takes in
                // `position_rows`: its pad above, its display height, then
                // the row gap and its pad below.
                let image = ImageRef {
                    hash: BlobHash(String::new()),
                    mime: String::new(),
                    width: Some(width),
                    height: Some(height),
                    display: None,
                };
                let (x, w, h) = rowlayout::image_display_box(&image, Align::Start, layout_w);
                y += theme::IMAGE_PAD;
                blocks.push(Rect::new(x, y, w, h));
                y += h.max(line) + theme::TASK_GAP + theme::IMAGE_PAD;
            }
            Reserved {
                ticket: one.ticket,
                before_row: before_row(body, one),
                height: y,
                trailing: theme::TASK_GAP + theme::IMAGE_PAD,
                top: 0.0,
                blocks,
            }
        })
        .collect();
    // Stable, so two pastes at one place stack in the order they were made.
    reserved.sort_by_key(|band| band.before_row);
    body.reserved = reserved;
}

/// Paint every pending image's skeleton, under the content root.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    body: &Body,
    pending: &[PendingImage],
    engine: &mut TextEngine,
    scroll: f32,
    viewport_h: f32,
    now: Instant,
    reduced_motion: bool,
) {
    for band in &body.reserved {
        let Some(one) = pending.iter().find(|p| p.ticket == band.ticket) else {
            continue;
        };
        // A slow breath between two strengths of the placeholder colour, on
        // the shared loop. Under Reduce Motion it rests at the stronger one:
        // the block alone says "something goes here".
        let alpha = if reduced_motion {
            1.0
        } else {
            let phase = anim::loop_phase(one.started, now, theme::PULSE_LOOP_MS);
            0.55 + 0.45 * (0.5 + 0.5 * (phase * std::f32::consts::TAU).cos())
        };
        for block in band.blocks() {
            let y = block.y - scroll;
            if y + block.h < 0.0 || y > viewport_h {
                continue;
            }
            let _inspection = koru_draw::inspect::scope(
                "Image loading placeholder",
                "unregistered",
                root,
                rect(block.x, y, block.w, block.h),
                |record| {
                    record.property(
                        "implementation",
                        "koru_editor::pending_image::draw",
                        "view-local skeleton; not a document node",
                    );
                    record.property(
                        "display size",
                        format!("{:.1} × {:.1}", block.w, block.h),
                        "image_display_box of the header size; logical units",
                    );
                    record.property("reduced motion", reduced_motion, "ViewState");
                },
            );
            let p = theme::p();
            fill(
                scene,
                root,
                p.chip_bg.multiply_alpha(alpha),
                &rrect(block.x, y, block.w, block.h, 6.0),
            );
            // The image glyph the missing-image placeholder wears, without its
            // cross: this one is on its way, not unavailable.
            if block.h >= 56.0 && block.w >= 56.0 {
                let glyph = engine.icon_layout(icons::IMAGE, 24.0);
                koru_draw::scene::draw_layout(
                    scene,
                    root,
                    &glyph,
                    block.x + (block.w - glyph.width()) * 0.5,
                    y + (block.h - glyph.height()) * 0.5,
                    Some(p.text_dim.multiply_alpha(0.6 * alpha)),
                );
            }
        }
    }
}
