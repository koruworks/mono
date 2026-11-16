// SPDX-License-Identifier: MPL-2.0

//! **A story's state**: everything a person can change about the component
//! on show, and nothing else.
//!
//! [`Story`] is one of the kinds the Lab can run: the dialog button, pressed
//! for real ([`ButtonStory`]); the shared text input, typed in
//! ([`FieldStory`]); the element layer's row or column with its gap and
//! padding ([`LayoutStory`]); and a token table shown whole ([`TokenTable`]).
//! Each is plain data with no window, text engine or clock behind it, which
//! is what lets a story be driven in a test exactly as the window drives it.
//!
//! **What a story offers is the registration's and the production type's.**
//! Its inputs are the recipe's [`Control`]s, its styles are the recipe's
//! [`Styles`], a spacing choice is `Space::ALL` and a token table's rows are
//! the table's own `ALL`: a story cannot offer a style, a step or a role the
//! production code does not have. Nothing here is saved: a reset, showing
//! another record, or closing the Lab, is the end of it.

use koru_draw::el::Axis;
use koru_draw::theme::{Ink, Line, Profile, Space, Surface, roles};
use koru_ui::button::ButtonRole;

use super::registry::{self, Control, FieldStyle, Recipe, Styles};

/// The button under test, and what has happened to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonStory {
    /// The label a fresh or reset story starts with: synthetic content, in
    /// the display language the Lab opened in.
    default_label: String,
    label: String,
    styles: &'static [ButtonRole],
    style: ButtonRole,
    presses: u64,
}

impl ButtonStory {
    /// The registered dialog story, starting from `default_label`.
    pub(crate) fn new(default_label: &str) -> Self {
        let recipe = registry::story(registry::BUTTON_DIALOG_STORY).map(|(_, recipe)| recipe);
        let styles = match recipe.map(|recipe| recipe.styles) {
            Some(Styles::Dialog(styles)) => styles,
            _ => &[],
        };
        Self {
            default_label: default_label.to_owned(),
            label: default_label.to_owned(),
            styles,
            style: Self::default_style(styles),
            presses: 0,
        }
    }

    /// The first registered style, which the registration keeps equal to the
    /// production default.
    fn default_style(styles: &[ButtonRole]) -> ButtonRole {
        styles.first().copied().unwrap_or_default()
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn style(&self) -> ButtonRole {
        self.style
    }

    /// The styles on offer, in the registration's order.
    pub(crate) fn styles(&self) -> &'static [ButtonRole] {
        self.styles
    }

    pub(crate) fn presses(&self) -> u64 {
        self.presses
    }

    /// Take the label field's text, exactly as typed: an empty label is a
    /// real state of the production button and is shown as one.
    pub(crate) fn set_label(&mut self, label: &str) -> bool {
        if self.label == label {
            return false;
        }
        label.clone_into(&mut self.label);
        true
    }

    /// Wear the style at `slot`. A slot the registration does not have
    /// changes nothing.
    pub(crate) fn choose(&mut self, slot: usize) -> bool {
        match self.styles.get(slot) {
            Some(style) if *style != self.style => {
                self.style = *style;
                true
            }
            _ => false,
        }
    }

    /// The button was activated, by pointer or by keyboard.
    pub(crate) fn press(&mut self) {
        self.presses = self.presses.saturating_add(1);
    }

    /// Whether a reset would change anything.
    pub(crate) fn is_pristine(&self) -> bool {
        self.label == self.default_label
            && self.style == Self::default_style(self.styles)
            && self.presses == 0
    }

    /// Back to how the story opened: the default label and style, and no
    /// presses.
    pub(crate) fn reset(&mut self) -> bool {
        if self.is_pristine() {
            return false;
        }
        self.label.clone_from(&self.default_label);
        self.style = Self::default_style(self.styles);
        self.presses = 0;
        true
    }
}

/// What a person reads for one option of a choice: a translated word where
/// it is a human choice, the identifier itself where it is a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OptionLabel {
    Key(koru_i18n::Key),
    /// A technical identifier and its value: the same in every language.
    Literal(String),
}

/// One option of a choice a story offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoryOption {
    /// The production value's own identifier: what a written recipe names,
    /// and the option's identity in the window.
    pub(crate) part: &'static str,
    pub(crate) label: OptionLabel,
    pub(crate) selected: bool,
}

