// SPDX-License-Identifier: MPL-2.0

//! **The widget stories, one place each.**
//!
//! A shared widget's story is a [`Widget`]: a value with no state of its own
//! that says how big the production component is, where its pressable parts
//! are, how it is painted and what a press does. Everything a person changed
//! is in the [`WidgetStory`] it is handed: the recipe's inputs as plain
//! values and the little the component remembers ([`Held`]). The window
//! draws, hit-tests and publishes every widget story through these few
//! questions, so adding one is writing its `Widget` and listing it in
//! [`ALL`], beside its recipe in the registry; nothing in the window names
//! it.
//!
//! ```text
//! Component under test                one slot on the stage, one Tab stop
//!   part                              a pressable piece, where the component's
//!                                     own geometry puts it
//! ```
//!
//! **One geometry.** [`Widget::parts`] is asked once a frame, for the
//! rectangle the slot was given, and answers from the production component's
//! own layout (`Segmented::layout`, `SplitButton::action_rect`, …). The
//! frame keeps the answer: the pointer is tested against it, the keyboard
//! steps through it and the screen reader's tree is built from it, so the
//! three cannot disagree with what was painted.
//!
//! **The keyboard is on one part at a time.** The component is one Tab
//! stop; the arrow keys move between its parts and Enter or Space presses
//! the one the keyboard is on, unless the widget says an arrow does
//! something else (a segmented control's arrows choose).

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use vello::Scene;
use vello::kurbo::Affine;

use super::registry::Control;
use super::story::{Held, StoryOption, WidgetStory};

/// What a part of a component is, to a screen reader.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PartRole {
    /// Pressed for an effect.
    Button,
    /// A button that stays down: on or off.
    Pressed(bool),
    /// A switch: on or off.
    Switch(bool),
    /// One of a set of which one is chosen.
    Radio(bool),
    /// A field that shows one value and opens the list of the rest.
    Combo,
    /// Something to read, not to press: a stepper's value.
    Readout,
}

/// One piece of the component on the stage.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Part {
    /// The part's identity in the window: a code keyword, the same in every
    /// language.
    pub(crate) part: &'static str,
    /// Where the production component puts it, in the window's coordinates.
    pub(crate) rect: Rect,
    pub(crate) role: PartRole,
    /// What a screen reader calls it, in the display language.
    pub(crate) name: String,
    /// What it holds, where that is not its name.
    pub(crate) value: Option<String>,
    pub(crate) disabled: bool,
    /// Whether the production component draws its own keyboard-focus mark
    /// for this part. Where it does not, the window draws a ring round the
    /// part, and the manifest lists the component as lacking one.
    pub(crate) own_focus: bool,
}

impl Part {
    /// A part that is pressed for an effect and draws its own focus mark.
    pub(crate) fn button(part: &'static str, rect: Rect, name: impl Into<String>) -> Self {
        Self {
            part,
            rect,
            role: PartRole::Button,
            name: name.into(),
            value: None,
            disabled: false,
            own_focus: true,
        }
    }

    pub(crate) fn role(mut self, role: PartRole) -> Self {
        self.role = role;
        self
    }

    pub(crate) fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The component has no focus mark for this part: the window rings it.
    pub(crate) fn ringed(mut self) -> Self {
        self.own_focus = false;
        self
    }

    /// Whether a press, by pointer or keyboard, does anything here.
    pub(crate) fn pressable(&self) -> bool {
        !self.disabled && self.role != PartRole::Readout
    }
}

/// Where the pointer and the keyboard are on the component, by part.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pointed {
    /// The part under the pointer.
    pub(crate) hovered: Option<usize>,
    /// The part the keyboard is on, while the component has the keyboard.
    pub(crate) focused: Option<usize>,
    /// Whether the list this component opened is showing.
    pub(crate) open: bool,
}

/// What a painter draws with.
pub(crate) struct Canvas<'a> {
    pub(crate) scene: &'a mut Scene,
    pub(crate) root: Affine,
    pub(crate) engine: &'a mut TextEngine,
}

