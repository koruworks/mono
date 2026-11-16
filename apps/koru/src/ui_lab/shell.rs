// SPDX-License-Identifier: MPL-2.0

//! **The Lab's one window**, on the shared single-window runner.
//!
//! [`Lab`] is a `koru_window::WindowApp` and nothing more: it is handed a
//! text engine, draws into a scene, and answers pointer and key events. It is
//! not a workspace view and is built from none of the workspace host's
//! startup, so it has no profile, no Store session, no settings file and no
//! background worker of its own. Closing the window ends the process.
//!
//! The surface is one element tree ([`koru_draw::el`]). One layout gives the
//! frame that paint, the pointer and the keyboard all read, so where a
//! control is drawn and where it is pressed cannot disagree.

use koru_draw::el::{self, El, Frame, FrameCx, Key, TextCache};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Ink, Line, Metric, Space, Surface, roles};
use koru_ui::caption;
use koru_window::{WindowApp, WindowRequest};
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::PhysicalSize;
use winit::keyboard::{Key as KeyboardKey, ModifiersState};
use winit::window::{ResizeDirection, WindowAttributes};

use super::placement::Placement;

/// The caption row: the band that drags the window and holds its controls.
const CAPTION: Key = Key::surface("ui-lab", "caption");

/// The caption's height, the one every Koru window's caption has.
const CAPTION_HEIGHT: Metric = Metric::new("CAPTION_H", theme::CAPTION_H);

/// The room the three window controls take at the caption's end, kept clear
/// of the title.
const CAPTION_CONTROLS: Metric = Metric::new("CAPTION_CONTROLS_W", theme::CAPTION_BTN * 3.0);

/// What the last frame drew, kept so the pointer is answered from the same
/// layout the person is looking at.
struct Laid {
    frame: Frame,
}

impl Laid {
    fn rect(&self, key: Key) -> Option<Rect> {
        self.frame.node(key).map(|node| node.rect)
    }
}

/// The UI Lab window.
pub(crate) struct Lab {
    placement: Placement,
    /// Set by the caption's ✕ or the system's close; the runner ends the
    /// process when it reads it.
    closed: bool,
    maximised: bool,
    /// A caption control's request, taken by the runner after the event.
    request: Option<WindowRequest>,
    caption_hover: Option<caption::Action>,
    cache: TextCache,
    laid: Option<Laid>,
}

impl Lab {
    pub(crate) fn new(placement: Placement) -> Self {
        Self {
            placement,
            closed: false,
            maximised: false,
            request: None,
            caption_hover: None,
            cache: TextCache::default(),
            laid: None,
        }
    }

    fn title() -> &'static str {
        koru_i18n::t(koru_i18n::current(), koru_i18n::Key::UiLabTitle)
    }

    fn caption_rect(&self) -> Option<Rect> {
        self.laid.as_ref()?.rect(CAPTION)
    }

    fn caption_action_at(&self, x: f32, y: f32) -> Option<caption::Action> {
        caption::direct_hit(self.caption_rect()?, x, y)
    }

    /// The window as elements: the caption over the Lab's body.
    fn tree() -> El {
        let caption = el::row("Caption")
            .id(CAPTION)
            .h(CAPTION_HEIGHT)
            .pad_start(Space::S4)
            .items_center()
            .child(
                el::text(Self::title())
                    .named("Window title")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            )
            .child(el::boxed("Window controls").w(CAPTION_CONTROLS));
        el::col("UI Lab")
            .bg(Surface::Ground)
            .child(caption)
            .child(el::divider(Line::Divider))
            .child(el::col("Lab body").fill())
    }
}

impl WindowApp for Lab {
    type Event = ();

    fn window(&self) -> WindowAttributes {
        self.placement.attributes(Self::title())
    }

    fn should_exit(&self) -> bool {
        self.closed
    }

    fn close_requested(&mut self) {
        self.closed = true;
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.caption_rect()
            .is_some_and(|caption| caption.contains(x, y))
            && self.caption_action_at(x, y).is_none()
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        koru_window::resize::edge(width, height, x, y, maximised)
    }

    fn resized(&mut self, _size: PhysicalSize<u32>, _scale_factor: f64) {
        // The next frame is laid out at the size the runner hands `draw`;
        // until then the last frame no longer matches the window.
        self.laid = None;
    }

    fn paint_before_showing(&self) -> bool {
        true
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.request.take()
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        let changed = self.maximised != maximised;
        self.maximised = maximised;
        changed
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        let hover = self.caption_action_at(x, y);
        let changed = self.caption_hover != hover;
        self.caption_hover = hover;
        changed
    }

    fn pointer_left(&mut self) -> bool {
        self.caption_hover.take().is_some()
    }

    fn click(&mut self, _engine: &mut TextEngine, x: f32, y: f32, _ctrl: bool) -> bool {
        match self.caption_action_at(x, y) {
            Some(caption::Action::Minimise) => self.request = Some(WindowRequest::Minimise),
            Some(caption::Action::ToggleMaximise) => {
                self.request = Some(WindowRequest::ToggleMaximise);
            }
            Some(caption::Action::Close) => self.closed = true,
            // The Lab's caption has no window menu.
            Some(caption::Action::WindowMenu) | None => return false,
        }
        true
    }

    fn key(
        &mut self,
        _engine: &mut TextEngine,
        _key: &KeyboardKey,
        _text: Option<&str>,
        _mods: ModifiersState,
    ) -> bool {
        false
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        let tree = Self::tree();
        let frame = tree.layout(
            engine,
            &mut self.cache,
            Rect::new(0.0, 0.0, w, h),
            FrameCx::root(),
        );
        let state = caption::State {
            hovered: self.caption_hover,
            maximized: self.maximised,
            menu: caption::MenuVisual::default(),
        };
        frame.paint_with_host(
            &tree,
            scene,
            Affine::IDENTITY,
            engine,
            |node, scene, root, engine| {
                if node.key == Some(CAPTION) {
                    caption::draw_direct_controls(scene, root, engine, node.rect, state);
                }
            },
        );
        self.laid = Some(Laid { frame });
    }
}

#[cfg(test)]
mod tests;
