// SPDX-License-Identifier: MPL-2.0

//! **KoruMediaViewer**: a drawing or a picture in its own view — the window
//! dimmed behind it, the stage at a zoom the person chooses, scrolled or
//! dragged where it overflows, with a toolbar of the actions its host offers
//! (chat-attachments D11; ADR-482).
//!
//! One viewer for two things. A Mermaid diagram from a document
//! ([`Stage::Diagram`]) and a picture from a chat or the Files view
//! ([`Stage::Bitmap`]) are zoomed, panned, fitted and closed by the same
//! geometry, keys and toolbar, so a person learns one viewer. The host
//! chooses which buttons the toolbar shows ([`ActionSet`]): a diagram can be
//! copied and saved, a chat's picture opened in an external app.
//!
//! Opened by its host; closed by `Esc`, the toolbar's `×`, or a click on the
//! dimmed window. While it is up it is **modal** in the routing sense: every
//! key and every press is its own, and nothing underneath answers — the same
//! claim a confirm makes, through the same shared scrim ([`crate::modal`]).
//!
//! **A diagram is a drawing, not a block.** It takes the `PreparedDiagram`
//! the page paints — the very same shaped labels — so the viewer cannot show
//! a different picture from the one the person clicked, and a later
//! keystroke in the text cannot change the picture under them. The drawing
//! is painted through the same painter at the chosen zoom, so zooming in is
//! a vector redraw: lines stay one pixel wide and the text stays sharp at
//! 800%, where a stretched bitmap would blur. The painter divides stroke
//! widths by the scale it is given; the viewer hands it the zoom, so a
//! zoomed-in drawing has proportionally heavier lines, as a magnified
//! picture should.
//!
//! **A picture is drawn from mips built off the window thread.** vello has
//! no mipmaps and samples an image with a single tap, so a 4K screenshot
//! drawn into a fitted 900 px box would be point-sampled from a texture far
//! larger than the box. [`ViewerImage::decode`] (run on a worker, never the
//! window thread) caps the picture to the GPU's texture limit and builds a
//! chain of Lanczos3 halvings; below 100% the viewer draws the smallest
//! level still at least as large as the picture on screen, and at 100% and
//! above the base level, at `ImageQuality::High`.
//!
//! **The stage is the window above the toolbar.** The picture is centred,
//! fitted and clamped against the window minus the band the toolbar sits in
//! ([`STAGE_BOTTOM`]), so no part of it is ever only reachable under the
//! toolbar: scrolled to its bottom edge, its last row sits clear above the
//! buttons.
//!
//! **Zoom and pan are plain geometry**, kept as two numbers and clamped
//! after every change by [`MediaViewer::settle`]: a picture smaller than the
//! stage sits centred and cannot be moved; one larger can be moved until its
//! edge meets the stage's, never past it, so no gesture can push the picture
//! out of sight. The plain wheel scrolls it, as a page scrolls — a picture
//! taller than the window is read by wheeling down it, not by finding the
//! drag — `Shift`+wheel scrolls it sideways, the arrow keys step it, and a
//! drag moves it under the hand. `Ctrl`+wheel and a pinch zoom around the
//! pointer; the toolbar and the keys zoom around the stage's centre; either
//! way the point under the anchor stays where it was.
//!
//! **The viewer only asks.** Copying, saving and opening in another app need
//! the clipboard, a dialog, a raster or a file, which are the host's, so
//! [`Outcome::Copy`], [`Outcome::SaveAs`] and [`Outcome::OpenExternally`] are
//! what the viewer leaves behind for its host to act on. The copy button
//! confirms at once, the way every copy icon in Koru does
//! ([`CopyAffordance`]): the glyph turns to the check and holds, while the
//! host writes the clipboard on its next pass.
//!
//! **Its words and its semantics are its own.** Every label is a
//! `koru-i18n` key, and [`MediaViewer::semantics`] answers, as plain data,
//! what a screen reader should hear: the picture's name and size, the zoom,
//! and each button with its label, rect and action. The host projects that
//! into its accessibility tree.

use std::sync::Arc;
use std::time::Instant;

use koru_draw::diagram::PreparedDiagram;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use koru_i18n::{Key, Lang, t};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as KeyPress, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::CopyAffordance;
use crate::button_group::ButtonGroup;
use crate::icon_button::{IconButton, State as ButtonState};

#[cfg(test)]
mod tests;

