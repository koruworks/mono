// SPDX-License-Identifier: MPL-2.0

//! Theme-aware explanatory chips with one shared timing contract.

use std::time::{Duration, Instant};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Brush;
use koru_draw::{anim::ease_out_cubic, scene, theme};
use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;

/// The widest a [wrapped](TooltipContent::wrapped) tooltip grows: about
/// fifty characters of the hint role, a comfortable measure for a sentence
/// or two. A one-line tooltip is bounded by the surface it floats over
/// instead, and elides into it.
pub const MAX_W: f32 = 320.0;

/// Where a tooltip stands against its anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// Centred under the anchor: every control's tooltip.
    #[default]
    Below,
    /// Beside the anchor, its top level with the anchor's: to the right
    /// where it fits, else to the left. For a row of a list, which a tooltip
    /// under it would cover the next rows of — the anchor is then the row's
    /// height across the whole list, so the tooltip clears the list.
    Beside,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TooltipContent {
    pub key: u64,
    pub anchor: Rect,
    pub label: String,
    pub shortcut: Option<String>,
    /// A label too long for one line wraps onto more, at most [`MAX_W`]
    /// wide, instead of eliding: for a tooltip whose whole job is to show
    /// words the anchor cut short.
    pub wrap: bool,
    pub placement: Placement,
}

impl TooltipContent {
    pub fn new(key: u64, anchor: Rect, label: impl Into<String>) -> Self {
        Self {
            key,
            anchor,
            label: label.into(),
            shortcut: None,
            wrap: false,
            placement: Placement::Below,
        }
    }

    /// Wrap a long label at [`MAX_W`] rather than eliding it.
    pub fn wrapped(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Stand beside the anchor rather than under it ([`Placement::Beside`]).
    pub fn beside(mut self) -> Self {
        self.placement = Placement::Beside;
        self
    }

    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Carry a chord only if there is one to show.
    ///
    /// **This is the shape `koru_keys::show_hint` answers in**, and the reason
    /// it is here rather than a `.map` at every call site: whether tooltips
    /// teach their chords is one decision for the app, and `None` has to mean
    /// *draw nothing* rather than an empty second line the chip still makes room
    /// for. This crate deliberately links no key registry — it is a widget crate
    /// and resolves nothing — so the lookup happens in the caller and the answer
    /// arrives here already made.
    pub fn with_optional_shortcut(self, shortcut: Option<impl Into<String>>) -> Self {
        match shortcut {
            Some(s) => self.with_shortcut(s),
            None => self,
        }
    }
}

#[derive(Clone, Debug)]
enum Phase {
    Hidden,
    Waiting {
        content: TooltipContent,
        show_at: Instant,
    },
    Appearing {
        content: TooltipContent,
        started: Instant,
    },
    Visible(TooltipContent),
    Hiding {
        content: TooltipContent,
        started: Instant,
        from_alpha: f32,
    },
}

/// Hover/focus state for one family of adjacent tooltip targets.
///
/// Waiting exposes a maintenance deadline but is not an animation, so a 400 ms
/// pause costs one wake rather than fifty redraws. Only the two short fades
/// report [`animating`](Self::animating).
#[derive(Clone, Debug)]
pub struct Tooltip {
    phase: Phase,
    hovered: Option<u64>,
    warm_until: Option<Instant>,
    transient_until: Option<Instant>,
}

impl Default for Tooltip {
    fn default() -> Self {
        Self {
            phase: Phase::Hidden,
            hovered: None,
            warm_until: None,
            transient_until: None,
        }
    }
}

impl Tooltip {
    pub fn set_hover(&mut self, content: Option<TooltipContent>, now: Instant) -> bool {
        self.transient_until = None;
        let next = content.as_ref().map(|value| value.key);
        if next == self.hovered {
            if let Some(content) = content {
                self.replace_content(content);
            }
            return false;
        }

        self.hovered = next;
        match content {
            Some(content) => {
                let warm =
                    self.is_showing(now) || self.warm_until.is_some_and(|until| now <= until);
                self.phase = if warm {
                    Phase::Visible(content)
                } else {
                    Phase::Waiting {
                        content,
                        show_at: now + theme::motion::tooltip().hover_delay,
                    }
                };
            }
            None => {
                let alpha = self.alpha(now);
                let old = std::mem::replace(&mut self.phase, Phase::Hidden);
                if alpha > 0.0
                    && let Some(content) = phase_content(old)
                {
                    self.phase = Phase::Hiding {
                        content,
                        started: now,
                        from_alpha: alpha,
                    };
                    self.warm_until = Some(now + theme::motion::tooltip().warm_window);
                }
            }
        }
        true
    }

