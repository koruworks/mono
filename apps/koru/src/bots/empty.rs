// SPDX-License-Identifier: MPL-2.0

//! **The Bots view with no Bot in it** (QUESTIONS O155, the first-bot
//! experience): what a Bot is, in the main area, and the one thing to do
//! about it.
//!
//! ```text
//! col  Bots empty view                   the content area, on the window's ground
//!   Spacer
//!   row  Bots empty middle
//!     Spacer
//!     col  Bots empty content            at most the design's width; S5 between
//!       row  Bots empty faces row        three faces, each over the one before
//!       col  Bots empty words            the heading, then the paragraph
//!       row  Bots empty actions          New bot (the primary action)
//!       Bots empty examples              what people use Bots for
//!     Spacer
//!   Spacer
//! ```
//!
//! The window shows it in the content area while the Bots panel is what the
//! navigator shows and the roster has answered that the workspace has no
//! Bot (`MainView::bots_empty_visible`); a roster still loading, or one the
//! service did not answer, is never *no Bots*. It holds no data and asks
//! for nothing: one element frame is its paint, its pointer target, its
//! keyboard stop and its accessibility node.
//!
//! The faces are decoration: three of the table's, wearing sample initials,
//! at rest. The words are the design's except where the design says
//! something a Bot does not do (it runs on this computer while Koru is
//! open; nothing runs it on a schedule until a Routine is set up).

use std::cell::RefCell;

use koru_draw::el::{
    Action, El, ElState, Frame, FrameCx, Interaction, Key, Leaf, Role, TextCache, col, leaf, row,
    spacer, text,
};
use koru_draw::face::{Face, FacePose};
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Metric, Space, Surface, roles};
use koru_i18n::{Key as Words, Lang, t};
use koru_ui::button::ActionStyle;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::faces;
use super::view::controls::ButtonLeaf;

/// The surface every key below belongs to.
pub(crate) const SURFACE: &str = "bots-empty";
pub(crate) const ROOT: Key = Key::surface(SURFACE, "root");
pub(crate) const TITLE: Key = Key::surface(SURFACE, "title");
pub(crate) const BODY: Key = Key::surface(SURFACE, "body");
pub(crate) const NEW_BOT: Key = Key::surface(SURFACE, "new-bot");
pub(crate) const EXAMPLES: Key = Key::surface(SURFACE, "examples");

/// The widest the words are set: the design's measure.
const CONTENT_W: Metric = Metric::new("BOTS_EMPTY_W", 420.0);

/// The three faces drawn, and the sample initial each wears. Decoration:
/// they name no Bot.
const FACES: [(u8, &str); 3] = [(1, "K"), (5, "L"), (7, "J")];

/// **The three overlapping faces**: each after the first sits over the one
/// before it, with a rim of the ground between them so they read as three.
struct IntroFaces;

impl Leaf for IntroFaces {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let step = faces::INTRO_FACE.value - faces::INTRO_OVERLAP.value;
        (
            faces::INTRO_FACE.value + step * (FACES.len() as f32 - 1.0),
            faces::INTRO_FACE.value,
        )
    }

    fn paint(
        &self,
        scene_out: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        let size = faces::INTRO_FACE.value.min(at.h);
        let step = size - faces::INTRO_OVERLAP.value;
        // The rim: the face's own outline, a little larger, in the ground.
        let rim = Space::S1.px(cx.profile) * 0.75;
        let ground = cx.surface(Surface::Ground);
        for (index, (face, initial)) in FACES.into_iter().enumerate() {
            let x = at.x + step * index as f32;
            if index > 0 {
                let outline = koru_draw::face::geometry(face, size + rim * 2.0, FacePose::STILL);
                scene::fill(
                    scene_out,
                    root * Affine::translate((f64::from(x - rim), f64::from(at.y - rim))),
                    ground,
                    &outline.tile,
                );
            }
            Face::new(face, initial).draw_in(
                scene_out,
                root,
                engine,
                Rect::new(x, at.y, size, size),
                cx.palette(),
            );
        }
    }
}

/// What a press or a key on the view asks of the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Not the view's.
    Ignored,
    /// Taken, and nothing to do: repaint.
    Consumed,
    /// *New bot* was pressed: the window opens the dialog.
    NewBot,
}

/// The view's own state: whether the keyboard has been used on it (its ring
/// is drawn only then), the pointer, and its text cache.
#[derive(Default)]
pub(crate) struct EmptyView {
    /// The keyboard is on *New bot*, visibly.
    focused: bool,
    hot: bool,
    cache: RefCell<TextCache>,
}

