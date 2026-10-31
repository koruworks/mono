// SPDX-License-Identifier: MPL-2.0

//! **One page at a time, laid out once and walked twice.**
//!
//! Settings is a sidebar of pages, and a page is a title, a sentence, and a
//! stack of cards of rows. This module is the builder a page's content pushes
//! onto and the record it produces: every card, row and control is measured
//! here into [`Block`]s and a flat list of [`Hit`]s, and both the draw pass and
//! every hit test walk those lists — the rule the surface followed before, *one
//! function for drawing and hit-testing*, kept.
//!
//! The controls are the shared `koru-ui` components — [`Segmented`],
//! [`Dropdown`], [`Toggle`], [`Swatch`], [`Stepper`], [`PreviewTile`] — so a row
//! here is the same control, with the same inspector identity, as anywhere
//! else in Koru. What the page decides is only which control a setting gets:
//! a handful of short words is a segmented track; anything longer, or longer
//! than the row can hold, is a dropdown.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_i18n::{Key, Lang};
use koru_ui::{
    ActionButton, Card, CardRow, Dropdown, DropdownOption, PageShell, PreviewTile, Segmented,
    Stepper, Toggle,
};
use vello::peniko::Color;

use crate::action::Action;

/// The pages, in sidebar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageId {
    Appearance,
    Editor,
    Reminders,
    Terminal,
    System,
}

impl PageId {
    pub const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Editor,
        Self::Reminders,
        Self::Terminal,
        Self::System,
    ];

    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Appearance => koru_i18n::t(lang, Key::SettingsAppearance),
            Self::Editor => koru_i18n::t(lang, Key::SettingsEditor),
            Self::Reminders => koru_i18n::t(lang, Key::ReminderTitle),
            Self::Terminal => koru_i18n::t(lang, Key::SettingsTerminal),
            Self::System => koru_i18n::t(lang, Key::SettingsSystem),
        }
    }

    pub(crate) fn description(self, lang: Lang) -> &'static str {
        match self {
            Self::Appearance => koru_i18n::t(lang, Key::SettingsAppearanceDescription),
            Self::Editor => koru_i18n::t(lang, Key::SettingsEditorDescription),
            Self::Reminders => koru_i18n::t(lang, Key::SettingsRemindersDescription),
            Self::Terminal => koru_i18n::t(lang, Key::SettingsTerminalDescription),
            Self::System => koru_i18n::t(lang, Key::SettingsSystemDescription),
        }
    }

    /// What a search in the sidebar matches besides the name — the settings
    /// the page holds, in the words a person would type for them.
    pub(crate) fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "theme dark light system font fonts typeface density pinned tabs caret color cursor typography"
            }
            Self::Editor => {
                "page width layout background canvas spelling spell check language dictionary voice dictation microphone speech model quality shortcut consent"
            }
            Self::Reminders => {
                "reminder notification notifications sound tone week day end of day deliver test"
            }
            Self::Terminal => "shell cursor zoom font theme input blink composer",
            Self::System => {
                "login startup start quit background close window app menu launcher dock desktop entry renderer graphics vulkan directx opengl ai provider model privacy telemetry usage data analytics diagnostics"
            }
        }
    }

    fn localized_keywords(self, lang: Lang) -> String {
        let keys: &[Key] = match self {
            Self::Appearance => &[
                Key::SettingsTheme,
                Key::SettingsDensity,
                Key::SettingsPinnedTabs,
                Key::SettingsAppFont,
                Key::SettingsDocumentFonts,
                Key::SettingsCaret,
                Key::TerminalCursorColor,
                Key::SettingsShape,
                Key::SettingsLight,
                Key::SettingsDark,
            ],
            Self::Editor => &[
                Key::DocumentsHeading,
                Key::SettingsPageWidthForNewDocuments,
                Key::SettingsPageBackground,
                Key::SettingsDictionary,
                Key::SettingsVoiceInput,
                Key::Language,
                Key::SettingsMicrophone,
                Key::SettingsSpeechModel,
                Key::SettingsQuality,
                Key::VoiceGlobalShortcut,
                Key::SettingsConsent,
                Key::SpellingSectionTitle,
                Key::SpellingCheckWhileTyping,
                Key::SpellingPersonalDictionary,
            ],
            Self::Reminders => &[
                Key::ReminderTitle,
                Key::SettingsSchedule,
                Key::SettingsNotifications,
                Key::SettingsNotificationTestHint,
                Key::ReminderDefaultTone,
                Key::ReminderWeekStart,
            ],
            Self::Terminal => &[
                Key::SettingsTerminal,
                Key::SettingsFont,
                Key::SettingsTheme,
                Key::AiToolInput,
                Key::SettingsMatchTheEditorCaret,
                Key::TerminalCursor,
                Key::TerminalCursorBlink,
                Key::SettingsZoom,
                Key::SettingsSeparateInputArea,
            ],
            Self::System => &[
                Key::SettingsStartupAndWindows,
                Key::SettingsWhenTheLastWindowCloses,
                Key::SettingsProvidersAndModels,
                Key::SettingsAdvanced,
                Key::SettingsStartOnLogin,
                Key::SettingsKoruAi,
                Key::SettingsPrivacy,
                Key::SettingsShareUsageData,
            ],
        };
        keys.iter()
            .map(|key| koru_i18n::t(lang, *key))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether a sidebar search for `query` keeps this page.
    pub(crate) fn matches(self, lang: Lang, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let haystack = format!(
            "{} {} {} {}",
            self.label(lang),
            self.description(lang),
            self.keywords(),
            self.localized_keywords(lang)
        )
        .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }
}