/// **One widget's story.** See the module note.
pub(crate) trait Widget: Sync {
    /// The story's registered ID: the one its recipe carries.
    fn id(&self) -> &'static str;

    /// The sample text a free-text input opens with.
    fn sample(&self, _control: Control) -> Option<koru_i18n::Key> {
        None
    }

    /// Where a choice opens: the slot of its option in force.
    fn opens_at(&self, _control: Control) -> usize {
        0
    }

    /// Whether a yes-or-no input opens as a yes.
    fn opens_on(&self, _control: Control) -> bool {
        false
    }

    /// What the component remembers when the story opens, and after a
    /// reset.
    fn held(&self) -> Held {
        Held::default()
    }

    /// The options of a choice the registration's option sets do not list:
    /// content the story supplies, such as sample colors.
    fn options(&self, _control: Control) -> Option<Vec<StoryOption>> {
        None
    }

    /// How big the component is, in a column `room` wide.
    fn size(&self, story: &WidgetStory, engine: &mut TextEngine, room: f32) -> (f32, f32);

    /// The component's parts, laid out in `rect` by the production
    /// component's own geometry, in keyboard order.
    fn parts(&self, story: &WidgetStory, engine: &mut TextEngine, rect: Rect) -> Vec<Part>;

    /// Draw the production component in `rect`.
    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed);

    /// The part at `part` was pressed. Returns whether the story changed.
    fn press(&self, story: &mut WidgetStory, part: usize) -> bool;

    /// An arrow key while the keyboard is on the component. `parts` is the
    /// last frame's. The keyboard moves to the next or previous part that
    /// can be pressed, stopping at either end, unless the widget says
    /// otherwise.
    fn arrow(&self, story: &mut WidgetStory, parts: &[Part], forward: bool) -> bool {
        let at = story.cursor();
        let next = if forward {
            (at + 1..parts.len()).find(|index| parts[*index].pressable())
        } else {
            (0..at.min(parts.len()))
                .rev()
                .find(|index| parts[*index].pressable())
        };
        next.is_some_and(|next| story.set_cursor(next))
    }

    /// The list a press on `part` opens instead of doing anything itself:
    /// its options, the one in force marked.
    fn list(&self, _story: &WidgetStory, _part: usize) -> Option<Vec<StoryOption>> {
        None
    }

    /// The option at `slot` of the list [`Widget::list`] gave was chosen.
    fn pick(&self, _story: &mut WidgetStory, _slot: usize) -> bool {
        false
    }

    /// What using the component has done, as the line under it.
    fn result(&self, story: &WidgetStory) -> Option<String>;
}

mod actions;
mod choices;
mod pickers;

/// Every widget story the Lab can run.
pub(crate) const ALL: &[&dyn Widget] = &[
    &actions::IconButtonStory,
    &actions::ButtonGroupStory,
    &actions::SplitButtonStory,
    &actions::CopyStory,
    &choices::ToggleStory,
    &choices::ChipStory,
    &choices::SegmentedStory,
    &choices::DropdownStory,
    &pickers::SwatchStory,
    &pickers::ColorPickerRowStory,
    &pickers::StepperStory,
    &pickers::PreviewTileStory,
];

/// A string of the Lab's, in the display language.
fn text(key: koru_i18n::Key) -> &'static str {
    koru_i18n::t(koru_i18n::current(), key)
}

/// The line under a component that is pressed for an effect: how often it
/// has been, in the words the Button's story uses.
fn presses_line(presses: u64) -> String {
    match presses {
        0 => text(koru_i18n::Key::UiLabNotPressed).to_owned(),
        presses => koru_i18n::fill(
            text(koru_i18n::Key::UiLabPressCount),
            &[&koru_i18n::number::decimal(koru_i18n::current(), presses)],
        ),
    }
}

/// The line under a component that chooses: what is chosen now.
fn chosen_line(chosen: &str) -> String {
    koru_i18n::fill(text(koru_i18n::Key::UiLabResultChosen), &[chosen])
}

/// The widget story registered as `story_id`.
pub(crate) fn find(story_id: &str) -> Option<&'static dyn Widget> {
    ALL.iter().copied().find(|widget| widget.id() == story_id)
}