    /// Keyboard focus has already expressed intent; it never waits or fades in.
    pub fn set_focus(&mut self, content: Option<TooltipContent>, now: Instant) -> bool {
        self.transient_until = None;
        match content {
            Some(content) => {
                self.hovered = Some(content.key);
                self.phase = Phase::Visible(content);
                self.warm_until = Some(now + theme::motion::tooltip().warm_window);
            }
            None => return self.set_hover(None, now),
        }
        true
    }

    /// An action on the explained control dismisses the explanation immediately.
    pub fn clear(&mut self) -> bool {
        let changed = !matches!(self.phase, Phase::Hidden) || self.hovered.is_some();
        self.phase = Phase::Hidden;
        self.hovered = None;
        self.transient_until = None;
        changed
    }

    /// Show action feedback immediately, then dismiss it through the same
    /// appearance and disappearance clock as explanatory tooltips.
    pub fn show_transient(
        &mut self,
        content: TooltipContent,
        now: Instant,
        hold: Duration,
    ) -> bool {
        self.hovered = None;
        self.warm_until = None;
        self.transient_until = Some(now + theme::motion::tooltip().enter_duration + hold);
        self.phase = Phase::Appearing {
            content,
            started: now,
        };
        true
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        let phase = match &self.phase {
            Phase::Waiting { show_at, .. } => Some(*show_at),
            _ => None,
        };
        [phase, self.transient_until].into_iter().flatten().min()
    }

    pub fn is_present(&self) -> bool {
        !matches!(self.phase, Phase::Hidden)
    }

    /// The sentence this is currently carrying, in any phase.
    ///
    /// A read accessor rather than a second copy of the string at the call
    /// site: what a tooltip *says* is the whole of what it does, and a test
    /// asserting only that the clock is running would pass on a tooltip
    /// carrying the wrong words.
    pub fn label(&self) -> Option<&str> {
        match &self.phase {
            Phase::Waiting { content, .. }
            | Phase::Appearing { content, .. }
            | Phase::Visible(content)
            | Phase::Hiding { content, .. } => Some(content.label.as_str()),
            Phase::Hidden => None,
        }
    }

    /// The chord this is currently teaching, in any phase.
    ///
    /// [`label`](Self::label)'s other half, and for the same reason: on a chip
    /// whose entire job is to teach a chord — a stepper's `−`, whose glyph
    /// cannot say `Ctrl+-` — asserting the sentence and not the chord would
    /// pass on a chip teaching the wrong key.
    pub fn shortcut(&self) -> Option<&str> {
        match &self.phase {
            Phase::Waiting { content, .. }
            | Phase::Appearing { content, .. }
            | Phase::Visible(content)
            | Phase::Hiding { content, .. } => content.shortcut.as_deref(),
            Phase::Hidden => None,
        }
    }

    /// Current eased opacity of the shared hover interaction clock.
    ///
    /// Most callers use [`draw`](Self::draw), but icon furniture that reveals
    /// on the same deliberate-hover contract can consume the clock without
    /// drawing a second tooltip surface or reimplementing its timings.
    pub fn visibility(&self, now: Instant) -> f32 {
        self.alpha(now)
    }

