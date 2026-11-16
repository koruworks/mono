// SPDX-License-Identifier: MPL-2.0

//! **The component on show**: what stands on the stage for each kind of
//! story, the line under it, and the inputs the properties column draws for
//! it.
//!
//! ```text
//! col  Preview stage                  the panel; as wide as the preview width asks
//!   Component under test              a slot the production control is drawn in   (button, field)
//!   row|col  Layout under test        the element layer's own stack               (row, column)
//!     box  Layout item
//!   col  Token table                  a row a token, from the table itself        (tokens)
//!     row  Token                      a sample drawn with the token, its name, its value
//! ```
//!
//! **Everything on the stage is the production thing.** The button and the
//! text input are the shared controls, drawn in the slot the tree gives
//! them. The row and the column are `koru_draw::el` itself, as elements of
//! this window's own tree, so F12 shows them as it shows the app's. A token
//! table's samples are drawn *with* the token: a spacing step's bar is
//! `Length::Space(step)` wide, a surface's swatch is filled with
//! `Surface::…`, a text role's sample is set in that role. Their values in
//! words come from the same tables ([`TokenTable::rows`]).
//!
//! **A property's control is chosen by what kind of input it is**, never by
//! which story it belongs to: free text is a field, a choice among a few is
//! the shared segmented control, a choice among many is the shared dropdown,
//! and a yes or a no is the shared switch. A story may have several of each.
//!
//! **A widget story's component is one slot and its parts.** The slot is
//! sized by the story's [`Widget`](crate::ui_lab::stories::Widget), which
//! also says where the production component's own geometry puts each part;
//! a press, a key or a screen reader's request on a part all end in
//! [`Lab::press_part`].

use koru_draw::el::Axis;
use koru_draw::theme::Space;

use super::settings::PreviewWidth;
use super::*;
use crate::ui_lab::registry::{FieldStyle, Input};
use crate::ui_lab::story::{OptionLabel, Story, StoryOption, TokenRow, TokenTable};

/// The surface a token's row is identified under; its part is the token.
pub(super) const TOKEN: &str = "ui-lab.token";

/// The most options a choice shows side by side. Past this it is a dropdown:
/// fifteen spacing steps do not fit a properties column.
const SEGMENTS_MAX: usize = 4;

/// What a person reads for an option.
pub(super) fn option_text(label: &OptionLabel) -> String {
    match label {
        OptionLabel::Key(key) => text(*key).to_owned(),
        OptionLabel::Literal(literal) => literal.clone(),
    }
}

/// The box the shared text input is drawn in, for a style of the story's.
pub(super) fn field_box(style: FieldStyle, focused: bool) -> InputStyle {
    match style {
        FieldStyle::Form => InputStyle::form(focused),
        FieldStyle::Search => InputStyle::search(),
        FieldStyle::Inline => InputStyle::inline(),
    }
}

impl Lab {
    /// The story's input that `key` is the control of.
    pub(super) fn control_of(&self, key: Key) -> Option<Control> {
        self.story
            .as_ref()?
            .controls()
            .iter()
            .copied()
            .find(|control| control_key(*control) == key)
    }

    /// The choices the story shows as segments, each with its options:
    /// those with few enough to stand side by side.
    pub(super) fn segmented_controls(&self) -> Vec<(Control, Vec<StoryOption>)> {
        let Some(story) = self.story.as_ref() else {
            return Vec::new();
        };
        story
            .controls()
            .iter()
            .filter(|control| control.input() == Input::Choice)
            .map(|control| (*control, story.options(*control)))
            .filter(|(_, options)| options.len() <= SEGMENTS_MAX)
            .collect()
    }

    /// The choice `key` is the segmented control of.
    pub(super) fn segmented_of(&self, key: Key) -> Option<Control> {
        let control = self.control_of(key)?;
        let story = self.story.as_ref()?;
        (control.input() == Input::Choice && story.options(control).len() <= SEGMENTS_MAX)
            .then_some(control)
    }