/// One zoom step, in or out: a quarter at a time, so five steps in are
/// about three times the size and the percentages read as round numbers.
pub const ZOOM_STEP: f64 = 1.25;
/// The smallest zoom: a tenth of natural size.
pub const ZOOM_MIN: f64 = 0.1;
/// The largest: eight times natural size — enough to read the smallest
/// label of a large flowchart, and the point past which a page of vector
/// lines stops being a picture.
pub const ZOOM_MAX: f64 = 8.0;
/// Air between the stage's edge and a fitted picture, on every side.
pub const FIT_MARGIN: f32 = 48.0;
/// The card a drawing sits on: the page colour, padded this much past the
/// drawing on every side, so a drawing with no frame of its own still reads
/// as a picture on the dimmed window rather than lines floating on it. A
/// bitmap has its own edges and sits on the card unpadded.
pub const CARD_PAD: f32 = 24.0;
/// The toolbar's height, and the height of each of its button slots.
pub const TOOLBAR_H: f32 = 36.0;
/// Each toolbar button's width.
pub const BUTTON_W: f32 = 32.0;
/// The toolbar's distance from the window's bottom edge.
pub const TOOLBAR_BOTTOM: f32 = 24.0;
/// The band at the window's bottom the toolbar lives in — the bar with as
/// much air above it as below — which the stage stops short of, so the
/// picture is never centred, fitted or scrolled under the buttons.
pub const STAGE_BOTTOM: f32 = TOOLBAR_BOTTOM * 2.0 + TOOLBAR_H;
/// How far one wheel notch scrolls the picture: about two lines of a
/// diagram's text, the way a page's wheel step is a few lines of prose.
pub const PAN_LINE: f32 = 48.0;
/// How far an arrow key moves the picture.
pub const PAN_KEY: f32 = 40.0;
/// The longest edge a picture is kept at for the GPU: vello uploads an image
/// as one texture, and 8192 is `max_texture_dimension_2d` on most adapters
/// (wgpu's default limit). A larger picture is downscaled when it is decoded;
/// the file itself is untouched, and *Open in external app* shows it whole.
pub const MAX_TEXTURE_EDGE: u32 = 8192;
/// The smallest mip kept: below this a picture is a thumbnail, and the
/// viewer never fits one that small.
const MIN_MIP_EDGE: u32 = 64;
/// Air between the toolbar's button groups, where a divider would be.
const GROUP_GAP: f32 = 10.0;
/// The toolbar's side padding.
const TOOLBAR_PAD: f32 = 6.0;
/// The percent label's slot between the zoom-out and zoom-in buttons.
const PERCENT_W: f32 = 48.0;
/// The toolbar's corner radius.
const TOOLBAR_RADIUS: f32 = 8.0;
/// How far a press has to travel before it is a drag rather than a click
/// that happens to have moved a pixel.
const DRAG_SLOP: f32 = 3.0;

// ----------------------------------------------------------------- stage

/// What the viewer shows.
#[derive(Clone)]
pub enum Stage {
    /// A drawn diagram, repainted as vectors at every zoom.
    Diagram(Arc<PreparedDiagram>),
    /// A picture, with its mips built off the window thread.
    Bitmap { image: Arc<ViewerImage> },
}

impl Stage {
    /// The size 100% shows, in logical pixels: a diagram's laid-out bounds,
    /// a picture's pixels as the file holds them.
    fn natural_size(&self) -> (f64, f64) {
        match self {
            Self::Diagram(prepared) => {
                let size = prepared.natural_size();
                (size.width, size.height)
            }
            Self::Bitmap { image } => {
                let (w, h) = image.natural_size();
                (f64::from(w), f64::from(h))
            }
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Diagram(_) => "diagram",
            Self::Bitmap { .. } => "bitmap",
        }
    }
}

/// A decoded picture ready to draw: its natural size, and a chain of levels
/// from the base (the picture, or the picture capped at
/// [`MAX_TEXTURE_EDGE`]) down by halves. Built once, off the window thread;
/// drawing only chooses a level.
pub struct ViewerImage {
    natural: (u32, u32),
    levels: Vec<ImageBrush>,
}

