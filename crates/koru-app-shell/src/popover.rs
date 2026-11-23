// SPDX-License-Identifier: MPL-2.0

//! One floating panel, anchored under the chip that opened it.
//!
//! **The shell owns open, dismiss, anchor and clamp; the rows are the host's.**
//! That split is the whole of DESIGN's "The banner" §8 as a type: this holds
//! *which slot is open* and *where the panel landed*, and asks the host to draw
//! into the rect it worked out. A shell that owned the rows would be a shell
//! that knew what a profile is.
//!
//! Every rule here comes from `koru_ui::popover::PopoverShell::anchored`, which
//! already flips above when below would overflow and clamps into the viewport
//! with `EDGE = 8` and its own `GAP = 6`. Nothing is reimplemented; what is added
//! are the two decisions that module cannot make — **the viewport is the content
//! rect, not the window**, so a popover never lands over the caption it came
//! from, and **the clearance is [`GAP`], Koru's own**, measured from the chip a
//! person can see rather than from the slot behind it.

use std::time::Instant;

use koru_draw::anim::ease_out_cubic as ease;
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_shell_chrome::banner;
use koru_ui::popover::{PopoverAlign, PopoverShell};
use vello::Scene;
use vello::kurbo::Affine;

use crate::host::BannerSlot;

/// **Which edge a replacing view travels in from.**
///
/// The shell learns that the host swapped what is in the panel and which way
/// the new content came; it does not learn what either view *is*. That is the
/// same seam every other popover method keeps — the panel is the shell's, the
/// rows are the host's — expressed for a panel whose rows changed under it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SlideFrom {
    Left,
    Right,
}

impl SlideFrom {
    /// Which side of the panel the **arriving** view starts on, as a multiple
    /// of the panel's width: `-1` a width to the left, `+1` a width to the
    /// right. The leaving view starts at zero and travels the other way, which
    /// is the whole of [`Popover::view_offsets`].
    fn sign(self) -> f32 {
        match self {
            Self::Left => -1.0,
            Self::Right => 1.0,
        }
    }
}

/// A view swap in flight: when it began, which way the new view came, and the
/// height the panel is travelling from.
#[derive(Copy, Clone, PartialEq, Debug)]
struct Slide {
    started: Instant,
    from: SlideFrom,
    from_h: f32,
    /// How far through, eased, 0 to 1. **A field rather than a call to
    /// `Instant::now`**: drawing and hit-testing have to agree about where a
    /// row is, and two reads of the clock in one frame do not.
    progress: f32,
}

/// A popover in flight: which slot opened it, and the panel it landed in.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Popover {
    pub slot: BannerSlot,
    shell: PopoverShell,
    /// The anchor and the viewport this panel was placed against, kept so it
    /// can be **re-placed** at a new height without the shell having to work
    /// them out a second time. A panel whose height animates is a panel that
    /// re-runs `PopoverShell::anchored` every frame, and re-deriving the anchor
    /// from the slot each time would let a chip that moved mid-slide drag the
    /// panel with it.
    anchor: Rect,
    viewport: Rect,
    width: f32,
    /// The height the current view asked for, already capped to the viewport.
    height: f32,
    slide: Option<Slide>,
}

/// `PopoverShell`'s own clearance from the viewport's edges. Not re-exported
/// there, and repeated here for one reason: [`Popover::open`] has to know how
/// much room the clamp will insist on **before** it hands over a height.
const EDGE: f32 = 8.0;

/// **The one gap between a chip and the panel it opens**, and it is the
/// reference's rather than ours: `POPOVER_GAP` in
/// `apps/koru/src/panes/explore.rs`, the number Koru's own switcher leaves
/// between its header row and the panel under it.
///
/// It cannot be *imported* — `apps/koru` is a binary, and the constant is
/// private to a module inside it — so it is written here with its source named,
/// and `the_gap_under_a_chip_is_the_one_koru_leaves` measures the distance the
/// panel actually lands at rather than trusting the arithmetic that put it
/// there.
///
/// **One constant for both homes.** The caption's chips and the pane header's
/// go through the same [`Popover::open`], so there is no second number to keep
/// in step with this one.
pub const GAP: f32 = 4.0;

