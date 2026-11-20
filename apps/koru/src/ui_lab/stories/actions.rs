// SPDX-License-Identifier: MPL-2.0

//! **The shared action widgets on the stage**: the icon button, the button
//! group, the split button and the copy affordance.
//!
//! ```text
//! Component under test
//!   button                            icon_button::draw_focused: one, pressed and counted
//!   minimize · maximize · close       ButtonGroup: the caption's cluster, each an IconButton
//!   action · menu                     SplitButton: two targets in one pill
//!   copy                              CopyAffordance: the icon that confirms
//! ```
//!
//! **Nothing here reaches outside the Lab.** The copy affordance's own
//! `click` writes the system clipboard, so the story never calls it: a
//! press runs `confirm`, the half of `click` that shows the check, and the
//! text that would have been copied is kept in the story and shown under
//! the icon.

use std::time::Instant;

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, theme};
use koru_i18n::Key;
use koru_ui::CopyAffordance;
use koru_ui::button_group::{self, ButtonGroup};
use koru_ui::icon_button::{self, IconButton, Metrics, State, Tone};
use koru_ui::split_button::{SplitButton, SplitTone};

use super::{Canvas, Part, PartRole, Pointed, Widget, presses_line, text};
use crate::ui_lab::registry::{self, Control};
use crate::ui_lab::story::WidgetStory;

// ------------------------------------------------------------ icon button

/// `koru_ui::icon_button::draw_focused`: the entry point that takes every
/// input the icon button has. The older ones (`draw`, `draw_toned`,
/// `draw_with`, `draw_with_in`, `IconButton::draw`) call the same painter
/// with fewer of them.
pub(super) struct IconButtonStory;

impl IconButtonStory {
    /// `Tone`'s members, in the order the registration lists them.
    const TONES: [Tone; 3] = [Tone::Default, Tone::Danger, Tone::Quiet];
    /// `Metrics`' constants, in the order the registration lists them.
    const METRICS: [Metrics; 3] = [Metrics::STANDARD, Metrics::INLINE, Metrics::COMPACT];
    /// A page's or a row's own button: the slot an inline button is given.
    const INLINE: f32 = 28.0;

    fn metrics(story: &WidgetStory) -> Metrics {
        Self::METRICS[story.choice(Control::Size).min(Self::METRICS.len() - 1)]
    }
}

impl Widget for IconButtonStory {
    fn id(&self) -> &'static str {
        registry::ICON_BUTTON_STORY
    }

    /// The slot each metrics is made for: a caption button's, an inline
    /// square, or the compact control's own measured size.
    fn size(&self, story: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        match story.choice(Control::Size) {
            0 => (theme::CAPTION_BTN, theme::CAPTION_H),
            1 => (Self::INLINE, Self::INLINE),
            _ => (
                Metrics::COMPACT.control_width(),
                Metrics::COMPACT.control_height(),
            ),
        }
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let button = Part::button("button", rect, text(Key::UiLabOptionPin))
            .disabled(story.flag(Control::Disabled));
        vec![if story.flag(Control::Selected) {
            button.role(PartRole::Pressed(true))
        } else {
            button
        }]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        icon_button::draw_focused(
            canvas.scene,
            canvas.root,
            canvas.engine,
            rect,
            icons::PUSH_PIN,
            State {
                hovered: pointed.hovered.is_some(),
                active: story.flag(Control::Selected),
                disabled: story.flag(Control::Disabled),
            },
            Self::metrics(story),
            Self::TONES[story.choice(Control::Tone).min(Self::TONES.len() - 1)],
            pointed.focused.is_some(),
        );
    }

    fn press(&self, story: &mut WidgetStory, _: usize) -> bool {
        story.held.presses[0] = story.held.presses[0].saturating_add(1);
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(presses_line(story.held.presses[0]))
    }
}

// ----------------------------------------------------------- button group

/// `koru_ui::button_group::ButtonGroup`: the cluster the window caption's
/// three controls are, laid out by `slots_right`, each an `IconButton`
/// with a typed click result.
pub(super) struct ButtonGroupStory;