impl std::fmt::Debug for ViewerImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerImage")
            .field("natural", &self.natural)
            .field(
                "levels",
                &self
                    .levels
                    .iter()
                    .map(|level| (level.image.width, level.image.height))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ViewerImage {
    /// Decode `bytes` within `limits`, cap it to [`MAX_TEXTURE_EDGE`] and
    /// build its mips. **Slow: call it on a worker.** `None` when the bytes
    /// are not a picture this build decodes, or are larger than the limits
    /// allow — the format is sniffed, never trusted.
    pub fn decode(bytes: &[u8], limits: image::Limits) -> Option<Self> {
        let started = Instant::now();
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        reader.limits(limits);
        let decoded = reader.decode().ok()?;
        let natural = (decoded.width(), decoded.height());
        let image = Self::from_rgba(decoded.into_rgba8(), natural)?;
        tracing::debug!(
            target: "koru_ui::viewer",
            stage = "bitmap",
            decode_ms = started.elapsed().as_secs_f64() * 1_000.0,
            mip_levels = image.levels.len(),
            base_width = image.levels[0].image.width,
            base_height = image.levels[0].image.height,
            "viewer picture decoded"
        );
        Some(image)
    }

    /// Build the levels from decoded pixels whose file holds `natural`
    /// pixels. **Slow: call it on a worker.** `None` for an empty picture.
    pub fn from_rgba(rgba: image::RgbaImage, natural: (u32, u32)) -> Option<Self> {
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 || natural.0 == 0 || natural.1 == 0 {
            return None;
        }
        let mut base = rgba;
        if w.max(h) > MAX_TEXTURE_EDGE {
            let (tw, th) = fit_edge(w, h, MAX_TEXTURE_EDGE);
            base = image::imageops::resize(&base, tw, th, image::imageops::FilterType::Lanczos3);
        }
        let mut levels = Vec::new();
        let mut current = base;
        loop {
            let (cw, ch) = current.dimensions();
            let next = (cw.max(ch) / 2 >= MIN_MIP_EDGE).then(|| {
                image::imageops::resize(
                    &current,
                    (cw / 2).max(1),
                    (ch / 2).max(1),
                    image::imageops::FilterType::Lanczos3,
                )
            });
            levels.push(brush(current));
            match next {
                Some(next) => current = next,
                None => break,
            }
        }
        Some(Self { natural, levels })
    }

    /// The picture's size as the file holds it: what 100% means, and what a
    /// screen reader hears.
    pub fn natural_size(&self) -> (u32, u32) {
        self.natural
    }

    /// How many levels were built, the base first.
    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// One level's pixels.
    pub fn level(&self, index: usize) -> &ImageBrush {
        &self.levels[index.min(self.levels.len() - 1)]
    }

    /// The level to draw for a picture `device_width` device pixels wide on
    /// screen: the smallest still at least that wide, so a level is only
    /// ever shrunk on the GPU, never stretched. At or above the base's width
    /// it is the base.
    pub fn level_for(&self, device_width: f64) -> usize {
        let mut chosen = 0;
        for (index, level) in self.levels.iter().enumerate() {
            if f64::from(level.image.width) + 0.5 >= device_width {
                chosen = index;
            } else {
                break;
            }
        }
        chosen
    }
}

/// `(w, h)` scaled so the longer edge is `edge`, keeping the shape.
fn fit_edge(w: u32, h: u32, edge: u32) -> (u32, u32) {
    if w >= h {
        (
            edge,
            ((u64::from(h) * u64::from(edge)) / u64::from(w)).max(1) as u32,
        )
    } else {
        (
            ((u64::from(w) * u64::from(edge)) / u64::from(h)).max(1) as u32,
            edge,
        )
    }
}

fn brush(rgba: image::RgbaImage) -> ImageBrush {
    let (width, height) = rgba.dimensions();
    ImageBrush::new(ImageData {
        data: Blob::new(Arc::new(rgba.into_raw())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    })
    .with_quality(ImageQuality::High)
}

// --------------------------------------------------------------- actions

/// One of the toolbar's buttons, or a gesture with no button.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Action {
    ZoomOut,
    ZoomIn,
    /// The whole picture in the stage, as large as fits.
    Fit,
    /// Natural size, centred. No button: the `0` key, a double click, and
    /// the zoom readout's action for a screen reader.
    ActualSize,
    /// The picture to the clipboard, through the host.
    Copy,
    /// *Save as…* through the host's dialog.
    SaveAs,
    /// *Open in external app*, through the host.
    OpenExternally,
    Close,
}

impl Action {
    /// The toolbar's buttons in their order, a gap between groups: zoom,
    /// fit, copy · save · open, close. Actual size has no button.
    const TOOLBAR: [&'static [Action]; 4] = [
        &[Action::ZoomOut, Action::ZoomIn],
        &[Action::Fit],
        &[Action::Copy, Action::SaveAs, Action::OpenExternally],
        &[Action::Close],
    ];

    fn bit(self) -> u8 {
        match self {
            Self::ZoomOut => 1 << 0,
            Self::ZoomIn => 1 << 1,
            Self::Fit => 1 << 2,
            Self::ActualSize => 1 << 3,
            Self::Copy => 1 << 4,
            Self::SaveAs => 1 << 5,
            Self::OpenExternally => 1 << 6,
            Self::Close => 1 << 7,
        }
    }

    /// The words a person hears, and reads in a tooltip.
    pub fn label(self, lang: Lang) -> &'static str {
        t(
            lang,
            match self {
                Self::ZoomOut => Key::ViewerZoomOut,
                Self::ZoomIn => Key::ViewerZoomIn,
                Self::Fit => Key::ViewerFit,
                Self::ActualSize => Key::ViewerActualSize,
                Self::Copy => Key::ViewerCopy,
                Self::SaveAs => Key::ViewerSaveAs,
                Self::OpenExternally => Key::ViewerOpenExternally,
                Self::Close => Key::ViewerClose,
            },
        )
    }

    fn glyph(self) -> char {
        match self {
            Self::ZoomOut => icons::MINUS,
            Self::ZoomIn => icons::PLUS,
            Self::Fit => icons::ARROWS_IN_SIMPLE,
            // Never drawn: actual size has no button.
            Self::ActualSize => icons::MAGNIFYING_GLASS_PLUS,
            Self::Copy => icons::COPY,
            Self::SaveAs => icons::DOWNLOAD,
            Self::OpenExternally => icons::POP_OUT,
            Self::Close => icons::WINDOW_CLOSE,
        }
    }
}

/// Which actions the host offers. The zoom, fit, actual-size and close
/// gestures are always the viewer's own; copy, save and opening in another
/// app appear on the toolbar, and answer their keys, only when the host can
/// carry them out.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ActionSet(u8);

impl ActionSet {
    /// A diagram from a document: copied as a picture, saved through the
    /// block's *Save as…*.
    pub const DIAGRAM: Self = Self(0)
        .with(Action::ZoomOut)
        .with(Action::ZoomIn)
        .with(Action::Fit)
        .with(Action::ActualSize)
        .with(Action::Copy)
        .with(Action::SaveAs)
        .with(Action::Close);
    /// A picture from a chat or the Files view: opened in the system's app.
    pub const PICTURE: Self = Self(0)
        .with(Action::ZoomOut)
        .with(Action::ZoomIn)
        .with(Action::Fit)
        .with(Action::ActualSize)
        .with(Action::OpenExternally)
        .with(Action::Close);

    pub const fn with(self, action: Action) -> Self {
        // `Action::bit` is not const; the same table, spelled for a const.
        let bit = match action {
            Action::ZoomOut => 1 << 0,
            Action::ZoomIn => 1 << 1,
            Action::Fit => 1 << 2,
            Action::ActualSize => 1 << 3,
            Action::Copy => 1 << 4,
            Action::SaveAs => 1 << 5,
            Action::OpenExternally => 1 << 6,
            Action::Close => 1 << 7,
        };
        Self(self.0 | bit)
    }

    pub fn without(self, action: Action) -> Self {
        Self(self.0 & !action.bit())
    }

    pub fn contains(self, action: Action) -> bool {
        self.0 & action.bit() != 0
    }
}

/// How the host opens the viewer.
#[derive(Clone, Debug)]
pub struct ViewerOptions {
    pub actions: ActionSet,
    /// What a screen reader names the dialog: the picture's file name, a
    /// diagram's own title. Empty names it by its kind (*Diagram*,
    /// *Picture*), in the reader's language.
    pub title: String,
}

/// The display-zoom chords, claimed for the picture while the viewer is up.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ZoomKey {
    In,
    Out,
    ActualSize,
}