impl EmptyView {
    /// Whether the keyboard is visibly on *New bot*.
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }

    /// Put the keyboard on *New bot*, visibly: where it returns when the
    /// dialog this view opened closes.
    pub(crate) fn focus(&mut self) -> bool {
        !std::mem::replace(&mut self.focused, true)
    }

    /// The view went away, or the pointer was used elsewhere: no ring.
    pub(crate) fn blur(&mut self) -> bool {
        let changed = self.focused || self.hot;
        self.focused = false;
        self.hot = false;
        changed
    }

    /// The view as elements.
    pub(crate) fn tree(&self, engine: &mut TextEngine, lang: Lang) -> El {
        let label = t(lang, Words::BotsNewBot).to_owned();
        let content = col("Bots empty content")
            .fill()
            .max_w(CONTENT_W)
            .gap(Space::S5)
            .child(
                row("Bots empty faces row")
                    .child(leaf("Bots empty faces", IntroFaces).decorative())
                    .child(spacer()),
            )
            .child(
                col("Bots empty words")
                    .gap(Space::S2_5)
                    .child(
                        text(t(lang, Words::BotsIntroTitle))
                            .named("Bots empty title")
                            .id(TITLE)
                            .role(roles::SUBTITLE)
                            .emphasis(true)
                            .wrap()
                            .semantics(Role::Heading)
                            .name(Words::BotsIntroTitle),
                    )
                    .child(
                        text(t(lang, Words::BotsIntroBody))
                            .named("Bots empty body")
                            .id(BODY)
                            .role(roles::UI)
                            .ink(Ink::Secondary)
                            .wrap()
                            .semantics(Role::Group)
                            .name(Words::BotsIntroBody),
                    ),
            )
            .child(
                row("Bots empty actions")
                    .child(
                        leaf(
                            "Bots empty new bot",
                            ButtonLeaf::new(
                                engine,
                                label,
                                ActionStyle::Primary,
                                false,
                                self.focused,
                            ),
                        )
                        .semantic()
                        .id(NEW_BOT)
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::Button)
                        .name(Words::BotsNewBot),
                    )
                    .child(spacer()),
            )
            .child(
                text(t(lang, Words::BotsIntroExamples))
                    .named("Bots empty examples")
                    .id(EXAMPLES)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap()
                    .semantics(Role::Group)
                    .name(Words::BotsIntroExamples),
            );
        col("Bots empty view")
            .id(ROOT)
            .bg(Surface::Ground)
            .pad_x(Space::S6)
            .pad_y(Space::S10)
            .semantics(Role::Group)
            .name(Words::BotsTitle)
            .child(spacer())
            .child(
                row("Bots empty middle")
                    .child(spacer())
                    .child(content)
                    .child(spacer()),
            )
            .child(spacer())
    }

    fn interaction(&self) -> Interaction {
        Interaction {
            hot: self.hot.then_some(NEW_BOT),
            pressed: None,
            focused: self.focused.then_some(NEW_BOT),
        }
    }

    fn layout(&self, engine: &mut TextEngine, area: Rect, lang: Lang) -> (El, Frame) {
        let tree = self.tree(engine, lang);
        let frame = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            area,
            FrameCx::root().with_interaction(self.interaction()),
        );
        (tree, frame)
    }

    /// The frame as input, accessibility and a test read it.
    pub(crate) fn with_frame<R>(
        &self,
        area: Rect,
        lang: Lang,
        read: impl FnOnce(&Frame) -> R,
    ) -> R {
        let (_, frame) = crate::input_engine::with(|engine| self.layout(engine, area, lang));
        read(&frame)
    }

    pub(crate) fn draw(
        &self,
        scene_out: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        lang: Lang,
    ) {
        let (tree, frame) = self.layout(engine, area, lang);
        frame.paint(&tree, scene_out, root, engine);
    }

    /// A press in the content area: *New bot*, or nothing. Either way it is
    /// the view's, so the tab drawn under it never sees it.
    pub(crate) fn mouse_down(&mut self, area: Rect, lang: Lang, x: f32, y: f32) -> Outcome {
        self.focused = false;
        match self.with_frame(area, lang, |frame| frame.hit_at(x, y)) {
            Some(key) if key == NEW_BOT => Outcome::NewBot,
            _ => Outcome::Consumed,
        }
    }

    pub(crate) fn mouse_move(&mut self, area: Rect, lang: Lang, x: f32, y: f32) -> bool {
        let hot = self.with_frame(area, lang, |frame| frame.hit_at(x, y)) == Some(NEW_BOT);
        std::mem::replace(&mut self.hot, hot) != hot
    }

    /// Whether the pointer is over *New bot*.
    pub(crate) fn hot(&self) -> bool {
        self.hot
    }

    /// **A key while the view is in the content area.** Tab puts the
    /// keyboard on *New bot*; Enter or Space presses it; nothing else is
    /// the view's, and none of it reaches the tab drawn under it.
    pub(crate) fn key(&mut self, key: &WinitKey, mods: ModifiersState) -> Outcome {
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return Outcome::Ignored;
        }
        match key {
            WinitKey::Named(NamedKey::Tab) => {
                self.focused = true;
                Outcome::Consumed
            }
            WinitKey::Named(NamedKey::Enter | NamedKey::Space) => {
                self.focused = true;
                Outcome::NewBot
            }
            WinitKey::Character(text) if text == " " => {
                self.focused = true;
                Outcome::NewBot
            }
            WinitKey::Named(NamedKey::Escape) if self.focused => {
                self.focused = false;
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }
}

#[cfg(test)]
mod tests;
