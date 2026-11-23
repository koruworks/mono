// SPDX-License-Identifier: MPL-2.0

//! **A story's state**: everything a person can change about the component
//! on show, and nothing else.
//!
//! [`Story`] is one of the kinds the Lab can run: the dialog button, pressed
//! for real ([`ButtonStory`]); the shared text input, typed in
//! ([`FieldStory`]); the element layer's row or column with its gap and
//! padding ([`LayoutStory`]); a token table shown whole ([`TokenTable`]);
//! and any other shared widget ([`WidgetStory`]), whose inputs are held as
//! plain values and whose behavior is its [`Widget`]'s, so that a widget's
//! story is written in one place ([`super::stories`]).
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

use super::registry::{self, Control, FieldStyle, Input, Recipe, Styles};
use super::stories::{self, Widget};

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

/// What one input of a widget story holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// Free text, exactly as typed.
    Text(String),
    /// The slot of the option in force.
    Choice(usize),
    /// A yes or a no.
    Flag(bool),
}

/// **What a component on the stage remembers** beyond its inputs: what
/// using it has done. Plain data, read and written only by the story's
/// [`Widget`], which says what each field means for it; a reset puts back
/// what the widget opens with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    /// How often each thing that can be pressed was: a button once, a split
    /// button's two halves separately.
    pub(crate) presses: [u64; 2],
    /// Which of a set is chosen.
    pub(crate) chosen: Option<usize>,
    /// Whether a switch is on, or a confirmation is showing.
    pub(crate) on: bool,
    /// Where a stepper stands on its scale.
    pub(crate) level: usize,
    /// What the component would have handed to something outside the Lab:
    /// the text a copy would have put on the clipboard.
    pub(crate) note: Option<String>,
}

/// **Any other shared widget**: the recipe's inputs as plain values, the
/// part the keyboard is on, and what the component remembers. What they
/// mean is its [`Widget`]'s.
#[derive(Clone)]
pub(crate) struct WidgetStory {
    widget: &'static dyn Widget,
    recipe: &'static Recipe,
    /// What each input opened holding, in the recipe's order.
    opened: Vec<Value>,
    values: Vec<Value>,
    /// The part of the component the keyboard is on.
    cursor: usize,
    pub(crate) held: Held,
}

impl std::fmt::Debug for WidgetStory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WidgetStory")
            .field("id", &self.widget.id())
            .field("values", &self.values)
            .field("cursor", &self.cursor)
            .field("held", &self.held)
            .finish()
    }
}

impl PartialEq for WidgetStory {
    fn eq(&self, other: &Self) -> bool {
        self.widget.id() == other.widget.id()
            && self.values == other.values
            && self.cursor == other.cursor
            && self.held == other.held
    }
}

impl WidgetStory {
    /// `widget`'s story at its defaults, offering `recipe`'s inputs. Its
    /// sample text is in the display language.
    pub(crate) fn open(widget: &'static dyn Widget, recipe: &'static Recipe) -> Self {
        let opened: Vec<Value> = recipe
            .controls
            .iter()
            .map(|control| match control.input() {
                Input::Text => Value::Text(widget.sample(*control).map_or(String::new(), |key| {
                    koru_i18n::t(koru_i18n::current(), key).to_owned()
                })),
                Input::Choice => Value::Choice(widget.opens_at(*control)),
                Input::Toggle => Value::Flag(widget.opens_on(*control)),
            })
            .collect();
        Self {
            widget,
            recipe,
            values: opened.clone(),
            opened,
            cursor: 0,
            held: widget.held(),
        }
    }

    pub(crate) fn widget(&self) -> &'static dyn Widget {
        self.widget
    }

    fn value(&self, control: Control) -> Option<&Value> {
        let slot = self.recipe.controls.iter().position(|c| *c == control)?;
        self.values.get(slot)
    }

    fn value_mut(&mut self, control: Control) -> Option<&mut Value> {
        let slot = self.recipe.controls.iter().position(|c| *c == control)?;
        self.values.get_mut(slot)
    }

    /// What a free-text input holds; nothing for an input the recipe does
    /// not have.
    pub(crate) fn text(&self, control: Control) -> &str {
        match self.value(control) {
            Some(Value::Text(text)) => text,
            _ => "",
        }
    }

    /// The slot of a choice's option in force.
    pub(crate) fn choice(&self, control: Control) -> usize {
        match self.value(control) {
            Some(Value::Choice(slot)) => *slot,
            _ => 0,
        }
    }

    /// Whether a yes-or-no input says yes.
    pub(crate) fn flag(&self, control: Control) -> bool {
        matches!(self.value(control), Some(Value::Flag(true)))
    }

    /// The part the keyboard is on.
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put the keyboard on the part at `part`. Returns whether it moved.
    pub(crate) fn set_cursor(&mut self, part: usize) -> bool {
        let changed = self.cursor != part;
        self.cursor = part;
        changed
    }

    /// A choice's options: the story's own where it supplies them, else the
    /// members of the option set the recipe lists under the input's name.
    fn options(&self, control: Control) -> Vec<StoryOption> {
        let chosen = self.choice(control);
        let mut options = self.widget.options(control).unwrap_or_else(|| {
            self.recipe
                .options
                .iter()
                .find(|set| set.name == control.name())
                .map_or(Vec::new(), |set| {
                    set.members
                        .iter()
                        .map(|member| StoryOption {
                            part: member.ident,
                            label: member.label.map_or_else(
                                || OptionLabel::Literal(member.ident.to_owned()),
                                OptionLabel::Key,
                            ),
                            selected: false,
                        })
                        .collect()
                })
        });
        for (slot, option) in options.iter_mut().enumerate() {
            option.selected = slot == chosen;
        }
        options
    }

    fn set(&mut self, control: Control, value: Value) -> bool {
        match self.value_mut(control) {
            Some(held) if *held != value => {
                *held = value;
                true
            }
            _ => false,
        }
    }

    fn is_pristine(&self) -> bool {
        self.values == self.opened && self.cursor == 0 && self.held == self.widget.held()
    }
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
    Widget(WidgetStory),
}

