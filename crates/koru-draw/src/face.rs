// SPDX-License-Identifier: MPL-2.0

//! The face a Bot wears: a rounded square, two eyes, and the Bot's initial
//! for a mouth.
//!
//! **One component, drawn at every size.** The roster row (28 px), the tab
//! header (36 to 46 px), the picker's tiles (46 px), the onboarding greeting
//! (56 px) and the dialog's preview (96 px) all draw this, from one number.
//! Everything is vector geometry in a 70-unit box scaled by `size / 70`, and
//! the initial is laid out at its final pixel size, so no size is a resample
//! of another.
//!
//! **Provenance.** The eight eye shapes and the 24 rows of [`ROWS`] are the
//! design of DiceBear's "Initial Face" avatar style
//! (<https://www.dicebear.com/styles/initial-face/>), which DiceBear
//! publishes under CC0 1.0 (public domain dedication; no attribution is
//! required, and it is recorded in `licensing/THIRD-PARTY.md` anyway). Only
//! the shapes and the numbers are taken: no DiceBear code is linked, no SVG
//! file is shipped, and nothing is fetched. The colors are **not**
//! DiceBear's: each row names one of the nine accent tones of the app's own
//! palette ([`theme::Palette::accents`]), so a face follows the theme.
//! The motion ([`FacePose`]) is Koru's own; DiceBear's is CSS inside an SVG.
//!
//! **Plain values in, paint out.** A face is an index and a name. This crate
//! does not know what a Bot is: which face a Bot wears (its stored choice, or
//! the one derived from its id) is decided by `koru_ai_core::bots::bot_avatar`,
//! and whether it moves is the host's to say.
//!
//! **Still unless told otherwise.** [`FacePose::STILL`] is the default. A
//! face moves only while its host says its Bot is working, and then every
//! face in the window takes its pose from one [`FaceClock`], which owns one
//! repaint deadline for all of them. The motion is paint-only: it changes
//! two transforms, never a size, so nothing is measured or laid out again.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, RoundedRect, Shape};

use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::{scene, theme};

/// How many faces there are. A stored choice is an index below this, and
/// `koru_ai_core::bots::BOT_AVATAR_COUNT` (which bounds the stored column)
/// must equal it: `apps/koru` asserts that at compile time.
///
/// **The table is append-only.** A face's index is stored per Bot, and the
/// face of a Bot with no stored choice is derived from its id modulo this
/// count, so reordering rows or changing the count would change faces people
/// already know.
pub const FACE_COUNT: u8 = 24;

/// The side of the box the design is drawn in.
const BOX: f64 = 70.0;
/// The tile's corner radius in the box: 28 percent of its side.
const RADIUS: f64 = 19.6;
/// Where the head group starts in the box.
const HEAD_ORIGIN: (f64, f64) = (13.0, 16.0);
/// The point of the head group it tilts about, and the axis its right eye
/// is mirrored on (`matrix(-1 0 0 1 44 0)` is a mirror about x = 22).
const HEAD_PIVOT: (f64, f64) = (22.0, 22.0);
/// The initial's size in the box.
const MOUTH_SIZE: f64 = 14.0;
/// Where the design anchors the initial in the head group, before the row's
/// own `mouth_y`: `translate(12, 30 + mouth_y)` then `x=10 y=7`.
const MOUTH_ANCHOR: (f64, f64) = (22.0, 37.0);
/// **How far above its anchor the initial's ink sits.** The design centres
/// the text on the anchor with `dominant-baseline: middle`, which puts the
/// *baseline* half an x-height below it; a capital's ink is then centred
/// about a tenth of an em higher than the anchor. Here the glyph is centred
/// on its ink ([`scene::centre_in`]), which is the same for every font and
/// script, so that tenth of an em is applied as a constant.
const MOUTH_INK_LIFT: f64 = 1.4;

