// SPDX-License-Identifier: MPL-2.0

//! **Voice input in the composer** (koru-ai D103): the microphone in the
//! composer's footer, and the row that footer becomes while it records.
//!
//! ```text
//!   idle      [privacy] [access]      [mode] [model]   🎤  ↑
//!   recording ▁▂▅▇▅▂▁ · · · · · · · · · · · · ·   0:07  ✕  ■
//! ```
//!
//! ## What is here, and what is not
//!
//! **The words are not here.** A dictation is the editor's ephemeral
//! composition over the composer's own document — `ChatComposer`'s
//! `DictationTarget` — and the controller deciding what happens to it is
//! `koru_dictation::Dictation`, owned by the host: the workspace shell for the
//! pane and a chat tab. The host routes the
//! controller's four actions into the composer exactly as it did before there
//! was a button, so the recognised words stream in at the caret, drawn dim and
//! underlined while they are provisional and as ordinary text once the final
//! transcript commits — the same composition a document shows.
//!
//! What this module adds is what a person **sees and presses** about the
//! gesture, and it is told about the gesture rather than running it:
//!
//! - the microphone asks the host to begin ([`ChatSurface::take_wants_dictation`]);
//! - the host pushes what its controller is doing ([`DictationShown`]: a level
//!   bucket and two flags, never a transcript) and `None` the pass it ends;
//! - while one is shown, the footer row is the recording row — a waveform from
//!   the level, a timer, *Cancel dictation* (✕) and *Stop dictation* (■) — and
//!   [`ChatSurface::dictation_press_at`] tells the host which of the two a
//!   press means **before** the press reaches anything else. The text area
//!   above stays where it was and stays the composer.
//!
//! No audio, no microphone and no model are touched from here, so nothing in
//! this module can block the UI thread on a device; the host's controller
//! talks to the Agent-owned voice worker as it always has.
//!
//! ## Frames
//!
//! **Only while the microphone is open.** The waveform scrolls on
//! [`FRAME`] through `maintenance_deadline`, the deadline path every other
//! clock on this surface uses — never `animating`, which is the host's 8 ms
//! wake. Before the microphone opens (a model loading), while the last words
//! are being finished, and whenever nothing is recording, the deadline is
//! `None` and an idle chat is never woken by voice.
//!
//! ## The row is elements
//!
//! The recording row is one `koru_draw::el` tree, laid out by the element
//! solver into one frame that painting, hit-testing and the accessibility
//! names all read — the model picker's arrangement, so ✕ is pressed where it
//! is drawn by construction. The waveform and the two round buttons are named
//! leaves; the timer is element text.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use koru_draw::el::{
    Action, El, ElState, Frame, FrameCx, Interaction, Key as ElKey, Leaf, Role, TextCache, leaf,
    row, text,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Profile, Space, roles};
use koru_draw::{icons, scene};
use koru_ui::icon_button::{self, Metrics, State as IconButtonState};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, NamedKey};

use super::{COMPOSER_ICON_SIZE, COMPOSER_PAD_X, ChatSurface, Focus};

/// The loudest level a host reports: `koru_voice::LevelBucket::MAX`. Written
/// here rather than linked, because this crate links no voice crate — the
/// host converts, and a louder number is clamped rather than refused.
pub(crate) const LEVEL_MAX: u8 = 8;

/// **What a host's dictation is doing, as the composer draws it.**
///
/// Three `Copy` fields and never the words: the waveform needs *that*
/// somebody is speaking and how loudly, and the row needs to know whether the
/// microphone is open yet and whether the last words are being finished.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct DictationShown {
    /// The input level, `0..=`[`LEVEL_MAX`].
    pub level: u8,
    /// The microphone is open. `false` while a model loads: the clock has not
    /// started and the waveform is a flat line.
    pub listening: bool,
    /// The recording was stopped and the recogniser is finishing the last
    /// words. The clock and the waveform hold still, and ■ is spent.
    pub finishing: bool,
}

/// **What a press means while the composer records**, answered before the
/// press reaches anything — see [`ChatSurface::dictation_press_at`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DictationPress {
    /// ■, or anywhere on the composer: finish the last words and keep them.
    /// The press is spent on the stop — the composer's editor cannot take a
    /// click while a composition is open.
    Stop,
    /// ✕: throw away what this recording put in; what was typed before stays.
    Cancel,
}

