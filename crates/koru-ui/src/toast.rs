// SPDX-License-Identifier: MPL-2.0

//! A message with one action: *Moved "Spec" to Trash — Undo — ×*.
//!
//! **It carries a composed string and reports which of its two targets was
//! pressed.** It does not know what Undo means, and it holds no id — the owner
//! remembers what it raised the toast about. That is what keeps it a widget
//! rather than a piece of the trash feature.
//!
//! ### It asks for frames only while something moves
//!
//! The obvious [`animating`] is "is there a toast", which is true for five
//! seconds — and at the shell's 8 ms animation frame that is roughly **625
//! full-window redraws of a message that is not moving.** A chip holding still
//! is not an animation. So it reports `false` for the hold and `true` only once
//! the fade is due; the idle loop wakes about every 177 ms anyway and polls it,
//! so the fade begins within one idle tick of the deadline, which nobody can
//! see. A toast composing an indeterminate [`ProgressBar`] is the deliberate
//! exception: its segment is moving, so it keeps requesting frames until its
//! owner replaces or dismisses it. Determinate progress stays still between
//! updates. Every transient toast still returns to `false` on its own, which is
//! ADR-26's actual rule.
//!
//! [`animating`]: Toast::animating
//!
//! ### A line under the message
//!
//! A toast may carry a second paragraph with answers of its own
//! ([`Toast::with_line`]) — an import's summary that also offers to read the
//! pictures it brought. It is still one chip: the line's answers are accent
//! words like the action, reported by index, and the owner can put the
//! keyboard on them ([`Toast::set_line_focus`]), which holds the chip the way
//! the pointer does.

use std::time::{Duration, Instant};

use crate::progress_bar::{ProgressBar, ProgressMode};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Brush;
use koru_draw::{scene, theme};
use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;
use winit::window::CursorIcon;

const RADIUS: f32 = 8.0;
/// Shortest chip — one line of message plus air.
pub const H_MIN: f32 = 40.0;
/// Hard cap on chip width. Longer copy wraps and the chip grows taller.
pub const MAX_W: f32 = 500.0;
/// A progress toast holds a fixed width so a stage label changing
/// (Converting → Reading media) does not grow and shrink the chip. The message
/// wraps inside it instead. Wider than the natural import message by design.
const PROGRESS_W: f32 = 360.0;
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 10.0;
/// Between the message, the action and the close box.
const GAP: f32 = 14.0;
const CLOSE: f32 = 20.0;
const PROGRESS_GAP: f32 = 8.0;
const PROGRESS_H: f32 = 4.0;
/// Between the message and a [line](Toast::with_line) under it.
const LINE_GAP: f32 = 8.0;
/// How far the chip floats above whatever is under it.
pub const MARGIN: f32 = 20.0;
/// How long it holds before it starts to go.
const HOLD: Duration = Duration::from_millis(5_000);
/// And how long that takes.
const FADE: Duration = Duration::from_millis(250);

const MESSAGE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::SM, theme::FontWeight::Medium);
const ACTION: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::SM, theme::FontWeight::SemiBold);

/// What a press on the toast asks for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The press missed it — it is not modal, so this means "not mine".
    Miss,
    /// The action was taken. What it *does* is the owner's business.
    Action,
    /// The second action, when the toast offers two — *View task · Undo*.
    Secondary,
    Dismiss,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Hit {
    Action,
    Secondary,
    Close,
}

pub struct Toast {
    /// Composed once, at construction — never per frame.
    message: String,
    /// The action's label, or `None` for a toast that only reports.
    action: Option<&'static str>,
    /// A second action after the first — a toast that offers a place to go
    /// *and* a way back. Never on a progress toast.
    secondary: Option<&'static str>,
    progress: Option<ProgressBar>,
    monospace_digits: bool,
    /// A second paragraph under the message with answers of its own — see
    /// [`with_line`](Self::with_line). Never on a progress toast.
    line: Option<Line>,
    /// The line's action under the pointer, by index.
    line_hover: Option<usize>,
    /// The line's action the keyboard is on, by index. The owner decides
    /// where the keyboard is and says so before drawing; while it is on the
    /// line the chip holds, as it does under the pointer.
    line_focus: Option<usize>,
    shown: Instant,
    hover: Option<Hit>,
    /// Whether the last [`mouse_move`] put the pointer on the chip.
    ///
    /// [`cursor_icon`](Self::cursor) is read from `&self` and cannot re-measure
    /// the chip, so the container trusts this flag from the last move.
    over_chip: bool,
}