/// `PopoverShell::anchored`'s own gap, which is **not** [`GAP`] and is not
/// re-exported there either.
///
/// The shell is `koru-ui`'s and is not ours to change, so the way to land on
/// Koru's number through it is to hand it an anchor already adjusted by the
/// difference — see [`anchored_on`].
const SHELL_GAP: f32 = 6.0;

/// **What a popover actually hangs from.**
///
/// Two corrections to the raw slot, in one place because they are one decision:
///
/// - the chip a person sees is [`banner::chip_band`], not the 36 px slot behind
///   it, and a panel that cleared the *slot* sat a whole `CAPTION_INSET_BOTTOM`
///   lower than the reference's — which is the "hangs visibly lower" the owner
///   reported;
/// - `PopoverShell::anchored` then adds [`SHELL_GAP`] of its own, so the band is
///   pulled in by the difference and the shell's arithmetic lands on [`GAP`].
///   Symmetrically, because the shell measures *down* from the anchor's bottom
///   when it goes below and *up* from its top when it flips above.
fn anchored_on(slot: Rect) -> Rect {
    let band = banner::chip_band(slot);
    let correction = SHELL_GAP - GAP;
    Rect::new(
        band.x,
        band.y + correction,
        band.w,
        (band.h - correction * 2.0).max(0.0),
    )
}

/// **The clamp may never push a panel *further* from the chip it hangs from.**
///
/// `PopoverShell` insists on [`EDGE`] of clearance from every side of the
/// viewport it is given, which is right for a window edge and wrong for the
/// content rect's top: that edge is a boundary the panel must not cross, not a
/// margin it must float below. With the full `EDGE` applied there the panel
/// landed at `content.y + 8` — nine pixels below where the reference puts it,
/// and *further* from its own chip than the anchor asked for, which is the one
/// direction a clamp should never move it.
///
/// So the viewport's top is relaxed to exactly what the below-placement wants,
/// and no further: `anchor.bottom + SHELL_GAP - EDGE`, which is the `y` the
/// shell is about to compute minus the margin it is about to insist on. The
/// bound that remains is *the panel may not start above its own chip's gap* —
/// which is the rule the content rect was standing in for, stated about the
/// thing it is actually about.
///
/// What that costs is one pixel. A chip in a band sits `CAPTION_INSET_BOTTOM` above
/// the band's floor, so the relaxed bound lands on the band's closing hairline
/// rather than below it, and a panel that has to flip or clamp can reach the
/// same line. Koru's own switcher covers that hairline too
/// (`popover_panel` has no upper bound at all), so this is the reference's
/// behaviour rather than a hole in the rule the content rect was protecting: a
/// popover still cannot land *in* the caption or the band, which is the failure
/// that put the content rect here.
///
/// The bottom edge is untouched, so which of below/above the shell picks is
/// decided exactly as before.
fn clamped_into(content: Rect, anchor: Rect) -> Rect {
    let top = content.y.min(anchor.y + anchor.h + SHELL_GAP - EDGE);
    Rect::new(content.x, top, content.w, content.h + (content.y - top))
}

impl Popover {
    /// Place a popover of `(width, height)` under `anchor`, clamped into
    /// `content`.
    ///
    /// `PopoverAlign::Start`, because every banner chip is left-aligned in a row
    /// that reads left to right and a popover that hung off the right of its own
    /// chip would read as belonging to the chip beside it.
    ///
    /// **The height is capped to what the viewport can hold**, and that is the
    /// shell's job rather than the host's: a host sizes its rows and cannot see
    /// the window. `PopoverShell::anchored` clamps the panel's `y` between
    /// `viewport.y + EDGE` and `viewport.y + viewport.h - height - EDGE`, and a
    /// panel taller than the viewport makes the lower bound exceed the upper —
    /// which is a `f32::clamp` panic, not a squeeze. Below that it was still
    /// wrong in the visible way: a panel taller than its viewport is one whose
    /// pinned heading and pinned footer are off the bottom of the window, so the
    /// host draws a heading nothing can see and rows that spill over the surface
    /// underneath. Capping here means the host's own layout — which is told the
    /// panel rect and nothing else — pins its heading and footer inside the
    /// window and scrolls what is left, with no second question to ask.
    pub fn open(slot: BannerSlot, anchor: Rect, content: Rect, size: (f32, f32)) -> Self {
        let anchor = anchored_on(anchor);
        let viewport = clamped_into(content, anchor);
        let mut popover = Self {
            slot,
            shell: PopoverShell::anchored(anchor, viewport, size.0, 0.0, PopoverAlign::Start),
            anchor,
            viewport,
            width: size.0,
            height: 0.0,
            slide: None,
        };
        popover.retarget(size);
        popover
    }