/// What the host does after an input the viewer handled.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Handled; nothing to do beyond the redraw the handling asks for.
    Handled,
    /// Put the picture on the clipboard.
    Copy,
    /// Raise *Save as…*.
    SaveAs,
    /// Open the picture in the system's app.
    OpenExternally,
    /// Take the viewer down.
    Close,
}

/// A press on the picture that may become a drag.
#[derive(Copy, Clone, Debug)]
struct Drag {
    /// Where the press landed.
    from: (f32, f32),
    /// The pan when it landed; the drag adds the pointer's travel to it.
    pan: (f32, f32),
    /// Whether it has travelled past [`DRAG_SLOP`].
    moved: bool,
}

/// Which way a wheel notch moves the picture, from the modifier held with
/// it. `Shift`+wheel scrolling sideways is every browser's and editor's
/// convention for a mouse with one wheel; `Alt`+wheel is its mirror, so a
/// trackpad's sideways swipe can scroll down. `Ctrl`+wheel is not here: it
/// zooms, through `zoom_wheel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAxis {
    /// As the wheel turned: down scrolls down, a sideways wheel scrolls across.
    AsTurned,
    /// `Shift`: the wheel scrolls across whichever way it turned.
    Sideways,
    /// `Alt`: the wheel scrolls up and down whichever way it turned.
    UpAndDown,
}

impl WheelAxis {
    /// The axis the modifiers held ask for; `Shift` wins over `Alt`.
    pub fn from_mods(mods: ModifiersState) -> Self {
        if mods.shift_key() {
            Self::Sideways
        } else if mods.alt_key() {
            Self::UpAndDown
        } else {
            Self::AsTurned
        }
    }
}

// ------------------------------------------------------------- semantics

/// The viewer as a screen reader should hear it, in window coordinates.
/// Plain data: the host projects it into its own tree.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerSemantics {
    /// The dialog's name: the title, or the stage's kind.
    pub name: String,
    /// A picture's size, *1920 × 1080*; `None` for a diagram.
    pub description: Option<String>,
    /// The whole window the viewer covers.
    pub bounds: Rect,
    /// The zoom readout, its value spoken as *Zoom 125%*; its action is
    /// [`Action::ActualSize`].
    pub zoom: ViewerControl,
    /// The toolbar's buttons, in order.
    pub buttons: Vec<ViewerControl>,
}

/// One control a screen reader can press.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerControl {
    pub action: Action,
    pub label: String,
    /// The current value, for the zoom readout.
    pub value: Option<String>,
    pub rect: Rect,
}

// ---------------------------------------------------------------- viewer

/// The viewer's state: what it shows, at what zoom and pan, and what the
/// pointer is doing to it.
pub struct MediaViewer {
    stage: Stage,
    options: ViewerOptions,
    viewport: (f32, f32),
    /// Natural size is 1.
    zoom: f64,
    /// The picture's offset from centred in the stage, in window pixels.
    /// `(0, 0)` is centred; clamped by [`Self::settle`] so the picture never
    /// leaves a gap on a side it overflows.
    pan: (f32, f32),
    drag: Option<Drag>,
    /// A press on the dimmed window, waiting for its release to close.
    press_on_scrim: bool,
    hover: Option<Action>,
    pointer: (f32, f32),
    /// The copy button's confirmation: check, hold, back to copy.
    copy: CopyAffordance,
}

impl MediaViewer {
    /// Open on `stage` in a window `viewport` large. Opens at natural size
    /// when that fits the stage, and fitted when it does not — a small
    /// picture is not blown up on arrival, and a large one is seen whole
    /// before it is zoomed into.
    pub fn open(stage: Stage, options: ViewerOptions, viewport: (f32, f32)) -> Self {
        tracing::debug!(target: "koru_ui::viewer", stage = stage.kind(), "viewer opened");
        let mut viewer = Self {
            stage,
            options,
            viewport,
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag: None,
            press_on_scrim: false,
            hover: None,
            pointer: (viewport.0 * 0.5, viewport.1 * 0.5),
            copy: CopyAffordance::default(),
        };
        viewer.zoom = viewer.fit_zoom().clamp(ZOOM_MIN, 1.0);
        viewer.settle();
        viewer
    }

    /// What the viewer shows.
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The drawing, when the viewer shows one.
    pub fn diagram(&self) -> Option<&Arc<PreparedDiagram>> {
        match &self.stage {
            Stage::Diagram(prepared) => Some(prepared),
            Stage::Bitmap { .. } => None,
        }
    }

    pub fn options(&self) -> &ViewerOptions {
        &self.options
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn pan(&self) -> (f32, f32) {
        self.pan
    }

    /// The window the viewer lays itself out in.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// The window changed size: keep the zoom, re-clamp the pan.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.settle();
    }

    // ---------------------------------------------------------- geometry

    /// The part of the window the picture lives in: everything above the
    /// toolbar's band. Never shorter than a pixel, so a window too short
    /// for the toolbar still has somewhere to centre on.
    pub fn stage_rect(&self) -> Rect {
        Rect::new(
            0.0,
            0.0,
            self.viewport.0.max(1.0),
            (self.viewport.1 - STAGE_BOTTOM).max(1.0),
        )
    }

    /// The picture's size at the current zoom.
    fn scaled(&self) -> (f32, f32) {
        let (w, h) = self.stage.natural_size();
        ((w * self.zoom) as f32, (h * self.zoom) as f32)
    }

    /// The zoom at which the whole picture fits inside the stage with
    /// [`FIT_MARGIN`] on every side, within the zoom bounds. A picture with
    /// no size fits at 1.
    pub fn fit_zoom(&self) -> f64 {
        let (w, h) = self.stage.natural_size();
        if w <= 0.0 || h <= 0.0 {
            return 1.0;
        }
        let stage = self.stage_rect();
        let room_w = f64::from((stage.w - 2.0 * FIT_MARGIN).max(1.0));
        let room_h = f64::from((stage.h - 2.0 * FIT_MARGIN).max(1.0));
        (room_w / w).min(room_h / h).clamp(ZOOM_MIN, ZOOM_MAX)
    }