/// A paragraph after the message, with its own answers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    sentence: String,
    actions: Vec<&'static str>,
}

impl Toast {
    /// Raise one. `now` is passed in rather than read here so a test can drive
    /// the clock.
    pub fn new(message: String, action: Option<&'static str>, now: Instant) -> Self {
        Self {
            message,
            action,
            secondary: None,
            progress: None,
            monospace_digits: false,
            line: None,
            line_hover: None,
            line_focus: None,
            shown: now,
            hover: None,
            over_chip: false,
        }
    }

    /// **A second paragraph with answers of its own**, under the message and
    /// its action: an import's summary that also offers to read the pictures
    /// it brought — *Import succeeded. View Notebook* and, under it, *412
    /// pictures and scanned PDFs can be found by the words in them… Turn on ·
    /// Not now* (OCR Plan 3.3).
    ///
    /// The answers follow the sentence the way the message's action follows
    /// the message, in the same accent words, so the chip still reads as one
    /// toast. Like the action, the toast knows only their labels; a press
    /// reports an index ([`line_action_at`](Self::line_action_at)) and the
    /// owner remembers what each means. A line with no answers is a sentence
    /// alone. Ignored on a progress toast.
    pub fn with_line(mut self, sentence: String, actions: Vec<&'static str>) -> Self {
        self.line = Some(Line { sentence, actions });
        self
    }

    /// Take the line away and keep the rest — the answer was given.
    pub fn clear_line(&mut self) {
        self.line = None;
        self.line_hover = None;
        self.line_focus = None;
    }

    /// The line's answers, in reading order; empty without a line.
    pub fn line_actions(&self) -> &[&'static str] {
        self.line
            .as_ref()
            .filter(|_| self.progress.is_none())
            .map_or(&[], |line| line.actions.as_slice())
    }

    /// The line's sentence, when there is one.
    pub fn line_sentence(&self) -> Option<&str> {
        self.line
            .as_ref()
            .filter(|_| self.progress.is_none())
            .map(|line| line.sentence.as_str())
    }

    /// Where the keyboard is on the line, as its owner said. An index past
    /// the line's answers is no focus.
    pub fn set_line_focus(&mut self, focus: Option<usize>) {
        let was = self.line_focus;
        self.line_focus = focus.filter(|index| *index < self.line_actions().len());
        // Leaving after the original hold would have expired restarts a full
        // hold, as leaving with the pointer does, so it does not vanish the
        // instant the keyboard moves on.
        if was.is_some() && self.line_focus.is_none() {
            let now = Instant::now();
            if now.saturating_duration_since(self.shown) >= HOLD {
                self.shown = now;
            }
        }
    }

    pub fn line_focus(&self) -> Option<usize> {
        self.line_focus
    }

    /// Offer a second action after the first: *Added to Next — View task ·
    /// Undo*. Drawn only beside a first action, in the same flow.
    pub fn with_secondary(mut self, label: &'static str) -> Self {
        self.secondary = Some(label);
        self
    }