    /// The yes-or-no input `key` is the switch of.
    pub(super) fn toggle_of(&self, key: Key) -> Option<Control> {
        self.control_of(key)
            .filter(|control| control.input() == Input::Toggle)
    }

    /// Whether `key` is a choice of the story's that opens a list.
    pub(super) fn is_story_dropdown(&self, key: Key) -> bool {
        self.control_of(key)
            .is_some_and(|control| control.input() == Input::Choice)
            && self.segmented_of(key).is_none()
    }

    /// Press the part at `part` of the component under test: the keyboard
    /// goes to it, and it does what the story says a press on it does, or
    /// opens the list the story hangs from it. A part that is switched off
    /// or only read does nothing.
    pub(super) fn press_part(&mut self, engine: &mut TextEngine, part: usize) -> bool {
        let pressable = self
            .laid
            .as_ref()
            .and_then(|laid| laid.parts.get(part))
            .is_some_and(Part::pressable);
        let Some(story) = self
            .story
            .as_mut()
            .and_then(Story::as_widget_mut)
            .filter(|_| pressable)
        else {
            return false;
        };
        let moved = story.set_cursor(part);
        let widget = story.widget();
        if widget.list(story, part).is_some() {
            return self.open_menu(engine, SUBJECT) || moved;
        }
        widget.press(story, part) || moved
    }

    /// What using the component has done, or what it is, as the line under
    /// it. `None` for a story with nothing to say beyond what it shows.
    pub(super) fn result_line(&self) -> Option<String> {
        let lang = koru_i18n::current();
        match self.story.as_ref()? {
            Story::Button(button) => Some(match button.presses() {
                0 => text(koru_i18n::Key::UiLabNotPressed).to_owned(),
                presses => koru_i18n::fill(
                    text(koru_i18n::Key::UiLabPressCount),
                    &[&koru_i18n::number::decimal(lang, presses)],
                ),
            }),
            Story::Field(field) => Some(koru_i18n::fill(
                text(koru_i18n::Key::UiLabTypedCount),
                &[&koru_i18n::number::decimal(
                    lang,
                    field.typed().chars().count() as u64,
                )],
            )),
            // The call that draws what is on the stage, as code: identifiers
            // only, so it reads the same in every language.
            Story::Layout(layout) => Some(format!(
                "el::{}(\"Layout under test\").gap(Space::{}).pad(Space::{})",
                match layout.axis() {
                    Axis::Horizontal => "row",
                    Axis::Vertical => "col",
                },
                layout.gap().name(),
                layout.padding().name(),
            )),
            Story::Tokens(_) => None,
            Story::Widget(story) => story.widget().result(story),
        }
    }

