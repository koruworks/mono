// SPDX-License-Identifier: MPL-2.0

//! Cutting a window into five: the lane at the top of the pane, the caption
//! row, the two side columns, and the active view between them.
//!
//! One function does the arithmetic and one function reads it back, which is
//! `DESIGN.md`'s rule for chrome and the reason the document toolbar has
//! `buttons()` and `hit()` rather than two copies of the same offsets.

use koru_draw::geometry::Rect;
use koru_draw::theme;

use crate::{banner, caption, lane, tabs};

/// The regions of a container window, in logical pixels.
///
/// A collapsed column is a **zero-width rect**, not an `Option` and not an
/// absence: every caller then does the same arithmetic whether the column is
/// open, shut, or halfway through sliding, and there is no second code path to
/// get wrong at the ends of an animation.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Regions {
    /// The window's own band at the top of the pane: the mark or the lights,
    /// Search, New and the sidebar toggle
    /// (`docs/features/workspace-foundation/TECHNICAL.final.md` §5.1). The
    /// top `CAPTION_H` of the sidebar, and zero-width when the pane is shut —
    /// its controls then fold into the caption's leading chain.
    pub lane: Rect,
    /// The strip along the top of the content. Carries the leading chain, the
    /// tabs, and — once the window is undecorated — the minimise/maximise/close
    /// buttons at its right end.
    pub caption: Rect,
    /// The explore column, on the left. The lane is its top band.
    pub sidebar: Rect,
    /// The active tab's view.
    pub content: Rect,
    /// The context column, on the right.
    pub context: Rect,
    /// The lane at the window's right edge, below the caption and above the
    /// foot, that no column reaches into: zero-width unless [`with_ground`]
    /// carved it.
    ///
    /// [`with_ground`]: Self::with_ground
    pub edge: Rect,
    /// The lane along the window's foot, the full width, that no column
    /// reaches into: zero-height unless [`with_ground`] carved it.
    ///
    /// [`with_ground`]: Self::with_ground
    pub foot: Rect,
}

impl Regions {
    /// Carve the ground's two lanes, `w` wide: one off the right edge below
    /// the caption, one along the foot. The content gives up the right lane
    /// and a context column keeps its width and shifts left, so `CONTEXT_W`
    /// stays true; every column gives up the foot. The caption keeps its
    /// full width — the window buttons sit over the lane, which is theirs to
    /// sit over.
    pub fn with_ground(mut self, w: f32) -> Self {
        let w = w.max(0.0).min(self.content.w).min(self.content.h);
        self.content.w -= w;
        self.context.x -= w;
        self.sidebar.h -= w;
        self.content.h -= w;
        self.context.h -= w;
        self.edge = Rect::new(
            self.context.x + self.context.w,
            self.content.y,
            w,
            self.content.h,
        );
        let bottom = self.content.y + self.content.h;
        self.foot = Rect::new(0.0, bottom, self.edge.x + self.edge.w, w);
        self
    }
}

/// Cut a `w` × `h` window given the two columns' current widths.
///
/// Takes **widths rather than open/shut flags** so that sliding a column open is
/// nothing but interpolating a number, and this function stays pure and
/// testable rather than growing an animation clock.
///
/// Content is protected. When the window is too narrow to give everyone their
/// width, the context column is dropped first and the sidebar second, because
/// the thing being edited is never what should be squeezed. Below that, content
/// simply runs out of room — a window that small is mid-drag or mid-restore, and
/// the only requirement is that every rect stays finite and non-negative.
pub fn solve(w: f32, h: f32, sidebar_w: f32, context_w: f32) -> Regions {
    let w = w.max(0.0);
    let h = h.max(0.0);
    let caption_h = theme::CAPTION_H.min(h);

    // Ask for what was wanted, then give way in order.
    let mut sw = sidebar_w.max(0.0).min(w);
    let mut cw = context_w.max(0.0).min(w);
    if w - sw - cw < theme::CONTENT_MIN_W {
        cw = (w - sw - theme::CONTENT_MIN_W).max(0.0).min(cw);
    }
    if w - sw - cw < theme::CONTENT_MIN_W {
        sw = (w - cw - theme::CONTENT_MIN_W).max(0.0).min(sw);
    }
    // Still too narrow: the columns are already gone, so content takes what is
    // left rather than going negative.
    let content_w = (w - sw - cw).max(0.0);
    let below = (h - caption_h).max(0.0);

    Regions {
        // **The lane and the caption share one band.** Same height, same
        // hairline at its foot, so the row reads as one whether the pane is
        // open or shut.
        lane: Rect::new(0.0, 0.0, sw, caption_h),
        // **The caption starts after the pane.** The explore column owns the
        // full height of its own strip, so the chain, the tabs and the window
        // buttons all shift right with it rather than sitting above it — which
        // is what makes the pane read as a place rather than a drawer.
        //
        // Everything in `caption` derives from `caption.x`, so `tab_strip`,
        // `context_toggle` and `koru_ui::caption::controls` follow from this
        // one line.
        caption: Rect::new(sw, 0.0, (w - sw).max(0.0), caption_h),
        sidebar: Rect::new(0.0, 0.0, sw, h),
        content: Rect::new(sw, caption_h, content_w, below),
        context: Rect::new(w - cw, caption_h, cw, below),
        edge: Rect::new(w, caption_h, 0.0, below),
        foot: Rect::new(0.0, h, w, 0.0),
    }
}