/// Stable destinations another surface can link into — layout data, never
/// inferred from the words a card currently carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    Voice,
}

/// Where the page's title sits under the window's top edge — the shared
/// shell's, as is the column's width.
pub(crate) const TOP: f32 = PageShell::TOP;
/// The title, the sentence under it, and the air before the first card.
const TITLE_GAP: f32 = PageShell::TITLE_GAP;
const DESC_GAP: f32 = PageShell::LEAD_GAP;
/// The air below the last card, so it does not sit on the window's edge.
const BOTTOM_PAD: f32 = PageShell::BOTTOM_PAD;
/// A segmented track takes at most this share of a row; past it the setting
/// is a dropdown, so the row's name always keeps room to be read.
const SEGMENTED_SHARE: f32 = 0.6;
/// A setting with more values than this is a dropdown however short they are.
const SEGMENTED_MAX: usize = 3;
/// Between the two halves of a paired control (a caret's width and height).
const PAIR_GAP: f32 = 8.0;

/// A preview a theme tile paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TilePreview {
    System,
    Light,
    Dark,
}

/// One pressable rect, and what pressing it means.
pub(crate) struct Hit {
    pub(crate) rect: Rect,
    pub(crate) target: Target,
}

pub(crate) enum Target {
    Action(Action),
    /// Open dropdown `n` of the page's [`Laid::dropdowns`].
    Dropdown(usize),
}

/// A dropdown's list, kept beside the layout so a press can open it.
pub(crate) struct DropdownLaid {
    pub(crate) field: Rect,
    pub(crate) options: Vec<(Action, DropdownOption)>,
}

/// The control at a row's right end.
pub(crate) enum Widget {
    None,
    Segmented {
        control: Segmented,
        labels: Vec<String>,
        selected: Option<usize>,
        /// The first segment's index in [`Laid::hits`]; the rest follow.
        first: usize,
    },
    Dropdown {
        rect: Rect,
        value: String,
        face: Option<&'static str>,
        hit: usize,
        dropdown: usize,
    },
    Toggle {
        rect: Rect,
        on: bool,
        hit: usize,
    },
    Swatches {
        rects: Vec<Rect>,
        colors: Vec<Option<Color>>,
        selected: Option<usize>,
        first: usize,
    },
    Button {
        rect: Rect,
        label: String,
        style: koru_ui::ActionStyle,
        hit: usize,
    },
    Stepper {
        control: Stepper,
        value: String,
        less: Option<usize>,
        more: Option<usize>,
    },
    Pair(Box<Widget>, Box<Widget>),
}

pub(crate) struct RowLaid {
    pub(crate) rect: Rect,
    pub(crate) label: String,
    pub(crate) hint: Option<String>,
    /// A link ending the hint, and its index in [`Laid::hits`].
    pub(crate) link: Option<(String, usize)>,
    /// A key chip beside the name.
    pub(crate) chip: Option<String>,
    pub(crate) text_w: f32,
    pub(crate) widget: Widget,
}

pub(crate) struct CardLaid {
    pub(crate) rect: Rect,
    pub(crate) title: Option<String>,
    pub(crate) aside: Option<String>,
    /// The caption line ends with the document fonts, shown rather than
    /// named — *Preview: body · Heading · code*.
    pub(crate) font_preview: bool,
    pub(crate) rows: Vec<RowLaid>,
}