/// The eight eye shapes, as SVG path data in the head group's units. Each is
/// the *left* eye; the right one is its mirror.
const EYES: [&str; 8] = [
    "M.25 8.12C1.66 11.86 12 16 12 16s5.17-9.58 3.76-13.32c0 0-1.41-3.74-5.3-2.38-3.87 1.36-2.7 4.48-2.7 4.48S6.6 1.66 2.73 3.02.25 8.12.25 8.12",
    "M9.5 10c-3.88 0-7.11-4.23-6.4-4.85s2.63 1.3 6.4 1.3 5.69-2 6.4-1.3S13.38 10 9.5 10",
    "M11.86 7.5c0-1.42-4.14-2.85-4.82-4.98C6.34.4 16 5.37 16 7.5s-9.65 7.11-8.96 4.98 4.82-3.56 4.82-4.98",
    "M8 8.36S8 4 12 4s4 4.36 4 4.36v2.91s0 .73-.67.73c-.66 0-.66-2.9-3.33-2.9S9.33 12 8.67 12 8 11.27 8 11.27z",
    "M16 8c0 2.2-1.34 4-3 4s-3-1.8-3-4 1.34-4 3-4 3 1.8 3 4",
    "M16 8c0 3.31-1.34 6-3 6s-3-2.69-3-6 1.34-6 3-6 3 2.69 3 6",
    "M11.5 6C8.29 6 7 7.36 7 8.04c0 3.4 1.29 1.35 4.5 1.35S16 11.43 16 8.04C16 7.36 14.71 6 11.5 6",
    "M16 8c0 1.66-1.12 3-2.5 3S11 9.66 11 8s1.12-3 2.5-3S16 6.34 16 8",
];

// The design's nine background and ink pairs, each named by the accent tone
// (an index into `theme::ACCENT_TOKENS`) that stands for it. Seven have a
// tone of their own hue. Sky and lime have none among the nine, and red and
// gray have no pair, so sky wears gray and lime wears pink: every tone is
// used, and no two pairs share one.
const ROSE: u8 = 0; // red
const ORANGE: u8 = 1;
const YELLOW: u8 = 2;
const GREEN: u8 = 3;
const TEAL: u8 = 4;
const BLUE: u8 = 5;
const PURPLE: u8 = 6;
const LIME: u8 = 7; // pink
const SKY: u8 = 8; // gray

/// One face: its tone, its eyes, and where they sit.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FaceRow {
    /// An index into [`theme::Palette::accents`]: `fill` is the tile, `ink`
    /// the eyes and the initial.
    pub accent: u8,
    /// An index into the eight eye shapes.
    pub eyes: u8,
    pub eye_x: f32,
    pub eye_y: f32,
    pub mouth_y: f32,
    pub head_x: f32,
    /// The head's tilt in degrees, clockwise.
    pub tilt: f32,
}

const fn row(
    accent: u8,
    eyes: u8,
    eye_x: f32,
    eye_y: f32,
    mouth_y: f32,
    head_x: f32,
    tilt: f32,
) -> FaceRow {
    FaceRow {
        accent,
        eyes,
        eye_x,
        eye_y,
        mouth_y,
        head_x,
        tilt,
    }
}

/// The 24 faces, in the order the picker offers them (two sets of twelve).
/// Numbers exactly as DiceBear's generator gives them; see the module note.
pub const ROWS: [FaceRow; FACE_COUNT as usize] = [
    row(SKY, 4, 1.49, 2.67, -3.12, -0.11, -14.64),
    row(YELLOW, 6, 0.01, 2.33, -2.98, 0.97, -7.96),
    row(TEAL, 3, -1.35, 0.32, -3.66, -0.87, 5.37),
    row(PURPLE, 1, -2.98, 4.45, -3.13, -1.22, -1.52),
    row(BLUE, 5, -1.81, 4.08, -1.38, -1.23, -13.07),
    row(ROSE, 7, -2.87, 2.21, -3.64, -0.14, -0.38),
    row(ORANGE, 2, -2.89, 4.39, -3.61, 0.63, -1.37),
    row(GREEN, 3, -1.22, 4.61, -1.35, 0.12, 13.98),
    row(LIME, 1, -1.02, 1.82, -0.7, -0.35, -7.53),
    row(SKY, 0, -0.68, 3.68, -1.55, -0.84, 11.73),
    row(BLUE, 4, 1.5, 4.58, -4.09, 0.8, -6.41),
    row(TEAL, 5, -1.18, 1.41, -1.34, 1.29, -0.45),
    row(ORANGE, 6, -0.44, 0.33, -1.4, -0.37, 7.32),
    row(YELLOW, 0, -1.91, 4.25, -3.58, 1.05, 12.48),
    row(PURPLE, 2, -0.68, 4.58, -1.75, -0.67, -8.73),
    row(GREEN, 7, 1.53, 0.38, -2.25, 1.15, 5.01),
    row(ROSE, 1, -1.24, 3.36, -2.98, -1.08, -13.79),
    row(LIME, 6, 1.27, 2.77, -0.64, 1.1, 10.32),
    row(SKY, 2, 1.03, 0.45, -0.48, 0.88, 7.12),
    row(BLUE, 0, -2.29, 2.97, -0.9, 0.42, 9.21),
    row(PURPLE, 5, 1.09, 0.43, -0.46, -0.45, 0.9),
    row(TEAL, 4, 1.1, 1.34, -0.91, -1.11, 5.23),
    row(YELLOW, 3, 0.71, 2.59, -0.23, 0.99, -11.98),
    row(GREEN, 6, 0.09, 3.6, -2.45, 0.48, -0.9),
];