/// What is under a window-space point.
///
/// Ordered by precedence rather than by position: the caption's furniture is
/// small and sits inside the strip, so each piece claims itself before the
/// strip claims what is left.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    /// The explore column, below its lane. Which row is the column's own
    /// question.
    Sidebar,
    /// The context column.
    Context,
    /// The active view.
    Content,

    Tab(usize),
    /// The close box inside a tab. Outranks [`Hit::Tab`], which is why it is a
    /// variant of its own rather than a flag on one.
    TabClose(usize),
    NewTab,
    /// Horizontal scrollbar under a crowded tab strip.
    TabScroll,

    ToggleSidebar,
    ToggleContext,
    /// The lane's Search — in the lane while the pane is open, in the
    /// caption's leading chain while it is shut.
    Search,
    /// The lane's New, likewise. What it creates is the host's business.
    New,
    /// An optional product-owned caption action. Chrome owns only its slot.
    HostAction,

    /// The caption or the lane, but none of its furniture — the part you drag
    /// the window by. The mark and the lights' reserve answer this too: a logo
    /// is not a verb.
    Caption,
    /// A host-neutral control from `koru-ui`; the container maps it to its
    /// own window request or menu behavior.
    Window(koru_ui::caption::Action),
}

/// Resolve a window-space point for a window with a lane.
///
/// `window_buttons` is false while the OS still draws its own titlebar, so the
/// right end of the caption is empty space rather than three buttons.
/// `context` is false while the context column is not offered, and its toggle
/// is then a zero-width rect the tab strip has already taken. `leading` says
/// which lane controls the window offers, and is read in both of the places
/// they can be. All are parameters rather than build flags, because which
/// furniture a window has is the container's decision — and because the tests
/// drive every combination.
#[allow(clippy::too_many_arguments)]
pub fn hit(
    r: &Regions,
    tab_widths: &[f32],
    tab_scroll: f32,
    window_buttons: bool,
    context: bool,
    leading: lane::Leading,
    // When true, reserve and hit-test one host-owned action immediately before
    // the window controls.
    host_action: bool,
    // When false, the overflow thumb is hidden and must not steal hits from
    // the tabs sitting above its band.
    tab_scrollbar: bool,
    x: f32,
    y: f32,
) -> Hit {
    resolve(
        r,
        tab_widths,
        tab_scroll,
        window_buttons,
        context,
        leading,
        host_action,
        tab_scrollbar,
        banner::Slots::NONE,
        x,
        y,
    )
}

/// The same, for a window whose caption carries a banner and no lane.
///
/// The banner's own slots are **not** resolved here: they are asked first, by
/// the container, through [`banner::hit`], which is the precedence
/// `DESIGN.md` states (window controls → sidebar toggle → profile → workspace →
/// host action → …). Nothing overlaps, so asking the banner before this is the
/// same answer as a fifth arm inside it — and it keeps `Hit` free of variants
/// that mean nothing to a window with no banner. What this needs the slots for
/// is the *lane*: the tabs begin at [`banner::end`], and an index resolved
/// against the wrong start would select a different tab from the one under the
/// pointer.
#[allow(clippy::too_many_arguments)]
pub fn hit_with_banner(
    r: &Regions,
    tab_widths: &[f32],
    tab_scroll: f32,
    window_buttons: bool,
    context: bool,
    host_action: bool,
    tab_scrollbar: bool,
    slots: banner::Slots,
    x: f32,
    y: f32,
) -> Hit {
    resolve(
        r,
        tab_widths,
        tab_scroll,
        window_buttons,
        context,
        lane::Leading::NONE,
        host_action,
        tab_scrollbar,
        slots,
        x,
        y,
    )
}