pub(crate) struct TileLaid {
    pub(crate) rect: Rect,
    pub(crate) label: String,
    pub(crate) preview: TilePreview,
    pub(crate) selected: bool,
    pub(crate) hit: usize,
}

pub(crate) enum Block {
    Title {
        title: String,
        description: String,
        x: f32,
        y: f32,
        w: f32,
        title_h: f32,
    },
    /// A sentence standing on its own between cards — the read-only reason,
    /// a notice, a blocked section.
    Note {
        text: String,
        rect: Rect,
    },
    Tiles {
        title: Option<String>,
        y: f32,
        x: f32,
        tiles: Vec<TileLaid>,
    },
    Card(CardLaid),
    /// A card's caption standing alone, with a link at its right end — the
    /// *Show* / *Hide* of a card that can be folded away.
    Caption {
        title: String,
        x: f32,
        y: f32,
        link: Option<(String, Rect, usize)>,
    },
    /// Room reserved for a page whose rows belong to the host; its rect is
    /// [`Laid::host`].
    Host,
}

/// What one layout pass produced.
#[derive(Default)]
pub(crate) struct Laid {
    pub(crate) blocks: Vec<Block>,
    pub(crate) hits: Vec<Hit>,
    pub(crate) dropdowns: Vec<DropdownLaid>,
    pub(crate) anchors: Vec<(Anchor, f32)>,
    pub(crate) host: Option<Rect>,
    pub(crate) bottom: f32,
}

/// The card being filled.
struct OpenCard {
    title: Option<String>,
    aside: Option<String>,
    font_preview: bool,
    top: f32,
    rows: Vec<RowLaid>,
}

/// The accumulator a page's content pushes onto.
pub(crate) struct Builder<'a> {
    engine: &'a mut TextEngine,
    x: f32,
    w: f32,
    y: f32,
    laid: Laid,
    card: Option<OpenCard>,
    /// Set by [`next_row_link`](Self::next_row_link) and
    /// [`next_row_chip`](Self::next_row_chip); taken by the next row.
    pending_link: Option<(String, Action)>,
    pending_chip: Option<String>,
}

/// What a row asks for before it knows where it is.
enum Spec {
    None,
    Segmented(Vec<(Action, String, bool)>),
    Dropdown(Vec<(Action, DropdownOption)>),
    Toggle(bool, Action),
    Swatches(Vec<(Action, Option<Color>, bool)>),
    Button(Action, String, koru_ui::ActionStyle),
    Stepper {
        value: String,
        widest: String,
        less: Option<Action>,
        more: Option<Action>,
    },
    Pair(Box<Spec>, Box<Spec>),
}

impl<'a> Builder<'a> {
    pub(crate) fn new(engine: &'a mut TextEngine, x: f32, w: f32, top: f32) -> Self {
        Self {
            engine,
            x,
            w,
            y: top,
            laid: Laid::default(),
            card: None,
            pending_link: None,
            pending_chip: None,
        }
    }

    /// End the next row's hint with a link that does `action` — *apply to
    /// all* after a sentence saying what it would change.
    pub(crate) fn next_row_link(&mut self, text: &str, action: Action) {
        self.pending_link = Some((text.to_owned(), action));
    }

    /// Put a key chip beside the next row's name.
    pub(crate) fn next_row_chip(&mut self, text: &str) {
        self.pending_chip = Some(text.to_owned());
    }

    pub(crate) fn finish(mut self) -> Laid {
        self.end_card();
        self.laid.bottom = self.y + BOTTOM_PAD;
        self.laid
    }

    // ----------------------------------------------------------- page level

    pub(crate) fn title(&mut self, title: &str, description: &str) {
        let title_style = crate::title_style();
        let title_h = self
            .engine
            .simple_layout_wrapped(title, title_style, self.w)
            .height();
        let desc_h = self
            .engine
            .simple_layout_wrapped(description, theme::role::ui(), self.w)
            .height();
        self.laid.blocks.push(Block::Title {
            title: title.to_owned(),
            description: description.to_owned(),
            x: self.x,
            y: self.y,
            w: self.w,
            title_h,
        });
        self.y += title_h + TITLE_GAP + desc_h + DESC_GAP;
    }

