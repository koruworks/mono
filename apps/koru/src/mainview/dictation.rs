// SPDX-License-Identifier: MPL-2.0

//! **The listening surface**: where it hangs, when it is alive, and how it
//! stops asking for frames.
//!
//! Deliberately the smallest honest indicator — a microphone, a word and a
//! coarse meter. The voice plan's Milestone 5 replaces what is *drawn* here
//! with the designed pill and **inherits all three of the answers below
//! unchanged**, which is why the anchor and the lifetime are built once and
//! the decoration later.
//!
//! ## Where it hangs
//!
//! [`MainView::dictation_anchor`] converts whatever surface the dictation was
//! routed into to **window coordinates**, and every conversion is named there
//! rather than guessed here. The rule the plan sets — `composition_rect()`,
//! falling back to `Editing::caret_screen_rect()` — is the *document's*
//! answer and lives in `BuiltInDocument::dictation_anchor`; the container's own
//! prompts are modal panels whose rectangles it already computes for drawing,
//! so those are what it hangs from (Q-V62).
//!
//! ## When it is alive
//!
//! Exactly while `koru_dictation::Dictation::status()` answers `Some`, which
//! the shell pushes through [`View::dictation_status`]. That is bounded on
//! both sides by the controller's own `HOLD_CAP` and `FINALISE_TIMEOUT`, so
//! "alive" is not something a bug can make permanent.
//!
//! ## How it stops asking for frames
//!
//! **It is not in `View::animating`, and it must never be put there.**
//! `animating` is the shell's 8 ms wake, and a meter that asked for one would
//! hold the whole window at 125 frames a second for the length of a sentence —
//! more expensive than the recogniser. It uses the mechanism the caret blink
//! already uses instead: [`METER_FRAME`] on `maintenance_deadline`, and
//! `maintain` answering `true` when that deadline passes. Both go back to
//! `None`/`false` the pass the session ends, because [`Listening`] is gone by
//! then.
//!
//! ## One geometry function
//!
//! [`geometry`] is the only arithmetic, and both drawing and hit-testing come
//! to it — the rule `docview::buttons` and `Table::row_rect` already follow.
//! The indicator moves with the caret, so two copies would put the microphone
//! where it is not drawn. Its inputs are [`Metrics`]: the measured widths,
//! taken once when the state word changes, so hit-testing never needs a
//! `TextEngine`.

use std::time::{Duration, Instant};

use koru_dictation::{LevelBucket, Refusal, Stage, State, Status};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene as draw_scene, theme};
use koru_keys::{Section, ShortcutId, show_hint};
use koru_workspace_host::routing::{EditableTarget, FieldId};
use vello::Scene;
use vello::kurbo::Affine;

use super::MainView;
use super::tab::TabView;

/// **How often the meter redraws while somebody is speaking.**
///
/// Ten hertz, fed by the level buckets the worker already sends. It is a
/// *deadline*, not a frame rate: the shell takes the minimum of this, the
/// caret's blink and the document's next poll, so a dictation costs one extra
/// wake per hundred milliseconds and nothing at all when none is running.
///
/// Interpolating between buckets at the display's rate was considered and is
/// the thing this constant exists to refuse.
pub(crate) const METER_FRAME: Duration = Duration::from_millis(100);

/// The panel's own padding, gap between its parts, and corner.
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
const RADIUS: f32 = 8.0;

/// How far below (or above) the composition the indicator sits, and how close
/// it may come to the window's edge — the same eight pixels every anchored
/// surface in this app keeps.
const ANCHOR_GAP: f32 = 8.0;
const EDGE: f32 = 8.0;

/// The microphone glyph's size. `CAPTION_ICON` rather than `ICON_SIZE`: this is
/// an instrument floating over the page, not a toolbar control.
const ICON: f32 = theme::CAPTION_ICON;

/// One cell of the meter, and the space between two.
const CELL_W: f32 = 3.0;
const CELL_GAP: f32 = 3.0;

/// **Nine cells, because the level is a bucket `0..=8`.** Derived from
/// [`LevelBucket::MAX`] rather than written out, so a wider scale is one edit
/// in `koru-voice`.
const CELLS: usize = LevelBucket::MAX as usize + 1;

/// The meter's width: every cell, and a gap between each pair.
const METER_W: f32 = CELLS as f32 * CELL_W + (CELLS - 1) as f32 * CELL_GAP;

