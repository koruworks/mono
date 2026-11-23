// SPDX-License-Identifier: MPL-2.0

//! **The preview's settings**: the theme, how wide the preview is, and the
//! display language, each a dropdown in the toolbar; and the one list every
//! dropdown in the window opens.
//!
//! ```text
//! Setting field                       the shared dropdown, showing the value in force
//! Menu                                the shared menu, over the window, while one is open
//! ```
//!
//! **All three belong to this process and to nothing else.** The theme is the
//! drawing crate's root theme, the language is the translation crate's
//! display language, and the width is a number the page lays out against.
//! Each is a value in this process's memory: nothing is read from a
//! preference and nothing is written to one, so a Koru that is running beside
//! the Lab is not touched, and the next Lab opens at the defaults again.
//!
//! **A change of theme redraws everything from nothing.** A shaped layout
//! carries the brush it was shaped with and a field's editor carries its
//! ink, so [`Lab::set_theme`] drops the layout cache and builds every field
//! again with the text it held; the caption, the navigator and the open
//! story are all laid out from the tree on the next frame as they always
//! are. A change of language reads the gallery again, because a record is
//! found by its name in that language.
//!
//! **The list is the shared `Menu`**, opened by the shared `Dropdown`: the
//! arrows step it, Enter or Space chooses, Escape or a press outside closes
//! it, and the keyboard never leaves the field it hangs from.

use koru_draw::theme::themes::{self, Theme};
use koru_i18n::Lang;
use koru_ui::{Dropdown, DropdownOption, Menu};

use super::*;

/// The surface a setting's field is identified under.
const SETTING: &str = "ui-lab.setting";
/// The surface an open list's options are identified under; an option's
/// part is the keyword of what it chooses.
pub(super) const OPTION: &str = "ui-lab.option";
/// The open list itself.
pub(super) const MENU: Key = Key::surface("ui-lab", "menu");

/// One setting of the preview.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Setting {
    Theme,
    Width,
    Language,
}

impl Setting {
    /// In toolbar order.
    pub(super) const ALL: [Self; 3] = [Self::Theme, Self::Width, Self::Language];

    pub(super) const fn key(self) -> Key {
        Key::surface(
            SETTING,
            match self {
                Self::Theme => "theme",
                Self::Width => "width",
                Self::Language => "language",
            },
        )
    }

    /// What the setting is called.
    pub(super) const fn name(self) -> koru_i18n::Key {
        match self {
            Self::Theme => koru_i18n::Key::UiLabTheme,
            Self::Width => koru_i18n::Key::UiLabPreviewWidth,
            Self::Language => koru_i18n::Key::UiLabLanguage,
        }
    }

    pub(super) fn of(key: Key) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.key() == key)
    }
}

/// How wide the preview is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PreviewWidth {
    /// As wide as its column: what the window leaves it.
    Fit,
    /// This many logical pixels, or the column when that is narrower.
    Fixed(u16),
}

impl PreviewWidth {
    /// The widths on offer: the column, then a phone, a narrow pane, a
    /// document column and a wide one.
    pub(super) const ALL: [Self; 5] = [
        Self::Fit,
        Self::Fixed(320),
        Self::Fixed(480),
        Self::Fixed(640),
        Self::Fixed(800),
    ];

    const fn part(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Fixed(320) => "320",
            Self::Fixed(480) => "480",
            Self::Fixed(640) => "640",
            Self::Fixed(_) => "800",
        }
    }

    fn label(self) -> String {
        let lang = koru_i18n::current();
        match self {
            Self::Fit => text(koru_i18n::Key::UiLabWidthFit).to_owned(),
            Self::Fixed(pixels) => koru_i18n::fill(
                text(koru_i18n::Key::UiLabWidthPixels),
                &[&koru_i18n::number::decimal(lang, u64::from(pixels))],
            ),
        }
    }

    /// The width a preview gets in a column `room` wide.
    pub(super) fn within(self, room: f32) -> f32 {
        match self {
            Self::Fit => room,
            Self::Fixed(pixels) => f32::from(pixels).min(room),
        }
    }
}

