// SPDX-License-Identifier: MPL-2.0

//! **The shared controls a Bot's settings are made of**, as element leaves
//! (Bots P1.8 S5): the frame gives each its allocation, and each draws the
//! library's own component there — `TextInput` for the name,
//! `ActionButton` for an action, `Segmented` for the access choice — the
//! way the chat's question card hosts its field and its actions
//! (`koru-ai-ui/src/questions/elements.rs`). Nothing here draws a control of
//! its own.

use std::cell::RefCell;
use std::rc::Rc;

use koru_draw::el::{ElState, FrameCx, Leaf};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::metrics;
use koru_ui::button::{ActionButton, ActionStyle, Lead};
use koru_ui::segmented::Segmented;
use koru_ui::text_field::TextArea;
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;

/// The name field: the shared `TextInput`, drawn as a form's field.
pub(crate) struct FieldLeaf {
    pub(crate) input: Rc<RefCell<TextInput>>,
    pub(crate) placeholder: String,
    pub(crate) focused: bool,
}

impl Leaf for FieldLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, metrics::BUTTON_MIN.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        self.input.borrow_mut().draw(
            scene,
            root,
            engine,
            at,
            InputStyle::form(self.focused),
            Some(&self.placeholder),
            self.focused,
        );
    }
}

/// **The role's text** (S8): the shared multi-line `TextArea`, as tall as
/// four form rows; its own scroll takes what is longer.
pub(crate) struct AreaLeaf {
    pub(crate) input: Rc<RefCell<TextArea>>,
    pub(crate) placeholder: String,
    pub(crate) focused: bool,
}

/// How many form rows the role's text is tall.
pub(crate) const AREA_ROWS: f32 = 4.0;

impl Leaf for AreaLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, metrics::BUTTON_MIN.value * AREA_ROWS)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        self.input.borrow_mut().draw(
            scene,
            root,
            engine,
            at,
            Some(&self.placeholder),
            self.focused,
        );
    }
}

/// An action: the shared compact `ActionButton`, lit under the pointer,
/// ringed while the keyboard is on it, drawn inert while it cannot act.
pub(crate) struct ButtonLeaf {
    pub(crate) label: String,
    /// Measured with the engine before the tree is built: a leaf measures
    /// without one.
    pub(crate) width: f32,
    pub(crate) style: ActionStyle,
    pub(crate) disabled: bool,
    pub(crate) focused: bool,
    /// A mark before the label and its inks (`ActionButton`'s `Lead`): the
    /// mode chip's icon in the mode's color (P1.8 §20). `None` for every
    /// other action.
    pub(crate) lead: Option<Lead>,
}

impl ButtonLeaf {
    pub(crate) fn new(
        engine: &mut TextEngine,
        label: String,
        style: ActionStyle,
        disabled: bool,
        focused: bool,
    ) -> Self {
        Self::led(engine, label, style, disabled, focused, None)
    }

    /// The same action led by `lead`, measured with it.
    pub(crate) fn led(
        engine: &mut TextEngine,
        label: String,
        style: ActionStyle,
        disabled: bool,
        focused: bool,
        lead: Option<Lead>,
    ) -> Self {
        let width = ActionButton::new(&label, style, None).width_led(engine, lead);
        Self {
            label,
            width,
            style,
            disabled,
            focused,
            lead,
        }
    }
}

impl Leaf for ButtonLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, ActionButton::H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        let button = ActionButton::new(&self.label, self.style, None);
        if self.disabled {
            button.draw_disabled_led(scene, root, engine, at, self.lead);
        } else {
            button.draw_led(
                scene,
                root,
                engine,
                at,
                state.hovered,
                self.focused,
                self.lead,
            );
        }
    }
}

/// **A Routine's switch**: the shared `Toggle`, showing the state the owner
/// last confirmed. On and off differ by the knob's place as well as the
/// track's fill; `pending` dims it while a change is with the owner.
pub(crate) struct ToggleLeaf {
    pub(crate) on: bool,
    pub(crate) enabled: bool,
    pub(crate) pending: bool,
    pub(crate) focused: bool,
    pub(crate) rtl: bool,
}

impl Leaf for ToggleLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (
            koru_ui::toggle::Toggle::TRACK_W,
            metrics::BUTTON_MIN.value.max(koru_ui::toggle::Toggle::H),
        )
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        koru_ui::toggle::Toggle::new(self.on)
            .enabled(self.enabled)
            .pending(self.pending)
            .draw(
                scene,
                root,
                engine,
                at,
                self.rtl,
                state.hovered,
                self.focused,
            );
    }
}

/// **An icon-only action**: the shared inline `icon_button` — a page's or a
/// row's `⋯` — in a square as tall as an action. Its name is the element's;
/// its keyboard ring is the element's focus treatment.
pub(crate) struct IconLeaf {
    pub(crate) glyph: char,
    pub(crate) disabled: bool,
}

impl Leaf for IconLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (ActionButton::H, ActionButton::H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        koru_ui::icon_button::draw_with(
            scene,
            root,
            engine,
            at,
            self.glyph,
            koru_ui::icon_button::State {
                hovered: state.hovered,
                active: false,
                disabled: self.disabled,
            },
            koru_ui::icon_button::Metrics::INLINE,
        );
    }
}

/// The access choice: the shared `Segmented` track over the two approved
/// choices. Its segments are found from the same layout the paint uses
/// ([`segment_at`]).
pub(crate) struct ChoiceLeaf {
    pub(crate) labels: [String; 2],
    pub(crate) width: f32,
    pub(crate) selected: usize,
    pub(crate) hovered: Option<usize>,
    pub(crate) disabled: bool,
}

impl ChoiceLeaf {
    pub(crate) fn new(
        engine: &mut TextEngine,
        labels: [String; 2],
        selected: usize,
        hovered: Option<usize>,
        disabled: bool,
    ) -> Self {
        let width = Segmented::width(engine, &[labels[0].as_str(), labels[1].as_str()]);
        Self {
            labels,
            width,
            selected,
            hovered,
            disabled,
        }
    }
}

impl Leaf for ChoiceLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, Segmented::H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        let labels = [self.labels[0].as_str(), self.labels[1].as_str()];
        let track = Segmented::layout(engine, &labels, at.x, at.y);
        // A choice that cannot change while a change is on its way is the
        // same track, dimmed: nothing moves.
        let alpha = if self.disabled { 0.5 } else { 1.0 };
        track.draw(
            scene,
            root,
            engine,
            &labels,
            Some(self.selected),
            self.hovered,
            alpha,
        );
    }
}

/// The segment of a choice track laid at `at` that `(x, y)` is on: the
/// same `Segmented::layout` the paint draws.
pub(crate) fn segment_at(
    engine: &mut TextEngine,
    labels: &[String; 2],
    at: Rect,
    x: f32,
    y: f32,
) -> Option<usize> {
    Segmented::layout(
        engine,
        &[labels[0].as_str(), labels[1].as_str()],
        at.x,
        at.y,
    )
    .segment_at(x, y)
}

/// Each segment's rect, for the accessibility tree.
pub(crate) fn segments(engine: &mut TextEngine, labels: &[String; 2], at: Rect) -> Vec<Rect> {
    Segmented::layout(
        engine,
        &[labels[0].as_str(), labels[1].as_str()],
        at.x,
        at.y,
    )
    .segments
}
