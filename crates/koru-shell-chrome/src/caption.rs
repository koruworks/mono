// SPDX-License-Identifier: MPL-2.0

//! The caption row's own furniture: the two panel toggles, the band the tabs
//! run in, and the window buttons.
//!
//! ```text
//! [≡] [ Welcome ][ Product brief ✕ ][ + ]                  📌   ─   □   ✕
//!  ^                    ^                                  ^^^^^^^^^^^^^
//!  sidebar_toggle    tab_strip                              window_buttons
//! ```
//!
//! ...or, with the experimental window menu on, which is the default:
//!
//! ```text
//! [≡] [ Welcome ][ Product brief ✕ ][ + ]                            ⋮
//!                                                                    ^
//!                                                        window_buttons
//! ```
//!
//! The context toggle sits between the tabs and the window buttons whenever it
//! is offered; a window that offers neither leaves a zero-width rect there and
//! draws nothing.
//!
//! **The leading furniture is a left-to-right chain from `caption.x`** — the
//! lane's controls, folded into the caption while the pane is shut
//! ([`leading_chain`]; `docs/features/workspace-foundation/TECHNICAL.final.md`
//! §5.1):
//!
//! ```text
//! [K] [≡] [🔍] [+] |  tabs …
//!  ^   ^    ^    ^  ^
//!  mark toggle search new divider
//! ```
//!
//! With [`crate::lane::Leading::NONE`] every link but the toggle is
//! zero-width, the divider too, and the chain is the sidebar toggle alone —
//! exactly the caption every window had before the lane existed.
//!
//! **The trailing furniture is one right-to-left chain**, and [`trailing_edge`]
//! is where it starts. Each rect takes its right edge from its neighbour's `.x`:
//!
//! ```text
//! window buttons ← settings slot ← context toggle ← host action ← lane
//! ```
//!
//! The trailing search slot that used to sit between the host action and the
//! lane is gone: search is a leading control now, in the lane or in the
//! chain, and a window has one search button rather than two slots for it.
//!
//! The banner's Settings slot is the outermost of them (`banner::settings_slot`),
//! which is why every function below that has to know where the lane begins takes
//! [`banner::Slots`]: with [`banner::Slots::NONE`] the settings slot is
//! zero-width, the chain collapses onto the window buttons, and every rect is
//! what it was before any of this existed.
//!
//! `window_buttons` and `context` are **parameters, not build flags**: which
//! furniture a window offers is the container's decision, and this crate does
//! not have opinions about product surface. Both are also what the tests drive
//! to cover the shapes at once.
//!
//! Every rect here is derived from the caption alone, so drawing and
//! hit-testing ask the same functions and cannot disagree.

use koru_draw::geometry::Rect;
use koru_draw::theme;

use crate::{banner, lane};

/// The panel toggles are square and the height of the strip; the window buttons
/// are wider, as they are on Windows, so the pointer can slam into the
/// top-right corner and still land on close.
fn toggle_w(caption: Rect) -> f32 {
    caption.h.min(theme::CAPTION_H)
}

/// Collapse or expand the explore pane. Leftmost in the caption — which, once
/// the pane is open, *begins after it*, so the logo travels right with the tabs
/// rather than sitting above the pane it opened.
///
/// Nothing here assumes the caption starts at the window's left edge, and that
/// is the whole reason this file is one function per rect: the pane moved and
/// not a line of arithmetic below changed.
pub fn sidebar_toggle(caption: Rect) -> Rect {
    let w = toggle_w(caption).min(caption.w);
    Rect::new(caption.x, caption.y, w, caption.h)
}

/// Where the caption's trailing furniture starts: the window controls' left
/// edge, or the caption's own right edge while the OS still draws its titlebar.
///
/// **The one anchor of the right-to-left chain.** Everything after it derives
/// from a neighbour rather than recomputing this, so a slot inserted into the
/// chain moves exactly the rects to its left and nothing else.
pub fn trailing_edge(caption: Rect, window_buttons: bool) -> f32 {
    caption.x + caption.w - buttons_w(caption, window_buttons)
}

