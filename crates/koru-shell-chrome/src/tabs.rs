// SPDX-License-Identifier: MPL-2.0

//! The tab strip: where each tab sits, and how it is drawn.
//!
//! Tabs are sized from their labels in the current UI font, clamped between
//! [`TAB_MIN_W`](theme::TAB_MIN_W) and [`TAB_MAX_W`](theme::TAB_MAX_W). When the
//! total exceeds the strip they scroll horizontally rather than shrinking.
//!
//! **Hit stays pure arithmetic.** Preferred widths are measured when a
//! `TextEngine` is available (draw / font change) and cached by the container as
//! plain `f32`s. [`layout`] takes those numbers — no engine on the hit path.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_draw::{icons, scene};
use vello::Scene;
use vello::kurbo::Affine;

/// One tab's rectangles: what it claims, what it draws, and the close box.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TabRect {
    pub index: usize,
    /// The tab's **hit area** — the full height of the strip, so the pointer
    /// does not have to find the pill inside it.
    pub body: Rect,
    /// Where the tab is **drawn**: inset from the row top and bottom by
    /// [`TAB_INSET_TOP`](theme::TAB_INSET_TOP) and
    /// [`TAB_INSET_BOTTOM`](theme::TAB_INSET_BOTTOM). The label centres in
    /// this, not in `body`.
    pub pill: Rect,
    /// The close box, centred in the pill. Zero-sized when the tab is too
    /// narrow to show one, so it can never be hit.
    pub close: Rect,
}

/// What the strip needs to know about one tab.
pub struct Tab<'a> {
    pub label: &'a str,
    pub icon: Option<TabIcon<'a>>,
    pub closable: bool,
    /// How far left or right of its slot to *draw* this tab.
    pub slide: f32,
    pub dragging: bool,
    /// Air the slot carries after the chip that is **not the chip's**: the pin
    /// group's gap, folded into the last pin's width so [`layout`] stays pure
    /// arithmetic. The chip stops short of it; the slot (and the hit) keep it.
    /// Drawn as chip, it made the last pin six pixels wider than its
    /// neighbours and its icon sit left of centre (the CTO's note).
    pub gap_after: f32,
    /// A chat's status (koru-ai D83): a turning ring **in place of** the
    /// icon while it works, a dot on the icon's corner when it needs the
    /// person or holds something unseen. `None` on every other tab.
    pub status: Option<crate::status_mark::StatusMark>,
}

impl Tab<'_> {
    /// An icon and nothing else: the pin with Show title off. Its icon
    /// centres in the chip rather than sitting where a label's would start.
    pub fn is_icon_only(&self) -> bool {
        self.label.is_empty() && !self.closable
    }
}

