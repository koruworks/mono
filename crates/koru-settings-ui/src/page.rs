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
    ActionButton, Card, CardRow, Chip, Dropdown, DropdownOption, PageShell, PreviewTile, Segmented,
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
    /// *Coding → Agent providers*: which coding agents Koru shows, and
    /// whether their CLIs are ready. The host fills its rows.
    AgentProviders,
}

/// A heading in the sidebar over the pages that belong together. A page
/// with none sits in the ungrouped run at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageGroup {
    Coding,
}

impl PageGroup {
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Coding => koru_i18n::t(lang, Key::SettingsCoding),
        }
    }
}

impl PageId {
    pub const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Editor,
        Self::Reminders,
        Self::Terminal,
        Self::System,
        Self::AgentProviders,
    ];

    /// The sidebar heading this page sits under, if any. A group's pages
    /// follow the ungrouped ones in [`ALL`](Self::ALL).
    pub fn group(self) -> Option<PageGroup> {
        match self {
            Self::AgentProviders => Some(PageGroup::Coding),
            Self::Appearance | Self::Editor | Self::Reminders | Self::Terminal | Self::System => {
                None
            }
        }
    }

    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Appearance => koru_i18n::t(lang, Key::SettingsAppearance),
            Self::Editor => koru_i18n::t(lang, Key::SettingsEditor),
            Self::Reminders => koru_i18n::t(lang, Key::ReminderTitle),
            Self::Terminal => koru_i18n::t(lang, Key::SettingsTerminal),
            Self::System => koru_i18n::t(lang, Key::SettingsSystem),
            Self::AgentProviders => koru_i18n::t(lang, Key::SettingsAgentProviders),
        }
    }

    pub(crate) fn description(self, lang: Lang) -> &'static str {
        match self {
            Self::Appearance => koru_i18n::t(lang, Key::SettingsAppearanceDescription),
            Self::Editor => koru_i18n::t(lang, Key::SettingsEditorDescription),
            Self::Reminders => koru_i18n::t(lang, Key::SettingsRemindersDescription),
            Self::Terminal => koru_i18n::t(lang, Key::SettingsTerminalDescription),
            Self::System => koru_i18n::t(lang, Key::SettingsSystemDescription),
            Self::AgentProviders => koru_i18n::t(lang, Key::SettingsAgentProvidersDescription),
        }
    }

    /// What a search in the sidebar matches besides the name — the settings
    /// the page holds, in the words a person would type for them.
    pub(crate) fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "theme dark light system font fonts typeface density pinned tabs caret color cursor typography icon icons phosphor tabler lucide remix sample preview"
            }
            Self::Editor => {
                "page width layout background canvas spelling spell check language dictionary voice dictation microphone speech model quality shortcut consent"
            }
            Self::Reminders => {
                "reminder notification notifications sound tone week day end of day deliver test"
            }
            Self::Terminal => "shell cursor zoom font theme input blink composer",
            Self::System => {
                "login startup start quit background close window renderer graphics vulkan directx opengl advanced privacy telemetry usage data analytics diagnostics"
            }
            Self::AgentProviders => {
                "coding agents agent providers cli claude codex opencode grok kimi antigravity copilot qwen muse install path enable disable hide"
            }
        }
    }

    fn localized_keywords(self, lang: Lang) -> String {
        let keys: &[Key] = match self {
            Self::Appearance => &[
                Key::SettingsTheme,
                Key::SettingsIconStyle,
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
                Key::SettingsAdvanced,
                Key::SettingsStartOnLogin,
                Key::SettingsPrivacy,
                Key::SettingsShareUsageData,
            ],
            Self::AgentProviders => &[
                Key::SettingsCoding,
                Key::SettingsEnabledAgents,
                Key::SettingsRestoreDefaults,
                Key::SettingsRefreshAgentStatus,
                Key::SettingsAgentSetupGuide,
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
    /// The move handle of draggable row `n` ([`Laid::drag_rows`]): a press
    /// takes the row's focus and starts a drag.
    Grip(usize),
    /// The rest of draggable row `n`: a press takes its focus, which the
    /// move chords then act on. Pushed after the row's controls, so a
    /// switch or a link still wins.
    Row(usize),
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
    /// A wrapping row of choice chips across the whole row — *Default
    /// agent*'s *Auto* and one per agent. The row has no name of its own.
    Chips(Vec<ChipLaid>),
}

/// One chip of a [`Widget::Chips`] row.
pub(crate) struct ChipLaid {
    pub(crate) rect: Rect,
    pub(crate) label: String,
    /// A brand mark, by profile key, in the chip's leading slot.
    pub(crate) mark: Option<String>,
    pub(crate) selected: bool,
    pub(crate) hit: usize,
}

/// The leading slot a chip with a brand mark reserves: the mark and the air
/// after it.
pub(crate) const CHIP_MARK_SLOT: f32 = 20.0;
/// The mark's own square inside it.
pub(crate) const CHIP_MARK_SIZE: f32 = 14.0;

pub(crate) struct RowLaid {
    pub(crate) rect: Rect,
    pub(crate) label: String,
    pub(crate) hint: Option<String>,
    /// A link ending the hint, and its index in [`Laid::hits`].
    pub(crate) link: Option<(String, usize)>,
    /// A key chip beside the name.
    pub(crate) chip: Option<String>,
    /// A brand mark before the name, by profile key — an agent's row on
    /// *Agent providers*. The text then starts [`MARK_SLOT`] further in.
    pub(crate) mark: Option<String>,
    /// A row that can be dragged to a new place: its index among the
    /// page's draggable rows, and its move handle, in the card's left inset.
    pub(crate) drag: Option<(usize, Rect)>,
    pub(crate) text_w: f32,
    pub(crate) widget: Widget,
}

impl RowLaid {
    /// Where the row's text is laid: the row, less a leading mark's slot.
    pub(crate) fn text_rect(&self) -> Rect {
        let indent = if self.mark.is_some() { MARK_SLOT } else { 0.0 };
        Rect::new(
            self.rect.x + indent,
            self.rect.y,
            self.rect.w - indent,
            self.rect.h,
        )
    }
}

/// The move handle's slot in a draggable row: in the card's left inset, the
/// editor's grip size, a little in from the card's edge.
pub(crate) const GRIP_INSET: f32 = 3.0;

/// The room a row's leading brand mark takes: the mark and the air after it.
pub(crate) const MARK_SLOT: f32 = 28.0;
/// The mark's own square.
pub(crate) const MARK_SIZE: f32 = 18.0;

pub(crate) struct CardLaid {
    pub(crate) rect: Rect,
    pub(crate) title: Option<String>,
    pub(crate) aside: Option<String>,
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
        /// A page-wide action at the title's right end — *Restore
        /// defaults* — and its index in [`Laid::hits`].
        button: Option<(String, Rect, usize)>,
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
    /// *Preview in sample document* above the document fonts.
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
    /// Every draggable row's rect, by its index — where a drag lands and
    /// where the drop line is drawn.
    pub(crate) drag_rows: Vec<Rect>,
}

/// The card being filled.
struct OpenCard {
    title: Option<String>,
    aside: Option<String>,
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
    /// Set by [`next_row_mark`](Self::next_row_mark); taken by the next row.
    pending_mark: Option<String>,
    /// Set by [`next_row_draggable`](Self::next_row_draggable); taken by
    /// the next row.
    pending_drag: Option<usize>,
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
            pending_mark: None,
            pending_drag: None,
        }
    }

    /// Make the next row draggable — index `n` among the page's draggable
    /// rows — with a move handle in its left inset.
    pub(crate) fn next_row_draggable(&mut self, n: usize) {
        self.pending_drag = Some(n);
    }

    /// Put a brand mark — the profile `key`'s — before the next row's name.
    pub(crate) fn next_row_mark(&mut self, key: &str) {
        self.pending_mark = Some(key.to_owned());
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

    /// The page's title and its sentence, with an optional page-wide action
    /// at its right end — the shared secondary button, level with the
    /// title's first line. The title and its sentence wrap short of it.
    pub(crate) fn title_with_button(
        &mut self,
        title: &str,
        description: &str,
        button: Option<(&str, Action)>,
    ) {
        let title_style = crate::title_style();
        let button = button.map(|(label, action)| {
            let w = ActionButton {
                label,
                style: koru_ui::ActionStyle::Secondary,
                trailing_icon: None,
            }
            .width(self.engine);
            let line = title_style.metrics.line_height;
            let at = Rect::new(
                self.x + self.w - w,
                self.y + (line - ActionButton::H).max(0.0) * 0.5,
                w,
                ActionButton::H,
            );
            let hit = self.push_hit(at, Target::Action(action));
            (label.to_owned(), at, hit)
        });
        let text_w = match &button {
            Some((_, at, _)) => (self.w - at.w - CardRow::GAP).max(40.0),
            None => self.w,
        };
        let title_h = self
            .engine
            .simple_layout_wrapped(title, title_style, text_w)
            .height();
        let desc_h = self
            .engine
            .simple_layout_wrapped(description, theme::role::ui(), text_w)
            .height();
        self.laid.blocks.push(Block::Title {
            title: title.to_owned(),
            description: description.to_owned(),
            x: self.x,
            y: self.y,
            w: text_w,
            title_h,
            button,
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
            top: self.y,
            rows: Vec::new(),
        });
        self.y += header;
    }

    /// A caption with a link at its right end, and no card of its own: what
    /// follows is the untitled card it heads (`b.card(None)` and its rows),
    /// which then reads exactly like a titled card with a link beside its
    /// title.
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

    /// A switch with a secondary button before it — an agent's *Install*
    /// beside the switch that shows it. The two controls are the existing
    /// [`Spec::Pair`], the same composition as a caret's width and height, so
    /// no control is new: the button is the one [`row_button`](Self::row_button)
    /// draws, the switch the one [`row_toggle`](Self::row_toggle) does.
    pub(crate) fn row_button_toggle(
        &mut self,
        label: &str,
        hint: Option<&str>,
        (button, action): (&str, Action),
        on: bool,
        press: Action,
    ) {
        self.row(
            label,
            hint,
            Spec::Pair(
                Box::new(Spec::Button(
                    action,
                    button.to_owned(),
                    koru_ui::ActionStyle::Secondary,
                )),
                Box::new(Spec::Toggle(on, press)),
            ),
        );
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

    /// **A row of choice chips**, wrapping across the card's width — the
    /// shared [`Chip`], each optionally led by a brand mark (by profile key).
    /// The row has no name; the card's title says what the chips choose.
    pub(crate) fn row_chips(&mut self, items: Vec<(Action, String, Option<String>, bool)>) {
        if self.card.is_none() {
            self.card(None);
        }
        let left = self.x + CardRow::PAD_X;
        let right = self.x + self.w - CardRow::PAD_X;
        let top = self.y + CardRow::PAD_Y;
        let (mut x, mut y) = (left, top);
        let mut chips = Vec::with_capacity(items.len());
        for (action, label, mark, selected) in items {
            let slot = if mark.is_some() { CHIP_MARK_SLOT } else { 0.0 };
            let w = Chip::new(&label, selected)
                .leading(slot)
                .width(self.engine)
                .max(koru_ui::chip::MIN_W);
            if x > left && x + w > right {
                x = left;
                y += Chip::H + koru_ui::chip::GAP;
            }
            let rect = Rect::new(x, y, w, Chip::H);
            let hit = self.push_hit(rect, Target::Action(action));
            chips.push(ChipLaid {
                rect,
                label,
                mark,
                selected,
                hit,
            });
            x += w + koru_ui::chip::GAP;
        }
        let bottom = chips
            .iter()
            .map(|chip| chip.rect.y + chip.rect.h)
            .fold(top, f32::max);
        let h = (bottom + CardRow::PAD_Y - self.y).max(CardRow::MIN_H);
        let rect = Rect::new(self.x, self.y, self.w, h);
        if let Some(card) = self.card.as_mut() {
            card.rows.push(RowLaid {
                rect,
                label: String::new(),
                hint: None,
                link: None,
                chip: None,
                mark: None,
                drag: None,
                text_w: 0.0,
                widget: Widget::Chips(chips),
            });
        }
        self.y += h;
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
        let mark = self.pending_mark.take();
        let indent = if mark.is_some() { MARK_SLOT } else { 0.0 };
        let text_w = CardRow::text_w(self.w - indent, control_w);
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
        // The handle's press covers the whole inset, the row's full height,
        // so it is not a 12-pixel target.
        let drag = self.pending_drag.take().map(|n| {
            let grip = Rect::new(
                rect.x + GRIP_INSET,
                rect.y + (rect.h - koru_draw::theme::GRIP_H) * 0.5,
                koru_draw::theme::GRIP_W,
                koru_draw::theme::GRIP_H,
            );
            self.push_hit(
                Rect::new(rect.x, rect.y, CardRow::PAD_X, rect.h),
                Target::Grip(n),
            );
            if self.laid.drag_rows.len() <= n {
                self.laid
                    .drag_rows
                    .resize(n + 1, Rect::new(0.0, 0.0, 0.0, 0.0));
            }
            self.laid.drag_rows[n] = rect;
            (n, grip)
        });
        let text_rect = Rect::new(rect.x + indent, rect.y, rect.w - indent, rect.h);
        let at = row.link_rect(self.engine, text_rect, text_w);
        let link = match (pending_link, at) {
            (Some((text, action)), Some(at)) => {
                Some((text, self.push_hit(at, Target::Action(action))))
            }
            _ => None,
        };
        let slot = CardRow::control_slot(rect, control_w, control_h);
        let widget = self.place(spec, slot);
        if let Some((n, _)) = drag {
            self.push_hit(rect, Target::Row(n));
        }
        if let Some(card) = self.card.as_mut() {
            card.rows.push(RowLaid {
                rect,
                label: label.to_owned(),
                hint: hint.map(str::to_owned),
                link,
                chip,
                mark,
                drag,
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
