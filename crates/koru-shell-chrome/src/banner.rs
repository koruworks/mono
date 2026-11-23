// SPDX-License-Identifier: MPL-2.0

//! The banner slots: where the account profile, the workspace, and Settings
//! live in the caption row.
//!
//! ```text
//! [≡] [◯ profile][ Workspace ⌄ ]  [ tab ][ tab ✕ ][ + ]      [⚙]  ⋮  ─  □  ✕
//!       ^              ^                                      ^
//!       profile_slot   workspace_slot                         settings_slot
//! ```
//!
//! Chrome owns only the slots. Which profile is showing, what a workspace is,
//! and what opening Settings means are all the host's business
//! ([ADR-262](../../../docs/decisions/README.md)): a press here answers
//! [`Hit::Profile`], never a profile id. The popovers a slot opens are the
//! shell's, and their *rows* are the host's — this file is the arithmetic both
//! share.
//!
//! Every rect is derived from the caption alone, in the caption module's idiom:
//! a slot that is not offered is a **zero-width rect** rather than an absence,
//! so the neighbouring arithmetic reclaims the space with no second code path,
//! and hit-testing excludes it explicitly because `Rect::contains` is inclusive
//! on every edge.
//!
//! Since SP-12 the Settings slot is the outermost member of the caption's
//! trailing chain rather than an alias of the context toggle's rect: a window
//! may now offer both, and the toggle takes the square to Settings' left.
//!
//! ## The identity has a second home
//!
//! Owner, 2026-09-02: a window may put the profile and workspace selectors in
//! the **pane header band** instead — the `CAPTION_H` strip at the top of the
//! explore column, which is where Koru itself draws them.
//!
//! ```text
//! ┌──────────────────────┬──────────────────────────────────────────────
//! │ [◯ profile][ Notes ⌄]│ [≡]  [ tab ][ tab ✕ ][ + ]      [⚙]  ─  □  ✕
//! ├──────────────────────┼──────────────────────────────────────────────
//! │ PROJECTS          +  │
//! ```
//!
//! That is `pane_profile_slot` / `pane_workspace_slot`, and they are the same
//! arithmetic ([`identity_slots`]) laid out from the band's own left edge rather
//! than after the sidebar toggle. Settings does not move: it is caption
//! furniture. The ladder does not change either — only its budget, which is
//! [`pane_fits`] rather than [`fits`].
//!
//! ## Who gives way
//!
//! The tab lane is what the window is for, so the give-way order inside the
//! caption is the mirror of [`regions::solve`](crate::regions::solve)'s outside
//! it: the window controls, the sidebar toggle and then [`LANE_RESERVE`] — one
//! minimum tab, its gap and the `+` — are reserved *before* the banner takes
//! anything. [`fits`] answers whether a given set of slots leaves that much, and
//! the collapse ladder that walks a banner down until it does is the container's
//! (`DESIGN.md`, "The banner" §2): it is evaluated against the settled caption,
//! once per toggle, never per animation frame.

use crate::caption;
use koru_draw::geometry::Rect;
use koru_draw::theme;

/// The workspace slot never grows past this, however long the name: a banner is
/// furniture, and a novel of a workspace name is the name's problem. Measured
/// label widths are clamped here at draw *and* at hit, by the same function.
pub const WORKSPACE_MAX_W: f32 = 220.0;

/// The narrowest a *named* workspace slot goes before the ladder takes the name
/// away entirely — about 58 px of label plus its chevron
/// (`DESIGN.md`, "The banner" §2, step 1).
pub const WORKSPACE_MIN_W: f32 = 84.0;

/// What the tab lane keeps whatever the banner asks for: one minimum tab, the
/// gap after it, and the `+`.
///
/// Reserved whether or not the `+` is standing in the lane, which is what stops
/// a tab scrolling underneath it and keeps the reservation out of a feedback
/// loop.
pub const LANE_RESERVE: f32 = theme::TAB_MIN_W + theme::TAB_GAP + theme::TAB_NEW_W;

/// Breathing room inside the workspace slot: the label plus its chevron.
const WORKSPACE_PAD: f32 = 26.0;

/// Which banner slot a press landed in. The host maps these to meaning; chrome
/// has answered everything it knows.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    Profile,
    Workspace,
    Settings,
}