    pub fn secondary(&self) -> Option<&'static str> {
        self.secondary
    }

    /// Keep this toast visible and draw a progress bar beneath its message.
    pub fn with_progress(mut self, progress: ProgressMode) -> Self {
        self.progress = Some(ProgressBar::new(progress, self.shown));
        self
    }

    /// Keep changing counters in the code font while the surrounding message
    /// uses the UI font. Retained when `set_progress` replaces the message.
    pub fn with_monospace_digits(mut self) -> Self {
        self.monospace_digits = true;
        self
    }

    fn message_layout(&self, engine: &mut TextEngine, width: Option<f32>) -> Layout<Brush> {
        if self.monospace_digits {
            engine.numeric_layout(&self.message, MESSAGE, width)
        } else if let Some(width) = width {
            engine.simple_layout_wrapped(&self.message, MESSAGE, width)
        } else {
            engine.simple_layout(&self.message, MESSAGE)
        }
    }

    /// Update progress without restarting the toast's interaction state.
    pub fn set_progress(&mut self, message: String, progress: ProgressMode, now: Instant) {
        self.message = message;
        if let Some(bar) = self.progress.as_mut() {
            bar.set_mode(progress, now);
        } else {
            self.progress = Some(ProgressBar::new(progress, now));
        }
    }

    /// Replace the action without rebuilding the progress widget. Used when a
    /// cancellable operation has accepted Stop: the bar keeps moving while the
    /// action disappears so it cannot be pressed twice.
    pub fn set_action(&mut self, action: Option<&'static str>) {
        self.action = action;
        self.hover = None;
    }

    /// Whether the pointer was on the chip at the last [`mouse_move`].
    pub fn over_chip(&self) -> bool {
        self.over_chip
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn action(&self) -> Option<&'static str> {
        self.action
    }

    // ---------------------------------------------------------- geometry

    /// Message layout plus the action's position in the same text flow.
    ///
    /// The action follows the paragraph instead of owning a right-hand column.
    /// That lets short confirmations use their natural width; only content that
    /// genuinely reaches the 500 px cap wraps.
    fn flow(&self, engine: &mut TextEngine, max_w: f32) -> (Layout<Brush>, Option<Rect>, f32, f32) {
        let (message, action, _, width, height) = self.flow_all(engine, max_w);
        (message, action, width, height)
    }

    /// [`flow`](Self::flow) with the second action's rectangle as well: it
    /// follows the first with the same gap, on the same line when it fits.
    fn flow_all(
        &self,
        engine: &mut TextEngine,
        max_w: f32,
    ) -> (Layout<Brush>, Option<Rect>, Option<Rect>, f32, f32) {
        let natural = self.message_layout(engine, None);
        let message = if natural.width() <= max_w {
            natural
        } else {
            self.message_layout(engine, Some(max_w))
        };
        // A progress toast puts its action at the end of the bar rather than
        // treating it as the next word in the message paragraph.
        let Some(label) = self.action.filter(|_| self.progress.is_none()) else {
            let width = message.width().min(max_w);
            let h = message.height().max(scene::first_line_height(&message));
            return (message, None, None, width, h);
        };
        let action = engine.simple_layout(label, ACTION);
        let secondary = self
            .secondary
            .map(|label| engine.simple_layout(label, ACTION));
        let both_w = action.width() + secondary.as_ref().map(|l| GAP + l.width()).unwrap_or(0.0);
        let last = message.lines().last().map(|line| *line.metrics());
        let (x, y) = match last {
            Some(line) if line.advance + GAP + both_w <= max_w => {
                (line.advance + GAP, line.block_min_coord)
            }
            _ => (0.0, message.height()),
        };
        let action_rect = Rect::new(x, y, action.width(), action.height());
        let secondary_rect = secondary
            .as_ref()
            .map(|l| Rect::new(x + action.width() + GAP, y, l.width(), l.height()));
        let width = message.width().max(x + both_w).min(max_w);
        let height = message.height().max(y + action.height());
        (message, Some(action_rect), secondary_rect, width, height)
    }

    /// Chip width and height for the current content and available band.
    fn size(&self, engine: &mut TextEngine, over_w: f32) -> (f32, f32) {
        let cap = MAX_W.min((over_w - MARGIN * 2.0).max(0.0));
        let natural = self.message_layout(engine, None);
        let action_w = self
            .action
            .filter(|_| self.progress.is_none())
            .map(|label| GAP + engine.simple_layout(label, ACTION).width())
            .unwrap_or(0.0)
            + self
                .secondary
                .filter(|_| self.action.is_some() && self.progress.is_none())
                .map(|label| GAP + engine.simple_layout(label, ACTION).width())
                .unwrap_or(0.0);
        let progress_action_w = self
            .progress
            .as_ref()
            .and(self.action)
            .map(|label| GAP + engine.simple_layout(label, ACTION).width())
            .unwrap_or(0.0);
        let chrome = PAD_X * 2.0 + GAP + CLOSE;
        let content_w = (natural.width() + action_w).max(
            self.progress
                .is_some()
                .then_some(80.0 + progress_action_w)
                .unwrap_or(0.0),
        );
        // A progress toast keeps one width while the stage label changes; the
        // message wraps inside it rather than resizing the chip each update.
        // A toast with a line is as wide as it may be: the line is a whole
        // sentence or three, and wrapping it at the message's width would make
        // a tall, narrow chip.
        let w = if self.progress.is_some() {
            PROGRESS_W.min(cap).max(0.0)
        } else if self.line_sentence().is_some() {
            cap.max(0.0)
        } else {
            (chrome + content_w).min(cap).max(0.0)
        };
        let flow_w = (w - chrome).max(40.0);
        let (_, _, _, message_h) = self.flow(engine, flow_w);
        let content_h = message_h
            + self
                .line_flow(engine, flow_w)
                .map_or(0.0, |(_, _, line_h)| LINE_GAP + line_h);
        let progress_row_h = self
            .progress
            .as_ref()
            .map(|_| {
                self.action
                    .map(|label| engine.simple_layout(label, ACTION).height())
                    .unwrap_or(0.0)
                    .max(PROGRESS_H)
            })
            .unwrap_or(0.0);
        let progress_h = self
            .progress
            .is_some()
            .then_some(PROGRESS_GAP + progress_row_h)
            .unwrap_or(0.0);
        let h = (content_h + PAD_Y * 2.0 + progress_h).max(H_MIN);
        (w, h)
    }

    /// The line's sentence and its answers, laid out like the message and its
    /// action: the answers follow the sentence's last line when they fit,
    /// and start a line of their own when they do not. Rectangles are
    /// relative to the line's own origin; the height is the block's.
    fn line_flow(
        &self,
        engine: &mut TextEngine,
        max_w: f32,
    ) -> Option<(Layout<Brush>, Vec<Rect>, f32)> {
        let sentence = self.line_sentence()?;
        let layout = engine.simple_layout_wrapped(sentence, MESSAGE, max_w);
        let labels: Vec<Layout<Brush>> = self
            .line_actions()
            .iter()
            .map(|label| engine.simple_layout(label, ACTION))
            .collect();
        let text_h = layout.height().max(scene::first_line_height(&layout));
        if labels.is_empty() {
            return Some((layout, Vec::new(), text_h));
        }
        let all_w: f32 = labels.iter().map(Layout::width).sum::<f32>()
            + GAP * (labels.len().saturating_sub(1)) as f32;
        let last = layout.lines().last().map(|line| *line.metrics());
        let (mut x, y) = match last {
            Some(line) if line.advance + GAP + all_w <= max_w => {
                (line.advance + GAP, line.block_min_coord)
            }
            _ => (0.0, layout.height()),
        };
        let mut rects = Vec::with_capacity(labels.len());
        for label in &labels {
            rects.push(Rect::new(x, y, label.width(), label.height()));
            x += label.width() + GAP;
        }
        let height = text_h.max(y + labels[0].height());
        Some((layout, rects, height))
    }

    /// Where the line starts inside the chip `r`, and the width it flows in:
    /// under the message block, at the message's left edge.
    fn line_origin(&self, engine: &mut TextEngine, r: Rect) -> (f32, f32, f32) {
        let chrome = PAD_X * 2.0 + GAP + CLOSE;
        let flow_w = (r.w - chrome).max(40.0);
        let (_, _, _, message_h) = self.flow(engine, flow_w);
        (r.x + PAD_X, r.y + PAD_Y + message_h + LINE_GAP, flow_w)
    }

    /// **Where each of the line's answers is**, in reading order, in the
    /// coordinates the chip is drawn in. Drawing and pressing both ask this.
    pub fn line_rects(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Vec<Rect> {
        if self.line_actions().is_empty() {
            return Vec::new();
        }
        let r = self.rect(engine, over, floor);
        let (x, y, flow_w) = self.line_origin(engine, r);
        self.line_flow(engine, flow_w)
            .map(|(_, rects, _)| {
                rects
                    .into_iter()
                    .map(|rect| Rect::new(x + rect.x, y + rect.y, rect.w, rect.h))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The line's answer under `(x, y)`, by index.
    pub fn line_action_at(
        &self,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        x: f32,
        y: f32,
    ) -> Option<usize> {
        self.line_rects(engine, over, floor)
            .iter()
            .position(|rect| rect.contains(x, y))
    }

    /// Where the chip sits: centred on `over`, `MARGIN` up from `floor`.
    ///
    /// `over` is the content region rather than the whole window, so it does not
    /// sit half under the explore column; `floor` is passed separately because
    /// an update banner pushes it up.
    pub fn rect(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Rect {
        let (w, h) = self.size(engine, over.w);
        Rect::new(over.x + (over.w - w) * 0.5, floor - MARGIN - h, w, h)
    }

    /// The action and close box, in one place, so each is pressable exactly
    /// where it is drawn. The action follows the paragraph's final line.
    fn targets(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Vec<(Hit, Rect)> {
        let r = self.rect(engine, over, floor);
        let mut out = Vec::with_capacity(2);
        let close = Rect::new(
            r.x + r.w - PAD_X - CLOSE,
            r.y + PAD_Y,
            CLOSE,
            CLOSE.max(H_MIN - PAD_Y * 2.0).min(r.h - PAD_Y),
        );
        // Keep the hit box square-ish and at least CLOSE tall.
        let close = Rect::new(close.x, r.y + (r.h.min(H_MIN) - CLOSE) * 0.5, CLOSE, CLOSE);
        let chrome = PAD_X * 2.0 + GAP + CLOSE;
        let flow_w = (r.w - chrome).max(40.0);
        let (_, action, secondary, _, _) = self.flow_all(engine, flow_w);
        if let Some(action) = action {
            out.push((
                Hit::Action,
                Rect::new(
                    r.x + PAD_X + action.x,
                    r.y + PAD_Y + action.y,
                    action.w,
                    action.h,
                ),
            ));
            if let Some(second) = secondary {
                out.push((
                    Hit::Secondary,
                    Rect::new(
                        r.x + PAD_X + second.x,
                        r.y + PAD_Y + second.y,
                        second.w,
                        second.h,
                    ),
                ));
            }
        } else if let Some((_, action)) = self.progress_row(engine, r)
            && let Some(action) = action
        {
            out.push((Hit::Action, action));
        }
        out.push((Hit::Close, close));
        out
    }

    /// **Where the action is**, or `None` when the chip has none.
    ///
    /// Through [`Toast::targets`], which is also what `mouse_down` asks — so a
    /// caller that presses this rectangle presses the thing that is drawn, and
    /// there is no second arithmetic to disagree with. A host names it to hit
    /// its own toast in a test; the widget itself has never needed to ask.
    pub fn action_rect(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Option<Rect> {
        self.targets(engine, over, floor)
            .into_iter()
            .find(|(hit, _)| *hit == Hit::Action)
            .map(|(_, rect)| rect)
    }

    /// Where the second action is, or `None` when there is none.
    pub fn secondary_rect(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Option<Rect> {
        self.targets(engine, over, floor)
            .into_iter()
            .find(|(hit, _)| *hit == Hit::Secondary)
            .map(|(_, rect)| rect)
    }

    /// Where the close box is. Always present — a toast can always be hidden,
    /// which is exactly why hiding one is never the same as stopping what it
    /// reports.
    pub fn close_rect(&self, engine: &mut TextEngine, over: Rect, floor: f32) -> Option<Rect> {
        self.targets(engine, over, floor)
            .into_iter()
            .find(|(hit, _)| *hit == Hit::Close)
            .map(|(_, rect)| rect)
    }

    /// Progress bar and its optional trailing action, in chip coordinates.
    fn progress_row(&self, engine: &mut TextEngine, r: Rect) -> Option<(Rect, Option<Rect>)> {
        self.progress.as_ref()?;
        let right = r.x + r.w - PAD_X - GAP - CLOSE;
        let action_layout = self.action.map(|label| engine.simple_layout(label, ACTION));
        let row_h = action_layout
            .as_ref()
            .map(|layout| layout.height())
            .unwrap_or(0.0)
            .max(PROGRESS_H);
        let row_y = r.y + r.h - PAD_Y - row_h;
        let action = action_layout.as_ref().map(|layout| {
            Rect::new(
                right - layout.width(),
                row_y + (row_h - layout.height()) * 0.5,
                layout.width(),
                layout.height(),
            )
        });
        let bar_right = action.map(|action| action.x - GAP).unwrap_or(right);
        let bar = Rect::new(
            r.x + PAD_X,
            row_y + (row_h - PROGRESS_H) * 0.5,
            (bar_right - (r.x + PAD_X)).max(0.0),
            PROGRESS_H,
        );
        Some((bar, action))
    }

    // ------------------------------------------------------------- input

    pub fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        x: f32,
        y: f32,
    ) -> Outcome {
        match self.hit_at(engine, over, floor, x, y) {
            Some(Hit::Action) => Outcome::Action,
            Some(Hit::Secondary) => Outcome::Secondary,
            Some(Hit::Close) => Outcome::Dismiss,
            // A press on the chip but not on a control is swallowed by the
            // caller if it wants; the toast itself has nothing to say about it.
            None => Outcome::Miss,
        }
    }

    fn hit_at(
        &self,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        x: f32,
        y: f32,
    ) -> Option<Hit> {
        self.targets(engine, over, floor)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| h)
    }

    /// Whether the hover changed, so the caller knows to redraw.
    ///
    /// **While the pointer is on the chip the hold clock is frozen** — a toast
    /// you are reading does not disappear under the pointer. Leaving after the
    /// original hold would have expired restarts a full hold so it does not
    /// snap to the fade the instant you move away.
    pub fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        x: f32,
        y: f32,
    ) -> bool {
        let was_hover = self.hover;
        let was_line_hover = self.line_hover;
        let was_over = self.over_chip;
        self.over_chip = self.rect(engine, over, floor).contains(x, y);
        self.hover = if self.over_chip {
            self.hit_at(engine, over, floor, x, y)
        } else {
            None
        };
        self.line_hover = if self.over_chip {
            self.line_action_at(engine, over, floor, x, y)
        } else {
            None
        };
        if was_over && !self.over_chip {
            let now = Instant::now();
            if now.saturating_duration_since(self.shown) >= HOLD {
                self.shown = now;
            }
        }
        was_hover != self.hover || was_line_hover != self.line_hover || was_over != self.over_chip
    }

    pub fn contains(
        &self,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        x: f32,
        y: f32,
    ) -> bool {
        self.rect(engine, over, floor).contains(x, y)
    }

    /// The plain arrow throughout — **Show** / **Undo** and the close box are
    /// buttons, and Koru does not use the hand cursor (`docs/features/design/DESIGN.md`). What
    /// matters here is that the chip claims the pointer at all, so the I-beam
    /// of the document underneath does not show through it.
    pub fn cursor(&self) -> CursorIcon {
        match (self.hover, self.line_hover) {
            (Some(Hit::Action | Hit::Secondary | Hit::Close), _) | (None, Some(_)) => {
                crate::cursor::PRESS
            }
            (None, None) => crate::cursor::IDLE,
        }
    }

    // -------------------------------------------------------------- time

    /// The alpha to draw at, or `None` once it has expired and should be
    /// dropped.
    ///
    /// **Full opacity and no expiry while the pointer is on the chip** — see
    /// [`mouse_move`](Self::mouse_move).
    pub fn alpha(&self, now: Instant) -> Option<f32> {
        if self.progress.is_some() {
            return Some(1.0);
        }
        // The keyboard on the line holds the chip as the pointer does: a
        // toast somebody is answering does not fade under their keys.
        if self.over_chip || self.line_focus.is_some() {
            return Some(1.0);
        }
        let lived = now.saturating_duration_since(self.shown);
        if lived < HOLD {
            return Some(1.0);
        }
        let fading = (lived - HOLD).as_secs_f32() / FADE.as_secs_f32();
        (fading < 1.0).then(|| 1.0 - fading)
    }

    /// **Only during the fade or indeterminate progress.** See the module docs
    /// — holding a message still is not an animation, and asking for 8 ms frames
    /// to do it would cost about 625 of them. Hover freezes a transient hold, so
    /// it is not animating then either.
    pub fn animating(&self, now: Instant) -> bool {
        if let Some(progress) = &self.progress {
            return progress.animating();
        }
        !self.over_chip
            && self.line_focus.is_none()
            && now.saturating_duration_since(self.shown) >= HOLD
            && self.alpha(now).is_some()
    }

    // -------------------------------------------------------------- paint

    pub fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        over: Rect,
        floor: f32,
        alpha: f32,
        now: Instant,
    ) {
        let p = theme::p();
        let r = self.rect(engine, over, floor);
        // `Raised`, not `Flat`: the elevation contract says anything you act on
        // is raised, and this has two things you can press. `faded` carries the
        // shadow with it, which is why the fade goes through the panel rather
        // than through the colours.
        scene::floating_panel(
            scene,
            root,
            r.x,
            r.y,
            r.w,
            r.h,
            &scene::Panel::popover(RADIUS)
                .fill(p.bubble_bg)
                .borderless()
                .faded(alpha),
        );

        let chrome = PAD_X * 2.0 + GAP + CLOSE;
        let flow_w = (r.w - chrome).max(40.0);
        let (msg, _, _, _) = self.flow(engine, flow_w);
        scene::draw_layout(
            scene,
            root,
            &msg,
            r.x + PAD_X,
            r.y + PAD_Y,
            Some(p.bubble_fg.multiply_alpha(alpha)),
        );

        if let Some(progress) = &self.progress
            && let Some((bar, _)) = self.progress_row(engine, r)
        {
            progress.draw(scene, root, bar, now, alpha);
        }

        // The line: its sentence under the message, its answers after it in
        // the action's own words and colours, the keyboard's one ringed.
        let (line_x, line_y, line_w) = self.line_origin(engine, r);
        if let Some((sentence, answers, _)) = self.line_flow(engine, line_w) {
            scene::draw_layout(
                scene,
                root,
                &sentence,
                line_x,
                line_y,
                Some(p.bubble_fg.multiply_alpha(alpha)),
            );
            for (index, (label, rect)) in self.line_actions().iter().zip(answers).enumerate() {
                let (x, y) = (line_x + rect.x, line_y + rect.y);
                if self.line_focus == Some(index) {
                    scene::stroke(
                        scene,
                        root,
                        p.focus_ring.multiply_alpha(alpha),
                        theme::BORDER_W * 2.0,
                        &scene::rrect(x - 3.0, y - 2.0, rect.w + 6.0, rect.h + 4.0, 4.0),
                    );
                }
                let layout = engine.simple_layout(label, ACTION);
                let colour = if self.line_hover == Some(index) {
                    p.on_accent
                } else {
                    p.accent
                };
                scene::draw_layout(
                    scene,
                    root,
                    &layout,
                    x,
                    y,
                    Some(colour.multiply_alpha(alpha)),
                );
            }
        }

        for (hit, t) in self.targets(engine, over, floor) {
            match hit {
                Hit::Action | Hit::Secondary => {
                    let label = if hit == Hit::Action {
                        self.action
                    } else {
                        self.secondary
                    };
                    let Some(label) = label else { continue };
                    let l = engine.simple_layout(label, ACTION);
                    let colour = if self.hover == Some(hit) {
                        p.on_accent
                    } else {
                        p.accent
                    };
                    scene::draw_layout(
                        scene,
                        root,
                        &l,
                        t.x,
                        t.y,
                        Some(colour.multiply_alpha(alpha)),
                    );
                }
                Hit::Close => {
                    if self.hover == Some(Hit::Close) {
                        scene::fill(
                            scene,
                            root,
                            p.bubble_hover.multiply_alpha(alpha),
                            &scene::rrect(t.x, t.y, t.w, t.h, 4.0),
                        );
                    }
                    let l = engine.icon_layout(koru_draw::icons::WINDOW_CLOSE, theme::CAPTION_ICON);
                    scene::draw_layout(
                        scene,
                        root,
                        &l,
                        t.x + (t.w - l.width()) * 0.5,
                        t.y + (t.h - l.height()) * 0.5,
                        Some(p.bubble_dim.multiply_alpha(alpha)),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod secondary_tests {
    use super::*;

    /// Two actions read left to right after the message — *View task*, then
    /// *Undo* — each pressable where it is drawn, the close box after both.
    #[test]
    fn a_second_action_sits_after_the_first_and_before_the_close_box() {
        let mut engine = TextEngine::new();
        let over = Rect::new(0.0, 0.0, 800.0, 600.0);
        let toast = Toast::new("Added to Next".into(), Some("View task"), Instant::now())
            .with_secondary("Undo");
        assert_eq!(toast.secondary(), Some("Undo"));
        let first = toast
            .action_rect(&mut engine, over, 600.0)
            .expect("an action");
        let second = toast
            .secondary_rect(&mut engine, over, 600.0)
            .expect("a second action");
        let close = toast
            .close_rect(&mut engine, over, 600.0)
            .expect("a close box");
        assert!(second.x > first.x + first.w, "after the first");
        assert!(close.x > second.x + second.w, "and before the close box");
        assert_eq!(second.y, first.y, "on the same line");
        let mut pressed = toast;
        let (x, y) = second.centre();
        assert_eq!(
            pressed.mouse_down(&mut engine, over, 600.0, x, y),
            Outcome::Secondary
        );
        let (x, y) = first.centre();
        assert_eq!(
            pressed.mouse_down(&mut engine, over, 600.0, x, y),
            Outcome::Action
        );
    }
}