/// What the indicator has to be told before it can place itself.
///
/// Measured with a [`TextEngine`] when the state word changes, and never
/// again — a hit-test happens between frames, where there is no engine to
/// borrow, and a second measurement is a second answer.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub(crate) struct Metrics {
    pub icon_w: f32,
    pub icon_h: f32,
    pub state_w: f32,
    pub state_h: f32,
    /// `None` when [`show_hint`] says draw nothing — the chord column is not
    /// an empty gap, it is absent.
    pub hint_w: Option<f32>,
    pub hint_h: f32,
    /// The meter's height, from `role::meta()`'s line. The level is the one
    /// part of this surface with no text of its own, so it takes its size from
    /// the role it would have used.
    pub level_h: f32,
}

/// Every rectangle the indicator has, in window coordinates.
#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) struct Geometry {
    pub panel: Rect,
    pub mic: Rect,
    pub state: Rect,
    pub meter: Rect,
    pub hint: Option<Rect>,
}

/// What a press landed on.
///
/// Every one of them ends the dictation — the whole chip is the control in
/// this version, and M5's pill is where a distinct stop button narrows it. The
/// enum exists so the hit-test can be asserted element by element rather than
/// as one rectangle that would still pass with the parts in the wrong places.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Element {
    Mic,
    State,
    Meter,
    Hint,
    /// The panel's own padding: on the surface, on none of its parts.
    Surface,
}

impl Geometry {
    /// **The one hit-test.** `None` means the press was not on the indicator at
    /// all, and the caller may treat it as the ordinary local event it is.
    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Element> {
        if !self.panel.contains(x, y) {
            return None;
        }
        if self.mic.contains(x, y) {
            return Some(Element::Mic);
        }
        if self.state.contains(x, y) {
            return Some(Element::State);
        }
        if self.meter.contains(x, y) {
            return Some(Element::Meter);
        }
        if self.hint.is_some_and(|r| r.contains(x, y)) {
            return Some(Element::Hint);
        }
        Some(Element::Surface)
    }
}

/// **Place the indicator against `anchor`, inside `viewport`.**
///
/// Below the composition, flipped above it when there is no room, and clamped
/// to [`EDGE`] on every side — so a caret at any corner of the window still
/// gets a chip that is wholly on screen.
///
/// The left edge follows the composition rather than centring on it: a meter
/// that slid sideways as the transcript grew would be the one moving thing on
/// screen that nobody asked for.
pub(crate) fn geometry(anchor: Rect, viewport: (f32, f32), m: Metrics) -> Geometry {
    let hint_run = m.hint_w.map_or(0.0, |w| GAP + w);
    let inner_w = m.icon_w + GAP + m.state_w + GAP + METER_W + hint_run;
    let inner_h = m
        .icon_h
        .max(m.state_h)
        .max(m.level_h)
        .max(if m.hint_w.is_some() { m.hint_h } else { 0.0 });
    let w = inner_w + PAD * 2.0;
    let h = inner_h + PAD * 2.0;

    let below = anchor.y + anchor.h + ANCHOR_GAP;
    // Flipped above only when below would not fit. Testing the *bottom* rather
    // than the top is what makes a caret on the last line of a full window put
    // the chip over the text instead of off the end of it.
    let y = if below + h + EDGE > viewport.1 {
        anchor.y - ANCHOR_GAP - h
    } else {
        below
    };
    let x = clamp(anchor.x, EDGE, viewport.0 - w - EDGE);
    let y = clamp(y, EDGE, viewport.1 - h - EDGE);
    let panel = Rect::new(x, y, w, h);

    let centred = |eh: f32| y + (h - eh) * 0.5;
    let mut cx = x + PAD;
    let mic = Rect::new(cx, centred(m.icon_h), m.icon_w, m.icon_h);
    cx += m.icon_w + GAP;
    let state = Rect::new(cx, centred(m.state_h), m.state_w, m.state_h);
    cx += m.state_w + GAP;
    let meter = Rect::new(cx, centred(m.level_h), METER_W, m.level_h);
    cx += METER_W;
    let hint = m.hint_w.map(|hw| {
        cx += GAP;
        Rect::new(cx, centred(m.hint_h), hw, m.hint_h)
    });

    Geometry {
        panel,
        mic,
        state,
        meter,
        hint,
    }
}

/// `f32::clamp` panics when the window is narrower than the chip, which is a
/// real window on a real laptop with three columns open. The low edge wins.
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi.max(lo))
}

