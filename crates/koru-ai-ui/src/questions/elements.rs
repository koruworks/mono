// SPDX-License-Identifier: MPL-2.0

//! **An agent's questions as elements** (koru-ai D108; `koru_draw::el`): one
//! tree gives every part of the card its place, and the same tree is what a
//! press, the pointer's hover and the keyboard's ring read back.
//!
//! ```text
//! col  Question slot                    the card's row in the transcript
//!   col  Question card                  QUESTION_CARD_PAD round, on the hover plate
//!     row  Questions heading            ? and *Questions for you*
//!     col  Question                     a question a block, in the agent's order
//!       Question header                 meta role, secondary, elided
//!       Question words                  row role, wrapped
//!       col  Question options           the options in the agent's order
//!         row  Question option          its label and line, the ✓ when chosen
//!       Answer field                    *Other…*, an answer, or a masked secret
//!     row  Question actions             *Skip*, *Submit* at the end
//! ```
//!
//! An answered set is a smaller tree, *Questions answered*: its heading and
//! a line a question. Every value is a token or a tagged metric
//! ([`super::tokens`]); this file is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`). The answer box and the two actions are
//! leaves: the shared `TextInput` and `ActionButton`, drawn in the
//! allocation the frame gives them.

use koru_draw::el::{
    Align, El, ElState, Frame, FrameCx, Key, Leaf, Look, Ring, Role, StateRecipe, col, icon, leaf,
    row, spacer, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Radius, Surface, metrics, roles};
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::text_input::InputStyle;
use vello::Scene;
use vello::kurbo::Affine;

pub(crate) use super::tokens::*;

/// The developer names the card reads its geometry back by.
pub(crate) const SLOT: &str = "Question slot";
pub(crate) const CARD: &str = "Question card";
pub(crate) const HEADING: &str = "Questions heading";
pub(crate) const HEADING_WORDS: &str = "Questions heading words";
pub(crate) const BLOCK: &str = "Question";
pub(crate) const HEADER: &str = "Question header";
pub(crate) const WORDS: &str = "Question words";
pub(crate) const OPTIONS: &str = "Question options";
pub(crate) const OPTION: &str = "Question option";
pub(crate) const OPTION_WORDS: &str = "Option words";
pub(crate) const OPTION_LABEL: &str = "Option label";
pub(crate) const OPTION_LINE: &str = "Option line";
pub(crate) const CHECK_LANE: &str = "Option check lane";
pub(crate) const CHECK: &str = "Option check";
pub(crate) const FIELD: &str = "Answer field";
pub(crate) const ACTIONS: &str = "Question actions";
pub(crate) const SUBMIT: &str = "Submit";
pub(crate) const SKIP: &str = "Skip";
pub(crate) const SUMMARY: &str = "Questions answered";
pub(crate) const SUMMARY_LINE: &str = "Answer line";

/// One option as the card draws it.
pub(crate) struct OptionSpec {
    pub(crate) key: Key,
    pub(crate) label: String,
    /// The line under it; empty when the agent gave none.
    pub(crate) line: String,
    pub(crate) chosen: bool,
}

/// The answer box: *Other…*, an answer, or a masked secret.
pub(crate) struct FieldSpec {
    pub(crate) key: Key,
    pub(crate) input: super::AnswerInput,
    pub(crate) placeholder: String,
    pub(crate) focused: bool,
}

/// One question as the card draws it.
pub(crate) struct BlockSpec {
    pub(crate) header: String,
    pub(crate) question: String,
    pub(crate) options: Vec<OptionSpec>,
    pub(crate) field: Option<FieldSpec>,
}

/// *Skip* or *Submit*.
pub(crate) struct ButtonSpec {
    pub(crate) key: Key,
    pub(crate) label: String,
    /// Measured with the engine before the tree is built: a leaf measures
    /// without one.
    pub(crate) width: f32,
    pub(crate) primary: bool,
    pub(crate) disabled: bool,
    pub(crate) focused: bool,
}