    /// Where the picture's top-left is, in window coordinates: centred in
    /// the stage, moved by the pan.
    pub fn origin(&self) -> (f32, f32) {
        let (w, h) = self.scaled();
        let stage = self.stage_rect();
        (
            stage.x + (stage.w - w) * 0.5 + self.pan.0,
            stage.y + (stage.h - h) * 0.5 + self.pan.1,
        )
    }

    /// The picture's rect in window coordinates.
    pub fn picture_rect(&self) -> Rect {
        let (x, y) = self.origin();
        let (w, h) = self.scaled();
        Rect::new(x, y, w, h)
    }

    /// How far the card reaches past the picture: a drawing's padding, none
    /// for a bitmap, which has its own edges.
    fn card_pad(&self) -> f32 {
        match self.stage {
            Stage::Diagram(_) => CARD_PAD,
            Stage::Bitmap { .. } => 0.0,
        }
    }

    /// The card under the picture: the picture's rect, padded.
    pub fn card_rect(&self) -> Rect {
        let picture = self.picture_rect();
        let pad = self.card_pad();
        Rect::new(
            picture.x - pad,
            picture.y - pad,
            picture.w + 2.0 * pad,
            picture.h + 2.0 * pad,
        )
    }

    /// How far the picture may be moved from centre on each axis: nothing
    /// where it fits the stage, half the overflow where it does not — so
    /// its edge can reach the stage's edge and no further.
    fn pan_limits(&self) -> (f32, f32) {
        let (w, h) = self.scaled();
        let stage = self.stage_rect();
        (
            ((w - stage.w) * 0.5).max(0.0),
            ((h - stage.h) * 0.5).max(0.0),
        )
    }

    /// Whether the picture overflows the stage on either axis — the only
    /// case a drag, a wheel or an arrow moves anything.
    pub fn draggable(&self) -> bool {
        let limits = self.pan_limits();
        limits.0 > 0.0 || limits.1 > 0.0
    }

    /// Clamp the pan to what the picture's overflow allows.
    fn settle(&mut self) {
        let limits = self.pan_limits();
        self.pan = (
            self.pan.0.clamp(-limits.0, limits.0),
            self.pan.1.clamp(-limits.1, limits.1),
        );
    }

    // -------------------------------------------------------------- zoom

    /// Zoom to `zoom`, keeping the picture's point under `(x, y)` where it
    /// is — the stage's centre for a keyboard or toolbar zoom, the pointer
    /// for a wheel.
    pub fn zoom_to(&mut self, zoom: f64, around: (f32, f32)) {
        let zoom = if zoom.is_finite() {
            zoom.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            1.0
        };
        if zoom == self.zoom {
            return;
        }
        // The picture point under the anchor, in natural units.
        let origin = self.origin();
        let dx = f64::from(around.0 - origin.0) / self.zoom;
        let dy = f64::from(around.1 - origin.1) / self.zoom;
        self.zoom = zoom;
        // Where the top-left has to be for that point to stay under the
        // anchor, and the pan that puts it there.
        let (w, h) = self.scaled();
        let stage = self.stage_rect();
        let want_x = f64::from(around.0) - dx * zoom;
        let want_y = f64::from(around.1) - dy * zoom;
        self.pan = (
            want_x as f32 - (stage.x + (stage.w - w) * 0.5),
            want_y as f32 - (stage.y + (stage.h - h) * 0.5),
        );
        self.settle();
    }

    /// The stage's centre: what the toolbar and the keys zoom around.
    fn centre(&self) -> (f32, f32) {
        let stage = self.stage_rect();
        (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5)
    }

    /// One step in, around the stage's centre.
    pub fn zoom_in(&mut self) {
        self.zoom_to(self.zoom * ZOOM_STEP, self.centre());
    }

    /// One step out, around the stage's centre.
    pub fn zoom_out(&mut self) {
        self.zoom_to(self.zoom / ZOOM_STEP, self.centre());
    }

    /// Natural size, centred.
    pub fn actual_size(&mut self) {
        self.zoom_to(1.0, self.centre());
        self.pan = (0.0, 0.0);
    }

    /// The whole picture in the stage, centred.
    pub fn fit(&mut self) {
        self.zoom_to(self.fit_zoom(), self.centre());
        self.pan = (0.0, 0.0);
    }

    /// The display-zoom chords, claimed for the picture while it is up.
    pub fn zoom_key(&mut self, key: ZoomKey) {
        match key {
            ZoomKey::In => self.zoom_in(),
            ZoomKey::Out => self.zoom_out(),
            ZoomKey::ActualSize => self.actual_size(),
        }
    }