/// What a theme is called: the two built-in ones by name, any other by its
/// own key, which is a technical identifier.
fn theme_label(theme: &'static Theme) -> String {
    if std::ptr::eq(theme, &themes::LIGHT_THEME) {
        text(koru_i18n::Key::UiLabThemeLight).to_owned()
    } else if std::ptr::eq(theme, &themes::DARK_THEME) {
        text(koru_i18n::Key::UiLabThemeDark).to_owned()
    } else {
        theme.key.to_owned()
    }
}

/// One option of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    /// The keyword of what it chooses: the same in every language.
    pub(super) part: &'static str,
    /// What a person reads.
    pub(super) label: String,
    /// Whether it is the one in force.
    pub(super) selected: bool,
}

/// A list that is open, and the field it hangs from.
pub(super) struct OpenMenu {
    pub(super) owner: Key,
    pub(super) menu: Menu<DropdownOption>,
    /// The options it opened with, in the menu's order.
    pub(super) choices: Vec<Choice>,
}

/// Make `lang` the display language of everything else in this process: the
/// inspector's panel and the window's own commands read it where the Lab's
/// window does not hand it to them. A test changes no process-wide state,
/// so there the Lab's own language ([`Lab::in_language`]) is all there is.
fn publish_language(lang: Lang) {
    #[cfg(not(test))]
    koru_i18n::set_current(lang);
    #[cfg(test)]
    let _ = lang;
}

impl Lab {
    /// Run `body` with the Lab's display language as the language every
    /// string is read in.
    pub(super) fn in_language<T>(lang: Lang, body: impl FnOnce() -> T) -> T {
        koru_i18n::with_current(lang, body)
    }

    /// The options `owner`'s list offers, the one in force marked.
    pub(super) fn choices(&self, owner: Key) -> Vec<Choice> {
        match Setting::of(owner) {
            Some(Setting::Theme) => themes::registered()
                .iter()
                .map(|theme| Choice {
                    part: theme.key,
                    label: theme_label(theme),
                    selected: std::ptr::eq(*theme, themes::active()),
                })
                .collect(),
            Some(Setting::Width) => PreviewWidth::ALL
                .into_iter()
                .map(|width| Choice {
                    part: width.part(),
                    label: width.label(),
                    selected: width == self.width,
                })
                .collect(),
            // Each language in itself, so the list can be read by someone
            // who reads only the language they are looking for.
            Some(Setting::Language) => Lang::ALL
                .into_iter()
                .map(|lang| Choice {
                    part: lang.tag(),
                    label: lang.endonym().to_owned(),
                    selected: lang == self.lang,
                })
                .collect(),
            // The list the component under test opened: its story's.
            None if owner == SUBJECT => self
                .story
                .as_ref()
                .and_then(Story::as_widget)
                .and_then(|story| story.widget().list(story, story.cursor()))
                .unwrap_or_default()
                .into_iter()
                .map(|option| Choice {
                    part: option.part,
                    label: preview::option_text(&option.label),
                    selected: option.selected,
                })
                .collect(),
            // A choice of the story's: its options are the story's.
            None => match (self.control_of(owner), &self.story) {
                (Some(control), Some(story)) => story
                    .options(control)
                    .into_iter()
                    .map(|option| Choice {
                        part: option.part,
                        label: preview::option_text(&option.label),
                        selected: option.selected,
                    })
                    .collect(),
                _ => Vec::new(),
            },
        }
    }

    /// The option of `owner` that is in force.
    pub(super) fn chosen(&self, owner: Key) -> Option<Choice> {
        self.choices(owner)
            .into_iter()
            .find(|choice| choice.selected)
    }

    /// Put the option at `slot` of `owner` in force. Returns whether that
    /// changed anything.
    pub(super) fn choose(&mut self, engine: &mut TextEngine, owner: Key, slot: usize) -> bool {
        match Setting::of(owner) {
            Some(Setting::Theme) => themes::registered()
                .get(slot)
                .is_some_and(|theme| self.set_theme(engine, theme)),
            Some(Setting::Width) => PreviewWidth::ALL
                .get(slot)
                .is_some_and(|width| self.set_width(*width)),
            Some(Setting::Language) => Lang::ALL
                .get(slot)
                .is_some_and(|lang| self.set_language(engine, *lang)),
            None if owner == SUBJECT => self
                .story
                .as_mut()
                .and_then(Story::as_widget_mut)
                .is_some_and(|story| story.widget().pick(story, slot)),
            None => match (self.control_of(owner), self.story.as_mut()) {
                (Some(control), Some(story)) => story.choose(control, slot),
                _ => false,
            },
        }
    }