    /// Cap a wanted height to the viewport and re-place the panel at it.
    fn retarget(&mut self, size: (f32, f32)) {
        self.width = size.0;
        self.height = size.1.min((self.viewport.h - EDGE * 2.0).max(0.0));
        self.place();
    }

    /// Run `PopoverShell::anchored` again at whatever height the panel is at
    /// *now* — the target at rest, the interpolated one mid-slide.
    fn place(&mut self) {
        self.shell = PopoverShell::anchored(
            self.anchor,
            self.viewport,
            self.width,
            self.drawn_height(),
            PopoverAlign::Start,
        );
    }

    /// The height the panel is drawn at this frame.
    fn drawn_height(&self) -> f32 {
        match self.slide {
            Some(Slide { from_h, .. }) => from_h + (self.height - from_h) * self.progress(),
            None => self.height,
        }
    }

    /// **The host swapped the panel's contents for another view.**
    ///
    /// The panel stays where it is and keeps its identity; what changes is what
    /// is inside it and how tall that is. The travel is the layout kind of
    /// motion — it says where the view that left went and where this one came
    /// from — but the *duration* is the popover's own
    /// [`theme::POPOVER_FADE_MS`], not a column's `PANEL_SLIDE_MS`: DESIGN's
    /// reason for the shorter number ("anything you can perceive as travel
    /// between clicking and reading is a menu that feels slow") is about
    /// exactly this moment, and it applies with more force to the *second*
    /// click inside an open menu than to the one that opened it. Nothing
    /// reflows here, which is the cost the 140 ms is paying for elsewhere.
    ///
    /// **The height travels with it** rather than snapping or being fixed to
    /// the taller view. Fixing it would leave the shorter view standing over a
    /// band of empty panel, which is the very gap the bottom inset was just
    /// closed to remove; snapping it would resize the panel under a pointer
    /// that is still moving. One clock drives both, so the two halves of one
    /// event cannot drift apart.
    pub fn slide_to(&mut self, size: (f32, f32), from: SlideFrom, now: Instant) {
        let from_h = self.drawn_height();
        self.slide = Some(Slide {
            started: now,
            from,
            from_h,
            progress: 0.0,
        });
        self.retarget(size);
    }

    /// How far through the swap, 0 to 1, eased. 1 at rest.
    fn progress(&self) -> f32 {
        self.slide.map_or(1.0, |slide| slide.progress)
    }

    /// **How far each view is drawn from the panel's own left edge**, as
    /// `(leaving, arriving)` offsets in logical pixels.
    ///
    /// `None` at rest: there is one view and it sits at zero. The host draws
    /// both while this answers, clipped to [`bounds`](Self::bounds), and the
    /// pair always travels one panel width apart — so the footers, the
    /// headings and the rows of a view move together and no band has to be
    /// cross-faded against another.
    pub fn view_offsets(&self) -> Option<(f32, f32)> {
        let slide = self.slide?;
        let t = self.progress();
        let width = self.shell.rect.w;
        let sign = slide.from.sign();
        Some((-sign * width * t, sign * width * (1.0 - t)))
    }

    pub fn animating(&self) -> bool {
        self.slide.is_some()
    }

    /// Advance the swap. Returns whether anything moved.
    ///
    /// **Deadline-driven** (DESIGN, "Motion"): elapsed time against a fixed
    /// duration, so a dropped frame cannot strand it, and it must return to
    /// `false` or the shell's loop spins.
    pub fn advance(&mut self, now: Instant) -> bool {
        let Some(slide) = self.slide.as_mut() else {
            return false;
        };
        let secs = (theme::POPOVER_FADE_MS / 1000.0).max(f32::EPSILON);
        let t = (now.duration_since(slide.started).as_secs_f32() / secs).clamp(0.0, 1.0);
        slide.progress = ease(t);
        if t >= 1.0 {
            self.slide = None;
        }
        self.place();
        true
    }