    pub(crate) fn note(&mut self, text: &str) {
        self.end_card();
        let h = self
            .engine
            .simple_layout_wrapped(text, theme::role::meta(), self.w)
            .height();
        self.laid.blocks.push(Block::Note {
            text: text.to_owned(),
            rect: Rect::new(self.x, self.y, self.w, h),
        });
        self.y += h + Card::GAP;
    }

    pub(crate) fn anchor(&mut self, anchor: Anchor) {
        self.laid.anchors.push((anchor, self.y));
    }

    /// Reserve `height` for the host's own rows and return where they go. A
    /// zero height reports the insertion point without taking any room.
    pub(crate) fn host(&mut self, height: f32) -> Rect {
        self.end_card();
        let rect = Rect::new(self.x, self.y, self.w, height);
        self.laid.blocks.push(Block::Host);
        self.laid.host = Some(rect);
        if height > 0.0 {
            self.y += height + Card::GAP;
        }
        rect
    }

    /// A row of preview tiles — the theme choice.
    pub(crate) fn tiles(
        &mut self,
        title: Option<&str>,
        items: Vec<(Action, String, TilePreview, bool)>,
    ) {
        self.end_card();
        let header = Card::new(title).header_h();
        let rects = PreviewTile::row(self.x, self.y + header, self.w, items.len());
        let mut tiles = Vec::with_capacity(items.len());
        for (rect, (action, label, preview, selected)) in rects.into_iter().zip(items) {
            let hit = self.push_hit(rect, Target::Action(action));
            tiles.push(TileLaid {
                rect,
                label,
                preview,
                selected,
                hit,
            });
        }
        self.laid.blocks.push(Block::Tiles {
            title: title.map(str::to_owned),
            y: self.y,
            x: self.x,
            tiles,
        });
        self.y += header + PreviewTile::H + Card::GAP;
    }

    // ----------------------------------------------------------- card level

    pub(crate) fn card(&mut self, title: Option<&str>) {
        self.card_with_aside(title, None);
    }

    pub(crate) fn card_with_aside(&mut self, title: Option<&str>, aside: Option<&str>) {
        self.end_card();
        let header = Card { title, aside }.header_h();
        self.card = Some(OpenCard {
            title: title.map(str::to_owned),
            aside: aside.map(str::to_owned),
            font_preview: false,
            top: self.y,
            rows: Vec::new(),
        });
        self.y += header;
    }

    /// A card whose caption line ends with the document fonts set in their
    /// own faces.
    pub(crate) fn card_with_font_preview(&mut self, title: &str) {
        self.card(Some(title));
        if let Some(card) = self.card.as_mut() {
            card.font_preview = true;
        }
    }

    /// A caption with a link at its right end, and no card of its own: what
    /// follows is either the card it folds (`b.card(None)` and its rows) or,
    /// folded, the one line that says what is in it (`b.note`).
    pub(crate) fn caption_with_link(&mut self, title: &str, link: &str, action: Action) {
        self.end_card();
        let style = theme::role::meta();
        let w = self.engine.simple_layout(link, style).width();
        let at = Rect::new(self.x + self.w - w, self.y, w, style.metrics.line_height);
        let hit = self.push_hit(at, Target::Action(action));
        self.laid.blocks.push(Block::Caption {
            title: title.to_owned(),
            x: self.x,
            y: self.y,
            link: Some((link.to_owned(), at, hit)),
        });
        self.y += Card::TITLE_H + Card::TITLE_GAP;
    }

    fn end_card(&mut self) {
        let Some(card) = self.card.take() else {
            return;
        };
        if card.rows.is_empty() {
            // A card with nothing in it is not drawn, and gives back its title.
            self.y = card.top;
            return;
        }
        let rect = Rect::new(self.x, card.top, self.w, self.y - card.top);
        self.laid.blocks.push(Block::Card(CardLaid {
            rect,
            title: card.title,
            aside: card.aside,
            font_preview: card.font_preview,
            rows: card.rows,
        }));
        self.y += Card::GAP;
    }

    // ------------------------------------------------------------ row level

    /// A row with nothing to press — a fact, or a sentence explaining why the
    /// rest of the card is absent.
    pub(crate) fn row_text(&mut self, label: &str, hint: Option<&str>) {
        self.row(label, hint, Spec::None);
    }