/// Where a tab's icon slot starts: at the label's padding for a titled tab,
/// centred in the pill for an icon-only pin — there is no label and no close
/// box to leave room for, so left-aligning it read as space kept for a hidden
/// `×` (the CTO's note).
pub fn icon_slot_x(pill: Rect, icon_only: bool) -> f32 {
    if icon_only {
        pill.x + ((pill.w - crate::PAGE_ICON_SLOT) * 0.5).max(0.0)
    } else {
        pill.x + theme::PANEL_PAD
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TabIcon<'a> {
    Emoji(&'a str),
    Glyph(char),
    /// An accent **token**, drawn as the rounded square a list wears in the
    /// tasks rail and on the Tasks page's own heading.
    ///
    /// A token rather than a colour, so a list coloured in one theme is read in
    /// the other — and `None` for a list whose colour the table has forgotten,
    /// which draws the neutral square rather than nothing. Both are the rule the
    /// other two swatches already follow; what is shared is `theme::accent_tone`
    /// and the choice of square-not-dot, and the six lines that paint it are not
    /// worth a crate boundary.
    Swatch(Option<&'a str>),
    /// A launch product's mark — a session tab wearing the logo of the
    /// program it runs (TERMINATAB Q-E6 P3/P4). Vector rather than a glyph
    /// because the icon font carries no other product's logo.
    Brand(&'static koru_draw::brand::BrandIcon),
}

/// Preferred width of one tab in the current UI font, clamped to
/// `[TAB_MIN_W, TAB_MAX_W]`.
///
/// **Icon-only pins are not titled tabs.** An empty non-closable label is the
/// pin with Show title off (`TAB_PINNED_W`). Clamping that to `TAB_MIN_W`
/// made the chip jump between a 36px square and a 96px empty pill whenever
/// the two width caches remesured.
pub fn preferred_width(
    engine: &mut TextEngine,
    label: &str,
    has_icon: bool,
    closable: bool,
) -> f32 {
    if label.is_empty() && !closable {
        return theme::TAB_PINNED_W;
    }
    let icon_w = if has_icon {
        crate::PAGE_ICON_SLOT + 4.0
    } else {
        0.0
    };
    // Always reserve close chrome for closable tabs so hover does not reflow.
    let close_w = if closable {
        theme::TAB_CLOSE + theme::PANEL_PAD
    } else {
        0.0
    };
    let pad = theme::PANEL_PAD * 2.0;
    let label_w = if label.is_empty() {
        0.0
    } else {
        engine.simple_layout(label, theme::role::row()).width()
    };
    (pad + icon_w + label_w + close_w).clamp(theme::TAB_MIN_W, theme::TAB_MAX_W)
}

/// The room tabs actually occupy: the strip, less the lane the `+` anchors in.
///
/// **The lane is reserved whether or not the `+` is standing in it.** With few
/// tabs the `+` sits immediately after the last one, well left of here, and the
/// lane is simply empty — but reserving it unconditionally is what stops a tab
/// from ever scrolling *underneath* the `+`, and it keeps the reservation out of
/// a feedback loop (how much room the tabs get would otherwise depend on whether
/// they overflowed, which depends on how much room they got).
///
/// Every scroll calculation measures against this, never against `strip`.
pub fn lane(strip: Rect) -> Rect {
    let w = (strip.w - theme::TAB_NEW_W - theme::TAB_GAP).max(0.0);
    Rect::new(strip.x, strip.y, w, strip.h)
}

/// Total width of the scrolled content: tabs + the gaps between them + the
/// gap before the first one.
///
/// The `+` is **not** in here — it has its own lane, which [`lane`] takes off
/// the viewport instead. Counting it in both places reserved it twice.
pub fn content_width(widths: &[f32]) -> f32 {
    if widths.is_empty() {
        return 0.0;
    }
    let tabs: f32 = widths.iter().sum();
    let gaps = widths.len() as f32 * theme::TAB_GAP;
    tabs + gaps
}

/// How far the strip can scroll (0 when everything fits).
pub fn max_scroll(strip: Rect, widths: &[f32]) -> f32 {
    (content_width(widths) - lane(strip).w).max(0.0)
}

pub fn clamp_scroll(strip: Rect, widths: &[f32], scroll_x: f32) -> f32 {
    scroll_x.clamp(0.0, max_scroll(strip, widths))
}

/// Where every tab sits, given preferred widths and horizontal scroll.
///
/// Pure arithmetic over numbers — no font. Tabs pack left-to-right at their
/// natural widths; they never shrink below the cached preferred size. The
/// first one starts one [`TAB_GAP`](theme::TAB_GAP) in — the strip's edge is
/// a wall with the same air as between two tabs, not a wall a tab leans on.
/// The pad is part of the scrolled content: it shows at rest and scrolls away
/// with the tabs.
pub fn layout(strip: Rect, widths: &[f32], scroll_x: f32) -> Vec<TabRect> {
    if widths.is_empty() || strip.w <= 0.0 {
        return Vec::new();
    }
    let scroll = clamp_scroll(strip, widths, scroll_x);
    let mut x = strip.x + theme::TAB_GAP - scroll;
    let mut out = Vec::with_capacity(widths.len());
    for (i, &w) in widths.iter().enumerate() {
        let body = Rect::new(x, strip.y, w, strip.h);
        let pill = pill(body);
        out.push(TabRect {
            index: i,
            body,
            pill,
            close: close_box(pill),
        });
        x += w + theme::TAB_GAP;
    }
    out
}

/// Slot left edges for the current order (no scroll). Used by the reorder slide.
pub fn slot_xs(strip: Rect, widths: &[f32]) -> Vec<f32> {
    layout(strip, widths, 0.0)
        .into_iter()
        .map(|t| t.body.x)
        .collect()
}

/// How far the tab at `from` must travel to land in slot `to` (variable widths).
pub fn travel(widths: &[f32], from: usize, to: usize) -> f32 {
    if from == to || from >= widths.len() || to >= widths.len() {
        return 0.0;
    }
    // Distance between left edges if packed from 0 without scroll.
    let left = |i: usize| -> f32 { widths[..i].iter().sum::<f32>() + i as f32 * theme::TAB_GAP };
    left(to) - left(from)
}

/// The `+`, following the last tab until it reaches its lane, then anchored.
///
/// It **travels with the tabs**: a new document's tab appears to its left and
/// pushes it along, so the button stays where the row ends rather than sitting
/// alone at the far edge with a gulf in between. Once the tabs are wide enough
/// that it would collide with the window menu it stops at
/// [`lane`]'s trailing edge and the tabs scroll under*neath* nothing — the lane
/// is theirs to stop at, not to cross.
///
/// Always visible, in both states. Scrolling the strip left does not take the
/// `+` off the end of it.
pub fn new_tab(strip: Rect, widths: &[f32], scroll_x: f32) -> Rect {
    let w = theme::TAB_NEW_W.min(strip.w.max(0.0));
    let pad = (strip.h - theme::CAPTION_H.min(strip.h)) * 0.5;
    // Where it comes to rest: hard against the right of the strip, which is
    // where the lane ends.
    let anchored = strip.x + strip.w - w;
    let after_last = layout(strip, widths, scroll_x)
        .last()
        .map_or(strip.x + theme::TAB_GAP, |t| {
            t.body.x + t.body.w + theme::TAB_GAP
        });
    let x = after_last.min(anchored).max(strip.x);
    Rect::new(x, strip.y + pad, w, strip.h - pad * 2.0)
}

/// Clamp a carried tab's left edge so it cannot be dragged past the strip ends.
///
/// Bounded by the **lane**, not by the `+`'s current position: the button moves
/// as tabs are added, and a drag limit that moved with it would let a tab be
/// carried further the more tabs there were. The left bound is slot zero's
/// left edge, one [`TAB_GAP`](theme::TAB_GAP) in — where the tab will actually
/// rest. The clamp itself is [`reorder::carry`](crate::reorder::carry), which
/// the apps rail shares.
pub fn carry_x(strip: Rect, w: f32, x: f32) -> f32 {
    let lane = lane(strip);
    crate::reorder::carry(x, strip.x + theme::TAB_GAP, lane.x + lane.w, w)
}

/// Which index a tab being dragged should land at, given its centre `x`.
///
/// [`reorder::drop_index`](crate::reorder::drop_index) over the tabs' bodies:
/// the half-way rule is written once, and the apps rail reads it too.
pub fn drop_index(strip: Rect, widths: &[f32], scroll_x: f32, from: usize, x: f32) -> usize {
    let slots: Vec<(f32, f32)> = layout(strip, widths, scroll_x)
        .iter()
        .map(|t| (t.body.x, t.body.w))
        .collect();
    crate::reorder::drop_index(&slots, from, x)
}

/// Horizontal scrollbar track under the strip when content overflows.
pub fn scroll_track(strip: Rect, widths: &[f32]) -> Option<Rect> {
    if max_scroll(strip, widths) <= 0.0 {
        return None;
    }
    let h = theme::TAB_SCROLLBAR_H;
    Some(Rect::new(
        strip.x,
        strip.y + strip.h - h - 1.0,
        lane(strip).w,
        h,
    ))
}

/// Thumb inside [`scroll_track`], mapped from `scroll_x`.
pub fn scroll_thumb(strip: Rect, widths: &[f32], scroll_x: f32) -> Option<Rect> {
    let track = scroll_track(strip, widths)?;
    let max = max_scroll(strip, widths);
    if max <= 0.0 || track.w <= 0.0 {
        return None;
    }
    let content = content_width(widths);
    let thumb_w = (track.w * (lane(strip).w / content).clamp(0.15, 1.0))
        .max(24.0)
        .min(track.w);
    let travel = (track.w - thumb_w).max(0.0);
    let scroll = clamp_scroll(strip, widths, scroll_x);
    let x = track.x
        + if max > 0.0 {
            travel * (scroll / max)
        } else {
            0.0
        };
    Some(Rect::new(x, track.y, thumb_w, track.h))
}

/// Map a pointer x on the track to a scroll value (thumb-centre mapping).
pub fn scroll_from_pointer(strip: Rect, widths: &[f32], pointer_x: f32) -> f32 {
    let Some(track) = scroll_track(strip, widths) else {
        return 0.0;
    };
    let max = max_scroll(strip, widths);
    if max <= 0.0 || track.w <= 0.0 {
        return 0.0;
    }
    let content = content_width(widths);
    let thumb_w = (track.w * (lane(strip).w / content).clamp(0.15, 1.0))
        .max(24.0)
        .min(track.w);
    let travel = (track.w - thumb_w).max(0.0);
    if travel <= 0.0 {
        return 0.0;
    }
    let ratio = ((pointer_x - track.x - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
    ratio * max
}

/// Adjust scroll so tab `i` is fully visible in the strip (excluding the `+`).
pub fn ensure_visible(strip: Rect, widths: &[f32], scroll_x: f32, i: usize) -> f32 {
    if i >= widths.len() {
        return clamp_scroll(strip, widths, scroll_x);
    }
    let max = max_scroll(strip, widths);
    if max <= 0.0 {
        return 0.0;
    }
    // A minimised window has a zero-width tab strip. `widths` still describes
    // the logical tabs, but `layout` deliberately produces no drawable rects.
    // Keep the carried scroll value rather than indexing geometry that cannot
    // exist until the window is restored.
    let rects = layout(strip, widths, 0.0);
    let Some(t) = rects.get(i).map(|tab| tab.body) else {
        return clamp_scroll(strip, widths, scroll_x);
    };
    // Viewport for tabs is strip minus the pinned `+`.
    let view_w = (strip.w - theme::TAB_NEW_W).max(0.0);
    let view_left = scroll_x;
    let view_right = scroll_x + view_w;
    let tab_left = t.x - strip.x; // unscrollled offset from strip start
    let tab_right = tab_left + t.w;
    let mut scroll = scroll_x;
    if tab_left < view_left {
        scroll = tab_left;
    } else if tab_right > view_right {
        scroll = tab_right - view_w;
    }
    clamp_scroll(strip, widths, scroll)
}

fn pill(body: Rect) -> Rect {
    let h = (body.h - theme::TAB_INSET_TOP - theme::TAB_INSET_BOTTOM).max(0.0);
    Rect::new(body.x, body.y + theme::TAB_INSET_TOP, body.w, h)
}

fn close_box(pill: Rect) -> Rect {
    if pill.w < theme::TAB_CLOSE * 3.0 || pill.h < theme::TAB_CLOSE {
        return Rect::new(pill.x, pill.y, 0.0, 0.0);
    }
    let pad_y = (pill.h - theme::TAB_CLOSE) * 0.5;
    Rect::new(
        pill.x + pill.w - theme::TAB_CLOSE - theme::PANEL_PAD,
        pill.y + pad_y,
        theme::TAB_CLOSE,
        theme::TAB_CLOSE,
    )
}

// --------------------------------------------------------------------- drawing

/// The hover chip behind a caption icon button — the `+`, the pane's toggle.
///
/// **The same rect a tab's pill occupies**, so a row of chips lines up with the
/// tabs beside them instead of each control picking its own size. It is inset
/// by the tab insets rather than filling the row, for the reason
/// [`theme::TAB_INSET_BOTTOM`] exists: nothing in this strip may sit *on* the
/// hairline under it. Square, and centred in whatever slot it is given.
pub fn hover_chip(slot: Rect) -> Rect {
    koru_ui::icon_button::hover_chip(slot)
}

/// Draw the strip. Called after the caption's background, before its buttons.
///
/// `hovered` is the tab under the pointer, if any — resolved by the container
/// through [`crate::regions::hit`] rather than recomputed here, so the highlight
/// and the click can never disagree about which tab is which. `hovered_plus` is
/// the same answer for the `+`.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    strip: Rect,
    tabs: &[Tab<'_>],
    widths: &[f32],
    scroll_x: f32,
    active: usize,
    hovered: Option<usize>,
    hovered_close: Option<usize>,
    hovered_plus: bool,
    // Thin overflow thumb — only while the pointer is over the strip (or the
    // thumb is being dragged). Hidden otherwise so labels stay clean.
    show_scrollbar: bool,
    // Darker thumb while the pointer is on it (or dragging); lighter otherwise,
    // same as the document scrollbar.
    scrollbar_active: bool,
) {
    let rects = layout(strip, widths, scroll_x);
    let _inspect_strip = koru_draw::inspect::scope(
        "Tab strip",
        "container",
        root,
        scene::rect(strip.x, strip.y, strip.w, strip.h),
        |r| {
            r.property(
                "layout",
                "Row / horizontal",
                "koru_shell_chrome::tabs::draw",
            );
            r.property("scroll x", scroll_x, "current strip state");
        },
    );
    // Clipped to the **lane**, not the strip: the `+`'s lane is the one piece of
    // the row tabs may not enter, and clipping is what enforces it. Drawing the
    // `+` on top of an unclipped strip only hid the overlap — a tab's label ran
    // under the button and reappeared either side of it.
    let lane = lane(strip);
    let inspect_clip = koru_draw::inspect::clip(root, scene::rect(lane.x, lane.y, lane.w, lane.h));
    scene.push_clip_layer(
        vello::peniko::Fill::NonZero,
        root,
        &scene::rect(lane.x, lane.y, lane.w, lane.h),
    );
    for t in &rects {
        if !tabs.get(t.index).is_some_and(|tab| tab.dragging) {
            draw_tab(scene, root, engine, t, tabs, active, hovered, hovered_close);
        }
    }

    // `+` is outside the scrolled content clip intent: draw after pop so it
    // stays on top and is never clipped by a tab slide.
    scene.pop_layer();
    drop(inspect_clip);

    let plus = new_tab(strip, widths, scroll_x);
    if plus.w > 0.0 {
        // The same chip the profile and workspace rows wear, and the pane's
        // toggle at the other end of the caption: one hover treatment for every
        // icon button in this row, so none of them looks like a different kind
        // of control.
        koru_ui::icon_button::draw(
            scene,
            root,
            engine,
            plus,
            icons::PLUS,
            koru_ui::icon_button::State {
                hovered: hovered_plus,
                ..Default::default()
            },
        );
    }

    // Dragged tab on top, clipped to the same lane — `carry_x` already stops it
    // entering the `+`'s, so this only guards the strip's own edges.
    let inspect_clip = koru_draw::inspect::clip(root, scene::rect(lane.x, lane.y, lane.w, lane.h));
    scene.push_clip_layer(
        vello::peniko::Fill::NonZero,
        root,
        &scene::rect(lane.x, lane.y, lane.w, lane.h),
    );
    for t in &rects {
        if tabs.get(t.index).is_some_and(|tab| tab.dragging) {
            draw_tab(scene, root, engine, t, tabs, active, hovered, hovered_close);
        }
    }
    scene.pop_layer();
    drop(inspect_clip);

    if show_scrollbar && let Some(thumb) = scroll_thumb(strip, widths, scroll_x) {
        scene::fill(
            scene,
            root,
            theme::scrollbar_thumb_color(scrollbar_active),
            &scene::rrect(thumb.x, thumb.y, thumb.w, thumb.h, thumb.h * 0.5),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tab(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    t: &TabRect,
    tabs: &[Tab<'_>],
    active: usize,
    hovered: Option<usize>,
    hovered_close: Option<usize>,
) {
    let p = theme::p();
    let Some(tab) = tabs.get(t.index) else {
        return;
    };
    if t.pill.w <= 0.0 || t.pill.h <= 0.0 {
        return;
    }
    // The chip ends where the pin group's gap begins; see `Tab::gap_after`.
    let pill = Rect::new(
        t.pill.x + tab.slide,
        t.pill.y,
        (t.pill.w - tab.gap_after).max(0.0),
        t.pill.h,
    );
    let _inspect_tab = koru_draw::inspect::scope(
        "Tab (legacy shared)",
        "container",
        root,
        scene::rect(pill.x, pill.y, pill.w, pill.h),
        |r| {
            r.property(
                "layout",
                "Row / horizontal; vertically centered label",
                "tabs::draw_tab",
            );
            r.property("horizontal padding", theme::PANEL_PAD, "theme::PANEL_PAD");
            r.property(
                "icon gap",
                4,
                "local literal in shared tab recipe; pending token review",
            );
            r.property(
                "font size / line height",
                format!(
                    "{} / {}",
                    theme::role::row().metrics.size,
                    theme::role::row().metrics.line_height
                ),
                "theme::role::row()",
            );
            r.property("requested family", theme::ui_stack(), "theme::ui_stack()");
            r.property(
                "requested weight",
                theme::role::row().weight.value(),
                "theme::role::row()",
            );
        },
    );
    let selected = t.index == active;
    let hot = hovered == Some(t.index) || tab.dragging;
    let chip = scene::rrect(pill.x, pill.y, pill.w, pill.h, 6.0);
    if selected {
        // **The active tab wears the explore column's active card** — the same
        // `panel_bg` fill and `panel_border` outline `explore::draw_row_card`
        // gives the open document, at the same radius. The two say the same
        // thing in two places, so they are drawn the same way: the row in the
        // library and the tab in the strip are one document, and a reader
        // should not have to learn that twice.
        //
        // This *replaces* "one grey for the whole strip": the active tab shared
        // `hover_tint` with every hovered control, which made the thing you are
        // looking at indistinguishable from the thing under your pointer.
        scene::fill(scene, root, p.panel_bg, &chip);
        scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &chip);
    } else if hot {
        // A merely hovered tab keeps the caption's one hover grey, shared with
        // the `+` and the pane toggle.
        scene::fill(scene, root, p.hover_tint, &chip);
    }

    let close_w = if tab.closable && t.close.w > 0.0 {
        theme::TAB_CLOSE + theme::PANEL_PAD
    } else {
        0.0
    };
    let icon_w = if tab.icon.is_some() {
        crate::PAGE_ICON_SLOT + 4.0
    } else {
        0.0
    };
    let text_x = pill.x + theme::PANEL_PAD + icon_w;
    let text_w = (pill.w - theme::PANEL_PAD * 2.0 - close_w - icon_w).max(0.0);
    let icon_x = icon_slot_x(pill, tab.is_icon_only());
    let colour = if selected { p.text } else { p.text_dim };
    let label = if text_w > 0.0 && !tab.label.is_empty() {
        Some(engine.simple_layout_wrapped(tab.label, theme::role::row(), text_w))
    } else {
        None
    };
    let line_h = label
        .as_ref()
        .and_then(|l| l.lines().next().map(|line| line.metrics().line_height))
        .unwrap_or(0.0);
    let y = if line_h > 0.0 {
        pill.y + (pill.h - line_h) * 0.5
    } else {
        pill.y
    };
    let label_bl = label
        .as_ref()
        .and_then(|l| l.lines().next().map(|line| y + line.metrics().baseline));
    // A working chat's ring takes the icon's slot; the icon comes back when
    // the work ends (koru-ai D83).
    let icon_slot = Rect::new(icon_x, pill.y, crate::PAGE_ICON_SLOT, pill.h);
    let spinning = matches!(
        tab.status,
        Some(crate::status_mark::StatusMark::Spinner { .. })
    );
    if let Some(mark @ crate::status_mark::StatusMark::Spinner { .. }) = tab.status {
        crate::status_mark::draw(scene, root, icon_slot, mark);
    }
    match tab.icon.filter(|_| !spinning) {
        Some(TabIcon::Emoji(icon)) => {
            crate::draw_page_emoji(scene, root, engine, icon, icon_x, pill, label_bl)
        }
        Some(TabIcon::Glyph(glyph)) => draw_icon(
            scene,
            root,
            engine,
            glyph,
            Rect::new(icon_x, pill.y, crate::PAGE_ICON_SLOT, pill.h),
            colour,
        ),
        Some(TabIcon::Swatch(token)) => {
            // Centred in the same slot a glyph gets, at the size the label's
            // own line reads at rather than a constant: a tab is the one place
            // this mark sits beside text of the strip's size, and a square
            // fixed in pixels would drift from it the first time density moved.
            let slot = Rect::new(icon_x, pill.y, crate::PAGE_ICON_SLOT, pill.h);
            let side = (slot.w * 0.55).max(6.0);
            let fill = token
                .and_then(theme::accent_tone)
                .map(|tone| tone.swatch)
                .unwrap_or(p.text_dim);
            scene::fill(
                scene,
                root,
                fill,
                &scene::rrect(
                    slot.x + (slot.w - side) * 0.5,
                    slot.y + (slot.h - side) * 0.5,
                    side,
                    side,
                    side * 0.25,
                ),
            );
        }
        // The same slot a glyph centres in, at the same CAPTION_ICON size, so
        // a strip mixing brand and glyph tabs reads as one row of icons.
        Some(TabIcon::Brand(brand)) => scene::draw_brand_icon(
            scene,
            root,
            Rect::new(
                icon_x + (crate::PAGE_ICON_SLOT - theme::CAPTION_ICON) * 0.5,
                pill.y,
                theme::CAPTION_ICON,
                pill.h,
            ),
            colour,
            brand,
        ),
        None => {}
    }
    // The dot sits on the icon's corner, over the mark at its visible size.
    if let Some(crate::status_mark::StatusMark::Dot(dot)) = tab.status {
        let side = theme::CAPTION_ICON;
        let mark = Rect::new(
            icon_slot.x + (icon_slot.w - side) * 0.5,
            icon_slot.y + (icon_slot.h - side) * 0.5,
            side,
            side,
        );
        crate::status_mark::draw_badge(scene, root, mark, dot);
    }
    if let Some(label) = &label {
        scene::draw_layout_first_line(scene, root, label, text_x, y, Some(colour));
    }

    if tab.closable && hot && !tab.dragging && t.close.w > 0.0 {
        let colour = if hovered_close == Some(t.index) {
            p.text
        } else {
            p.text_dim
        };
        let close = Rect::new(t.close.x + tab.slide, t.close.y, t.close.w, t.close.h);
        draw_icon(scene, root, engine, icons::WINDOW_CLOSE, close, colour);
    }
}

pub fn draw_icon(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    glyph: char,
    rect: Rect,
    colour: peniko::Color,
) {
    draw_icon_weight(scene, root, engine, glyph, rect, colour, false);
}

/// ...with the option of Phosphor's **Fill** weight, for a state that is
/// genuinely on. Same rect and the same optical centre, so a glyph filling in
/// does not also move.
pub fn draw_icon_filled(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    glyph: char,
    rect: Rect,
    colour: peniko::Color,
) {
    draw_icon_weight(scene, root, engine, glyph, rect, colour, true);
}

fn draw_icon_weight(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    glyph: char,
    rect: Rect,
    colour: peniko::Color,
    filled: bool,
) {
    let layout = if filled {
        engine.icon_layout_filled(glyph, theme::CAPTION_ICON)
    } else {
        engine.icon_layout(glyph, theme::CAPTION_ICON)
    };
    let (w, h) = (layout.width(), layout.height());
    scene::draw_layout(
        scene,
        root,
        &layout,
        rect.x + (rect.w - w) * 0.5,
        rect.y + (rect.h - h) * 0.5,
        Some(colour),
    );
}

#[cfg(test)]
mod tests;