    /// The panel's rect, in window space.
    pub fn bounds(&self) -> Rect {
        self.shell.rect
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.shell.contains(x, y)
    }

    /// Paint the panel. The rows go on top, drawn by the host into
    /// [`bounds`](Self::bounds).
    pub fn draw(&self, scene: &mut Scene, root: Affine) {
        self.shell.draw(scene, root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_draw::theme;

    fn content() -> Rect {
        Rect::new(0.0, theme::CAPTION_H, 900.0, 600.0)
    }

    /// **The viewport is the content rect.** A popover clamped to the window
    /// could land over the caption that opened it, which is the one thing an
    /// anchored panel must never do.
    ///
    /// Stated against the *chip* rather than the content rect since the gap was
    /// closed: the panel hangs under the chip that opened it, which is the
    /// stronger claim — the caption above the chip is unreachable from below —
    /// and it is what the reference does. The only thing the two answers differ
    /// about is the band's closing hairline, which Koru's own switcher covers
    /// too (`apps/koru/src/panes/explore.rs`, `popover_panel`).
    #[test]
    fn a_popover_never_lands_over_the_caption_it_came_from() {
        let chip = Rect::new(40.0, 0.0, 36.0, theme::CAPTION_H);
        let popover = Popover::open(BannerSlot::Profile, chip, content(), (260.0, 180.0));
        let band = banner::chip_band(chip);
        assert!(
            popover.bounds().y >= band.y + band.h,
            "it opened below the chip: {:?}",
            popover.bounds()
        );
        assert!(
            popover.bounds().y >= content().y - 1.0,
            "and clears all but the caption's own hairline: {:?}",
            popover.bounds()
        );
    }

    /// **The panel clears the chip by [`GAP`], measured from the chip a person
    /// can see.**
    ///
    /// Not from the slot behind it: a chip is drawn inside `banner::chip_band`,
    /// so clearing the slot left the panel a whole `CAPTION_INSET_BOTTOM` lower than
    /// the reference's — 7 px against Koru's 4, which is what the owner saw
    /// side by side. The assertion is the *measured* distance rather than the
    /// arithmetic, so it also fails if `PopoverShell`'s own gap ever moves under
    /// us.
    #[test]
    fn the_gap_under_a_chip_is_the_one_koru_leaves() {
        let content = content();
        let slot = Rect::new(8.0, 0.0, 160.0, theme::CAPTION_H);
        let popover = Popover::open(BannerSlot::Workspace, slot, content, (230.0, 180.0));
        let chip = banner::chip_band(slot);
        assert_eq!(
            popover.bounds().y - (chip.y + chip.h),
            GAP,
            "`POPOVER_GAP` in apps/koru/src/panes/explore.rs"
        );
    }

    /// And the same clearance the other way up, because the shell measures from
    /// the anchor's top when below would overflow. A flip that kept a different
    /// gap would make the panel jump as a window is resized past the fold.
    #[test]
    fn a_flipped_panel_clears_the_chip_by_the_same_gap() {
        let content = content();
        let slot = Rect::new(8.0, content.y + content.h - 60.0, 160.0, theme::CAPTION_H);
        let popover = Popover::open(BannerSlot::Workspace, slot, content, (230.0, 120.0));
        let chip = banner::chip_band(slot);
        let bounds = popover.bounds();
        assert_eq!(
            chip.y - (bounds.y + bounds.h),
            GAP,
            "flipped above: {bounds:?}"
        );
    }

    /// It prefers below, flips above when below would overflow, and clamps
    /// either way — all of it `koru_ui`'s, none of it copied.
    #[test]
    fn a_popover_flips_above_rather_than_leaving_the_content() {
        let content = content();
        let low = Rect::new(40.0, content.y + content.h - 20.0, 36.0, 36.0);
        let popover = Popover::open(BannerSlot::Workspace, low, content, (230.0, 300.0));
        let bounds = popover.bounds();
        assert!(
            bounds.y + bounds.h <= content.y + content.h,
            "clamped inside the content: {bounds:?}"
        );
    }

    /// **A panel never asks for more height than the window has.** The host
    /// sizes its rows and cannot see the window; below this the clamp inside
    /// `PopoverShell::anchored` had its lower bound above its upper one, which
    /// is a panic — and short of that, a heading pinned to the top of a panel
    /// hanging off the bottom of the window is a heading nobody can read.
    #[test]
    fn a_panel_is_never_taller_than_the_viewport_it_is_clamped_into() {
        let short = Rect::new(0.0, theme::CAPTION_H, 900.0, 120.0);
        let chip = Rect::new(40.0, 0.0, 36.0, theme::CAPTION_H);
        let popover = Popover::open(BannerSlot::Workspace, chip, short, (230.0, 420.0));
        let bounds = popover.bounds();
        let band = banner::chip_band(chip);
        assert!(bounds.h <= short.h, "capped: {bounds:?}");
        assert!(
            bounds.y >= band.y + band.h,
            "and still below its chip: {bounds:?}"
        );
        assert!(bounds.y + bounds.h <= short.y + short.h);
    }

    /// **Both directions, and the pair is always one width apart.**
    ///
    /// `Left` means the arriving view starts a width to the left and the
    /// leaving one goes right; `Right` is the mirror. Anything else and the two
    /// views would overlap or leave a gap of panel between them mid-travel.
    #[test]
    fn the_arriving_view_enters_from_the_named_edge_and_the_other_leaves_the_far_side() {
        for (from, sign) in [(SlideFrom::Left, -1.0), (SlideFrom::Right, 1.0_f32)] {
            let chip = Rect::new(40.0, 0.0, 36.0, theme::CAPTION_H);
            let mut popover = Popover::open(BannerSlot::Identity, chip, content(), (260.0, 180.0));
            let started = Instant::now();
            popover.slide_to((260.0, 240.0), from, started);
            let width = popover.bounds().w;

            let (leaving, arriving) = popover.view_offsets().expect("travelling");
            assert_eq!(leaving, 0.0);
            assert_eq!(arriving, sign * width, "{from:?} starts off that edge");

            for step in 1..=4 {
                let secs = theme::POPOVER_FADE_MS / 1000.0 * step as f32 / 5.0;
                popover.advance(started + std::time::Duration::from_secs_f32(secs));
                let (leaving, arriving) = popover.view_offsets().expect("still travelling");
                assert!(
                    (arriving - leaving - sign * width).abs() < 0.01,
                    "{from:?} at step {step}: {leaving} / {arriving}"
                );
                assert_eq!(
                    leaving.signum(),
                    -sign,
                    "the old one goes the other way: {leaving}"
                );
            }

            // Deadline-driven: past the duration it is at rest, whatever
            // happened to the frames in between.
            popover.advance(started + std::time::Duration::from_millis(1000));
            assert!(!popover.animating());
            assert_eq!(popover.view_offsets(), None);
            assert_eq!(popover.bounds().h, 240.0, "and at the new view's height");
        }
    }

    /// **The height travels with the views and is capped like any other.** A
    /// swap to a view taller than the window is clamped to what the viewport
    /// holds, exactly as an open is — the swap is a re-placement, not a second
    /// code path around the cap.
    #[test]
    fn a_swap_to_a_taller_view_is_still_capped_to_the_viewport() {
        let short = Rect::new(0.0, theme::CAPTION_H, 900.0, 200.0);
        let chip = Rect::new(40.0, 0.0, 36.0, theme::CAPTION_H);
        let mut popover = Popover::open(BannerSlot::Identity, chip, short, (260.0, 120.0));
        let started = Instant::now();
        popover.slide_to((260.0, 900.0), SlideFrom::Right, started);
        popover.advance(started + std::time::Duration::from_millis(1000));
        let bounds = popover.bounds();
        assert!(bounds.h <= short.h, "capped: {bounds:?}");
        assert!(bounds.y + bounds.h <= short.y + short.h);
    }

    #[test]
    fn a_press_inside_the_panel_is_the_hosts_and_outside_it_is_a_dismissal() {
        let chip = Rect::new(40.0, 0.0, 36.0, theme::CAPTION_H);
        let popover = Popover::open(BannerSlot::Identity, chip, content(), (260.0, 180.0));
        let (x, y) = popover.bounds().centre();
        assert!(popover.contains(x, y));
        assert!(!popover.contains(x, content().y + content().h - 1.0));
    }
}