/// **The shared text input, typed in for real.** What is typed is the
/// field's own text, kept here so Reset can clear it; the placeholder and
/// the box it is drawn in are the recipe's two inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldStory {
    default_placeholder: String,
    placeholder: String,
    styles: &'static [FieldStyle],
    style: FieldStyle,
    /// What the field under test holds.
    typed: String,
}

impl FieldStory {
    fn new(recipe: &Recipe, default_placeholder: &str) -> Self {
        let styles = match recipe.styles {
            Styles::Field(styles) => styles,
            _ => &[],
        };
        Self {
            default_placeholder: default_placeholder.to_owned(),
            placeholder: default_placeholder.to_owned(),
            styles,
            style: styles.first().copied().unwrap_or_default(),
            typed: String::new(),
        }
    }

    pub(crate) fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub(crate) fn style(&self) -> FieldStyle {
        self.style
    }

    pub(crate) fn typed(&self) -> &str {
        &self.typed
    }

    fn is_pristine(&self) -> bool {
        self.placeholder == self.default_placeholder
            && Some(&self.style) == self.styles.first()
            && self.typed.is_empty()
    }
}

/// **The element layer's row or column**, three boxes in it, with the space
/// between them and the space round them chosen from the spacing scale. The
/// values are `koru_draw::theme::Space` itself, so what is drawn is what
/// `El::gap` and `El::pad` do with that step.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct LayoutStory {
    axis: Axis,
    gap: Space,
    padding: Space,
}

impl LayoutStory {
    /// The gap and padding a fresh layout story has: a step of each that is
    /// plainly visible.
    const GAP: Space = Space::S2;
    const PADDING: Space = Space::S4;

    fn new(axis: Axis) -> Self {
        Self {
            axis,
            gap: Self::GAP,
            padding: Self::PADDING,
        }
    }

    pub(crate) fn axis(&self) -> Axis {
        self.axis
    }

    pub(crate) fn gap(&self) -> Space {
        self.gap
    }

    pub(crate) fn padding(&self) -> Space {
        self.padding
    }

    fn slot(&mut self, control: Control) -> Option<&mut Space> {
        match control {
            Control::Gap => Some(&mut self.gap),
            Control::Padding => Some(&mut self.padding),
            _ => None,
        }
    }

    fn value(&self, control: Control) -> Option<Space> {
        match control {
            Control::Gap => Some(self.gap),
            Control::Padding => Some(self.padding),
            _ => None,
        }
    }
}

/// A token table a page shows whole, from the table itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TokenTable {
    /// `Space::ALL`: each step, its value, and a bar that long.
    Space,
    /// `Surface::ALL`: each fill role, painted.
    Surface,
    /// `Ink::ALL`: each text color role, set in it.
    Ink,
    /// `Line::ALL`: each hairline role, drawn.
    Line,
    /// `roles::ALL`: each text role, set in it, with its size and line.
    TextRoles,
}

/// One row of a token table, as a page lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TokenRow {
    /// The token's identifier, as the source spells it.
    pub(crate) name: &'static str,
    /// What it resolves to, as text: a length, the palette field it reads,
    /// a type size over its line height.
    pub(crate) value: String,
}

impl TokenTable {
    /// The table's rows, read from the production table in its order.
    pub(crate) fn rows(self) -> Vec<TokenRow> {
        let profile = Profile::Desktop;
        match self {
            Self::Space => Space::ALL
                .into_iter()
                .map(|step| TokenRow {
                    name: step.name(),
                    value: number(step.px(profile)),
                })
                .collect(),
            Self::Surface => Surface::ALL
                .iter()
                .map(|role| TokenRow {
                    name: role.name(),
                    value: role.field().to_owned(),
                })
                .collect(),
            Self::Ink => Ink::ALL
                .iter()
                .map(|role| TokenRow {
                    name: role.name(),
                    value: role.field().to_owned(),
                })
                .collect(),
            Self::Line => Line::ALL
                .iter()
                .map(|role| TokenRow {
                    name: role.name(),
                    value: role.field().to_owned(),
                })
                .collect(),
            Self::TextRoles => roles::ALL
                .into_iter()
                .map(|role| {
                    let metrics = role.style().metrics;
                    TokenRow {
                        name: role.name,
                        value: format!(
                            "{} / {}",
                            number(metrics.size),
                            number(metrics.line_height)
                        ),
                    }
                })
                .collect(),
        }
    }
}