/// How the workspace slot is showing, which is the collapse ladder's middle
/// three steps expressed as geometry.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub enum Workspace {
    /// Not offered — a zero-width rect. Ladder step 3 (the merged identity
    /// slot) and a window with no workspace at all are the same arithmetic.
    #[default]
    Hidden,
    /// The name and its chevron, at this **measured** label width — measured at
    /// draw, cached by the container, never a `TextEngine` on the hit path. The
    /// clamp to [`WORKSPACE_MAX_W`] happens in
    /// [`workspace_slot`] and nowhere else.
    Named(f32),
    /// A square carrying the workspace's own icon or colour tile: no name, no
    /// chevron. Ladder step 2.
    IconOnly,
}

impl Workspace {
    /// The name clamped to [`WORKSPACE_MIN_W`] — collapse-ladder step 1, where
    /// the label gets an ellipsis rather than the slot getting narrower.
    ///
    /// A function rather than a fifth constant, so the padding it subtracts
    /// stays private to this file and cannot be guessed at wrong by a caller.
    pub fn truncated() -> Self {
        Self::Named(WORKSPACE_MIN_W - WORKSPACE_PAD)
    }
}

/// What the banner is showing in one window.
///
/// One carrier rather than three positional arguments, because every function
/// that has to know where the lane begins needs all of them and a fourth
/// `tab_strip_*` overload per flag is how the caption module got to five.
/// [`Slots::default`] is a window with no banner, and every rect then matches
/// exactly what it was before the banner existed.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Slots {
    pub profile: bool,
    pub workspace: Workspace,
    pub settings: bool,
}

impl Slots {
    /// A window that shows no banner. The tab lane then begins at the sidebar
    /// toggle's far edge, which is where it began before any of this existed.
    pub const NONE: Self = Self {
        profile: false,
        workspace: Workspace::Hidden,
        settings: false,
    };
}

/// The same stable width as the panel toggles: it holds one avatar-sized glyph.
fn slot_w(_caption: Rect) -> f32 {
    theme::CAPTION_CONTROL_W
}

/// **The two identity chips, laid out from `x` rightwards inside `band`.**
///
/// One arithmetic, two homes. The caption lays them out immediately after the
/// sidebar toggle; a **pane header band** lays them out at its own left edge,
/// which is where they live while the pane is open (`DESIGN.md`, "The banner"
/// §0). Two copies of this would be two clamps for one ellipsis, which is
/// exactly what §3 exists to forbid — so the second home is an argument, not a
/// second function.
///
/// The width a slot *wants* is [`wanted_identity_w`]; what it gets is clamped to
/// the room left in the band, because a slot that overhung its band would draw
/// outside the rect its own hit test uses.
fn identity_slots(band: Rect, x: f32, slots: Slots) -> (Rect, Rect) {
    let right = band.x + band.w;
    let profile_w = if slots.profile {
        slot_w(band).min((right - x).max(0.0))
    } else {
        0.0
    };
    let profile = Rect::new(x, band.y, profile_w, band.h);
    let workspace_x = profile.x + profile.w;
    let room = (right - workspace_x).max(0.0);
    let workspace_w = match slots.workspace {
        Workspace::Hidden => 0.0,
        Workspace::IconOnly => slot_w(band).min(room),
        Workspace::Named(label_w) => (label_w + WORKSPACE_PAD)
            .clamp(0.0, WORKSPACE_MAX_W)
            .min(room),
    };
    (profile, Rect::new(workspace_x, band.y, workspace_w, band.h))
}

/// What the two identity chips ask for, before any band clamps them.
///
/// The ladder's input on both sides: [`fits`] measures what is left of the
/// caption's lane, [`pane_fits`] measures whether this still fits the pane
/// header band.
fn wanted_identity_w(band: Rect, slots: Slots) -> f32 {
    let profile = if slots.profile { slot_w(band) } else { 0.0 };
    let workspace = match slots.workspace {
        Workspace::Hidden => 0.0,
        Workspace::IconOnly => slot_w(band),
        Workspace::Named(label_w) => (label_w + WORKSPACE_PAD).clamp(0.0, WORKSPACE_MAX_W),
    };
    profile + workspace
}

/// The account-profile slot: immediately after the sidebar toggle, leading the
/// banner the way the avatar leads the pane header it replaces.
pub fn profile_slot(caption: Rect, slots: Slots) -> Rect {
    let left = caption::sidebar_toggle(caption);
    identity_slots(caption, left.x + left.w, slots).0
}