/// One cell of the meter, left to right. `lit` says whether the level has
/// reached it.
pub(crate) fn cells(meter: Rect, level: LevelBucket) -> impl Iterator<Item = (Rect, bool)> {
    let step = level.step();
    (0..CELLS).map(move |i| {
        let x = meter.x + i as f32 * (CELL_W + CELL_GAP);
        (Rect::new(x, meter.y, CELL_W, meter.h), (i as u8) < step)
    })
}

/// **What the surface says it is doing.**
///
/// A pure function of the status, which is what lets it be tested without a
/// window: the gesture's stage first, because `Finalising` is true whatever the
/// worker last said, and the worker's own state only while the gesture is live.
pub(crate) fn state_key(status: Status) -> koru_i18n::Key {
    match (status.stage, status.worker) {
        // Either end may say the utterance is over: the gesture, when the key
        // has come up, or the worker, which is where the two can be a message
        // apart. Both mean the same thing to somebody looking at the chip.
        (Stage::Finalising, _) | (_, State::Finalising) => koru_i18n::Key::VoiceFinishing,
        (_, State::Listening) => koru_i18n::Key::VoiceListening,
        // `Idle` and `Loading` alike: the session has been asked for and the
        // microphone is not open yet, and *Starting* is the honest word for
        // both.
        (_, State::Idle | State::Loading) => koru_i18n::Key::VoiceStarting,
    }
}

/// **The sentence for a refusal.**
///
/// The surface refusal comes first because it has no `ErrorCode` of its own:
/// which box is a secret is the host's knowledge, not the worker's, so the
/// controller borrows `WrongState` to carry it and the flag is what separates
/// them.
pub(crate) fn refusal_key(refusal: Refusal) -> koru_i18n::Key {
    if refusal.surface {
        return koru_i18n::Key::VoiceErrorSurface;
    }
    // **The mapping is `ErrorCode`'s, not this surface's**, because the
    // notification area's voice submenu shows the same sentence for the same
    // code and cannot see this file. Which row a code deserves — including the
    // two `PermissionDenied` rows, one per platform (Q-V75) — is a fact about
    // the code; see `koru_voice::ErrorCode::sentence_key`.
    refusal.code.sentence_key()
}

/// **An open dictation, from the surface's side.**
///
/// Held only while the shell says a session is running. Everything that asks
/// for a frame hangs off this being `Some`.
pub(crate) struct Listening {
    pub(crate) status: Status,
    /// The state word, kept so the metrics are re-measured when it moves and
    /// not on every level bucket.
    pub(crate) word: koru_i18n::Key,
    /// The chord, already resolved through [`show_hint`] — `None` means draw
    /// no column, which is the switch's whole purpose.
    pub(crate) hint: Option<String>,
    pub(crate) metrics: Metrics,
    /// The next 10 Hz redraw. See [`METER_FRAME`].
    pub(crate) next_frame: Instant,
}

/// Measure the two strings and the glyph, once.
fn measure(engine: &mut TextEngine, word: &str, hint: Option<&str>) -> Metrics {
    let icon = engine.icon_layout(icons::MICROPHONE, ICON);
    let state = engine.simple_layout(word, theme::role::ui());
    let meta = engine.simple_layout("0", theme::role::meta());
    let hint_layout = hint.map(|h| engine.simple_layout(h, theme::role::hint()));
    Metrics {
        icon_w: icon.width(),
        icon_h: icon.height(),
        state_w: state.width(),
        state_h: state.height(),
        hint_w: hint_layout.as_ref().map(|l| l.width()),
        hint_h: hint_layout.as_ref().map_or(0.0, |l| l.height()),
        level_h: meta.height(),
    }
}