/// Collapse or expand the context column. Right-aligned, inside the window
/// buttons when the window draws its own.
///
/// `show` is false while the context column is not offered, and the answer is
/// then a **zero-width rect** rather than an absence — the same idiom a
/// collapsed column uses, and for the same reason: [`tab_strip`] does the same
/// arithmetic either way and reclaims the space with no second code path. A
/// zero-width rect has to be excluded from hit-testing explicitly, because
/// `Rect::contains` is inclusive on every edge.
///
/// This overload is the no-banner one; a window that offers **both** Settings
/// and a context column wants [`context_toggle_with_banner`].
pub fn context_toggle(caption: Rect, window_buttons: bool, show: bool) -> Rect {
    context_toggle_with_banner(caption, window_buttons, show, banner::Slots::NONE)
}

/// **The context toggle in a window that also offers the banner's Settings
/// slot** — the first window that does is Koru Terminal (SP-12).
///
/// `banner::settings_slot` keeps the rect against the window controls and the
/// context toggle takes the square immediately to its left. The order is not
/// arbitrary and it is not DESIGN's drawing being ignored: the context toggle is
/// offered **only while the column has content** (DESIGN, *"The context column's
/// caption toggle is hidden"*), so it appears and disappears as the active tab
/// changes. Anchoring Settings instead would move Settings 36 px on every switch
/// between a Welcome tab and a session — a control jumping under the pointer on
/// an ordinary action. This way the only thing that moves is the lane's right
/// edge, which moves whenever a tab opens anyway.
///
/// The collapse ladder's step 4 (the settings slot goes to zero) does then move
/// the context toggle right by 36 — but that step is reached only at the very
/// bottom of a resize, where the whole caption is already in motion.
pub fn context_toggle_with_banner(
    caption: Rect,
    window_buttons: bool,
    show: bool,
    slots: banner::Slots,
) -> Rect {
    let w = if show { toggle_w(caption) } else { 0.0 };
    let right = banner::settings_slot(caption, window_buttons, slots).x;
    Rect::new((right - w).max(caption.x), caption.y, w, caption.h)
}

/// One optional product-owned action immediately before the overflow button
/// when no Context control is present.
///
/// Chrome owns only the slot. The host chooses its glyph and meaning, which
/// keeps this crate independent of documents, workspaces, and product views.
pub fn host_action(caption: Rect, window_buttons: bool, show: bool) -> Rect {
    host_action_with_context(caption, window_buttons, false, show)
}

/// Product action immediately left of Context when both are present.
pub fn host_action_with_context(
    caption: Rect,
    window_buttons: bool,
    context: bool,
    show: bool,
) -> Rect {
    host_action_with_banner(caption, window_buttons, context, show, banner::Slots::NONE)
}

/// ...and the same once the banner's trailing slots have taken their room.
pub fn host_action_with_banner(
    caption: Rect,
    window_buttons: bool,
    context: bool,
    show: bool,
    slots: banner::Slots,
) -> Rect {
    let w = if show { toggle_w(caption) } else { 0.0 };
    let right = context_toggle_with_banner(caption, window_buttons, context, slots).x;
    Rect::new((right - w).max(caption.x), caption.y, w, caption.h)
}

/// The context toggle remains immediately before the window controls. A
/// product action, when present, sits to its left.
pub fn context_toggle_with_host_action(
    caption: Rect,
    window_buttons: bool,
    show: bool,
    _host_action_shown: bool,
) -> Rect {
    context_toggle(caption, window_buttons, show)
}

/// The lane's controls, folded into the caption's left end while the pane is
/// shut, in the canvas's order: leading · toggle · search · new · divider.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Chain {
    /// The mark, or the lights' reserve. Not a button: a press here drags.
    pub leading: Rect,
    pub toggle: Rect,
    pub search: Rect,
    pub new: Rect,
    /// A thin rule between the chain and the tabs; zero-width when the chain
    /// is the toggle alone, so the old caption is unchanged to the pixel.
    pub divider: Rect,
    /// Where the chain ends and the tab lane may begin.
    pub end: f32,
}

/// The divider between the folded chain and the tabs: a hairline with a
/// square's worth of air either side would read as a gap; one square's width
/// with the rule drawn at its centre keeps the tabs' pitch.
pub const DIVIDER_W: f32 = 9.0;