    /// The panel the story's component stands on. `room` is how wide the
    /// preview may be.
    pub(super) fn stage(&self, engine: &mut TextEngine, story: &Story, room: f32) -> El {
        let profile = Profile::Desktop;
        // What the stage's own inset leaves the component. What would not
        // fit is clipped at the stage rather than drawn over a neighbour.
        let inside = (room - 2.0 * STAGE_INSET.token.px(profile)).max(0.0);
        let entry = self.entry;
        let (subject, wide) = match story {
            Story::Button(button) => (
                el::leaf(
                    "Component under test",
                    Slot {
                        width: button::button_width(engine, button.label()),
                        height: button::H,
                    },
                )
                .semantic()
                .id(SUBJECT)
                .align_self(Align::Centre)
                .max_w(Length::Runtime(
                    inside.max(button::MIN_W),
                    "preview column less its insets",
                ))
                .hit()
                .focusable()
                .semantics(Role::Button)
                .name_text(button.label())
                .action(el::Action::Click)
                .action(el::Action::Focus),
                false,
            ),
            Story::Field(_) => (
                el::leaf(
                    "Component under test",
                    Slot {
                        width: FIELD_SUBJECT_WIDTH.value,
                        height: FIELD_HEIGHT.value,
                    },
                )
                .semantic()
                .id(SUBJECT)
                .align_self(Align::Centre)
                .max_w(Length::Runtime(inside, "preview column less its insets"))
                .hit()
                .focusable()
                // Published as a text field with its value, like a
                // property's.
                .semantics(Role::Group)
                .name(entry.name)
                .action(el::Action::Focus),
                false,
            ),
            Story::Layout(layout) => {
                let stack = match layout.axis() {
                    Axis::Horizontal => el::row("Layout under test"),
                    Axis::Vertical => el::col("Layout under test"),
                };
                (
                    stack
                        .id(SUBJECT)
                        .align_self(Align::Centre)
                        .gap(layout.gap())
                        .pad(layout.padding())
                        .bg(Surface::Chip)
                        .radius(Radius::M)
                        .semantics(Role::Group)
                        .name(entry.name)
                        .children((0..3).map(|_| {
                            el::boxed("Layout item")
                                .w(LAYOUT_ITEM)
                                .h(LAYOUT_ITEM)
                                .bg(Surface::Selected)
                                .radius(Radius::S)
                        })),
                    false,
                )
            }
            Story::Widget(story) => {
                let (width, height) = story.widget().size(story, engine, inside);
                (
                    el::leaf("Component under test", Slot { width, height })
                        .semantic()
                        .id(SUBJECT)
                        .align_self(Align::Centre)
                        .max_w(Length::Runtime(inside, "preview column less its insets"))
                        .hit()
                        .focusable()
                        // A group of its parts, each published where the
                        // component's own geometry puts it.
                        .semantics(Role::Group)
                        .name(entry.name)
                        .action(el::Action::Focus),
                    false,
                )
            }
            Story::Tokens(table) => (
                el::col("Token table")
                    .id(SUBJECT)
                    .gap(TOKEN_ROWS_GAP)
                    .semantics(Role::Group)
                    .name(entry.name)
                    .children(table.rows().into_iter().map(|row| token_row(*table, &row))),
                true,
            ),
        };
        let stage = el::col("Preview stage")
            .id(STAGE)
            .align_self(Align::Centre)
            .pad(STAGE_INSET)
            .bg(Surface::Raised)
            .radius(Radius::L)
            .child(subject);
        match self.width {
            // A table is laid out against a width of its own; anything else
            // is as wide as it needs.
            PreviewWidth::Fit if wide => stage.w(Length::Runtime(
                room.min(TOKEN_TABLE_WIDTH.value),
                "a token table's width, or the preview column",
            )),
            PreviewWidth::Fit => stage,
            // Exactly the width asked for, so the component is seen in it.
            PreviewWidth::Fixed(_) => stage.w(Length::Runtime(room, "the preview width setting")),
        }
    }

    /// One property's control, by what kind of input it is.
    pub(super) fn control(&self, engine: &mut TextEngine, control: Control) -> El {
        let key = control_key(control);
        if control.input() == Input::Text {
            return el::leaf(
                "Property field",
                Slot {
                    width: 0.0,
                    height: FIELD_HEIGHT.value,
                },
            )
            .semantic()
            .h(FIELD_HEIGHT)
            .id(key)
            .hit()
            .focusable()
            // The element layer has no text-field role; the window's
            // semantics publish this node as one, with its value.
            .semantics(Role::Group)
            .name(control.name())
            .action(el::Action::Focus);
        }
        if control.input() == Input::Toggle {
            let on = self.story.as_ref().and_then(|story| story.flag(control));
            return el::leaf(
                "Property switch",
                Slot {
                    width: Toggle::TRACK_W,
                    height: Toggle::H,
                },
            )
            .semantic()
            .align_self(Align::Start)
            .id(key)
            .hit()
            .focusable()
            // Published as a switch that is on or off.
            .semantics(Role::Button)
            .name(control.name())
            .selected(on == Some(true))
            .action(el::Action::Click)
            .action(el::Action::Focus);
        }
        if self.is_story_dropdown(key) {
            return self
                .dropdown(engine, key, control.name())
                .align_self(Align::Start);
        }
        let labels: Vec<String> = self.story.as_ref().map_or(Vec::new(), |story| {
            story
                .options(control)
                .iter()
                .map(|option| option_text(&option.label))
                .collect()
        });
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        // The shared segmented control draws no focus state: the box that
        // holds it wears the frame's, and is the control that is pressed.
        el::boxed("Property choice")
            .align_self(Align::Start)
            .pad(FOCUS_INSET)
            .radius(Radius::M)
            .states(&FRAME_STATES)
            .id(key)
            .hit()
            .focusable()
            .semantics(Role::Group)
            .name(control.name())
            .action(el::Action::Focus)
            .child(
                el::leaf(
                    "Property segments",
                    Slot {
                        width: Segmented::width(engine, &labels),
                        height: Segmented::H,
                    },
                )
                .decorative()
                .id(segments_key(control)),
            )
    }
}