    /// A choice among values: a segmented track when there are few short ones,
    /// a dropdown otherwise.
    pub(crate) fn row_choice(
        &mut self,
        label: &str,
        hint: Option<&str>,
        items: Vec<(Action, String, bool)>,
    ) {
        let labels: Vec<&str> = items.iter().map(|(_, l, _)| l.as_str()).collect();
        let fits = items.len() <= SEGMENTED_MAX
            && Segmented::width(self.engine, &labels) <= self.w * SEGMENTED_SHARE;
        if fits {
            self.row(label, hint, Spec::Segmented(items));
        } else {
            self.row_dropdown(
                label,
                hint,
                items
                    .into_iter()
                    .map(|(action, label, on)| (action, DropdownOption::new(label, on)))
                    .collect(),
            );
        }
    }

    pub(crate) fn row_dropdown(
        &mut self,
        label: &str,
        hint: Option<&str>,
        options: Vec<(Action, DropdownOption)>,
    ) {
        self.row(label, hint, Spec::Dropdown(options));
    }

    /// Two choices side by side under one name — a caret's width and height.
    pub(crate) fn row_pair(
        &mut self,
        label: &str,
        hint: Option<&str>,
        first: Vec<(Action, DropdownOption)>,
        second: Vec<(Action, DropdownOption)>,
    ) {
        self.row(
            label,
            hint,
            Spec::Pair(
                Box::new(Spec::Dropdown(first)),
                Box::new(Spec::Dropdown(second)),
            ),
        );
    }

    /// A switch. `press` is what pressing it does — the opposite of `on`.
    pub(crate) fn row_toggle(&mut self, label: &str, hint: Option<&str>, on: bool, press: Action) {
        self.row(label, hint, Spec::Toggle(on, press));
    }

    pub(crate) fn row_swatches(
        &mut self,
        label: &str,
        hint: Option<&str>,
        items: Vec<(Action, Option<Color>, bool)>,
    ) {
        self.row(label, hint, Spec::Swatches(items));
    }

    pub(crate) fn row_button(
        &mut self,
        label: &str,
        hint: Option<&str>,
        action: Action,
        button: &str,
    ) {
        self.row(
            label,
            hint,
            Spec::Button(action, button.to_owned(), koru_ui::ActionStyle::Secondary),
        );
    }

    /// A quiet text button — *Clear…* — for an action the row offers but does
    /// not recommend.
    pub(crate) fn row_text_button(
        &mut self,
        label: &str,
        hint: Option<&str>,
        action: Action,
        button: &str,
    ) {
        self.row(
            label,
            hint,
            Spec::Button(action, button.to_owned(), koru_ui::ActionStyle::Ghost),
        );
    }

    pub(crate) fn row_stepper(
        &mut self,
        label: &str,
        hint: Option<&str>,
        value: String,
        widest: &str,
        less: Option<Action>,
        more: Option<Action>,
    ) {
        self.row(
            label,
            hint,
            Spec::Stepper {
                value,
                widest: widest.to_owned(),
                less,
                more,
            },
        );
    }

    fn row(&mut self, label: &str, hint: Option<&str>, spec: Spec) {
        if self.card.is_none() {
            self.card(None);
        }
        let (control_w, control_h) = self.measure(&spec);
        let text_w = CardRow::text_w(self.w, control_w);
        let pending_link = self.pending_link.take();
        let chip = self.pending_chip.take();
        let row = CardRow {
            label,
            hint,
            link: pending_link.as_ref().map(|(text, _)| text.as_str()),
            chip: chip.as_deref(),
        };
        let h = row
            .height(self.engine, text_w, control_h)
            .max(CardRow::MIN_H);
        let rect = Rect::new(self.x, self.y, self.w, h);
        let at = row.link_rect(self.engine, rect, text_w);
        let link = match (pending_link, at) {
            (Some((text, action)), Some(at)) => {
                Some((text, self.push_hit(at, Target::Action(action))))
            }
            _ => None,
        };
        let slot = CardRow::control_slot(rect, control_w, control_h);
        let widget = self.place(spec, slot);
        if let Some(card) = self.card.as_mut() {
            card.rows.push(RowLaid {
                rect,
                label: label.to_owned(),
                hint: hint.map(str::to_owned),
                link,
                chip,
                text_w,
                widget,
            });
        }
        self.y += h;
    }