/// A length in the display language's digits, without a fraction it does
/// not have.
fn number(value: f32) -> String {
    let lang = koru_i18n::current();
    if value.fract() == 0.0 {
        koru_i18n::number::decimal(lang, value as u64)
    } else {
        format!("{value}")
    }
}

/// How a spacing step reads in a list: its name, and what it comes to.
fn space_label(step: Space) -> String {
    format!("{} ({})", step.name(), number(step.px(Profile::Desktop)))
}

/// **The story on show**: one of the kinds the Lab can run, with whatever a
/// person has changed about it. The shell reads and changes every kind
/// through the same few questions — its inputs, each one's text or options —
/// so the properties panel is built from the registration and not from what
/// kind of story it happens to be.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Story {
    Button(ButtonStory),
    Field(FieldStory),
    Layout(LayoutStory),
    Tokens(TokenTable),
}

impl Story {
    /// The story `story_id` names, at its defaults, its sample content in
    /// the display language. `None` for an ID the registry does not have or
    /// the Lab has no story for.
    pub(crate) fn open(story_id: &str) -> Option<Self> {
        let (_, recipe) = registry::story(story_id)?;
        let text = |key| koru_i18n::t(koru_i18n::current(), key);
        Some(match story_id {
            registry::BUTTON_DIALOG_STORY => Self::Button(ButtonStory::new(text(
                koru_i18n::Key::UiLabButtonSampleLabel,
            ))),
            registry::TEXT_INPUT_STORY => Self::Field(FieldStory::new(
                recipe,
                text(koru_i18n::Key::UiLabFieldSamplePlaceholder),
            )),
            registry::ROW_STORY => Self::Layout(LayoutStory::new(Axis::Horizontal)),
            registry::COLUMN_STORY => Self::Layout(LayoutStory::new(Axis::Vertical)),
            registry::SPACE_STORY => Self::Tokens(TokenTable::Space),
            registry::SURFACE_STORY => Self::Tokens(TokenTable::Surface),
            registry::INK_STORY => Self::Tokens(TokenTable::Ink),
            registry::LINE_STORY => Self::Tokens(TokenTable::Line),
            registry::TEXT_ROLES_STORY => Self::Tokens(TokenTable::TextRoles),
            _ => return None,
        })
    }