/// A waiting set as the card draws it.
pub(crate) struct CardSpec {
    pub(crate) heading: String,
    pub(crate) blocks: Vec<BlockSpec>,
    pub(crate) skip: ButtonSpec,
    pub(crate) submit: ButtonSpec,
}

/// An answered, skipped or closed set: a heading and a line a question.
pub(crate) struct SummarySpec {
    pub(crate) heading: String,
    pub(crate) lines: Vec<String>,
}

/// **An option's states**: on the raised plate at rest, the chip under the
/// pointer, the selected plate once chosen; the focus ring while the
/// keyboard is on it.
pub(crate) static OPTION_STATES: StateRecipe = StateRecipe {
    name: "question option",
    base: Look::bg(Surface::Raised),
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Chip),
    pressed: Look::bg(Surface::Chip),
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::bg(Surface::Selected),
    disabled: Look::bg(Surface::Raised),
    focus: Some(Ring::Line(Line::Focus)),
};

/// **An option**: its label and the line under it, wrapped, and the ✓'s
/// lane at its end, kept whether or not it shows.
fn option_row(spec: &OptionSpec) -> El {
    let mut words = col(OPTION_WORDS).fill().gap(OPTION_WORDS_GAP).child(
        text(spec.label.clone())
            .named(OPTION_LABEL)
            .role(roles::ROW)
            .ink(Ink::Primary)
            .wrap(),
    );
    if !spec.line.is_empty() {
        words = words.child(
            text(spec.line.clone())
                .named(OPTION_LINE)
                .role(roles::META)
                .ink(Ink::Secondary)
                .wrap(),
        );
    }
    let mut lane = col(CHECK_LANE)
        .w(metrics::TRAILING_LANE_W)
        .items_end()
        .align_self(Align::Centre);
    if spec.chosen {
        lane = lane.child(
            icon(icons::CHECK, IconSize::XS)
                .decorative()
                .ink(Ink::Accent)
                .named(CHECK),
        );
    }
    row(OPTION)
        .min_h(OPTION_MIN_HEIGHT)
        .radius(Radius::M)
        .pad(OPTION_INSET)
        .items_start()
        .states(&OPTION_STATES)
        .selected(spec.chosen)
        .id(spec.key)
        .hit()
        .focusable()
        .semantics(Role::Button)
        .name_text(spec.label.clone())
        .child(words)
        .child(lane)
}

/// **A question**: its header, its words, its options in the agent's
/// order, and the box its own words go in when it takes them.
fn block(spec: &BlockSpec) -> El {
    let mut block = col(BLOCK).gap(BLOCK_GAP);
    if !spec.header.is_empty() {
        block = block.child(
            text(spec.header.clone())
                .named(HEADER)
                .role(roles::META)
                .ink(Ink::Secondary)
                .truncate(),
        );
    }
    block = block.child(
        text(spec.question.clone())
            .named(WORDS)
            .role(roles::ROW)
            .ink(Ink::Primary)
            .wrap(),
    );
    if !spec.options.is_empty() {
        block = block.child(
            col(OPTIONS)
                .gap(OPTION_GAP)
                .children(spec.options.iter().map(option_row)),
        );
    }
    if let Some(field) = &spec.field {
        block = block.child(
            leaf(
                FIELD,
                FieldLeaf {
                    input: field.input.clone(),
                    placeholder: field.placeholder.clone(),
                    focused: field.focused,
                },
            )
            .semantic()
            .h(FIELD_HEIGHT)
            .id(field.key)
            .hit()
            .focusable()
            .name_text(field.placeholder.clone()),
        );
    }
    block
}

fn button(name: &'static str, spec: &ButtonSpec) -> El {
    leaf(
        name,
        ButtonLeaf {
            label: spec.label.clone(),
            width: spec.width,
            primary: spec.primary,
            disabled: spec.disabled,
            focused: spec.focused,
        },
    )
    .semantic()
    .id(spec.key)
    .hit()
    .focusable()
    .disabled(spec.disabled)
    .semantics(Role::Button)
    .name_text(spec.label.clone())
}