impl ButtonGroupStory {
    /// Each button: its part, its glyph, what it is called and its tone.
    const BUTTONS: [(&'static str, char, Key, Tone); 3] = [
        (
            "minimize",
            icons::WINDOW_MINIMISE,
            Key::WindowMinimize,
            Tone::Default,
        ),
        (
            "maximize",
            icons::SQUARE,
            Key::WindowMaximize,
            Tone::Default,
        ),
        (
            "close",
            icons::WINDOW_CLOSE,
            Key::WindowCloseWindow,
            Tone::Danger,
        ),
    ];

    /// The group as a caller builds it: fully formed buttons, each with the
    /// slot `slots_right` gave it and its index as its action.
    fn group(story: &WidgetStory, rect: Rect, hovered: Option<usize>) -> ButtonGroup<usize> {
        let slots = button_group::slots_right(rect, theme::CAPTION_BTN, Self::BUTTONS.len());
        ButtonGroup::new(
            slots
                .into_iter()
                .zip(Self::BUTTONS)
                .enumerate()
                .map(|(at, (slot, (_, glyph, _, tone)))| {
                    IconButton::new(glyph, slot)
                        .on_click(at)
                        .tone(tone)
                        .with_state(State {
                            hovered: hovered == Some(at),
                            active: false,
                            disabled: story.flag(Control::Disabled),
                        })
                })
                .collect::<Vec<_>>(),
        )
    }
}

impl Widget for ButtonGroupStory {
    fn id(&self) -> &'static str {
        registry::BUTTON_GROUP_STORY
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        (
            theme::CAPTION_BTN * Self::BUTTONS.len() as f32,
            theme::CAPTION_H,
        )
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let group = Self::group(story, rect, None);
        group
            .buttons()
            .iter()
            .zip(Self::BUTTONS)
            .map(|(button, (part, _, name, _))| {
                // `IconButton::draw` takes no focus: the window rings the
                // button the keyboard is on.
                Part::button(part, button.rect(), text(name))
                    .disabled(story.flag(Control::Disabled))
                    .ringed()
            })
            .collect()
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        Self::group(story, rect, pointed.hovered).draw(canvas.scene, canvas.root, canvas.engine);
    }

    /// The press is answered by the group's own typed result for the
    /// button's slot, as a host answers a click.
    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        let rect = Rect::new(
            0.0,
            0.0,
            theme::CAPTION_BTN * Self::BUTTONS.len() as f32,
            theme::CAPTION_H,
        );
        let group = Self::group(story, rect, None);
        let Some((x, y)) = group
            .buttons()
            .get(part)
            .map(|button| button.rect().centre())
        else {
            return false;
        };
        let Some(action) = group.action_at(x, y) else {
            return false;
        };
        story.held.chosen = Some(action);
        story.held.presses[0] = story.held.presses[0].saturating_add(1);
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(match story.held.chosen {
            Some(at) => koru_i18n::fill(
                text(Key::UiLabResultLastPressed),
                &[text(Self::BUTTONS[at.min(Self::BUTTONS.len() - 1)].2)],
            ),
            None => presses_line(0),
        })
    }
}

// ----------------------------------------------------------- split button

/// `koru_ui::split_button::SplitButton`: an action and, beside it in the
/// same pill, an independently pressed menu target.
pub(super) struct SplitButtonStory;

impl SplitButtonStory {
    /// `SplitTone`'s members, in the order the registration lists them.
    const TONES: [SplitTone; 4] = [
        SplitTone::Accent,
        SplitTone::Success,
        SplitTone::Neutral,
        SplitTone::Warning,
    ];

    /// The button as a caller states it. The reveal of the menu's lane and
    /// the chevron's turn are animated by the caller; the story shows each
    /// at rest or at its end, without the motion between.
    fn button(story: &WidgetStory, rect: Rect, pointed: Pointed) -> SplitButton {
        let open = story.held.on;
        let shown = open || pointed.hovered.is_some() || pointed.focused.is_some();
        SplitButton {
            rect,
            tone: Self::TONES[story.choice(Control::Tone).min(Self::TONES.len() - 1)],
            action_enabled: !story.flag(Control::Disabled),
            action_hot: pointed.hovered == Some(0),
            action_focused: pointed.focused == Some(0),
            menu_hot: pointed.hovered == Some(1),
            menu_focused: pointed.focused == Some(1),
            menu_open: open,
            reveal: if shown { 1.0 } else { 0.0 },
            chevron_turn: if open { 1.0 } else { 0.0 },
        }
    }
}