    pub fn maintain(&mut self, now: Instant) -> bool {
        if self.transient_until.is_some_and(|until| now >= until) {
            self.transient_until = None;
            let alpha = self.alpha(now);
            let old = std::mem::replace(&mut self.phase, Phase::Hidden);
            if alpha > 0.0
                && let Some(content) = phase_content(old)
            {
                self.phase = Phase::Hiding {
                    content,
                    started: now,
                    from_alpha: alpha,
                };
            }
            return true;
        }
        match &self.phase {
            Phase::Waiting { content, show_at } if now >= *show_at => {
                self.phase = Phase::Appearing {
                    content: content.clone(),
                    started: *show_at,
                };
                true
            }
            Phase::Appearing { content, started }
                if now.saturating_duration_since(*started)
                    >= theme::motion::tooltip().enter_duration =>
            {
                self.phase = Phase::Visible(content.clone());
                true
            }
            Phase::Hiding { started, .. }
                if now.saturating_duration_since(*started)
                    >= theme::motion::tooltip().exit_duration =>
            {
                self.phase = Phase::Hidden;
                true
            }
            _ => false,
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        match &self.phase {
            Phase::Appearing { started, .. } => {
                now.saturating_duration_since(*started) < theme::motion::tooltip().enter_duration
            }
            Phase::Hiding { started, .. } => {
                now.saturating_duration_since(*started) < theme::motion::tooltip().exit_duration
            }
            _ => false,
        }
    }

    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: Rect,
        now: Instant,
    ) -> Option<Rect> {
        let (content, alpha, offset) = self.presentation(now)?;
        if alpha <= 0.0 {
            return None;
        }
        Some(draw_tooltip(
            canvas, root, engine, content, viewport, alpha, offset,
        ))
    }

    fn replace_content(&mut self, content: TooltipContent) {
        match &mut self.phase {
            Phase::Waiting { content: old, .. }
            | Phase::Appearing { content: old, .. }
            | Phase::Visible(old)
            | Phase::Hiding { content: old, .. } => *old = content,
            Phase::Hidden => {}
        }
    }

    fn is_showing(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0 || matches!(self.phase, Phase::Visible(_))
    }

    fn alpha(&self, now: Instant) -> f32 {
        match &self.phase {
            Phase::Hidden | Phase::Waiting { .. } => 0.0,
            Phase::Appearing { started, .. } => ease_out(progress(
                now,
                *started,
                theme::motion::tooltip().enter_duration,
            )),
            Phase::Visible(_) => 1.0,
            Phase::Hiding {
                started,
                from_alpha,
                ..
            } => {
                from_alpha * (1.0 - progress(now, *started, theme::motion::tooltip().exit_duration))
            }
        }
    }

