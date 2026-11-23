// SPDX-License-Identifier: MPL-2.0

//! **The shared choice widgets on the stage**: the switch, the chip, the
//! segmented control and the dropdown.
//!
//! ```text
//! Component under test
//!   switch                            Toggle: the whole control
//!   <chip>…                           Chip: three, one of them chosen
//!   <segment>…                        Segmented: each segment of one track
//!   field                             Dropdown: the field; its list is the shared menu
//! ```
//!
//! Each draws the production component and nothing beside it. What a person
//! chooses between is sample content in the display language (three
//! densities, the eight languages by their own names); the inputs are the
//! production calls' own.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_i18n::Key;
use koru_ui::chip::{self, Chip};
use koru_ui::{Dropdown, DropdownOption, Segmented, Toggle};

use super::{Canvas, Part, PartRole, Pointed, Widget, chosen_line, text};
use crate::ui_lab::registry::{self, Control};
use crate::ui_lab::story::{Held, OptionLabel, StoryOption, WidgetStory};

/// Three words a person might choose between: a density.
const DENSITIES: [Key; 3] = [
    Key::UiLabOptionCompact,
    Key::UiLabOptionRegular,
    Key::UiLabOptionStandard,
];

/// A story that opens with its first item chosen.
fn first_chosen() -> Held {
    Held {
        chosen: Some(0),
        ..Held::default()
    }
}

// ----------------------------------------------------------------- toggle

/// `koru_ui::toggle::Toggle`: a switch that is pressed for real, with its
/// three builder inputs.
pub(super) struct ToggleStory;

impl ToggleStory {
    fn control(story: &WidgetStory) -> Toggle {
        let on = story.held.on;
        let toggle = Toggle::new(on)
            .enabled(!story.flag(Control::Disabled))
            .pending(story.flag(Control::Pending));
        if story.flag(Control::Leading) {
            // The pair the first caller passes: the glyph differs with the
            // state, so on and off differ by more than color.
            toggle.icon(if on { icons::CLOUD } else { icons::CLOUD_SLASH })
        } else {
            toggle
        }
    }
}

impl Widget for ToggleStory {
    fn id(&self) -> &'static str {
        registry::TOGGLE_STORY
    }

    fn size(&self, story: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        (Self::control(story).width(), Toggle::H)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        // A switch that is off or waiting on a change takes no press.
        let inert = story.flag(Control::Disabled) || story.flag(Control::Pending);
        vec![
            Part::button("switch", rect, text(Key::UiLabToggleName))
                .role(PartRole::Switch(story.held.on))
                .disabled(inert),
        ]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        Self::control(story).draw(
            canvas.scene,
            canvas.root,
            canvas.engine,
            rect,
            koru_i18n::current().is_rtl(),
            pointed.hovered.is_some(),
            pointed.focused.is_some(),
        );
    }

    fn press(&self, story: &mut WidgetStory, _: usize) -> bool {
        story.held.on = !story.held.on;
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(
            text(if story.held.on {
                Key::UiLabResultOn
            } else {
                Key::UiLabResultOff
            })
            .to_owned(),
        )
    }
}

// ------------------------------------------------------------------- chip

/// `koru_ui::chip::Chip`: three chips of which one is chosen. The first
/// one's label is the story's text input.
pub(super) struct ChipStory;

impl ChipStory {
    /// The room a chip keeps before its label when the story asks for a
    /// leading mark: the mark the story draws there is this wide.
    const MARK: f32 = 16.0;

    fn labels(story: &WidgetStory) -> [String; 3] {
        [
            story.text(Control::Label).to_owned(),
            text(DENSITIES[1]).to_owned(),
            text(DENSITIES[2]).to_owned(),
        ]
    }

    fn chip<'a>(story: &WidgetStory, labels: &'a [String; 3], at: usize) -> Chip<'a> {
        let chip = Chip::new(&labels[at], story.held.chosen == Some(at));
        if story.flag(Control::Leading) {
            chip.leading(Self::MARK)
        } else {
            chip
        }
    }

    /// The chips in a wrapping row from (`x`, `y`) to `right`: the
    /// arithmetic of `chip::wrap`, with each chip's leading slot counted.
    fn rects(
        story: &WidgetStory,
        engine: &mut TextEngine,
        x: f32,
        y: f32,
        right: f32,
    ) -> Vec<Rect> {
        let labels = Self::labels(story);
        let (mut cx, mut cy) = (x, y);
        let mut out = Vec::with_capacity(labels.len());
        for at in 0..labels.len() {
            let w = Self::chip(story, &labels, at)
                .width(engine)
                .max(chip::MIN_W);
            if at > 0 && cx + w > right {
                cx = x;
                cy += Chip::H + chip::GAP;
            }
            out.push(Rect::new(cx, cy, w, Chip::H));
            cx += w + chip::GAP;
        }
        out
    }
}