/// The workspace slot: name plus chevron, immediately after the profile slot —
/// or where it would have been, when the profile slot is not offered (a
/// zero-width slot takes no space, same idiom as everything else here).
///
/// The clamp to [`WORKSPACE_MAX_W`] happens in [`identity_slots`] and nowhere
/// else, so drawing and hit-testing cannot disagree about where a long name
/// stops.
pub fn workspace_slot(caption: Rect, slots: Slots) -> Rect {
    let left = caption::sidebar_toggle(caption);
    identity_slots(caption, left.x + left.w, slots).1
}

// ------------------------------------------------------- the pane header band
//
// The identity's other home. `DESIGN.md`, "The banner" §0 (owner, 2026-09-02):
// the profile avatar and the workspace name/chevron sit in the **pane header
// band** — the `CAPTION_H` strip at the top of the explore pane — while the pane
// is open, and collapse into the caption's one merged square while it is shut.
// Nothing below is a second idiom: the same `identity_slots`, the same clamp,
// the same zero-width-is-not-an-absence rule, laid out from the band's own left
// edge because there is no sidebar toggle in front of them there.

/// The band's inset, on **both** sides: the tree rows below are inset by
/// `NAV_PAD`, and an avatar whose edge lands one pixel off theirs reads as a
/// mistake at a glance. Draw and hit share the leading one through
/// `identity_slots`' `x`, and the trailing one through [`pane_workspace_slot`].
///
/// Koru's pane header is the same rect: `header_rects` in
/// `apps/koru/src/panes/explore.rs` builds its row as `area` inset by
/// `NAV_PAD` on each side, and the name half of that row runs to its far edge.
const PANE_BAND_INSET: f32 = theme::NAV_PAD;

/// The profile chip in a pane header band.
pub fn pane_profile_slot(band: Rect, slots: Slots) -> Rect {
    identity_slots(band, band.x + PANE_BAND_INSET.min(band.w), slots).0
}

/// The workspace chip in a pane header band — **the rest of the band**.
///
/// This is where the two homes stop being one arithmetic, and it is the only
/// place they do (owner, 2026-09-02). In the caption the chip is measured to its
/// label and capped at [`WORKSPACE_MAX_W`], because a chip that filled the
/// caption would eat the tab lane. A pane header has no lane: it has one row,
/// and Koru's fills it — the name half of `header_rects`' row runs to the band's
/// trailing inset with the chevron pinned at its far edge, so the header reads
/// as *the column's* header rather than as a short pill floating in a wide
/// strip.
///
/// The clamp is still one clamp. Widening the slot widens the box
/// [`named_chip_box`] draws and the width the painter elides the label into, so
/// the ellipsis follows the chip without a second rule; and because the fill is
/// a `min` against the band's trailing edge, a name too long for the column is
/// clamped here exactly as it was.
///
/// Only a **named** chip fills. `IconOnly` is a square carrying a tile and
/// stretching it would draw a 180 px rounded rectangle around a 22 px mark;
/// `Hidden` is still zero-width, which is the rule the whole module runs on.
pub fn pane_workspace_slot(band: Rect, slots: Slots) -> Rect {
    let slot = identity_slots(band, band.x + PANE_BAND_INSET.min(band.w), slots).1;
    if !matches!(slots.workspace, Workspace::Named(_)) {
        return slot;
    }
    let trailing = band.x + band.w - PANE_BAND_INSET.min(band.w);
    Rect::new(slot.x, slot.y, (trailing - slot.x).max(0.0), slot.h)
}

/// Where the pane header's identity stops. The band's own `+`/heading — if a
/// host puts one there — begins no earlier.
pub fn pane_end(band: Rect, slots: Slots) -> f32 {
    let slot = pane_workspace_slot(band, slots);
    slot.x + slot.w
}

/// Whether these slots fit the pane header band **unclamped**.
///
/// The pane header's ladder trigger, and the counterpart of [`fits`]: in the
/// caption the question is what the tab lane keeps, and in the pane header there
/// is no lane — the band is the whole budget, so the question is simply whether
/// the chips still fit inside it.
///
/// The budget is the band less **both** insets, which is the room
/// [`pane_workspace_slot`] actually lets a chip have. It was the leading inset
/// alone, and a ladder measuring against room the chip may not use is a ladder
/// that stops one rung too late — the name would be elided by the fill rather
/// than stepped down to a square, which is the wrong answer at the width where
/// the two disagree.
pub fn pane_fits(band: Rect, slots: Slots) -> bool {
    wanted_identity_w(band, slots) <= (band.w - PANE_BAND_INSET * 2.0).max(0.0)
}