    fn measure(&mut self, spec: &Spec) -> (f32, f32) {
        match spec {
            Spec::None => (0.0, 0.0),
            Spec::Segmented(items) => {
                let labels: Vec<&str> = items.iter().map(|(_, l, _)| l.as_str()).collect();
                (Segmented::width(self.engine, &labels), Segmented::H)
            }
            Spec::Dropdown(options) => {
                let options: Vec<DropdownOption> =
                    options.iter().map(|(_, option)| option.clone()).collect();
                (Dropdown::width_for(self.engine, &options), Dropdown::H)
            }
            Spec::Toggle(..) => (Toggle::new(false).width(), Toggle::H),
            Spec::Swatches(items) => {
                let colors: Vec<_> = items.iter().map(|(_, color, _)| *color).collect();
                let row = koru_ui::ColorPickerRow::new(&colors);
                (row.width(), row.height())
            }
            Spec::Button(_, label, style) => (
                ActionButton {
                    label,
                    style: *style,
                    trailing_icon: None,
                }
                .width(self.engine),
                ActionButton::H,
            ),
            Spec::Stepper { widest, .. } => (Stepper::width(self.engine, widest), Stepper::H),
            Spec::Pair(first, second) => {
                let (aw, ah) = self.measure(first);
                let (bw, bh) = self.measure(second);
                (aw + PAIR_GAP + bw, ah.max(bh))
            }
        }
    }

    fn push_hit(&mut self, rect: Rect, target: Target) -> usize {
        self.laid.hits.push(Hit { rect, target });
        self.laid.hits.len() - 1
    }

    fn place(&mut self, spec: Spec, slot: Rect) -> Widget {
        match spec {
            Spec::None => Widget::None,
            Spec::Segmented(items) => {
                let labels: Vec<String> = items.iter().map(|(_, l, _)| l.clone()).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let control = Segmented::layout(self.engine, &refs, slot.x, slot.y);
                let selected = items.iter().position(|(_, _, on)| *on);
                let first = self.laid.hits.len();
                for ((action, _, _), rect) in items.into_iter().zip(control.segments.clone()) {
                    self.push_hit(rect, Target::Action(action));
                }
                Widget::Segmented {
                    control,
                    labels,
                    selected,
                    first,
                }
            }
            Spec::Dropdown(options) => {
                let (value, face) = options
                    .iter()
                    .find(|(_, option)| option.selected)
                    .map(|(_, option)| (option.label.clone(), option.font_stack))
                    .unwrap_or_default();
                let dropdown = self.laid.dropdowns.len();
                self.laid.dropdowns.push(DropdownLaid {
                    field: slot,
                    options,
                });
                let hit = self.push_hit(slot, Target::Dropdown(dropdown));
                Widget::Dropdown {
                    rect: slot,
                    value,
                    face,
                    hit,
                    dropdown,
                }
            }
            Spec::Toggle(on, action) => {
                let hit = self.push_hit(slot, Target::Action(action));
                Widget::Toggle {
                    rect: slot,
                    on,
                    hit,
                }
            }
            Spec::Swatches(items) => {
                let selected = items.iter().position(|(_, _, on)| *on);
                let first = self.laid.hits.len();
                let colors: Vec<_> = items.iter().map(|(_, color, _)| *color).collect();
                // The shared row places them, so the card's swatches are the
                // same strip every other colour picker draws.
                let rects = koru_ui::ColorPickerRow::new(&colors).swatches(slot);
                for ((action, _, _), rect) in items.into_iter().zip(rects.iter()) {
                    self.push_hit(*rect, Target::Action(action));
                }
                Widget::Swatches {
                    rects,
                    colors,
                    selected,
                    first,
                }
            }
            Spec::Button(action, label, style) => {
                let hit = self.push_hit(slot, Target::Action(action));
                Widget::Button {
                    rect: slot,
                    label,
                    style,
                    hit,
                }
            }
            Spec::Stepper {
                value,
                widest,
                less,
                more,
            } => {
                let control = Stepper::layout(self.engine, &widest, slot.x, slot.y);
                let less = less.map(|a| self.push_hit(control.less, Target::Action(a)));
                let more = more.map(|a| self.push_hit(control.more, Target::Action(a)));
                Widget::Stepper {
                    control,
                    value,
                    less,
                    more,
                }
            }
            Spec::Pair(first, second) => {
                let (aw, ah) = self.measure(&first);
                let (bw, bh) = self.measure(&second);
                let a = self.place(
                    *first,
                    Rect::new(slot.x, slot.y + (slot.h - ah) * 0.5, aw, ah),
                );
                let b = self.place(
                    *second,
                    Rect::new(slot.x + aw + PAIR_GAP, slot.y + (slot.h - bh) * 0.5, bw, bh),
                );
                Widget::Pair(Box::new(a), Box::new(b))
            }
        }
    }
}

#[cfg(test)]
mod tests;