impl MainView {
    /// **Where the listening indicator hangs from, in window coordinates.**
    ///
    /// One `match` over the surface the dictation was routed into, because the
    /// coordinate space is the implementor's and is declared per implementor —
    /// `koru_ui::dictation`'s module note is explicit that two surfaces
    /// answering in different spaces must not be consumed interchangeably.
    /// This is the only place in the app that converts them.
    pub(crate) fn dictation_anchor(&self) -> Option<Rect> {
        let live = self.dictation.as_ref()?;
        match live.target {
            // The active tab's own space: content column and scroll already
            // applied, so window space is one translation away. The tab is
            // addressed by the index the dictation began in — the same reason
            // `LiveDictation` records it.
            EditableTarget::Tab(_)
            | EditableTarget::Field(FieldId::Find)
            | EditableTarget::Field(FieldId::Replace)
            | EditableTarget::Field(FieldId::DocumentPrompt) => {
                let leaf = match self.tabs().get(live.tab).map(|t| &t.view) {
                    Some(TabView::Leaf(leaf)) => leaf,
                    _ => return None,
                };
                let r = leaf.dictation_anchor()?;
                let c = self.content();
                Some(Rect::new(r.x + c.x, r.y + c.y, r.w, r.h))
            }
            // **The container's own prompts are modal panels**, and the panel
            // is the rectangle this container owns. A `TextField` reports its
            // composition along its own laid-out line, before the field's
            // horizontal scroll and before whatever box a host drew around it,
            // so converting one would mean five prompts each publishing their
            // field's drawn rect. Anchoring to the panel says the same thing —
            // *the words are going in here* — with no second arithmetic to
            // drift. See Q-V62.
            EditableTarget::Field(FieldId::Search) => Some(self.search_bounds()),
            EditableTarget::Field(FieldId::MoveTo) => Some(self.move_to_bounds()),
            EditableTarget::Field(FieldId::FontPicker) => {
                self.font_picker.as_ref().map(|_| self.font_picker_bounds())
            }
            EditableTarget::Field(FieldId::NewDocument) => self
                .new_document
                .as_ref()
                .map(|_| super::new_document::NewDocument::panel(self.viewport)),
            EditableTarget::Field(FieldId::Snapshot) => self
                .snapshot_prompt
                .as_ref()
                .map(|_| super::snapshot_prompt::SnapshotPrompt::panel(self.viewport)),
            EditableTarget::Field(FieldId::Appearance) => {
                self.workspace_icon.as_ref().map(|m| m.panel(self.viewport))
            }
            EditableTarget::Field(FieldId::Rename) => self.workspace_rename_row(),
            EditableTarget::Field(FieldId::Ai) => Some(self.ai_chat.dictation_anchor()),
            EditableTarget::Field(FieldId::ReminderCreation) => {
                Some(self.reminder_creation.dictation_anchor())
            }
            // The device-link code is a secret and cannot be dictated into.
            EditableTarget::Field(FieldId::LinkDevice) => None,
            // The rail's inline list-name field is a one-word affair drawn by
            // the explore column; it publishes no rect here and declines a
            // dictation the way the link code does.
            EditableTarget::Field(FieldId::NewList) => None,
            // The delegation modal's panel, for the reason every other
            // container prompt anchors to its panel: the objective is a
            // wrapped area with its own scroll, and converting its laid-out
            // line would be a second arithmetic to drift.
            EditableTarget::Field(FieldId::Delegate) => self.delegation_panel(),
            EditableTarget::None => None,
        }
    }

    /// The indicator's rectangles, or `None` when nothing is being dictated.
    pub(crate) fn dictation_geometry(&self) -> Option<Geometry> {
        let open = self.listening.as_ref()?;
        let anchor = self.dictation_anchor()?;
        Some(geometry(anchor, self.viewport, open.metrics))
    }

    /// **The shell says what the session is doing.** Answers whether anything
    /// on screen changed.
    ///
    /// `None` drops [`Listening`], which is what returns
    /// `maintenance_deadline` to its idle answer — the lifetime is one field's
    /// presence rather than a flag anybody has to remember to clear.
    pub(crate) fn set_dictation_status(&mut self, status: Option<Status>) -> bool {
        let Some(status) = status else {
            return self.listening.take().is_some();
        };
        let word = state_key(status);
        if self
            .listening
            .as_ref()
            .is_some_and(|open| open.word == word)
        {
            let open = self.listening.as_mut().expect("just checked");
            let changed = open.status != status;
            open.status = status;
            return changed;
        }
        // A new session, or a state word that moved. Both need a measurement,
        // and nothing else does — a level bucket changes nine pixels of fill.
        let hint = show_hint(ShortcutId::Dictate, Section::Guide);
        let metrics = measure(
            &mut self.engine,
            koru_i18n::t(crate::format::lang(), word),
            hint.as_deref(),
        );
        let next_frame = self
            .listening
            .as_ref()
            .map_or_else(|| Instant::now() + METER_FRAME, |open| open.next_frame);
        self.listening = Some(Listening {
            status,
            word,
            hint,
            metrics,
            next_frame,
        });
        true
    }