/// **The card as one tree**, inside its slot: the heading, a block a
/// question, and the two actions at its end.
pub(crate) fn tree(spec: &CardSpec) -> El {
    let heading = row(HEADING)
        .gap(HEADING_GAP)
        .items_center()
        .child(
            icon(icons::QUESTION, IconSize::S)
                .decorative()
                .ink(Ink::Accent),
        )
        .child(
            text(spec.heading.clone())
                .named(HEADING_WORDS)
                .role(roles::ROW)
                .ink(Ink::Primary)
                .emphasis(true)
                .truncate(),
        );
    let actions = row(ACTIONS)
        .gap(ACTION_GAP)
        .items_center()
        .child(spacer())
        .child(button(SKIP, &spec.skip))
        .child(button(SUBMIT, &spec.submit));
    let card = col(CARD)
        .pad(CARD_INSET)
        .gap(CARD_GAP)
        .bg(Surface::Hover)
        .radius(Radius::L)
        .semantics(Role::Group)
        .child(heading)
        .children(spec.blocks.iter().map(block))
        .child(actions);
    col(SLOT).child(card)
}

/// **An answered set as one tree**: its heading and a line a question.
pub(crate) fn summary_tree(spec: &SummarySpec) -> El {
    let card = col(SUMMARY)
        .pad(SUMMARY_INSET)
        .gap(SUMMARY_GAP)
        .bg(Surface::Hover)
        .radius(Radius::L)
        .child(
            row(HEADING)
                .gap(HEADING_GAP)
                .items_center()
                .child(
                    icon(icons::QUESTION, IconSize::XS)
                        .decorative()
                        .ink(Ink::Secondary),
                )
                .child(
                    text(spec.heading.clone())
                        .named(HEADING_WORDS)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap(),
                ),
        )
        .children(spec.lines.iter().map(|line| {
            text(line.clone())
                .named(SUMMARY_LINE)
                .role(roles::META)
                .ink(Ink::Primary)
                .wrap()
        }));
    col(SLOT).child(card)
}

/// `tree` laid out in `area` under `cx`: on the engine and cache the
/// composer's panels share with the model picker.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    crate::model_picker::layout_elements(tree, area, cx)
}

/// The height the card in `tree` needs at `width`.
pub(crate) fn height(tree: &El, width: f32) -> f32 {
    let frame = layout(
        tree,
        Rect::new(0.0, 0.0, width, f32::from(u16::MAX)),
        FrameCx::root(),
    );
    frame
        .rect_of(CARD)
        .or_else(|| frame.rect_of(SUMMARY))
        .map_or(0.0, |card| card.h)
}

/// **The answer box** (D108): the shared `TextInput` — masked for a secret
/// — drawn as a form's field in the allocation the frame gives it.
pub(crate) struct FieldLeaf {
    input: super::AnswerInput,
    placeholder: String,
    focused: bool,
}

impl Leaf for FieldLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (0.0, FIELD_HEIGHT.value)
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
        self.input.draw(
            scene,
            root,
            engine,
            at,
            InputStyle::form(self.focused),
            &self.placeholder,
            self.focused,
        );
    }
}

/// **An action** (D108): the shared compact `ActionButton` — *Submit* in
/// the accent, *Skip* beside it — lit under the pointer, ringed while the
/// keyboard is on it, drawn inert while it cannot act.
pub(crate) struct ButtonLeaf {
    label: String,
    width: f32,
    primary: bool,
    disabled: bool,
    focused: bool,
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
        let style = if self.primary {
            ActionStyle::Primary
        } else {
            ActionStyle::Secondary
        };
        let button = ActionButton::new(&self.label, style, None);
        if self.disabled {
            button.draw_disabled(scene, root, engine, at);
        } else {
            button.draw_focused(scene, root, engine, at, state.hovered, self.focused);
        }
    }
}