impl Widget for ChipStory {
    fn id(&self) -> &'static str {
        registry::CHIP_STORY
    }

    fn sample(&self, control: Control) -> Option<Key> {
        (control == Control::Label).then_some(DENSITIES[0])
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, story: &WidgetStory, engine: &mut TextEngine, room: f32) -> (f32, f32) {
        let rects = Self::rects(story, engine, 0.0, 0.0, room);
        let right = rects.iter().map(|r| r.x + r.w).fold(0.0, f32::max);
        (right.min(room), chip::bottom(&rects, 0.0))
    }

    fn parts(&self, story: &WidgetStory, engine: &mut TextEngine, rect: Rect) -> Vec<Part> {
        const PARTS: [&str; 3] = ["first", "second", "third"];
        let labels = Self::labels(story);
        Self::rects(story, engine, rect.x, rect.y, rect.x + rect.w)
            .into_iter()
            .zip(labels)
            .enumerate()
            .map(|(at, (rect, label))| {
                Part::button(PARTS[at], rect, label)
                    .role(PartRole::Radio(story.held.chosen == Some(at)))
            })
            .collect()
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let labels = Self::labels(story);
        let rects = Self::rects(story, canvas.engine, rect.x, rect.y, rect.x + rect.w);
        for (at, rect) in rects.into_iter().enumerate() {
            let chip = Self::chip(story, &labels, at);
            chip.draw(
                canvas.scene,
                canvas.root,
                canvas.engine,
                rect,
                pointed.hovered == Some(at),
                pointed.focused == Some(at),
            );
            if story.flag(Control::Leading) {
                // The mark is the caller's to draw in the slot the chip
                // reserved; here a plain glyph stands for it.
                let slot = chip.leading_rect(rect);
                let mark = canvas.engine.icon_layout(icons::STAR, Self::MARK - 2.0);
                scene::draw_layout(
                    canvas.scene,
                    canvas.root,
                    &mark,
                    slot.x,
                    slot.y + (slot.h - mark.height()) * 0.5,
                    Some(theme::p().text_dim),
                );
            }
        }
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        let changed = story.held.chosen != Some(part);
        story.held.chosen = Some(part);
        changed
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        let labels = Self::labels(story);
        story.held.chosen.map(|at| chosen_line(&labels[at]))
    }
}

// -------------------------------------------------------------- segmented

/// `koru_ui::segmented::Segmented`: one track, three words, one chosen.
pub(super) struct SegmentedStory;

impl SegmentedStory {
    fn labels() -> [&'static str; 3] {
        DENSITIES.map(text)
    }
}

impl Widget for SegmentedStory {
    fn id(&self) -> &'static str {
        registry::SEGMENTED_STORY
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, _: &WidgetStory, engine: &mut TextEngine, _: f32) -> (f32, f32) {
        (Segmented::width(engine, &Self::labels()), Segmented::H)
    }

    fn parts(&self, story: &WidgetStory, engine: &mut TextEngine, rect: Rect) -> Vec<Part> {
        const PARTS: [&str; 3] = ["first", "second", "third"];
        let labels = Self::labels();
        Segmented::layout(engine, &labels, rect.x, rect.y)
            .segments
            .into_iter()
            .enumerate()
            .map(|(at, rect)| {
                Part::button(PARTS[at], rect, labels[at])
                    .role(PartRole::Radio(story.held.chosen == Some(at)))
            })
            .collect()
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let labels = Self::labels();
        Segmented::layout(canvas.engine, &labels, rect.x, rect.y).draw_focused(
            canvas.scene,
            canvas.root,
            canvas.engine,
            &labels,
            story.held.chosen,
            pointed.hovered,
            pointed.focused.is_some(),
            1.0,
        );
    }

    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        let changed = story.held.chosen != Some(part);
        story.held.chosen = Some(part);
        changed
    }

    /// The arrows choose: the control is one stop, and its ring is the
    /// track's.
    fn arrow(&self, story: &mut WidgetStory, parts: &[Part], forward: bool) -> bool {
        let at = story.held.chosen.unwrap_or(0);
        let next = if forward {
            (at + 1).min(parts.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        story.set_cursor(next);
        self.press(story, next)
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        story.held.chosen.map(|at| chosen_line(Self::labels()[at]))
    }
}

// --------------------------------------------------------------- dropdown

/// `koru_ui::dropdown::Dropdown`: the closed field, and the list it opens
/// (`Dropdown::menu`, the shared menu). Its values are the eight languages,
/// each in itself.
pub(super) struct DropdownStory;

impl DropdownStory {
    fn value(story: &WidgetStory) -> &'static str {
        koru_i18n::Lang::ALL[story.held.chosen.unwrap_or(0)].endonym()
    }
}

impl Widget for DropdownStory {
    fn id(&self) -> &'static str {
        registry::DROPDOWN_STORY
    }

    fn held(&self) -> Held {
        first_chosen()
    }

    fn size(&self, _: &WidgetStory, engine: &mut TextEngine, _: f32) -> (f32, f32) {
        let options: Vec<DropdownOption> = koru_i18n::Lang::ALL
            .into_iter()
            .map(|lang| DropdownOption::new(lang.endonym(), false))
            .collect();
        (Dropdown::width_for(engine, &options), Dropdown::H)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        vec![
            Part::button("field", rect, text(Key::UiLabDropdownName))
                .role(PartRole::Combo)
                .value(Self::value(story))
                .disabled(story.flag(Control::Disabled)),
        ]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        Dropdown::new(Self::value(story))
            .open(pointed.open)
            .enabled(!story.flag(Control::Disabled))
            .draw_focused(
                canvas.scene,
                canvas.root,
                canvas.engine,
                rect,
                pointed.hovered.is_some(),
                pointed.focused.is_some(),
                1.0,
            );
    }

    fn press(&self, _: &mut WidgetStory, _: usize) -> bool {
        // A press opens the list; choosing is `pick`.
        false
    }

    fn list(&self, story: &WidgetStory, _: usize) -> Option<Vec<StoryOption>> {
        Some(
            koru_i18n::Lang::ALL
                .into_iter()
                .enumerate()
                .map(|(at, lang)| StoryOption {
                    part: lang.tag(),
                    label: OptionLabel::Literal(lang.endonym().to_owned()),
                    selected: story.held.chosen == Some(at),
                })
                .collect(),
        )
    }

    fn pick(&self, story: &mut WidgetStory, slot: usize) -> bool {
        let changed = story.held.chosen != Some(slot);
        story.held.chosen = Some(slot);
        changed
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(chosen_line(Self::value(story)))
    }
}