    /// Re-measure after a theme, font or density change, which invalidates
    /// every layout in the process. Free when nothing is listening.
    pub(crate) fn remeasure_dictation(&mut self) {
        let Some(open) = self.listening.as_ref() else {
            return;
        };
        let (word, hint) = (open.word, open.hint.clone());
        let metrics = measure(
            &mut self.engine,
            koru_i18n::t(crate::format::lang(), word),
            hint.as_deref(),
        );
        if let Some(open) = self.listening.as_mut() {
            open.metrics = metrics;
        }
    }

    /// The 10 Hz meter tick. `true` asks for one redraw and no more.
    pub(crate) fn tick_dictation(&mut self, now: Instant) -> bool {
        let Some(open) = self.listening.as_mut() else {
            return false;
        };
        if now < open.next_frame {
            return false;
        }
        open.next_frame = now + METER_FRAME;
        true
    }

    /// When the meter next wants a frame. `None` whenever nothing is running,
    /// so an idle window is never woken by this surface.
    pub(crate) fn dictation_deadline(&self) -> Option<Instant> {
        self.listening.as_ref().map(|open| open.next_frame)
    }

    /// **Whether this press is the one click that does not discard.** See
    /// [`koru_workspace_host::View::dictation_hit`].
    pub(crate) fn dictation_pressed(&self, x: f32, y: f32) -> bool {
        self.dictation_geometry()
            .and_then(|g| g.hit(x, y))
            .is_some()
    }

    /// Say why a dictation will not run, in the surface this app already uses
    /// for something that has already happened: a toast. No new floating
    /// surface, no new palette field, and it fades itself out — which is the
    /// whole reason it was reached for rather than a second panel.
    pub(crate) fn report_dictation_refusal(&mut self, refusal: Refusal) -> bool {
        let message = koru_i18n::t(crate::format::lang(), refusal_key(refusal)).to_owned();
        self.toast = Some((
            koru_ui::Toast::new(message, None, Instant::now()),
            super::ToastPayload::None,
        ));
        true
    }

    /// **Paint it.** Last of all, over the prompts it may be annotating: the
    /// composition is inside one of them, so an indicator behind it would be
    /// describing something it was hidden by.
    pub(crate) fn draw_dictation(&mut self, scene: &mut Scene, root: Affine) {
        let Some(g) = self.dictation_geometry() else {
            return;
        };
        let Some(open) = self.listening.as_ref() else {
            return;
        };
        let status = open.status;
        // `&'static str` and an owned clone: both let the borrow of `self.listening`
        // end before `self.engine` is taken, which is the same two-disjoint-borrows
        // problem every other surface in this file solves by taking and putting back.
        let word = koru_i18n::t(crate::format::lang(), open.word);
        let hint = open.hint.clone();
        let p = theme::p();

        // Raised, because it can be pressed. A tooltip is `Flat` precisely
        // because it cannot.
        draw_scene::floating_panel(
            scene,
            root,
            g.panel.x,
            g.panel.y,
            g.panel.w,
            g.panel.h,
            &draw_scene::Panel::popover(RADIUS),
        );

        // The microphone lights only while the device is actually open: while a
        // model loads there is nothing to hear, and a lit microphone would be
        // saying otherwise.
        let mic = if status.worker == State::Listening {
            p.accent
        } else {
            p.text_dim
        };
        let icon = self.engine.icon_layout(icons::MICROPHONE, ICON);
        draw_scene::draw_layout(scene, root, &icon, g.mic.x, g.mic.y, Some(mic));

        let label = self.engine.simple_layout(word, theme::role::ui());
        draw_scene::draw_layout(scene, root, &label, g.state.x, g.state.y, Some(p.text));

        // Nine cells and never a float. `scrollbar_thumb` is the app's existing
        // faint-bar colour, so the meter adds no palette field.
        for (cell, lit) in cells(g.meter, status.level) {
            let colour = if lit { p.accent } else { p.scrollbar_thumb };
            draw_scene::fill(
                scene,
                root,
                colour,
                &draw_scene::rrect(cell.x, cell.y, cell.w, cell.h, CELL_W * 0.5),
            );
        }

        if let (Some(text), Some(rect)) = (hint, g.hint) {
            let layout = self.engine.simple_layout(&text, theme::role::hint());
            draw_scene::draw_layout(scene, root, &layout, rect.x, rect.y, Some(p.text_dim));
        }
    }
}

#[cfg(test)]
mod tests;