/// The row of face `face`. An index past the table wraps, so a value from a
/// newer build's longer table still draws a face rather than nothing.
pub fn face_row(face: u8) -> &'static FaceRow {
    &ROWS[usize::from(face % FACE_COUNT)]
}

/// One parsed eye: its outline, and the height of its middle, which a blink
/// closes towards.
struct Eye {
    path: BezPath,
    mid_y: f64,
}

fn eye(index: u8) -> &'static Eye {
    static PARSED: [OnceLock<Eye>; EYES.len()] = [const { OnceLock::new() }; EYES.len()];
    let index = usize::from(index) % EYES.len();
    PARSED[index].get_or_init(|| {
        let path =
            BezPath::from_svg(EYES[index]).expect("the eight eye paths are checked by the tests");
        let bounds = path.bounding_box();
        Eye {
            mid_y: (bounds.y0 + bounds.y1) * 0.5,
            path,
        }
    })
}

// ------------------------------------------------------------------ motion

/// How often a moving face is repainted. Every face reads its pose from the
/// same whole step of this, so one deadline serves all of them and a repaint
/// between two steps (the pointer moved, a row lit) draws the same face.
pub const FACE_FRAME_MS: u64 = 50;
/// One full sway of the head, left to right and back.
const SWAY_MS: u64 = 3200;
/// How far the head sways to each side of its resting tilt, in degrees.
const SWAY_DEG: f32 = 4.0;
/// How long the sway takes to reach its full reach after a face starts
/// moving, so the first frame does not jump.
const SWAY_IN_MS: u64 = 400;
/// How often a face blinks, and how long the blink lasts.
const BLINK_EVERY_MS: u64 = 4400;
const BLINK_MS: u64 = 200;
/// How open an eye is at the middle of a blink: not a hairline, which would
/// shimmer at 28 px.
const BLINK_SHUT: f32 = 0.12;
/// How far apart, in time, two neighbouring faces of the table blink.
const BLINK_STAGGER_MS: u64 = 370;

/// **How a face is held right now**: what the animation changes, and nothing
/// else. Both fields move paint only.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FacePose {
    /// Degrees added to the row's own tilt, clockwise.
    pub tilt: f32,
    /// How open the eyes are: `1.0` open, towards `0.0` shut.
    pub eyes_open: f32,
}

impl FacePose {
    /// The face at rest: exactly the row as designed.
    pub const STILL: Self = Self {
        tilt: 0.0,
        eyes_open: 1.0,
    };

    /// **The pose of face `face`, `elapsed_ms` after its window's faces
    /// started moving.** A pure function: the same two numbers always give
    /// the same pose, and every instant inside one [`FACE_FRAME_MS`] step
    /// gives the pose of that step.
    ///
    /// A gentle sway of the head and an occasional blink. Each face is a
    /// little out of step with its neighbours in the table, so a roster of
    /// working Bots does not nod in unison.
    pub fn at(elapsed_ms: u64, face: u8) -> Self {
        let step = elapsed_ms / FACE_FRAME_MS * FACE_FRAME_MS;
        let nth = u64::from(face % FACE_COUNT);
        let sway_at = (step + nth * SWAY_MS / u64::from(FACE_COUNT)) % SWAY_MS;
        let reach = crate::anim::ease_out_cubic((step.min(SWAY_IN_MS) as f32) / SWAY_IN_MS as f32);
        let tilt =
            SWAY_DEG * reach * (std::f32::consts::TAU * sway_at as f32 / SWAY_MS as f32).sin();
        let blink_at = (step + nth * BLINK_STAGGER_MS) % BLINK_EVERY_MS;
        let eyes_open = if blink_at < BLINK_MS {
            // Shut at the middle of the blink, open at both ends.
            let t = blink_at as f32 / BLINK_MS as f32;
            (2.0 * t - 1.0).abs().max(BLINK_SHUT)
        } else {
            1.0
        };
        Self { tilt, eyes_open }
    }
}

impl Default for FacePose {
    fn default() -> Self {
        Self::STILL
    }
}

