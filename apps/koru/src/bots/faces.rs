// SPDX-License-Identifier: MPL-2.0

//! **A Bot's face on a surface** (QUESTIONS O155, the first-bot experience):
//! the shared face primitive (`koru_draw::face`) as an element leaf, the
//! sizes the Bots surfaces draw it at, and the word for whether a Bot is
//! working, which is what moves its face.
//!
//! **Which face** a Bot wears is decided in one place
//! (`koru_ai_core::bots::Bot::avatar`: the stored choice, else the default
//! one). **Whether it moves** is decided by the window's one
//! [`FaceClock`](koru_draw::face::FaceClock) (`MainView::sync_bot_faces`):
//! a face is still unless its Bot is working or it is the face picked in
//! the standing *New bot* dialog, and no face asks for a frame while
//! neither is on screen. A leaf here only draws the pose it is handed.
//!
//! The face has no accessible name of its own: it stands beside its Bot's
//! name, so it is decorative there, and the *New bot* dialog names its
//! tiles and its preview on the element that hosts them.

use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::face::{Face, FacePose};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::Metric;
use vello::Scene;
use vello::kurbo::Affine;

/// A Bot's row in the Bots panel: its face.
pub(crate) const ROW_FACE: Metric = Metric::new("BOT_FACE_ROW", 28.0);
/// A Bot's row in the Bots panel: the least it is high, the face and its
/// breathing room.
pub(crate) const ROW_MIN: Metric = Metric::new("BOT_ROW_MIN", 36.0);
/// A Bot tab's header: the face beside the name.
pub(crate) const HEADER_FACE: Metric = Metric::new("BOT_FACE_HEADER", 28.0);
/// The empty view's three overlapping faces.
pub(crate) const INTRO_FACE: Metric = Metric::new("BOT_FACE_INTRO", 56.0);
/// How far each of the empty view's faces after the first sits over the one
/// before it.
pub(crate) const INTRO_OVERLAP: Metric = Metric::new("BOT_FACE_INTRO_OVERLAP", 12.0);
/// A face in the *New bot* dialog's grid.
pub(crate) const TILE_FACE: Metric = Metric::new("BOT_FACE_TILE", 46.0);
/// A grid tile: the face and the room its selection ring is drawn in.
pub(crate) const TILE: Metric = Metric::new("BOT_FACE_TILE_BOX", 54.0);
/// The *New bot* dialog's preview.
pub(crate) const PREVIEW_FACE: Metric = Metric::new("BOT_FACE_PREVIEW", 96.0);

/// **One face, drawn at `size`** in the middle of whatever it is given, in
/// the pose its host read from the window's clock ([`FacePose::STILL`] for a
/// face that never moves: the empty view's, a create tab's).
pub(crate) struct FaceLeaf {
    /// An index into `koru_draw::face::ROWS`.
    pub(crate) face: u8,
    /// The name whose initial the face wears; empty draws no initial.
    pub(crate) name: String,
    pub(crate) pose: FacePose,
    pub(crate) size: Metric,
}

impl FaceLeaf {
    /// A face at rest.
    pub(crate) fn still(face: u8, name: &str, size: Metric) -> Self {
        Self {
            face,
            name: name.to_owned(),
            pose: FacePose::STILL,
            size,
        }
    }
}

/// The square of side `size` in the middle of `at`.
pub(crate) fn centred(at: Rect, size: f32) -> Rect {
    let side = size.min(at.w).min(at.h).max(0.0);
    Rect::new(
        at.x + (at.w - side) * 0.5,
        at.y + (at.h - side) * 0.5,
        side,
        side,
    )
}

impl Leaf for FaceLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.size.value, self.size.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        Face::new(self.face, &self.name).posed(self.pose).draw_in(
            scene,
            root,
            engine,
            centred(at, self.size.value),
            cx.palette(),
        );
    }
}

#[cfg(test)]
mod tests;