/// **The leading chain.** Each rect takes its left edge from its neighbour's
/// right, starting at `caption.x`; a slot the window does not offer is
/// zero-width at its place. With [`lane::Leading::NONE`] this is
/// [`sidebar_toggle`] and nothing else, which is what every pre-lane test
/// still asserts.
pub fn leading_chain(caption: Rect, leading: lane::Leading) -> Chain {
    let s = toggle_w(caption);
    let right = caption.x + caption.w;
    let mut x = caption.x;
    let mut take = |w: f32| {
        let w = w.min((right - x).max(0.0));
        let r = Rect::new(x, caption.y, w, caption.h);
        x += w;
        r
    };
    let leading_rect = take(lane::leading_w(caption, leading.mark));
    let toggle = take(s);
    let search = take(if leading.search { s } else { 0.0 });
    let new = take(if leading.new { s } else { 0.0 });
    let folded = leading.search || leading.new || leading.mark != lane::Mark::None;
    let divider = take(if folded { DIVIDER_W } else { 0.0 });
    Chain {
        leading: leading_rect,
        toggle,
        search,
        new,
        divider,
        end: x,
    }
}

/// Total width the window buttons occupy, or zero while the OS still draws its
/// own titlebar. Always the four — overflow, minimise, maximise, close — since
/// the experimental one-button caption was removed (ADR-150).
fn buttons_w(caption: Rect, window_buttons: bool) -> f32 {
    if window_buttons {
        koru_ui::caption::reserved_width(caption)
    } else {
        0.0
    }
}

/// The band the tabs and the `+` run in: what is left of the caption once both
/// toggles and the window buttons have taken their ends.
pub fn tab_strip(caption: Rect, window_buttons: bool, context: bool) -> Rect {
    tab_strip_with_host_action(caption, window_buttons, context, false)
}

/// The tab strip after an optional host-owned action takes its caption slot.
pub fn tab_strip_with_host_action(
    caption: Rect,
    window_buttons: bool,
    context: bool,
    host_action_shown: bool,
) -> Rect {
    tab_strip_with_banner(
        caption,
        window_buttons,
        context,
        host_action_shown,
        banner::Slots::NONE,
    )
}

/// **The lane begins after the banner, not after the sidebar toggle.**
///
/// The one arithmetic change the banner needed (`DESIGN.md`, "The banner" §1,
/// row 4 of the chain). With [`banner::Slots::NONE`] — every window that shows
/// no banner, which is every window today — [`banner::end`] *is* the sidebar
/// toggle's far edge, so the four overloads above are unchanged to the pixel and
/// their callers do not know this function exists.
///
/// What is left over is not clamped here. A lane narrower than
/// [`banner::LANE_RESERVE`] is the container's signal to walk its banner down a
/// collapse step ([`banner::fits`]), and clamping it would hide exactly the
/// condition the ladder watches for.
///
/// **The lane stops at the banner's Settings slot too**, which is DESIGN's own
/// arithmetic ("The banner" §4 marks `- settings slot 36` as *the lane's right
/// edge*). It did not before SP-12, because Settings *was* the context toggle's
/// rect and the toggle was never offered — so the lane ran the last 36 px
/// underneath the Settings icon and a scrolled tab could pass behind it.
pub fn tab_strip_with_banner(
    caption: Rect,
    window_buttons: bool,
    context: bool,
    host_action_shown: bool,
    slots: banner::Slots,
) -> Rect {
    let x = banner::end(caption, slots);
    let right =
        host_action_with_banner(caption, window_buttons, context, host_action_shown, slots).x;
    Rect::new(x, caption.y, (right - x).max(0.0), caption.h)
}

/// **The lane begins after the leading chain.** The workspace window's
/// strip: the chain's `end` is the sidebar toggle's far edge for
/// [`lane::Leading::NONE`], so this and [`tab_strip_with_host_action`] agree
/// to the pixel for a window that folds nothing into its caption.
pub fn tab_strip_with_leading(
    caption: Rect,
    window_buttons: bool,
    context: bool,
    host_action_shown: bool,
    leading: lane::Leading,
) -> Rect {
    let x = leading_chain(caption, leading).end;
    let right = host_action_with_banner(
        caption,
        window_buttons,
        context,
        host_action_shown,
        banner::Slots::NONE,
    )
    .x;
    Rect::new(x, caption.y, (right - x).max(0.0), caption.h)
}

#[cfg(test)]
mod tests;