/// One waveform frame: about thirty a second, and only while the microphone
/// is open.
pub(crate) const FRAME: Duration = Duration::from_millis(33);

/// How often a new bar enters at the right. The level is sampled here, and
/// the bars slide between samples, so the line moves smoothly at [`FRAME`]
/// without inventing levels the worker did not send.
const BAR_EVERY: Duration = Duration::from_millis(80);

/// A bar's width and the space after it.
const BAR_W: f32 = 2.0;
const BAR_GAP: f32 = 2.0;

/// A silent sample is a dot this tall; the loudest bar takes this share of
/// the row.
const BAR_MIN_H: f32 = 2.0;
const BAR_REACH: f32 = 0.72;

/// The most bars kept: wider than any composer is, at [`BAR_W`] + [`BAR_GAP`].
const MAX_BARS: usize = 512;

/// After a stall (a window dragged, a machine slept) the waveform catches up
/// at most this many bars and then starts again from now, rather than
/// looping through every sample it missed.
const CATCH_UP: u32 = 32;

/// The ■ glyph's size inside its round button: the send arrow's.
const STOP_GLYPH: f32 = 13.0;

const SURFACE: &str = "Composer dictation";
/// The element keys of the two buttons — fixed parts of this surface.
pub(crate) const CANCEL: ElKey = ElKey::surface(SURFACE, "cancel");
pub(crate) const STOP: ElKey = ElKey::surface(SURFACE, "stop");
/// The row's developer names, as the inspector shows them.
pub(crate) const ROW: &str = "Dictation row";
pub(crate) const WAVEFORM: &str = "Dictation waveform";
pub(crate) const TIMER: &str = "Dictation timer";

/// The tooltip family's keys: the microphone and the two buttons.
const TIP_MIC: u64 = 1;
const TIP_CANCEL: u64 = 2;
const TIP_STOP: u64 = 3;
const TIP_CHOOSE: u64 = 4;

/// **The composer's voice input**: whether the host offers it, whether the
/// microphone was pressed, and the recording while one is shown.
#[derive(Default)]
pub(crate) struct ComposerDictation {
    /// The host can dictate into this composer — it owns a controller and
    /// voice is available on this installation. Without it there is no
    /// microphone, though a dictation the host routes here is still shown.
    pub(crate) offered: bool,
    /// The microphone was pressed; the host begins a dictation and takes it.
    wants: bool,
    live: Option<Recording>,
    tooltip: Tooltip,
    /// The chevron's panel: which microphone, and *Hold to record*
    /// ([`crate::microphone`]).
    pub(crate) microphone: crate::microphone::Panel,
}

impl ComposerDictation {
    /// Let the tooltip over the microphone or the chevron go.
    pub(crate) fn tooltip_clear(&mut self) {
        self.tooltip.clear();
    }
}

/// One recording, from the host's first `Some` to its `None`.
struct Recording {
    shown: DictationShown,
    /// When the microphone opened: the timer's zero. `None` while it has not.
    since: Option<Instant>,
    /// When the recording was stopped: the timer and the bars hold here.
    stopped: Option<Instant>,
    /// The sampled levels, oldest first.
    levels: VecDeque<u8>,
    /// When the newest bar was sampled.
    sampled: Instant,
    /// The next waveform frame.
    next_frame: Instant,
}

impl Recording {
    fn new(shown: DictationShown, now: Instant) -> Self {
        let mut recording = Recording {
            shown: DictationShown::default(),
            since: None,
            stopped: None,
            levels: VecDeque::new(),
            sampled: now,
            next_frame: now + FRAME,
        };
        recording.update(shown, now);
        recording
    }

    /// Take the host's latest. Answers whether anything drawn changed.
    fn update(&mut self, shown: DictationShown, now: Instant) -> bool {
        let shown = DictationShown {
            level: shown.level.min(LEVEL_MAX),
            ..shown
        };
        if self.since.is_none() && shown.listening {
            self.since = Some(now);
            self.sampled = now;
            self.next_frame = now + FRAME;
        }
        if self.stopped.is_none() && shown.finishing {
            self.stopped = Some(now);
        }
        let changed = self.shown != shown;
        self.shown = shown;
        changed
    }

    /// Whether the waveform is moving: the microphone open, and not stopped.
    fn running(&self) -> bool {
        self.since.is_some() && self.stopped.is_none()
    }