/// One resolver behind both entry points. A banner window has no lane
/// (`Leading::NONE`) and a lane window has no banner (`Slots::NONE`); the
/// arithmetic is the same either way, so there is one copy of it.
#[allow(clippy::too_many_arguments)]
fn resolve(
    r: &Regions,
    tab_widths: &[f32],
    tab_scroll: f32,
    window_buttons: bool,
    context: bool,
    leading: lane::Leading,
    host_action: bool,
    tab_scrollbar: bool,
    slots: banner::Slots,
    x: f32,
    y: f32,
) -> Hit {
    // Caption first, and that ordering is now doing real work in both
    // directions. With the pane **open** its left edge is `sidebar_w`, so a
    // point in the top-left fails this test and falls through to the lane —
    // which is the wanted answer, since the lane is what is drawn there. With
    // the pane **shut** the sidebar and the lane are zero-width rects, and
    // `contains` is inclusive on every edge, so asking either first would
    // swallow `x == 0` for the whole height of the window and the toggle would
    // stop responding.
    //
    // At exactly `x == sidebar_w` both rects contain the point and the caption
    // wins, which matches what the toggle did at `x == 0` before: the edge of a
    // small control belongs to the control.
    if r.caption.contains(x, y) {
        if window_buttons {
            if let Some(action) = koru_ui::caption::hit(r.caption, x, y) {
                return Hit::Window(action);
            }
        }
        // The leading chain, left to right. The mark and the divider are not
        // in it: a press on either is a drag.
        let chain = caption::leading_chain(r.caption, leading);
        let owns = |rect: Rect| rect.w > 0.0 && rect.contains(x, y);
        if owns(chain.toggle) {
            return Hit::ToggleSidebar;
        }
        if owns(chain.search) {
            return Hit::Search;
        }
        if owns(chain.new) {
            return Hit::New;
        }
        let host = caption::host_action_with_banner(
            r.caption,
            window_buttons,
            context,
            host_action,
            slots,
        );
        if owns(host) {
            return Hit::HostAction;
        }
        // A hidden toggle is a zero-width rect sitting on the strip's right
        // edge, and `contains` is inclusive — so it has to be excluded here
        // explicitly, exactly as a collapsed column is below.
        //
        // **The banner is what the toggle's rect depends on**, because Settings
        // sits outside it (`caption::context_toggle_with_banner`). The container
        // still asks `banner::hit` before this, so the Settings square itself
        // never reaches here.
        let context_toggle =
            caption::context_toggle_with_banner(r.caption, window_buttons, context, slots);
        if owns(context_toggle) {
            return Hit::ToggleContext;
        }
        // The lane begins after whichever leading furniture this window has:
        // the chain for a lane window, the banner for a banner window. With
        // neither, both answers are the sidebar toggle's far edge.
        let strip = if slots == banner::Slots::NONE {
            caption::tab_strip_with_leading(
                r.caption,
                window_buttons,
                context,
                host_action,
                leading,
            )
        } else {
            caption::tab_strip_with_banner(r.caption, window_buttons, context, host_action, slots)
        };
        if tabs::new_tab(strip, tab_widths, tab_scroll).contains(x, y) {
            return Hit::NewTab;
        }
        if tab_scrollbar && tabs::scroll_track(strip, tab_widths).is_some_and(|t| t.contains(x, y))
        {
            return Hit::TabScroll;
        }
        // A tab is only hittable where it is *drawn*, and it is drawn clipped to
        // the lane — so a point in the `+`'s lane belongs to no tab even when a
        // scrolled tab's rect reaches into it.
        let view = tabs::lane(strip);
        let view_right = view.x + view.w;
        if x >= view_right {
            return Hit::Caption;
        }
        for t in tabs::layout(strip, tab_widths, tab_scroll) {
            if t.body.x + t.body.w <= strip.x || t.body.x >= view_right {
                continue;
            }
            // The close box is inside the tab body, so it has to be asked first.
            if t.close.contains(x, y) {
                return Hit::TabClose(t.index);
            }
            if t.body.contains(x, y) {
                return Hit::Tab(t.index);
            }
        }
        return Hit::Caption;
    }
    // The lane: the pane's top band, asked before the pane because it sits on
    // it. Its empty part — and the mark — drag the window like the caption.
    if r.lane.w > 0.0 && r.lane.contains(x, y) {
        return match lane::hit(r.lane, leading, x, y) {
            Some(lane::Hit::Toggle) => Hit::ToggleSidebar,
            Some(lane::Hit::Search) => Hit::Search,
            Some(lane::Hit::New) => Hit::New,
            None => Hit::Caption,
        };
    }
    // A zero-width column cannot be hit: `contains` is inclusive on every edge,
    // so a collapsed sidebar would otherwise swallow the content's left edge.
    if r.sidebar.w > 0.0 && r.sidebar.contains(x, y) {
        return Hit::Sidebar;
    }
    if r.context.w > 0.0 && r.context.contains(x, y) {
        return Hit::Context;
    }
    Hit::Content
}

#[cfg(test)]
mod tests;