    /// Draw the Lab in `theme` from the next frame on. Everything that
    /// baked the old theme's ink is thrown away: the shaped layouts, and
    /// each field, which is built again holding what it held.
    pub(super) fn set_theme(&mut self, engine: &mut TextEngine, theme: &'static Theme) -> bool {
        if std::ptr::eq(theme, themes::active()) {
            return false;
        }
        themes::set_active(theme);
        self.cache.clear();
        for (_, field) in &mut self.fields {
            let held = field.raw_text().to_owned();
            *field = TextInput::with_text(engine, theme::role::row(), &held);
        }
        self.appearance_changed = true;
        true
    }

    pub(super) fn set_width(&mut self, width: PreviewWidth) -> bool {
        if self.width == width {
            return false;
        }
        self.width = width;
        true
    }

    /// Show the Lab in `lang` from the next frame on. The gallery is read
    /// again, because a search finds a record by its name in the display
    /// language; a story nobody has changed starts again, so its sample
    /// content is in that language too. One that was changed keeps what it
    /// was given.
    pub(super) fn set_language(&mut self, engine: &mut TextEngine, lang: Lang) -> bool {
        if self.lang == lang {
            return false;
        }
        self.lang = lang;
        publish_language(lang);
        Self::in_language(lang, || {
            self.nav.rebuild();
            if self.story.as_ref().is_some_and(Story::is_pristine) {
                self.story = Self::story_for(self.entry);
                self.restore_fields(engine);
            }
        });
        self.bring_shown_into_view = true;
        self.appearance_changed = true;
        true
    }

    /// Whether the theme or the language changed since this was last asked:
    /// the other window of the process then shows something out of date.
    pub(in crate::ui_lab) fn take_appearance_changed(&mut self) -> bool {
        std::mem::take(&mut self.appearance_changed)
    }

    /// Open `owner`'s list under its field, the option in force lit.
    pub(super) fn open_menu(&mut self, engine: &mut TextEngine, owner: Key) -> bool {
        // The component under test hangs its list from the part that was
        // pressed for it.
        let part = self
            .story
            .as_ref()
            .and_then(Story::as_widget)
            .filter(|_| owner == SUBJECT)
            .and_then(|story| self.laid.as_ref()?.parts.get(story.cursor()))
            .map(|part| part.rect);
        let Some(field) = part.or_else(|| self.rect(owner)) else {
            return false;
        };
        let choices = self.choices(owner);
        if choices.is_empty() {
            return false;
        }
        let options = choices
            .iter()
            .map(|choice| DropdownOption::new(choice.label.clone(), choice.selected))
            .collect();
        self.menu = Some(OpenMenu {
            owner,
            menu: Dropdown::menu(engine, field, options),
            choices,
        });
        true
    }

    pub(super) fn close_menu(&mut self) -> bool {
        self.menu.take().is_some()
    }

    /// The open list's viewport: the window the last frame was laid out for.
    fn menu_viewport(&self) -> (f32, f32) {
        self.laid.as_ref().map_or((0.0, 0.0), |laid| laid.size)
    }

    /// Choose the open list's option at `slot` and close it.
    pub(super) fn choose_from_menu(&mut self, engine: &mut TextEngine, slot: usize) -> bool {
        let Some(open) = self.menu.take() else {
            return false;
        };
        self.choose(engine, open.owner, slot);
        true
    }

    /// A press while a list is open: on an option it chooses it, anywhere
    /// else it closes the list and is not a press on what was under it.
    pub(super) fn menu_press(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        let viewport = self.menu_viewport();
        let Some(open) = self.menu.as_ref() else {
            return false;
        };
        match open.menu.item_at(viewport, x, y) {
            Some(slot) => self.choose_from_menu(engine, slot),
            // The list's own padding is not a way out of it.
            None if open.menu.panel(viewport).contains(x, y) => false,
            None => self.close_menu(),
        }
    }