/// A story of one of the first four kinds, by its registered ID. A widget
/// story is not listed here: it is found in [`stories::ALL`].
fn open_kind(story_id: &str, recipe: &Recipe) -> Option<Story> {
    let text = |key| koru_i18n::t(koru_i18n::current(), key);
    Some(match story_id {
        registry::BUTTON_DIALOG_STORY => Story::Button(ButtonStory::new(text(
            koru_i18n::Key::UiLabButtonSampleLabel,
        ))),
        registry::TEXT_INPUT_STORY => Story::Field(FieldStory::new(
            recipe,
            text(koru_i18n::Key::UiLabFieldSamplePlaceholder),
        )),
        registry::ROW_STORY => Story::Layout(LayoutStory::new(Axis::Horizontal)),
        registry::COLUMN_STORY => Story::Layout(LayoutStory::new(Axis::Vertical)),
        registry::SPACE_STORY => Story::Tokens(TokenTable::Space),
        registry::SURFACE_STORY => Story::Tokens(TokenTable::Surface),
        registry::INK_STORY => Story::Tokens(TokenTable::Ink),
        registry::LINE_STORY => Story::Tokens(TokenTable::Line),
        registry::TEXT_ROLES_STORY => Story::Tokens(TokenTable::TextRoles),
        _ => return None,
    })
}

impl Story {
    /// The story `story_id` names, at its defaults, its sample content in
    /// the display language. `None` for an ID the registry does not have or
    /// the Lab has no story for.
    pub(crate) fn open(story_id: &str) -> Option<Self> {
        let (_, recipe) = registry::story(story_id)?;
        match stories::find(story_id) {
            Some(widget) => Some(Self::Widget(WidgetStory::open(widget, recipe))),
            None => open_kind(story_id, recipe),
        }
    }

    /// The widget story this is, when it is one.
    pub(crate) fn as_widget(&self) -> Option<&WidgetStory> {
        match self {
            Self::Widget(story) => Some(story),
            _ => None,
        }
    }

    pub(crate) fn as_widget_mut(&mut self) -> Option<&mut WidgetStory> {
        match self {
            Self::Widget(story) => Some(story),
            _ => None,
        }
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
            Self::Widget(story) => story.widget.id(),
        }
    }

    /// The inputs the properties panel offers, in the registration's order.
    pub(crate) fn controls(&self) -> &'static [Control] {
        match self {
            Self::Widget(story) => story.recipe.controls,
            _ => registry::story(self.id()).map_or(&[][..], |(_, recipe)| recipe.controls),
        }
    }

    /// What a free-text input holds. `None` for an input that is not text,
    /// or not this story's.
    pub(crate) fn text(&self, control: Control) -> Option<&str> {
        match (self, control) {
            (Self::Button(button), Control::Label) => Some(button.label()),
            (Self::Field(field), Control::Placeholder) => Some(field.placeholder()),
            (Self::Widget(story), _) => match story.value(control) {
                Some(Value::Text(text)) => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    /// What a yes-or-no input says. `None` for an input that is not one, or
    /// not this story's.
    pub(crate) fn flag(&self, control: Control) -> Option<bool> {
        match self.as_widget()?.value(control)? {
            Value::Flag(on) => Some(*on),
            _ => None,
        }
    }

    /// Make a yes-or-no input say `on`. Returns whether the story changed.
    pub(crate) fn set_flag(&mut self, control: Control, on: bool) -> bool {
        match self {
            Self::Widget(story) if control.input() == Input::Toggle => {
                story.set(control, Value::Flag(on))
            }
            _ => false,
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
            (Self::Widget(story), _) if control.input() == Input::Text => {
                story.set(control, Value::Text(text.to_owned()))
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
            (Self::Widget(story), _) if control.input() == Input::Choice => story.options(control),
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
            (Self::Widget(story), _)
                if control.input() == Input::Choice && slot < story.options(control).len() =>
            {
                story.set(control, Value::Choice(slot))
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
            Self::Widget(story) => story.is_pristine(),
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
            Self::Widget(story) => {
                story.values.clone_from(&story.opened);
                story.cursor = 0;
                story.held = story.widget.held();
            }
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
        ButtonRole::Inverted => "inverted",
    }
}

#[cfg(test)]
mod tests;