    /// How long the microphone has been open, held at the stop.
    fn clock(&self, now: Instant) -> Duration {
        self.since.map_or(Duration::ZERO, |since| {
            self.stopped.unwrap_or(now).saturating_duration_since(since)
        })
    }

    /// Bring the bars up to `now`: one sample per [`BAR_EVERY`].
    fn sample(&mut self, now: Instant) {
        let mut pushed = 0;
        while now.saturating_duration_since(self.sampled) >= BAR_EVERY {
            if pushed == CATCH_UP {
                self.sampled = now;
                break;
            }
            self.levels.push_back(self.shown.level);
            if self.levels.len() > MAX_BARS {
                self.levels.pop_front();
            }
            self.sampled += BAR_EVERY;
            pushed += 1;
        }
    }

    /// How far the bars have slid since the newest was sampled, `0..1` of a
    /// bar's pitch. Held at the stop.
    fn phase(&self, now: Instant) -> f32 {
        if self.since.is_none() {
            return 0.0;
        }
        let at = self.stopped.unwrap_or(now);
        (at.saturating_duration_since(self.sampled).as_secs_f32() / BAR_EVERY.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

/// `0:07`, `1:23`, `12:05`.
pub(crate) fn clock_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

thread_local! {
    /// The engine and cache the row's layouts run on — its one text is the
    /// timer — so a hit-test between frames needs no borrowed engine. The
    /// model picker's arrangement (`model_picker::elements::layout`).
    static LAYOUT: RefCell<Option<(TextEngine, TextCache)>> = const { RefCell::new(None) };
}

fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    LAYOUT.with_borrow_mut(|slot| {
        let (engine, cache) = slot.get_or_insert_with(|| (TextEngine::new(), TextCache::default()));
        tree.layout(engine, cache, area, cx)
    })
}

impl ChatSurface {
    /// **Offer voice input here**: the host owns a dictation controller and
    /// voice is available on this installation. Shows the microphone.
    pub fn offer_dictation(&mut self, offered: bool) {
        self.dictation.offered = offered;
    }

    /// Whether the microphone was pressed since the host last asked. Taken by
    /// the asking: the host begins one dictation into this composer, through
    /// the same path its shortcut takes.
    pub fn take_wants_dictation(&mut self) -> bool {
        std::mem::take(&mut self.dictation.wants)
    }

    /// Whether the composer is showing a recording.
    pub fn dictating(&self) -> bool {
        self.dictation.live.is_some()
    }

    /// **What the host's dictation is doing**, pushed whenever it may have
    /// changed and `None` the pass it ends. Answers whether anything drawn
    /// changed, so the host redraws no more often than that.
    pub fn show_dictation(&mut self, shown: Option<DictationShown>, now: Instant) -> bool {
        let Some(shown) = shown else {
            let ended = self.dictation.live.take().is_some();
            if ended {
                self.dictation.tooltip.clear();
            }
            return ended;
        };
        match &mut self.dictation.live {
            Some(live) => live.update(shown, now),
            None => {
                self.dictation.live = Some(Recording::new(shown, now));
                self.dictation.wants = false;
                self.dictation.tooltip.clear();
                // The footer is the recording row now: no chevron, no panel.
                self.close_microphones();
                true
            }
        }
    }

    /// **What a press at `(x, y)` means while the composer records**, for the
    /// host to act on before the press reaches anything else: ✕ cancels, and
    /// ■ — or any other press on the composer — stops and keeps the words.
    /// `None` when nothing is recording, or the press is outside the
    /// composer: an ordinary press, which lands where it was aimed.
    pub fn dictation_press_at(&self, x: f32, y: f32) -> Option<DictationPress> {
        self.dictation.live.as_ref()?;
        let tree = self.dictation_tree(Instant::now())?;
        let frame = layout(&tree, self.dictation_row_rect(), FrameCx::root());
        if frame.hit_at(x, y) == Some(CANCEL) {
            return Some(DictationPress::Cancel);
        }
        self.composer_hit_rect
            .contains(x, y)
            .then_some(DictationPress::Stop)
    }

    /// Where the microphone is, as of the last frame, while it shows.
    #[doc(hidden)]
    pub fn microphone_for_host_test(&self) -> Option<Rect> {
        self.mic_button().filter(|_| self.dictation.live.is_none())
    }

    /// Where the chevron beside the microphone is, while it shows.
    #[doc(hidden)]
    pub fn microphone_chooser_for_host_test(&self) -> Option<Rect> {
        self.mic_chooser_button()
    }

    /// Where ✕ and ■ are, as of the last frame, while the composer records.
    #[doc(hidden)]
    pub fn dictation_buttons_for_host_test(&self) -> Option<(Rect, Rect)> {
        let (_, frame) = self.dictation_frame(Instant::now())?;
        Some((frame.node(CANCEL)?.rect, frame.node(STOP)?.rect))
    }

    /// The microphone, just before the send button — whether or not the
    /// composer has text. `None` where the host offers no voice input.
    pub(crate) fn mic_button(&self) -> Option<Rect> {
        if !self.dictation.offered {
            return None;
        }
        if let Some(shell) = &self.composer_shell {
            return shell.microphone();
        }
        let send = self.send_button();
        let gap = Space::S2.px(Profile::Desktop);
        let choice_width = if self.dictation.microphone.offered {
            Metrics::COMPACT.control_width() + Space::S0_5.px(Profile::Desktop)
        } else {
            0.0
        };
        Some(Rect::new(
            send.x - gap - choice_width - send.w,
            send.y,
            send.w,
            send.h,
        ))
    }

    /// The recording row: the footer's whole width inside the composer's
    /// padding, the send button's corner included — ■ takes its place.
    fn dictation_row_rect(&self) -> Rect {
        let composer = self.composer_hit_rect;
        let send = self.send_button();
        let x = composer.x + COMPOSER_PAD_X;
        Rect::new(x, send.y, (send.x + send.w - x).max(1.0), send.h)
    }

    /// The recording row as elements, while one is shown.
    fn dictation_tree(&self, now: Instant) -> Option<El> {
        let live = self.dictation.live.as_ref()?;
        let side = self.send_button().h;
        let finishing = live.stopped.is_some();
        let waveform = Waveform {
            levels: live.levels.iter().copied().collect(),
            phase: live.phase(now),
        };
        Some(
            row(ROW)
                .gap(Space::S2)
                .items_center()
                .child(leaf(WAVEFORM, waveform).decorative().fill())
                .child(
                    text(clock_text(live.clock(now)))
                        .named(TIMER)
                        .role(roles::META)
                        .ink(Ink::Secondary),
                )
                .child(
                    leaf(
                        "Cancel dictation",
                        RoundButton {
                            glyph: icons::WINDOW_CLOSE,
                            side,
                            primary: false,
                        },
                    )
                    .semantic()
                    .id(CANCEL)
                    .hit()
                    .action(Action::Click)
                    .semantics(Role::Button)
                    .name(koru_i18n::Key::AiDictationCancel),
                )
                .child(
                    leaf(
                        "Stop dictation",
                        RoundButton {
                            glyph: icons::STOP,
                            side,
                            primary: true,
                        },
                    )
                    .semantic()
                    .id(STOP)
                    .hit()
                    .action(Action::Click)
                    .semantics(Role::Button)
                    .name(koru_i18n::Key::AiDictationStop)
                    .disabled(finishing),
                ),
        )
    }

    /// The recording row's frame, under the pointer as it is now.
    pub(crate) fn dictation_frame(&self, now: Instant) -> Option<(El, Frame)> {
        let tree = self.dictation_tree(now)?;
        let area = self.dictation_row_rect();
        let hot = layout(&tree, area, FrameCx::root()).hit_at(self.pointer.0, self.pointer.1);
        let frame = layout(
            &tree,
            area,
            FrameCx::root().with_interaction(Interaction {
                hot,
                ..Interaction::default()
            }),
        );
        Some((tree, frame))
    }

    /// **The footer while recording.** Answers whether it drew the recording
    /// row, in which case the caller draws none of the toolbar's controls.
    pub(crate) fn draw_dictation_row(&mut self, canvas: &mut Scene, root: Affine) -> bool {
        let Some((tree, frame)) = self.dictation_frame(Instant::now()) else {
            return false;
        };
        frame.paint(&tree, canvas, root, &mut self.engine);
        true
    }

    /// The microphone, beside the send button, while nothing is recording.
    pub(crate) fn draw_mic(&mut self, canvas: &mut Scene, root: Affine, mic: Rect) {
        if self.dictation.live.is_some() {
            return;
        }
        icon_button::draw_with(
            canvas,
            root,
            &mut self.engine,
            mic,
            icons::MICROPHONE,
            IconButtonState {
                hovered: mic.contains(self.pointer.0, self.pointer.1),
                active: false,
                disabled: false,
            },
            Metrics {
                icon_size: COMPOSER_ICON_SIZE,
                ..Metrics::STANDARD
            },
        );
    }

    /// The tooltip over the microphone, ✕ or ■.
    pub(crate) fn draw_dictation_tooltip(&mut self, canvas: &mut Scene, root: Affine) {
        self.dictation.tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
    }

    /// **A press on the microphone, or on the recording row.** `None` when
    /// the press is neither, and it goes on to whatever it landed on.
    ///
    /// A host acts on ✕ and ■ through [`dictation_press_at`]
    /// (Self::dictation_press_at) before a press arrives here; this only
    /// makes sure a press that did arrive on a composer mid-composition is
    /// spent rather than given to the editor.
    pub(crate) fn dictation_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self.dictation.live.is_some() {
            return self.composer_hit_rect.contains(x, y).then_some(true);
        }
        let mic = self.mic_button()?;
        if !mic.contains(x, y) {
            return None;
        }
        self.dictation.wants = true;
        self.dictation.tooltip.clear();
        self.focus = Focus::Composer;
        Some(true)
    }

    /// **A key while the composer records** is the host's: Escape, or any
    /// key, stops the recording and keeps its words, and the key is spent on
    /// that. Hosts act on it before the surface; this keeps a key that did
    /// arrive away from the editor mid-composition — and, the case it is
    /// written for, keeps Escape from arming the stop of a running reply.
    pub(crate) fn dictation_key(&mut self, key: &Key) -> Option<bool> {
        self.dictation.live.as_ref()?;
        let modifier = matches!(
            key,
            Key::Named(
                NamedKey::Control
                    | NamedKey::Shift
                    | NamedKey::Alt
                    | NamedKey::AltGraph
                    | NamedKey::Super
                    | NamedKey::Meta
                    | NamedKey::Hyper
            )
        );
        (!modifier).then_some(true)
    }

    /// **The pointer moved**: the hover tint of the microphone or ✕, and the
    /// tooltip naming what is under it. Answers whether anything changed.
    pub(crate) fn dictation_hover(&mut self, old: (f32, f32), x: f32, y: f32) -> bool {
        let now = Instant::now();
        let before = self.dictation_target_at(old.0, old.1);
        let after = self.dictation_target_at(x, y);
        let lang = self.lang;
        let tip = after.map(|(key, anchor)| match key {
            TIP_MIC => {
                TooltipContent::new(key, anchor, koru_i18n::t(lang, koru_i18n::Key::AiDictate))
                    .with_optional_shortcut(koru_keys::show_hint(
                        koru_keys::ShortcutId::Dictate,
                        koru_keys::Section::Tooltip,
                    ))
            }
            TIP_CANCEL => TooltipContent::new(
                key,
                anchor,
                koru_i18n::t(lang, koru_i18n::Key::AiDictationCancel),
            ),
            TIP_CHOOSE => TooltipContent::new(
                key,
                anchor,
                koru_i18n::t(lang, koru_i18n::Key::AiMicrophoneChoose),
            ),
            _ => TooltipContent::new(
                key,
                anchor,
                koru_i18n::t(lang, koru_i18n::Key::AiDictationStop),
            )
            .with_optional_shortcut(koru_keys::show_hint(
                koru_keys::Hinted::Literal(escape_hint()),
                koru_keys::Section::Tooltip,
            )),
        });
        let tooltip = self.dictation.tooltip.set_hover(tip, now);
        // The chevron shows and goes with the pointer (`mic_chooser_button`).
        let chevron = self
            .mic_chooser_zone()
            .is_some_and(|zone| zone.contains(old.0, old.1) != zone.contains(x, y));
        tooltip || chevron || before.map(|(key, _)| key) != after.map(|(key, _)| key)
    }

    /// The microphone, ✕ or ■ under `(x, y)`, as a tooltip key and anchor.
    fn dictation_target_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        if self.dictation.live.is_none() {
            // The chevron says nothing while its panel is open.
            if let Some(chevron) = self.mic_chooser_slot().filter(|slot| slot.contains(x, y)) {
                return (!self.microphone_panel_open()).then_some((TIP_CHOOSE, chevron));
            }
            return self
                .mic_button()
                .filter(|mic| mic.contains(x, y))
                .map(|mic| (TIP_MIC, mic));
        }
        let tree = self.dictation_tree(Instant::now())?;
        let frame = layout(&tree, self.dictation_row_rect(), FrameCx::root());
        let key = frame.hit_at(x, y)?;
        let anchor = frame.node(key)?.rect;
        if key == CANCEL {
            Some((TIP_CANCEL, anchor))
        } else if key == STOP {
            Some((TIP_STOP, anchor))
        } else {
            None
        }
    }