/// One token: a sample drawn with it, its name as the source spells it, and
/// what it comes to.
fn token_row(table: TokenTable, row: &TokenRow) -> El {
    let name = el::text(row.name)
        .named("Token name")
        .role(roles::META)
        .fill()
        .truncate();
    let value = el::text(row.value.clone())
        .named("Token value")
        .role(roles::META)
        .ink(Ink::Hint)
        .fill()
        .truncate();
    let line = el::row("Token")
        .id(Key::surface(TOKEN, row.name))
        .min_h(theme::metrics::ROW_MIN)
        .gap(TOKEN_GAP)
        .items_center();
    // A text role is its own sample: the words are what is being shown, so
    // they come last and take what the row leaves.
    if table == TokenTable::TextRoles {
        let role = roles::ALL
            .into_iter()
            .find(|role| role.name == row.name)
            .unwrap_or(roles::ROW);
        return line.child(name).child(value).child(
            el::text(text(koru_i18n::Key::UiLabTextSample))
                .named("Token sample")
                .role(role)
                .fill_weight(2.0)
                .truncate(),
        );
    }
    line.child(
        el::boxed("Token sample")
            .w(TOKEN_SAMPLE_WIDTH)
            .child(token_sample(table, row.name)),
    )
    .child(name)
    .child(value)
}

/// The sample of the token named `name`, drawn with the token itself.
fn token_sample(table: TokenTable, name: &str) -> El {
    match table {
        // A bar exactly the step long.
        TokenTable::Space => {
            let step = Space::ALL
                .into_iter()
                .find(|step| step.name() == name)
                .unwrap_or(Space::S0);
            el::boxed("Space bar")
                .w(Length::Space(step))
                .h(TOKEN_BAR_HEIGHT)
                .align_self(Align::Start)
                .bg(Surface::Selected)
        }
        // The fill on the window's ground and on a raised surface, so a
        // role that is one of those two is still seen against the other.
        TokenTable::Surface => {
            let role = Surface::ALL
                .iter()
                .copied()
                .find(|role| role.name() == name)
                .unwrap_or(Surface::Raised);
            let on = |ground: Surface| {
                el::boxed("Surface backing")
                    .fill()
                    .pad(Space::S1)
                    .bg(ground)
                    .child(
                        el::boxed("Surface swatch")
                            .h(TOKEN_BAR_HEIGHT)
                            .bg(role)
                            .radius(Radius::S),
                    )
            };
            el::row("Surface sample")
                .child(on(Surface::Ground))
                .child(on(Surface::Raised))
        }
        // Two letters set in the ink.
        TokenTable::Ink => {
            let role = Ink::ALL
                .iter()
                .copied()
                .find(|role| role.name() == name)
                .unwrap_or(Ink::Primary);
            el::text("Aa")
                .named("Ink sample")
                .role(roles::SUBTITLE)
                .ink(role)
        }
        // The hairline itself.
        TokenTable::Line => {
            let role = Line::ALL
                .iter()
                .copied()
                .find(|role| role.name() == name)
                .unwrap_or(Line::Divider);
            el::col("Line sample").child(el::divider(role))
        }
        TokenTable::TextRoles => el::spacer(),
    }
}

#[cfg(test)]
mod tests;