    /// The registered ID of the story this is.
    pub(crate) fn id(&self) -> &'static str {
        match self {
            Self::Button(_) => registry::BUTTON_DIALOG_STORY,
            Self::Field(_) => registry::TEXT_INPUT_STORY,
            Self::Layout(layout) => match layout.axis {
                Axis::Horizontal => registry::ROW_STORY,
                Axis::Vertical => registry::COLUMN_STORY,
            },
            Self::Tokens(TokenTable::Space) => registry::SPACE_STORY,
            Self::Tokens(TokenTable::Surface) => registry::SURFACE_STORY,
            Self::Tokens(TokenTable::Ink) => registry::INK_STORY,
            Self::Tokens(TokenTable::Line) => registry::LINE_STORY,
            Self::Tokens(TokenTable::TextRoles) => registry::TEXT_ROLES_STORY,
        }
    }

    /// The inputs the properties panel offers, in the registration's order.
    pub(crate) fn controls(&self) -> &'static [Control] {
        registry::story(self.id()).map_or(&[][..], |(_, recipe)| recipe.controls)
    }

    /// What a free-text input holds. `None` for an input that is not text,
    /// or not this story's.
    pub(crate) fn text(&self, control: Control) -> Option<&str> {
        match (self, control) {
            (Self::Button(button), Control::Label) => Some(button.label()),
            (Self::Field(field), Control::Placeholder) => Some(field.placeholder()),
            _ => None,
        }
    }

    /// Take what a free-text input now holds, exactly as typed. Returns
    /// whether the story changed.
    pub(crate) fn set_text(&mut self, control: Control, text: &str) -> bool {
        match (self, control) {
            (Self::Button(button), Control::Label) => button.set_label(text),
            (Self::Field(field), Control::Placeholder) if field.placeholder != text => {
                text.clone_into(&mut field.placeholder);
                true
            }
            _ => false,
        }
    }

    /// The options a choice offers, the one in force marked. Empty for an
    /// input that is not a choice, or not this story's.
    pub(crate) fn options(&self, control: Control) -> Vec<StoryOption> {
        match (self, control) {
            (Self::Button(button), Control::Style) => button
                .styles()
                .iter()
                .map(|style| StoryOption {
                    part: dialog_part(*style),
                    label: OptionLabel::Key(registry::dialog_style_label(*style)),
                    selected: *style == button.style(),
                })
                .collect(),
            (Self::Field(field), Control::Style) => field
                .styles
                .iter()
                .map(|style| StoryOption {
                    part: style.ident(),
                    label: OptionLabel::Key(style.label()),
                    selected: *style == field.style,
                })
                .collect(),
            (Self::Layout(layout), Control::Gap | Control::Padding) => {
                let chosen = layout.value(control);
                Space::ALL
                    .into_iter()
                    .map(|step| StoryOption {
                        part: step.name(),
                        label: OptionLabel::Literal(space_label(step)),
                        selected: Some(step) == chosen,
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Put the option at `slot` of a choice in force. A slot the choice
    /// does not have changes nothing.
    pub(crate) fn choose(&mut self, control: Control, slot: usize) -> bool {
        match (self, control) {
            (Self::Button(button), Control::Style) => button.choose(slot),
            (Self::Field(field), Control::Style) => match field.styles.get(slot) {
                Some(style) if *style != field.style => {
                    field.style = *style;
                    true
                }
                _ => false,
            },
            (Self::Layout(layout), Control::Gap | Control::Padding) => {
                match (Space::ALL.get(slot), layout.slot(control)) {
                    (Some(step), Some(value)) if *value != *step => {
                        *value = *step;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Move a choice to its next or previous option, stopping at either
    /// end: what the arrow keys do on it.
    pub(crate) fn step(&mut self, control: Control, forward: bool) -> bool {
        let options = self.options(control);
        let Some(at) = options.iter().position(|option| option.selected) else {
            return false;
        };
        let next = if forward {
            at + 1
        } else {
            match at.checked_sub(1) {
                Some(previous) => previous,
                None => return false,
            }
        };
        self.choose(control, next)
    }

    /// Whether a reset would change anything.
    pub(crate) fn is_pristine(&self) -> bool {
        match self {
            Self::Button(button) => button.is_pristine(),
            Self::Field(field) => field.is_pristine(),
            Self::Layout(layout) => *layout == LayoutStory::new(layout.axis),
            Self::Tokens(_) => true,
        }
    }

    /// Back to how the story opened. Returns whether anything changed.
    pub(crate) fn reset(&mut self) -> bool {
        if self.is_pristine() {
            return false;
        }
        match self {
            Self::Button(button) => {
                button.reset();
            }
            Self::Field(field) => {
                field.placeholder.clone_from(&field.default_placeholder);
                field.style = field.styles.first().copied().unwrap_or_default();
                field.typed.clear();
            }
            Self::Layout(layout) => *layout = LayoutStory::new(layout.axis),
            Self::Tokens(_) => {}
        }
        true
    }

    /// Whether there is anything about the story a person can change, and
    /// so anything for Reset to put back. A token table is shown as it is.
    pub(crate) fn can_change(&self) -> bool {
        !matches!(self, Self::Tokens(_))
    }

    /// The component under test was activated, by pointer or by keyboard.
    /// Returns whether that did anything: only a button is pressed.
    pub(crate) fn press(&mut self) -> bool {
        match self {
            Self::Button(button) => {
                button.press();
                true
            }
            _ => false,
        }
    }

    /// Take what the component under test now holds, when it is one that is
    /// typed in.
    pub(crate) fn set_typed(&mut self, text: &str) -> bool {
        match self {
            Self::Field(field) if field.typed != text => {
                text.clone_into(&mut field.typed);
                true
            }
            _ => false,
        }
    }
}

/// A dialog style's identity: the enum member's own keyword. Exhaustive, so
/// a style added to the production button cannot be offered without one.
pub(crate) const fn dialog_part(style: ButtonRole) -> &'static str {
    match style {
        ButtonRole::Secondary => "secondary",
        ButtonRole::Primary => "primary",
        ButtonRole::Destructive => "destructive",
    }
}

#[cfg(test)]
mod tests;