    /// The next waveform frame, or the tooltip's next step. `None` whenever
    /// nothing is recording and no tooltip is waiting.
    pub(crate) fn dictation_deadline(&self) -> Option<Instant> {
        let frame = self
            .dictation
            .live
            .as_ref()
            .filter(|live| live.running())
            .map(|live| live.next_frame);
        [frame, self.dictation.tooltip.maintenance_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// One waveform frame, when it is due. Answers whether to redraw.
    pub(crate) fn maintain_dictation(&mut self, now: Instant) -> bool {
        let mut changed = self.dictation.tooltip.maintain(now);
        if let Some(live) = self.dictation.live.as_mut()
            && live.running()
            && now >= live.next_frame
        {
            live.sample(now);
            live.next_frame = now + FRAME;
            changed = true;
        }
        changed
    }

    /// Whether the dictation tooltip is fading in or out.
    pub(crate) fn dictation_animating(&self, now: Instant) -> bool {
        self.dictation.tooltip.animating(now)
    }
}

/// How Escape is written on this platform — it has no registry row, being
/// the platform's own key (`koru_keys::Hinted::Literal`).
fn escape_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⎋"
    } else {
        "Esc"
    }
}

/// **The waveform**: a bar per sample, newest at the right, sliding left
/// between samples; where there is no sample yet, a dot.
struct Waveform {
    levels: Vec<u8>,
    phase: f32,
}