    /// The pointer over an open list lights the option under it.
    pub(super) fn menu_hover(&mut self, x: f32, y: f32) -> bool {
        let viewport = self.menu_viewport();
        let Some(open) = self.menu.as_mut() else {
            return false;
        };
        let Some(slot) = open.menu.item_at(viewport, x, y) else {
            return false;
        };
        let changed = open.menu.hover != Some(slot);
        open.menu.hover = Some(slot);
        changed
    }

    /// A key while a list is open. Every key is the list's: the keyboard
    /// does not reach the window under it.
    pub(super) fn menu_key(
        &mut self,
        engine: &mut TextEngine,
        key: &KeyboardKey,
        repeat: bool,
    ) -> bool {
        let Some(open) = self.menu.as_mut() else {
            return false;
        };
        let enabled = vec![true; open.choices.len()];
        let before = open.menu.hover;
        match key {
            KeyboardKey::Named(NamedKey::ArrowDown) => open.menu.step(&enabled, 1),
            KeyboardKey::Named(NamedKey::ArrowUp) => open.menu.step(&enabled, -1),
            KeyboardKey::Named(NamedKey::Home) => open.menu.hover = Some(0),
            KeyboardKey::Named(NamedKey::End) => {
                open.menu.hover = open.choices.len().checked_sub(1);
            }
            KeyboardKey::Named(NamedKey::Enter | NamedKey::Space) if !repeat => {
                return match before {
                    Some(slot) => self.choose_from_menu(engine, slot),
                    None => self.close_menu(),
                };
            }
            KeyboardKey::Named(NamedKey::Escape | NamedKey::Tab) => return self.close_menu(),
            _ => return false,
        }
        self.menu
            .as_ref()
            .is_some_and(|open| open.menu.hover != before)
    }

    /// How wide `owner`'s field is at its fullest: as wide as its widest
    /// option, so it does not change size with its value.
    pub(super) fn dropdown_width(&self, engine: &mut TextEngine, owner: Key) -> f32 {
        let options: Vec<DropdownOption> = self
            .choices(owner)
            .into_iter()
            .map(|choice| DropdownOption::new(choice.label, choice.selected))
            .collect();
        Dropdown::width_for(engine, &options)
    }

    /// A dropdown's field in the tree: a slot at its full width. The shared
    /// dropdown drawn in it rings itself under the keyboard.
    pub(super) fn dropdown(&self, engine: &mut TextEngine, owner: Key, name: koru_i18n::Key) -> El {
        el::leaf(
            "Setting field",
            Slot {
                width: self.dropdown_width(engine, owner),
                height: Dropdown::H,
            },
        )
        .semantic()
        .id(owner)
        .hit()
        .focusable()
        // Published as a combo box with its value.
        .semantics(Role::Button)
        .name(name)
        .action(el::Action::Click)
        .action(el::Action::Focus)
    }

    /// The three settings, as the toolbar holds them. `shared` is a row the
    /// three have to themselves and may be too narrow for: each then takes an
    /// equal share of it, up to its full width and no further, and the shared
    /// dropdown clips its value before its chevron.
    pub(super) fn settings(&self, engine: &mut TextEngine, shared: bool) -> Vec<El> {
        Setting::ALL
            .into_iter()
            .map(|setting| {
                let field = self.dropdown(engine, setting.key(), setting.name());
                if shared {
                    let full = self.dropdown_width(engine, setting.key());
                    field
                        .fill()
                        .max_w(Length::Runtime(full, "the setting's widest option"))
                } else {
                    field
                }
            })
            .collect()
    }

    /// Paint `owner`'s field: the shared dropdown, showing the value in
    /// force, whether its list is open and whether the keyboard is on it.
    pub(super) fn draw_dropdown(
        value: &str,
        open: bool,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        state: el::ElState,
    ) {
        Dropdown::new(value).open(open).draw_focused(
            scene,
            root,
            engine,
            rect,
            state.hovered,
            state.focused,
            1.0,
        );
    }

    /// Whether `key` is a dropdown's field: a setting's, or a choice of the
    /// story's with too many options to stand side by side.
    pub(super) fn is_dropdown(&self, key: Key) -> bool {
        Setting::of(key).is_some() || self.is_story_dropdown(key)
    }
}

#[cfg(test)]
mod tests;