/// The one hit test for a pane header band, over the arithmetic it draws with.
/// Settings is not offered here — it is caption furniture and never moved.
pub fn pane_hit(band: Rect, slots: Slots, x: f32, y: f32) -> Option<Hit> {
    let profile = pane_profile_slot(band, slots);
    if profile.w > 0.0 && profile.contains(x, y) {
        return Some(Hit::Profile);
    }
    let workspace = pane_workspace_slot(band, slots);
    if workspace.w > 0.0 && workspace.contains(x, y) {
        return Some(Hit::Workspace);
    }
    None
}

/// **The vertical band a chip is actually drawn in**, whatever shape it is.
///
/// The tab's own top and bottom insets. Every chip in either band already lands
/// here — a named chip through [`named_chip_box`], a square one through
/// `koru_ui::icon_button::hover_chip`, which insets by the same two theme
/// constants — so this is where that agreement is written down rather than
/// inferred twice.
///
/// It matters beyond painting: a popover hangs from the chip a person *sees*,
/// not from the 36 px slot behind it, and Koru anchors its own switcher on the
/// inset header row for exactly that reason
/// (`apps/koru/src/panes/explore.rs`, `header_rects`).
pub fn chip_band(slot: Rect) -> Rect {
    Rect::new(
        slot.x,
        slot.y + theme::CAPTION_INSET_TOP,
        slot.w,
        (slot.h - theme::CAPTION_INSET_TOP - theme::CAPTION_INSET_BOTTOM).max(0.0),
    )
}

/// The box a **named** chip draws inside its slot.
///
/// [`chip_band`]'s vertical extent, pulled in 2 px each side so the workspace
/// chip and a tab pill are one shape at two places — `DESIGN.md`, "The window is
/// cut into four": those were separate literals once and they were separate
/// pixels out, which is extremely visible when the two sit side by side across
/// the divide. It is here rather than in the painter because the pane header's
/// parity test has to be able to name it without a `Scene`.
pub fn named_chip_box(slot: Rect) -> Rect {
    let band = chip_band(slot);
    Rect::new(band.x + 2.0, band.y, (band.w - 4.0).max(0.0), band.h)
}

/// Where the banner's leading slots stop and the tab strip may begin.
pub fn end(caption: Rect, slots: Slots) -> f32 {
    let slot = workspace_slot(caption, slots);
    slot.x + slot.w
}

/// The Settings slot: right-aligned, immediately left of the window buttons.
///
/// **The outermost of the caption's trailing furniture**, and the anchor
/// everything left of it derives from — see [`caption::trailing_edge`] and the
/// chain in that module's header. Until SP-12 this *was*
/// `caption::context_toggle`'s rect, on the reasoning that a window offers one or
/// the other set of right-hand furniture; Koru Terminal offers both, so the two
/// are now neighbours and the context toggle is the one that moved
/// ([`caption::context_toggle_with_banner`] says why it is that way round).
/// The rect this returns is unchanged to the pixel.
pub fn settings_slot(caption: Rect, window_buttons: bool, slots: Slots) -> Rect {
    let w = if slots.settings { slot_w(caption) } else { 0.0 };
    let right = caption::trailing_edge(caption, window_buttons);
    Rect::new((right - w).max(caption.x), caption.y, w, caption.h)
}

/// Whether these slots leave the tab lane its [`LANE_RESERVE`].
///
/// **The ladder's one trigger.** A container walks its banner down a step at a
/// time while this answers `false`, against the *settled* caption — never the
/// sliding one, because a chip that changed step halfway through a 140 ms slide
/// would flicker and re-measuring a label per frame is the reflow-per-frame
/// mistake in a smaller hat.
pub fn fits(caption: Rect, window_buttons: bool, context: bool, slots: Slots) -> bool {
    let lane = caption::tab_strip_with_banner(caption, window_buttons, context, false, slots);
    lane.w >= LANE_RESERVE
}

/// The one hit test, over the same arithmetic the slots draw with. A slot that
/// is not offered is zero-width and excluded here explicitly.
pub fn hit(caption: Rect, window_buttons: bool, slots: Slots, x: f32, y: f32) -> Option<Hit> {
    let p = profile_slot(caption, slots);
    if p.w > 0.0 && p.contains(x, y) {
        return Some(Hit::Profile);
    }
    let w = workspace_slot(caption, slots);
    if w.w > 0.0 && w.contains(x, y) {
        return Some(Hit::Workspace);
    }
    let s = settings_slot(caption, window_buttons, slots);
    if s.w > 0.0 && s.contains(x, y) {
        return Some(Hit::Settings);
    }
    None
}

#[cfg(test)]
mod tests;