impl Widget for SplitButtonStory {
    fn id(&self) -> &'static str {
        registry::SPLIT_BUTTON_STORY
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        (SplitButton::W, SplitButton::H)
    }

    fn parts(&self, story: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        let button = Self::button(story, rect, Pointed::default());
        vec![
            // The menu keeps its own target while the action is off.
            Part::button("action", button.action_rect(), text(Key::UiLabPartAction))
                .disabled(story.flag(Control::Disabled)),
            Part::button("menu", button.menu_rect(), text(Key::UiLabMenuName))
                .role(PartRole::Pressed(story.held.on)),
        ]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        Self::button(story, rect, pointed).draw(
            canvas.scene,
            canvas.root,
            canvas.engine,
            icons::ARROW_UP,
            icons::SPARKLE,
        );
    }

    /// The action is counted; the menu target opens and closes. The menu
    /// itself is the caller's and is not part of this component.
    fn press(&self, story: &mut WidgetStory, part: usize) -> bool {
        let at = part.min(1);
        story.held.presses[at] = story.held.presses[at].saturating_add(1);
        if at == 1 {
            story.held.on = !story.held.on;
        }
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        let lang = koru_i18n::current();
        let [action, menu] = story
            .held
            .presses
            .map(|count| koru_i18n::number::decimal(lang, count));
        Some(koru_i18n::fill(
            text(Key::UiLabResultSplit),
            &[&action, &menu],
        ))
    }
}

// ------------------------------------------------------------------- copy

/// `koru_ui::copy_affordance::CopyAffordance`: the copy icon that turns to
/// a check. See the module note for what a press does here.
pub(super) struct CopyStory;

impl CopyStory {
    /// The square the icon is given beside what it copies.
    const SLOT: f32 = 20.0;
}

impl Widget for CopyStory {
    fn id(&self) -> &'static str {
        registry::COPY_STORY
    }

    fn sample(&self, control: Control) -> Option<Key> {
        (control == Control::Text).then_some(Key::UiLabTextSample)
    }

    fn size(&self, _: &WidgetStory, _: &mut TextEngine, _: f32) -> (f32, f32) {
        (Self::SLOT, Self::SLOT)
    }

    fn parts(&self, _: &WidgetStory, _: &mut TextEngine, rect: Rect) -> Vec<Part> {
        // The affordance draws no keyboard-focus state.
        vec![Part::button("copy", rect, text(Key::MenuCopy)).ringed()]
    }

    fn paint(&self, story: &WidgetStory, canvas: &mut Canvas<'_>, rect: Rect, pointed: Pointed) {
        let mut affordance = CopyAffordance::default();
        if story.held.on {
            // The confirmation, as `click` shows it. In the app the check
            // holds for a moment and turns back on the host's clock; the
            // Lab runs no clock, so here it holds until Reset.
            affordance.confirm(Instant::now());
        }
        affordance.draw_icon(
            canvas.scene,
            canvas.root,
            canvas.engine,
            rect,
            pointed.hovered.is_some(),
        );
    }

    fn press(&self, story: &mut WidgetStory, _: usize) -> bool {
        // Never `CopyAffordance::click`: it writes the system clipboard.
        story.held.note = Some(story.text(Control::Text).to_owned());
        story.held.on = true;
        story.held.presses[0] = story.held.presses[0].saturating_add(1);
        true
    }

    fn result(&self, story: &WidgetStory) -> Option<String> {
        Some(match &story.held.note {
            Some(copied) => koru_i18n::fill(text(Key::UiLabResultCopied), &[copied]),
            None => text(Key::UiLabResultNotCopied).to_owned(),
        })
    }
}