    fn presentation(&self, now: Instant) -> Option<(&TooltipContent, f32, f32)> {
        match &self.phase {
            Phase::Appearing { content, started } => {
                let p = ease_out(progress(
                    now,
                    *started,
                    theme::motion::tooltip().enter_duration,
                ));
                Some((content, p, theme::motion::tooltip().rise * (1.0 - p)))
            }
            Phase::Visible(content) => Some((content, 1.0, 0.0)),
            Phase::Hiding {
                content,
                started,
                from_alpha,
            } => Some((
                content,
                from_alpha
                    * (1.0 - progress(now, *started, theme::motion::tooltip().exit_duration)),
                0.0,
            )),
            Phase::Hidden | Phase::Waiting { .. } => None,
        }
    }
}

fn phase_content(phase: Phase) -> Option<TooltipContent> {
    match phase {
        Phase::Waiting { content, .. }
        | Phase::Appearing { content, .. }
        | Phase::Visible(content)
        | Phase::Hiding { content, .. } => Some(content),
        Phase::Hidden => None,
    }
}

fn progress(now: Instant, started: Instant, duration: Duration) -> f32 {
    (now.saturating_duration_since(started).as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out(t: f32) -> f32 {
    ease_out_cubic(t.clamp(0.0, 1.0))
}

/// The panel's two 8px pads, which the label sits between.
const PAD: f32 = 8.0;

/// **The label the panel will draw, and how wide the panel then is.**
///
/// A tooltip is clamped to the surface it floats over, like every other
/// floating panel (`docs/features/design/DESIGN.md`: *"a tooltip is clamped to the
/// window like any other floating surface"*). It was clamped — and only the
/// *background* was: the text was laid out at its natural width and drawn
/// unclipped from the left pad, so a label too long for the clamp ran
/// straight out of the rounded background. The maintainers' screenshot was a
/// document pill's title doing that in the AI pane, where the surface is a
/// narrow column and a title is a whole sentence.
///
/// So the room is worked out first — the widest the panel may be, less its
/// two pads and whatever the shortcut takes — and the label is **elided into
/// it**, which is what the app does everywhere else a name has to fit a lane.
/// Growing instead was the alternative and is not open to a tooltip: it has
/// nowhere to grow to, being already as wide as the surface allows.
///
/// Split out from the drawing so the rule can be asserted without a scene,
/// and measured through `TextEngine::elided` — the same role, the real
/// glyphs — so it holds for a Japanese or an Arabic title exactly as it does
/// for an English one. A character count would not.
fn fitted(
    engine: &mut TextEngine,
    label: &str,
    shortcut_width: f32,
    viewport_w: f32,
) -> (String, f32) {
    let role = theme::role::hint();
    // The clamp leaves a pad either side of the panel; the panel leaves one
    // either side of its text; the shortcut takes the rest of the right.
    let widest = viewport_w - PAD * 2.0;
    let room = (widest - PAD * 2.0 - shortcut_width).max(1.0);
    let text = engine.elided(label, role, room);
    let width =
        (engine.simple_layout(&text, role).width() + shortcut_width + PAD * 2.0).min(widest);
    (text, width)
}

/// The panel's pad above and below its text: a one-line panel is this, a
/// hint line, and this — 26.
const PAD_Y: f32 = 6.0;
/// Between the anchor and the panel.
const GAP: f32 = 6.0;
/// The least room beside an anchor worth standing in; with less on both
/// sides a [`Placement::Beside`] tooltip takes the surface's width instead,
/// clamped into it like any other.
const BESIDE_MIN_W: f32 = MAX_W * 0.5;

/// **A wrapped label laid out, and how wide the panel then is**: broken at
/// `widest` less the pads and the shortcut, and the panel as wide as its
/// longest line — a short label is one line at its own width.
fn wrapped_fit(
    engine: &mut TextEngine,
    label: &str,
    shortcut_width: f32,
    widest: f32,
) -> (Layout<Brush>, f32) {
    let room = (widest - PAD * 2.0 - shortcut_width).max(1.0);
    let layout = engine.simple_layout_wrapped(label, theme::role::hint(), room);
    let width = (layout.width() + shortcut_width + PAD * 2.0).min(widest);
    (layout, width)
}

/// **Where the panel stands and what it draws**, split from the drawing so
/// the placement can be asserted without a scene: its rect before the rise,
/// the label's layout (one line, or wrapped), and the shortcut's.
fn arrange(
    engine: &mut TextEngine,
    content: &TooltipContent,
    viewport: Rect,
) -> (Rect, Layout<Brush>, Option<Layout<Brush>>) {
    let role = theme::role::hint();
    let anchor = content.anchor;
    let shortcut_layout = content
        .shortcut
        .as_deref()
        .map(|value| engine.simple_layout(value, role));
    let shortcut_width = shortcut_layout
        .as_ref()
        .map_or(0.0, |layout| layout.width() + 12.0);
    // The room either side of the anchor, inside the surface's inset.
    let right_room = viewport.x + viewport.w - PAD - (anchor.x + anchor.w + GAP);
    let left_room = anchor.x - GAP - (viewport.x + PAD);
    let surface = viewport.w - PAD * 2.0;
    let (label_layout, width) = if content.wrap {
        let mut widest = MAX_W.min(surface);
        if content.placement == Placement::Beside && right_room.max(left_room) >= BESIDE_MIN_W {
            widest = widest.min(right_room.max(left_room));
        }
        wrapped_fit(engine, &content.label, shortcut_width, widest)
    } else {
        let (label, width) = fitted(engine, &content.label, shortcut_width, viewport.w);
        (engine.simple_layout(&label, role), width)
    };
    let height = label_layout.height().max(role.metrics.line_height) + PAD_Y * 2.0;
    let left_edge = viewport.x + PAD;
    let right_edge = (viewport.x + viewport.w - width - PAD).max(left_edge);
    let bottom_edge = (viewport.y + viewport.h - height - PAD).max(viewport.y + PAD);
    let (x, y) = match content.placement {
        Placement::Below => (
            anchor.x + anchor.w * 0.5 - width * 0.5,
            (anchor.y + anchor.h + GAP).min(bottom_edge),
        ),
        Placement::Beside => {
            let right = anchor.x + anchor.w + GAP;
            let left = anchor.x - GAP - width;
            let x = if width <= right_room || (width > left_room && right_room >= left_room) {
                right
            } else {
                left
            };
            (x, anchor.y.clamp(viewport.y + PAD, bottom_edge))
        }
    };
    let rect = Rect::new(x.clamp(left_edge, right_edge), y, width, height);
    (rect, label_layout, shortcut_layout)
}

fn draw_tooltip(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    content: &TooltipContent,
    viewport: Rect,
    alpha: f32,
    offset: f32,
) -> Rect {
    let (rect, label_layout, shortcut_layout) = arrange(engine, content, viewport);
    let rect = Rect::new(rect.x, rect.y + offset, rect.w, rect.h);
    scene::floating_panel(
        canvas,
        root,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        &scene::Panel::tooltip(6.0).faded(alpha),
    );
    let ink = Some(theme::p().bubble_fg.multiply_alpha(alpha));
    if content.wrap {
        scene::draw_layout(
            canvas,
            root,
            &label_layout,
            rect.x + PAD,
            rect.y + PAD_Y,
            ink,
        );
    } else {
        scene::draw_layout_first_line(
            canvas,
            root,
            &label_layout,
            rect.x + PAD,
            rect.y + PAD_Y,
            ink,
        );
    }
    if let Some(layout) = shortcut_layout {
        scene::draw_layout_first_line(
            canvas,
            root,
            &layout,
            rect.x + rect.w - layout.width() - 8.0,
            rect.y + 6.0,
            Some(theme::p().bubble_dim.multiply_alpha(alpha)),
        );
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(key: u64) -> TooltipContent {
        TooltipContent::new(key, Rect::new(10.0, 10.0, 20.0, 20.0), "Search")
    }

    #[test]
    fn intentional_hover_waits_then_animates_in() {
        let start = Instant::now();
        let mut tip = Tooltip::default();
        tip.set_hover(Some(content(1)), start);
        assert_eq!(
            tip.maintenance_deadline(),
            Some(start + theme::motion::tooltip().hover_delay)
        );
        assert_eq!(
            tip.alpha(start + theme::motion::tooltip().hover_delay - Duration::from_millis(1)),
            0.0
        );
        assert!(tip.maintain(start + theme::motion::tooltip().hover_delay));
        assert!(tip.animating(start + theme::motion::tooltip().hover_delay));
        assert!(
            tip.alpha(start + theme::motion::tooltip().hover_delay + Duration::from_millis(50))
                > 0.5
        );
        assert!(tip.maintain(
            start + theme::motion::tooltip().hover_delay + theme::motion::tooltip().enter_duration
        ));
        assert_eq!(
            tip.alpha(
                start
                    + theme::motion::tooltip().hover_delay
                    + theme::motion::tooltip().enter_duration
            ),
            1.0
        );
    }

    #[test]
    fn leaving_fades_out_and_keeps_adjacent_controls_warm() {
        let start = Instant::now();
        let mut tip = Tooltip::default();
        tip.set_focus(Some(content(1)), start);
        tip.set_hover(None, start + Duration::from_millis(1));
        assert!(tip.alpha(start + Duration::from_millis(38)) > 0.0);
        assert!(tip.maintain(start + Duration::from_millis(76)));

        tip.set_hover(Some(content(2)), start + Duration::from_millis(200));
        assert_eq!(tip.alpha(start + Duration::from_millis(200)), 1.0);
    }

    #[test]
    fn keyboard_focus_is_immediate_and_clear_is_immediate() {
        let now = Instant::now();
        let mut tip = Tooltip::default();
        tip.set_focus(Some(content(1)), now);
        assert_eq!(tip.alpha(now), 1.0);
        assert!(tip.clear());
        assert_eq!(tip.alpha(now), 0.0);
    }

    #[test]
    fn visibility_exposes_the_same_eased_clock_as_tooltip_drawing() {
        let start = Instant::now();
        let mut tip = Tooltip::default();
        tip.set_hover(Some(content(1)), start);
        tip.maintain(start + theme::motion::tooltip().hover_delay);
        let halfway = tip.visibility(
            start
                + theme::motion::tooltip().hover_delay
                + theme::motion::tooltip().enter_duration / 2,
        );
        assert!(halfway > 0.5 && halfway < 1.0);
    }

    #[test]
    fn transient_feedback_appears_immediately_then_uses_the_shared_fade_out() {
        let start = Instant::now();
        let hold = Duration::from_millis(800);
        let mut tip = Tooltip::default();
        tip.show_transient(content(1), start, hold);
        assert!(tip.animating(start));
        assert_eq!(
            tip.maintenance_deadline(),
            Some(start + theme::motion::tooltip().enter_duration + hold)
        );

        let expires = start + theme::motion::tooltip().enter_duration + hold;
        tip.maintain(start + theme::motion::tooltip().enter_duration);
        assert_eq!(
            tip.visibility(start + theme::motion::tooltip().enter_duration),
            1.0
        );
        assert!(tip.maintain(expires));
        assert!(tip.animating(expires));
        assert!(tip.maintain(expires + theme::motion::tooltip().exit_duration));
        assert!(!tip.is_present());
    }

    /// **The text never leaves the rounded background.**
    ///
    /// The maintainers' screenshot: a document pill's tooltip in the AI pane reading
    /// *"Morning news briefing — September 9, 2026"*, the sentence running
    /// past the panel behind it. The panel was clamped to the surface and the
    /// label was not, so anything longer than the clamp simply overhung.
    ///
    /// Asserted in **all eight languages** over a title of the kind that
    /// caused it, because a Japanese or an Arabic sentence is not the width
    /// of an English one and a fix that only measured English would pass here
    /// and fail on a phone in Cairo.
    #[test]
    fn a_long_title_is_trimmed_into_the_panel_in_every_language() {
        use koru_draw::text::TextEngine;

        // One long document title per locale — a date-stamped briefing, which
        // is the shape the report was about.
        let titles = [
            "Morning news briefing — September 9, 2026",
            "Resumen informativo de la mañana — 9 de septiembre de 2026",
            "Resumo das notícias da manhã — 9 de setembro de 2026",
            "Morgendliche Nachrichtenzusammenfassung — 9. September 2026",
            "Bulletin d'information du matin — 9 septembre 2026",
            "موجز الأخبار الصباحية — ٩ سبتمبر ٢٠٢٦",
            "晨间新闻简报 — 2026年9月9日",
            "朝のニュースブリーフィング — 2026年9月9日",
        ];
        let role = theme::role::hint();
        let mut engine = TextEngine::new();

        // The AI pane is a narrow column, which is where it showed.
        for viewport_w in [260.0_f32, 420.0, 1200.0] {
            for title in titles {
                for shortcut in [0.0_f32, 48.0] {
                    let (text, width) = fitted(&mut engine, title, shortcut, viewport_w);
                    let drawn = engine.simple_layout(&text, role).width();
                    assert!(
                        drawn + shortcut + PAD * 2.0 <= width + 0.5,
                        "{title:?} at {viewport_w} overhangs: {drawn} in {width}"
                    );
                    assert!(
                        width <= viewport_w - PAD * 2.0 + 0.5,
                        "{title:?} at {viewport_w} is wider than the surface allows"
                    );
                    assert!(!text.is_empty(), "{title:?} was trimmed away entirely");
                }
            }
        }
    }

    /// A sentence long enough to need several lines at [`MAX_W`].
    const LONG: &str = "Review the open pull request against the repository's \
        contribution rules, then write up every finding with the file and \
        line it is about, and what to change.";

    /// **A wrapped label breaks onto lines no wider than [`MAX_W`]**, and
    /// the panel grows down to hold them all — a hint line and the pads a
    /// line, as a one-line panel is.
    #[test]
    fn a_wrapped_label_breaks_at_the_maximum_and_the_panel_holds_every_line() {
        let mut engine = TextEngine::new();
        let viewport = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let anchor = Rect::new(100.0, 300.0, 200.0, 40.0);
        let content = TooltipContent::new(1, anchor, LONG).wrapped();
        let (rect, label, _) = arrange(&mut engine, &content, viewport);
        let lines = label.lines().count();
        assert!(lines > 1, "a long label wraps: {lines} line(s)");
        assert!(rect.w <= MAX_W + 0.5, "capped at MAX_W: {}", rect.w);
        assert!(
            label.width() + PAD * 2.0 <= rect.w + 0.5,
            "the text is inside"
        );
        assert!((rect.h - (label.height() + PAD_Y * 2.0)).abs() < 0.5);

        // A short one is one line, at its own width.
        let short = TooltipContent::new(1, anchor, "Search").wrapped();
        let (rect, label, _) = arrange(&mut engine, &short, viewport);
        assert_eq!(label.lines().count(), 1);
        assert!(rect.w < MAX_W * 0.5);
        assert!((rect.h - 26.0).abs() < 0.5, "a one-line panel: {}", rect.h);
    }

    /// **Beside its anchor**: to the right where it fits, its top level with
    /// the anchor's; to the left where the right has no room; never over
    /// the anchor.
    #[test]
    fn beside_stands_right_where_it_fits_and_left_where_it_does_not() {
        let mut engine = TextEngine::new();
        let viewport = Rect::new(0.0, 0.0, 1200.0, 800.0);

        let anchor = Rect::new(100.0, 300.0, 400.0, 40.0);
        let content = TooltipContent::new(1, anchor, LONG).wrapped().beside();
        let (rect, _, _) = arrange(&mut engine, &content, viewport);
        assert_eq!(rect.x, anchor.x + anchor.w + GAP, "to the right");
        assert_eq!(rect.y, anchor.y, "level with the anchor");

        let anchor = Rect::new(700.0, 300.0, 420.0, 40.0);
        let content = TooltipContent::new(1, anchor, LONG).wrapped().beside();
        let (rect, _, _) = arrange(&mut engine, &content, viewport);
        assert!(
            (rect.x + rect.w - (anchor.x - GAP)).abs() < 0.5,
            "to the left, ending a gap short of the anchor: {rect:?}"
        );
        assert!(rect.w <= MAX_W + 0.5);

        // Near the floor it rises to stay on the surface.
        let anchor = Rect::new(100.0, 780.0, 400.0, 40.0);
        let content = TooltipContent::new(1, anchor, LONG).wrapped().beside();
        let (rect, _, _) = arrange(&mut engine, &content, viewport);
        assert!(rect.y + rect.h <= viewport.h - PAD + 0.5);
    }

    /// The default is unchanged: one line, centred under the anchor.
    #[test]
    fn the_default_stays_one_line_under_the_anchor() {
        let mut engine = TextEngine::new();
        let viewport = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let anchor = Rect::new(500.0, 300.0, 40.0, 40.0);
        let (rect, label, _) =
            arrange(&mut engine, &TooltipContent::new(1, anchor, LONG), viewport);
        assert_eq!(label.lines().count(), 1);
        assert_eq!(rect.h, 26.0);
        assert_eq!(rect.y, anchor.y + anchor.h + GAP);
        assert!((rect.x + rect.w * 0.5 - (anchor.x + anchor.w * 0.5)).abs() < 0.5);
    }

    /// ...and a label that fits is left alone — no ellipsis on *Search*.
    #[test]
    fn a_short_label_is_not_trimmed_and_the_panel_shrinks_to_it() {
        use koru_draw::text::TextEngine;

        let mut engine = TextEngine::new();
        let (text, width) = fitted(&mut engine, "Search", 0.0, 1200.0);
        assert_eq!(text, "Search");
        assert!(
            width < 200.0,
            "the panel is the label's width, not the surface's: {width}"
        );
    }
}