/// **The one clock every face in a window moves by.**
///
/// A host keeps one of these beside the views that draw faces (the roster,
/// the open Bot tab). Each frame it says whether *any* Bot it shows is
/// working ([`set_working`](Self::set_working)), asks for each face's pose
/// ([`pose`](Self::pose)), and reports [`deadline`](Self::deadline) as its
/// maintenance deadline. That is one wake-up per [`FACE_FRAME_MS`] for the
/// whole window however many faces move, and none at all while no Bot is
/// working, the clock is told nothing is shown, or the person asked for
/// reduced motion.
///
/// It holds an `Instant` and a flag; it owns no thread and no timer.
#[derive(Clone, Debug, Default)]
pub struct FaceClock {
    started: Option<Instant>,
    reduced_motion: bool,
}

impl FaceClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Say whether any face this clock serves belongs to a working Bot *and
    /// is on screen*. A hidden roster says `false`. Returns whether that
    /// changed, which is when a repaint is owed.
    ///
    /// The clock restarts from zero each time work begins, so a face always
    /// eases into its sway rather than resuming mid-swing.
    pub fn set_working(&mut self, any: bool, now: Instant) -> bool {
        match (any, self.started) {
            (true, None) => {
                self.started = Some(now);
                true
            }
            (false, Some(_)) => {
                self.started = None;
                true
            }
            _ => false,
        }
    }

    /// The host's reduced-motion setting. Under it every face is still and
    /// no deadline is asked for. Returns whether it changed.
    pub fn set_reduced_motion(&mut self, reduced: bool) -> bool {
        let changed = self.reduced_motion != reduced;
        self.reduced_motion = reduced;
        changed
    }

    /// Whether any face is moving: the host's `animating()` for its faces.
    pub fn animating(&self) -> bool {
        self.started.is_some() && !self.reduced_motion
    }

    /// The pose to draw face `face` in at `now`. `working` is whether *this*
    /// face's Bot is working: an idle Bot beside a working one stays still.
    pub fn pose(&self, face: u8, working: bool, now: Instant) -> FacePose {
        match self.started {
            Some(started) if working && !self.reduced_motion => {
                FacePose::at(elapsed_ms(started, now), face)
            }
            _ => FacePose::STILL,
        }
    }

    /// **When the faces next change**: the start of the next
    /// [`FACE_FRAME_MS`] step, the same instant for every face. `None` while
    /// nothing moves, so the window loop sleeps.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let started = self.started.filter(|_| !self.reduced_motion)?;
        let next = (elapsed_ms(started, now) / FACE_FRAME_MS + 1) * FACE_FRAME_MS;
        started.checked_add(Duration::from_millis(next))
    }
}

fn elapsed_ms(started: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------- geometry

/// **Where everything of one face goes**, in pixels from the tile's top
/// left. What [`Face::draw_in`] paints, and what the tests measure.
#[derive(Clone, Debug)]
pub struct FaceGeometry {
    /// The rounded square.
    pub tile: RoundedRect,
    /// The eye's outline, in its own units. Paint it under each transform.
    pub eye: &'static BezPath,
    pub left_eye: Affine,
    pub right_eye: Affine,
    /// The head's transform for text laid out in pixels: its offset and its
    /// tilt, without the unit scale.
    pub head: Affine,
    /// Where the initial's ink is centred, in the head's pixel space.
    pub mouth: Point,
    /// The initial's font size in pixels.
    pub mouth_size: f32,
}

/// The geometry of face `face` at `size` pixels in `pose`.
pub fn geometry(face: u8, size: f32, pose: FacePose) -> FaceGeometry {
    let row = face_row(face);
    let eye = eye(row.eyes);
    let s = f64::from(size) / BOX;
    let tilt = f64::from(row.tilt + pose.tilt).to_radians();
    // `translate(13 16) translate(head_x 0) rotate(tilt 22 22)`, with the
    // unit scale folded into the offsets so text can share it.
    let head = Affine::translate((
        (HEAD_ORIGIN.0 + f64::from(row.head_x)) * s,
        HEAD_ORIGIN.1 * s,
    )) * Affine::rotate_about(tilt, Point::new(HEAD_PIVOT.0 * s, HEAD_PIVOT.1 * s));
    // A blink closes the eye towards its own middle.
    let open = f64::from(pose.eyes_open.clamp(0.0, 1.0));
    let placed = Affine::translate((f64::from(row.eye_x), f64::from(row.eye_y)))
        * Affine::translate((0.0, eye.mid_y))
        * Affine::scale_non_uniform(1.0, open)
        * Affine::translate((0.0, -eye.mid_y));
    let units = head * Affine::scale(s);
    let mirror = Affine::new([-1.0, 0.0, 0.0, 1.0, HEAD_PIVOT.0 * 2.0, 0.0]);
    FaceGeometry {
        tile: RoundedRect::new(0.0, 0.0, BOX * s, BOX * s, RADIUS * s),
        eye: &eye.path,
        left_eye: units * placed,
        right_eye: units * mirror * placed,
        head,
        mouth: Point::new(
            MOUTH_ANCHOR.0 * s,
            (MOUTH_ANCHOR.1 + f64::from(row.mouth_y) - MOUTH_INK_LIFT) * s,
        ),
        mouth_size: (MOUTH_SIZE * s) as f32,
    }
}

/// **The initial a face wears for a mouth**: the first character of the
/// trimmed name, upper-cased where its script has case. Empty for an empty
/// name, which draws no mouth.
///
/// A character that needs what follows it to be itself keeps it: combining
/// accents, an emoji's variation selector or skin tone, and the second half
/// of a flag. A letter whose upper case is more than one character (`ß`)
/// stays as written rather than becoming two letters.
pub fn initial(name: &str) -> String {
    let mut chars = name.trim().chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut upper = first.to_uppercase();
    let mut out = match (upper.next(), upper.next()) {
        (Some(one), None) => one.to_string(),
        _ => first.to_string(),
    };
    let flag = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
    let mut rest = chars.peekable();
    if flag(first)
        && let Some(second) = rest.next_if(|c| flag(*c))
    {
        out.push(second);
    }
    while let Some(mark) = rest.next_if(|c| {
        matches!(*c,
            '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FE20}'..='\u{FE2F}'
            | '\u{1F3FB}'..='\u{1F3FF}')
    }) {
        out.push(mark);
    }
    out
}

/// What to draw. Borrowed rather than owned: every caller already has these.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Face<'a> {
    /// Which face: an index into [`ROWS`] (it wraps past the end).
    pub face: u8,
    /// Where the initial comes from. An empty name draws no mouth.
    pub name: &'a str,
    /// How it is held: [`FacePose::STILL`], or a [`FaceClock`]'s pose while
    /// the Bot is working.
    pub pose: FacePose,
}