    /// `Ctrl`+wheel or a pinch at `(x, y)`: in on a push away, out on a
    /// pull back, around the pointer. A line delta is one step a notch; a
    /// pixel delta (a trackpad) is scaled so a hundred pixels is one step,
    /// which is about one flick.
    pub fn zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) {
        let steps = match delta {
            MouseScrollDelta::LineDelta(_, lines) => f64::from(lines),
            MouseScrollDelta::PixelDelta(p) => p.y / 100.0,
        };
        if !steps.is_finite() || steps == 0.0 {
            return;
        }
        self.zoom_to(self.zoom * ZOOM_STEP.powf(steps), (x, y));
    }

    /// The zoom as the toolbar prints it: a whole percentage.
    pub fn percent_label(&self) -> String {
        format!("{}%", self.percent())
    }

    fn percent(&self) -> i64 {
        (self.zoom * 100.0).round() as i64
    }

    // --------------------------------------------------------------- pan

    /// Move the picture by `(dx, dy)` window pixels, within its overflow.
    /// `true` when it moved.
    pub fn pan_by(&mut self, dx: f32, dy: f32) -> bool {
        if !dx.is_finite() || !dy.is_finite() {
            return false;
        }
        let before = self.pan;
        self.pan = (self.pan.0 + dx, self.pan.1 + dy);
        self.settle();
        self.pan != before
    }

    /// The wheel: scroll the picture the way a page scrolls, a notch being
    /// [`PAN_LINE`] pixels and a pixel delta itself. Wheeling down reveals
    /// the lower part of the picture, as it reveals the lower part of a
    /// page; `axis` is the modifier's say over which way. `true` when the
    /// picture moved; a picture that fits does not.
    pub fn pan_wheel(&mut self, delta: MouseScrollDelta, axis: WheelAxis) -> bool {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * PAN_LINE, y * PAN_LINE),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        let (dx, dy) = match axis {
            WheelAxis::AsTurned => (dx, dy),
            WheelAxis::Sideways => (dx + dy, 0.0),
            WheelAxis::UpAndDown => (0.0, dx + dy),
        };
        self.pan_by(dx, dy)
    }

    // ------------------------------------------------------------ toolbar

    /// The toolbar's groups, each holding only the actions on offer; a
    /// group with none is left out, and so is its gap.
    fn groups(&self) -> impl Iterator<Item = Vec<Action>> + '_ {
        Action::TOOLBAR
            .iter()
            .map(|group| {
                group
                    .iter()
                    .copied()
                    .filter(|action| self.options.actions.contains(*action))
                    .collect::<Vec<_>>()
            })
            .filter(|group| !group.is_empty())
    }

    /// Whether the zoom readout sits on the toolbar: with either zoom button.
    fn has_zoom_group(&self) -> bool {
        self.options.actions.contains(Action::ZoomOut)
            || self.options.actions.contains(Action::ZoomIn)
    }

    /// The toolbar's panel: bottom centre of the window, as wide as the
    /// buttons on offer.
    pub fn toolbar_rect(&self) -> Rect {
        let groups: Vec<_> = self.groups().collect();
        let buttons: usize = groups.iter().map(Vec::len).sum();
        let gaps = groups.len().saturating_sub(1);
        let percent = if self.has_zoom_group() {
            PERCENT_W
        } else {
            0.0
        };
        let w = TOOLBAR_PAD * 2.0 + BUTTON_W * buttons as f32 + percent + GROUP_GAP * gaps as f32;
        Rect::new(
            ((self.viewport.0 - w) * 0.5).round(),
            (self.viewport.1 - TOOLBAR_BOTTOM - TOOLBAR_H).round(),
            w,
            TOOLBAR_H,
        )
    }

    /// Each toolbar button's action and slot, in order, with the percent
    /// label's slot after zoom-out and a gap between the groups: zoom, fit,
    /// copy · save · open, close. One layout, read by the paint, the hit
    /// test and the semantics alike.
    fn layout(&self) -> Vec<(Action, Rect)> {
        let bar = self.toolbar_rect();
        let mut x = bar.x + TOOLBAR_PAD;
        let mut slots = Vec::new();
        for (index, group) in self.groups().enumerate() {
            if index > 0 {
                x += GROUP_GAP;
            }
            if group.contains(&Action::ZoomIn) && !group.contains(&Action::ZoomOut) {
                x += PERCENT_W;
            }
            for action in group {
                slots.push((action, Rect::new(x, bar.y, BUTTON_W, bar.h)));
                x += BUTTON_W;
                if action == Action::ZoomOut {
                    x += PERCENT_W;
                }
            }
        }
        slots
    }

    /// The toolbar's buttons, in order. The copy button wears the copy
    /// affordance's glyph — the check while a copy is being confirmed.
    pub fn buttons(&self) -> ButtonGroup<Action> {
        let buttons: Vec<_> = self
            .layout()
            .into_iter()
            .map(|(action, rect)| {
                let (glyph, active) = match action {
                    Action::Copy => (self.copy.glyph(), self.copy.confirming()),
                    other => (other.glyph(), false),
                };
                IconButton::new(glyph, rect)
                    .on_click(action)
                    .with_state(ButtonState {
                        hovered: self.hover == Some(action),
                        active,
                        disabled: false,
                    })
            })
            .collect();
        ButtonGroup::new(buttons)
    }

    /// Where the percent label sits: between the two zoom buttons.
    fn percent_rect(&self) -> Rect {
        let bar = self.toolbar_rect();
        let lead = if self.options.actions.contains(Action::ZoomOut) {
            BUTTON_W
        } else {
            0.0
        };
        Rect::new(bar.x + TOOLBAR_PAD + lead, bar.y, PERCENT_W, bar.h)
    }

    /// The button under `(x, y)`, if any.
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons().action_at(x, y)
    }

    /// Perform an action: a toolbar button, a screen reader's press, or the
    /// host's own. One the host does not offer does nothing.
    pub fn act(&mut self, action: Action) -> Outcome {
        if !self.options.actions.contains(action) {
            return Outcome::Handled;
        }
        match action {
            Action::ZoomOut => self.zoom_out(),
            Action::ZoomIn => self.zoom_in(),
            Action::Fit => self.fit(),
            Action::ActualSize => self.actual_size(),
            Action::Copy => return self.ask_copy(),
            Action::SaveAs => return Outcome::SaveAs,
            Action::OpenExternally => return Outcome::OpenExternally,
            Action::Close => return Outcome::Close,
        }
        Outcome::Handled
    }

    /// Ask for the copy and confirm it at once: the button's glyph turns to
    /// the check and holds while the host writes the clipboard.
    fn ask_copy(&mut self) -> Outcome {
        self.copy.confirm(Instant::now());
        Outcome::Copy
    }

    /// The glyph the copy button shows right now: the copy icon, or the
    /// check while a copy is being confirmed.
    pub fn copy_glyph(&self) -> char {
        self.copy.glyph()
    }

    // ---------------------------------------------------------- the clock

    /// Whether a frame is owed for the copy confirmation's morph.
    pub fn animating(&self, now: Instant) -> bool {
        self.copy.animating(now)
    }

    /// When the copy confirmation next needs a tick: the end of its hold.
    pub fn maintenance_deadline(&self) -> Option<Instant> {
        self.copy.maintenance_deadline()
    }

    /// Advance the copy confirmation. `true` when something drawn changed.
    pub fn maintain(&mut self, now: Instant) -> bool {
        self.copy.maintain(now)
    }

    // -------------------------------------------------------------- input

    /// A key while the viewer is up. Every key is the viewer's — it is
    /// modal — and the ones it understands act: `Esc` closes; `+`, `=` and
    /// `-` zoom, with or without the primary modifier; `0` returns to natural
    /// size; the arrows scroll a picture that overflows; `Ctrl+C` copies and
    /// the registered *Save diagram as…* chord saves, where the host offers
    /// them.
    pub fn key(&mut self, key: &KeyPress, text: Option<&str>, mods: ModifiersState) -> Outcome {
        if let KeyPress::Named(named) = key {
            match named {
                NamedKey::Escape => return Outcome::Close,
                NamedKey::ArrowLeft => {
                    self.pan_by(PAN_KEY, 0.0);
                }
                NamedKey::ArrowRight => {
                    self.pan_by(-PAN_KEY, 0.0);
                }
                NamedKey::ArrowUp => {
                    self.pan_by(0.0, PAN_KEY);
                }
                NamedKey::ArrowDown => {
                    self.pan_by(0.0, -PAN_KEY);
                }
                _ => {}
            }
        }
        if self.options.actions.contains(Action::SaveAs)
            && koru_keys::registry::get(koru_keys::ShortcutId::SaveDiagramAs)
                .chord
                .matches(key, text, mods)
        {
            return Outcome::SaveAs;
        }
        let primary = koru_keys::has_primary(mods) && !koru_keys::has_foreign_primary(mods);
        if primary
            && !mods.shift_key()
            && !mods.alt_key()
            && koru_keys::letter(key, text) == Some('c')
        {
            return if self.options.actions.contains(Action::Copy) {
                self.ask_copy()
            } else {
                Outcome::Handled
            };
        }
        // Shift is allowed through: on most layouts `+` *is* Shift+`=`, and
        // the chord form `Ctrl+=` arrives as `=`. Alt is not: AltGr makes
        // other characters of these keys.
        if !mods.alt_key() {
            match koru_keys::letter(key, text) {
                Some('+') | Some('=') => self.zoom_in(),
                Some('-') => self.zoom_out(),
                Some('0') => self.actual_size(),
                _ => {}
            }
        }
        Outcome::Handled
    }

    /// A press: a toolbar button acts; the picture arms a drag; the dimmed
    /// window arms a close on release — a press that turns into a drag is
    /// not a dismissal.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> Outcome {
        self.pointer = (x, y);
        if let Some(action) = self.action_at(x, y) {
            return self.act(action);
        }
        if self.toolbar_rect().contains(x, y) {
            return Outcome::Handled;
        }
        if self.card_rect().contains(x, y) {
            self.drag = Some(Drag {
                from: (x, y),
                pan: self.pan,
                moved: false,
            });
            return Outcome::Handled;
        }
        self.press_on_scrim = true;
        Outcome::Handled
    }

    /// The pointer moved: the toolbar's hover, or the drag's pan. `true`
    /// when something drawn changed.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(drag) = self.drag.as_mut() {
            let dx = x - drag.from.0;
            let dy = y - drag.from.1;
            if !drag.moved && dx * dx + dy * dy < DRAG_SLOP * DRAG_SLOP {
                return false;
            }
            drag.moved = true;
            let pan = drag.pan;
            self.pan = (pan.0 + dx, pan.1 + dy);
            let before = self.pan;
            self.settle();
            // Drawn only when the clamp let it move; a picture that fits
            // stays put under a drag, and no frame is spent saying so.
            return before == self.pan || self.draggable();
        }
        let hover = self.action_at(x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// A release: a press on the dimmed window that stayed a press closes;
    /// a drag ends where it is.
    pub fn mouse_up(&mut self) -> Outcome {
        self.drag = None;
        if std::mem::take(&mut self.press_on_scrim) {
            return Outcome::Close;
        }
        Outcome::Handled
    }

    /// A double click on the picture: natural size if it is fitted, fitted
    /// if it is not — the two sizes a person switches between most.
    pub fn double_click(&mut self, x: f32, y: f32) -> Outcome {
        if self.action_at(x, y).is_some() || self.toolbar_rect().contains(x, y) {
            return Outcome::Handled;
        }
        if (self.zoom - 1.0).abs() < 1e-6 {
            self.fit();
        } else {
            self.actual_size();
        }
        Outcome::Handled
    }

    /// The pointer's shape: a press target over the toolbar, the closed
    /// hand mid-drag, the open hand over a picture that can be dragged, and
    /// the plain arrow everywhere else — the dimmed window included, which is
    /// a thing you click to close but, by the design rule, does not say so
    /// with the pointer.
    pub fn cursor(&self) -> CursorIcon {
        if self.drag.as_ref().is_some_and(|drag| drag.moved) {
            return CursorIcon::Grabbing;
        }
        if self.hover.is_some() {
            return crate::cursor::PRESS;
        }
        let (x, y) = self.pointer;
        if self.draggable() && self.card_rect().contains(x, y) {
            return CursorIcon::Grab;
        }
        crate::cursor::IDLE
    }

    // ---------------------------------------------------------- semantics

    /// What a screen reader should hear, in `lang`: the dialog named by its
    /// title (or its kind), a picture's size, the zoom, and every button on
    /// offer with its label and rect. The host adds its own offset when the
    /// viewer is drawn away from the window's origin.
    pub fn semantics(&self, lang: Lang) -> ViewerSemantics {
        let name = if self.options.title.trim().is_empty() {
            t(
                lang,
                match self.stage {
                    Stage::Diagram(_) => Key::ViewerDiagram,
                    Stage::Bitmap { .. } => Key::ViewerPicture,
                },
            )
            .to_owned()
        } else {
            self.options.title.clone()
        };
        let description = match &self.stage {
            Stage::Diagram(_) => None,
            Stage::Bitmap { image } => {
                let (w, h) = image.natural_size();
                Some(koru_i18n::fill(
                    t(lang, Key::ViewerPictureSize),
                    &[&w.to_string(), &h.to_string()],
                ))
            }
        };
        let buttons = self
            .layout()
            .into_iter()
            .map(|(action, rect)| ViewerControl {
                action,
                label: action.label(lang).to_owned(),
                value: None,
                rect,
            })
            .collect();
        ViewerSemantics {
            name,
            description,
            bounds: Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            zoom: ViewerControl {
                action: Action::ActualSize,
                label: Action::ActualSize.label(lang).to_owned(),
                value: Some(koru_i18n::fill(
                    t(lang, Key::ViewerZoomPercent),
                    &[&self.percent().to_string()],
                )),
                rect: self.percent_rect(),
            },
            buttons,
        }
    }

    // -------------------------------------------------------------- paint

    /// Paint the viewer over the whole window: the scrim, the card and the
    /// picture on it, then the toolbar.
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine) {
        let viewport = self.viewport;
        let p = theme::p();
        crate::modal::draw(canvas, root, viewport, |canvas| {
            let card = self.card_rect();
            let picture = self.picture_rect();
            let _viewer = inspect::scope(
                "KoruMediaViewer",
                "component",
                root,
                scene::rect(0.0, 0.0, viewport.0, viewport.1),
                |r| {
                    r.property(
                        "implementation",
                        "koru_ui::media_viewer",
                        "MediaViewer; ADR-482",
                    );
                    r.property("stage", self.stage.kind(), "Stage");
                    r.property("zoom", self.percent_label(), "MediaViewer::zoom");
                    r.property(
                        "pan",
                        format!("{:.0}, {:.0}", self.pan.0, self.pan.1),
                        "MediaViewer::pan; window pixels from centred",
                    );
                    r.property(
                        "draggable",
                        self.draggable(),
                        "the picture overflows the stage",
                    );
                },
            );
            // The card: the page colour under the picture, so a drawing's
            // background role is honoured and a transparent screenshot reads
            // as it did on the page, raised off the dimmed window.
            {
                let radius = match self.stage {
                    Stage::Diagram(_) => 6.0,
                    Stage::Bitmap { .. } => 2.0,
                };
                let _card = inspect::scope(
                    "Card",
                    "part",
                    root,
                    scene::rect(card.x, card.y, card.w, card.h),
                    |r| {
                        r.property("fill", inspect::color(p.bg), "theme::p().bg");
                    },
                );
                scene::fill(
                    canvas,
                    root,
                    p.panel_shadow,
                    &scene::rrect(card.x + 2.0, card.y + 4.0, card.w, card.h, radius),
                );
                scene::fill(
                    canvas,
                    root,
                    p.bg,
                    &scene::rrect(card.x, card.y, card.w, card.h, radius),
                );
            }
            match &self.stage {
                Stage::Diagram(prepared) => {
                    let transform = root
                        * Affine::translate((f64::from(picture.x), f64::from(picture.y)))
                        * Affine::scale(self.zoom);
                    koru_draw::diagram::paint(prepared, p, transform, self.zoom, canvas);
                }
                Stage::Bitmap { image } => {
                    // The device pixels the picture covers decide the level:
                    // the root carries the window's scale factor.
                    let device = root_scale(root) * f64::from(picture.w);
                    let level = image.level(image.level_for(device));
                    let sx = f64::from(picture.w) / f64::from(level.image.width.max(1));
                    let sy = f64::from(picture.h) / f64::from(level.image.height.max(1));
                    let transform = root
                        * Affine::translate((f64::from(picture.x), f64::from(picture.y)))
                        * Affine::scale_non_uniform(sx, sy);
                    scene::draw_image(canvas, transform, level);
                }
            }

            // The toolbar: a popover panel with the buttons and the percent
            // between the two that change it.
            let bar = self.toolbar_rect();
            scene::floating_panel(
                canvas,
                root,
                bar.x,
                bar.y,
                bar.w,
                bar.h,
                &scene::Panel::popover(TOOLBAR_RADIUS),
            );
            self.buttons().draw(canvas, root, engine);
            if self.has_zoom_group() {
                let label = engine.simple_layout(&self.percent_label(), theme::role::hint());
                let slot = self.percent_rect();
                let lx = slot.x + (slot.w - label.width()) * 0.5;
                let ly = slot.y + (slot.h - label.height()) * 0.5;
                scene::draw_layout_first_line(canvas, root, &label, lx, ly, Some(p.text_dim));
            }
        });
    }
}

/// The scale `root` applies: the window's scale factor, and any zoom of the
/// host's around the viewer.
fn root_scale(root: Affine) -> f64 {
    let [a, b, _, _, _, _] = root.as_coeffs();
    (a * a + b * b).sqrt().max(f64::EPSILON)
}