impl Leaf for Waveform {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, BAR_MIN_H)
    }

    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let p = cx.palette();
        let pitch = BAR_W + BAR_GAP;
        let reach = (at.h * BAR_REACH - BAR_MIN_H).max(0.0);
        let mid = at.y + at.h * 0.5;
        let newest = at.x + at.w - BAR_W - self.phase * pitch;
        for i in 0.. {
            let x = newest - i as f32 * pitch;
            if x < at.x {
                break;
            }
            let level = self
                .levels
                .len()
                .checked_sub(i + 1)
                .map_or(0, |k| self.levels[k]);
            let h = BAR_MIN_H + reach * f32::from(level) / f32::from(LEVEL_MAX);
            let colour = if level > 0 { p.text } else { p.text_dim };
            scene::fill(
                canvas,
                root,
                colour,
                &scene::rrect(x, mid - h * 0.5, BAR_W, h, BAR_W * 0.5),
            );
        }
    }
}

/// **✕ and ■**, the send button's size: ✕ the toolbar's icon button, ■ the
/// send button's accent disc with a filled stop square.
struct RoundButton {
    glyph: char,
    side: f32,
    primary: bool,
}

impl Leaf for RoundButton {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.side, self.side)
    }

    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let p = cx.palette();
        if !self.primary {
            icon_button::draw_with_in(
                canvas,
                root,
                engine,
                at,
                self.glyph,
                IconButtonState {
                    hovered: state.hovered,
                    active: false,
                    disabled: state.disabled,
                },
                Metrics::STANDARD,
                p,
            );
            return;
        }
        let (fill, ink) = if state.disabled {
            (p.row_selected, p.text_dim)
        } else {
            (p.accent, p.on_accent)
        };
        scene::fill(
            canvas,
            root,
            fill,
            &scene::rrect(at.x, at.y, at.w, at.h, at.h * 0.5),
        );
        let glyph = engine.icon_layout_filled(self.glyph, STOP_GLYPH);
        scene::draw_layout_first_line(
            canvas,
            root,
            &glyph,
            at.x + (at.w - glyph.width()) * 0.5,
            at.y + (at.h - glyph.height()) * 0.5,
            Some(ink),
        );
    }
}

#[cfg(test)]
mod tests;