impl<'a> Face<'a> {
    /// Face `face` for `name`, at rest.
    pub fn new(face: u8, name: &'a str) -> Self {
        Self {
            face,
            name,
            pose: FacePose::STILL,
        }
    }

    /// The same face in `pose`.
    pub fn posed(mut self, pose: FacePose) -> Self {
        self.pose = pose;
        self
    }

    /// Draw it filling `at`, in the current theme. Square is assumed; pass a
    /// square rect.
    pub fn draw(&self, scene: &mut Scene, root: Affine, engine: &mut TextEngine, at: Rect) {
        self.draw_in(scene, root, engine, at, theme::p());
    }

    /// [`draw`](Self::draw) in `p`, the table of the theme it is shown
    /// under: an element leaf passes its frame's `cx.palette()`.
    #[track_caller]
    pub fn draw_in(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        p: &theme::Palette,
    ) {
        let size = at.w.min(at.h);
        if size <= 0.0 {
            return;
        }
        let row = face_row(self.face);
        let tone = &p.accents[usize::from(row.accent) % p.accents.len()];
        let g = geometry(self.face, size, self.pose);
        let here = root * Affine::translate((f64::from(at.x), f64::from(at.y)));
        let _inspection = crate::inspect::scope(
            "face",
            "part",
            root,
            scene::rect(at.x, at.y, size, size),
            |r| {
                r.property("face", self.face % FACE_COUNT, "koru_draw::face::ROWS");
                r.property(
                    "tone",
                    theme::ACCENT_TOKENS[usize::from(row.accent) % theme::ACCENT_TOKENS.len()],
                    "Palette::accents",
                );
                r.property("moving", self.pose != FacePose::STILL, "FacePose");
            },
        );
        scene::fill(scene, here, tone.fill, &g.tile);
        scene::fill(scene, here * g.left_eye, tone.ink, g.eye);
        scene::fill(scene, here * g.right_eye, tone.ink, g.eye);

        let letter = initial(self.name);
        if letter.is_empty() {
            return;
        }
        // Sized by the face's own geometry, not by a text tier, as a tile's
        // glyph is: laid out at its final pixel size so it is not a scaled
        // bitmap of another size.
        let layout = engine.simple_layout(
            &letter,
            theme::TextStyle::new(
                theme::TextMetrics::new(g.mouth_size, g.mouth_size),
                theme::FontWeight::SemiBold,
            ),
        );
        let (x, y) = scene::centre_in(
            &layout,
            Rect::new(g.mouth.x as f32, g.mouth.y as f32, 0.0, 0.0),
        );
        // The ink color for every glyph, an emoji included: the mouth is
        // part of the face, drawn in its ink.
        scene::draw_layout(scene, here * g.head, &layout, x, y, Some(tone.ink));
    }
}

#[cfg(test)]
mod tests;
