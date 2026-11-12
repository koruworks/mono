// SPDX-License-Identifier: MPL-2.0

//! **A Bot's settings region** (Bots P1.8 S5): the Bot tab's *Settings*,
//! opened from its header, in the conversation's place.
//!
//! ```text
//! scroll_y  Bot settings                     the region, the shared scrollbar
//!   col  Bot settings content                NAV_PAD at the sides, S3 above and below, S4 between
//!     Bot settings notice                    changed elsewhere, or a refusal's sentence
//!     Bot settings delivering                which change is still being delivered
//!     col  Bot settings card  (Bot)          on the raised plate, S3 inside, S3 between rows
//!       row  Bot name row                    *Name*, the field, *Save* or *Saving…*
//!       row  Bot role row                    *Standing role*, *Open role page*
//!     Bot settings title  (Access)
//!     col  Bot settings card  (Access)
//!       Bot access summary                   what the shown choice allows, naming the home
//!       row  Bot access row                  the two approved choices
//!       Bot pages title                      *Selected pages*
//!       row  Bot selected page               a page's title, *Remove {page}*
//!       row  Bot access actions              *Add a page…*, *Save* or *Saving…*
//!     Bot settings title  (Working folder)
//!     col  Bot settings card  (Working folder)
//!       Bot runs here                        *Runs on this computer*
//!       Bot folder path                      the folder the owner holds
//!       row  Bot folder actions              *Change…*, or the sentence that it is fixed
//!     col  Bot settings card  (Pause)
//!       row  Bot pause row                   the paused notice, *Pause bot* / *Resume bot* or *…ing*
//!     col  Bot disclosures                   runs on this computer using the Bot's agent;
//!                                            what Koru's access covers
//! ```
//!
//! **Every control is a shared component** — `TextInput`, `ActionButton`,
//! `Segmented` — hosted as a leaf the frame places ([`super::controls`]); every
//! padding and gap is a token. One frame gives paint, the pointer, the
//! keyboard's order, the accessibility tree and F12 their geometry.
//!
//! **What is absent, deliberately.** The lifecycle actions and the
//! conversation list, which are the tab's own (its header's ⋯, its column).
//! An unbuilt control is absent, not disabled.
//!
//! **The create form is made of these cards** (S8, [`super::create`]): the
//! access card, the *Agent and model* card, the keyboard's walk, the
//! accessibility tree and the press routing here serve it too, reading the
//! create form's draft where it is one ([`BotView::access_shown`],
//! [`BotView::agent_draft`]).

use std::cell::RefCell;
use std::rc::Rc;

use koru_ai_core::bots::{BotAccess, BotPage};
use koru_ai_ui::agent_catalog::config::modes;
use koru_draw::el::{El, Frame, Key, Role, col, leaf, row, scroll_y, spacer, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Radius, Space, Surface, alias, roles};
use koru_i18n::{Key as Words, t};
use koru_ui::button::{ActionStyle, Lead};
use koru_ui::text_field::Typed;
use koru_ui::text_input::TextInput;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::controls::{ButtonLeaf, ChoiceLeaf, FieldLeaf};
use super::{BotView, SURFACE};
use crate::bots::settings::Change;

/// The region, a scroll viewport.
pub(crate) const SETTINGS: Key = Key::surface(SURFACE, "settings");
/// The header's *Settings*, which opens and closes the region.
pub(crate) const SETTINGS_BUTTON: Key = Key::surface(SURFACE, "settings-button");
pub(crate) const SETTINGS_NOTICE: Key = Key::surface(SURFACE, "settings-notice");
/// The sentence saying which change of the Bot is still being delivered to
/// what runs it, while the Bot's read carries `pending_change`.
pub(crate) const SETTINGS_DELIVERING: Key = Key::surface(SURFACE, "settings-delivering");
pub(crate) const ABOUT_CARD: Key = Key::surface(SURFACE, "about-card");
pub(crate) const ACCESS_CARD: Key = Key::surface(SURFACE, "access-card");
pub(crate) const FOLDER_CARD: Key = Key::surface(SURFACE, "folder-card");
pub(crate) const PAUSE_CARD: Key = Key::surface(SURFACE, "pause-card");
pub(crate) const NAME_FIELD: Key = Key::surface(SURFACE, "name-field");
pub(crate) const SAVE_NAME: Key = Key::surface(SURFACE, "save-name");
pub(crate) const NAME_PENDING: Key = Key::surface(SURFACE, "name-pending");
pub(crate) const OPEN_ROLE: Key = Key::surface(SURFACE, "open-role");
pub(crate) const ACCESS_SUMMARY: Key = Key::surface(SURFACE, "access-summary");
pub(crate) const ACCESS_CHOICE: Key = Key::surface(SURFACE, "access-choice");
/// The choice's two segments, as the accessibility tree names them.
pub(crate) const ACCESS_SEGMENTS: [Key; 2] = [
    Key::surface(SURFACE, "access-default"),
    Key::surface(SURFACE, "access-workspace"),
];
pub(crate) const ADD_PAGE: Key = Key::surface(SURFACE, "add-page");
pub(crate) const SAVE_ACCESS: Key = Key::surface(SURFACE, "save-access");
pub(crate) const ACCESS_PENDING: Key = Key::surface(SURFACE, "access-pending");
pub(crate) const FOLDER_PATH: Key = Key::surface(SURFACE, "folder-path");
pub(crate) const CHANGE_FOLDER: Key = Key::surface(SURFACE, "change-folder");
pub(crate) const FOLDER_FIXED: Key = Key::surface(SURFACE, "folder-fixed");
pub(crate) const FOLDER_PENDING: Key = Key::surface(SURFACE, "folder-pending");
pub(crate) const PAUSE: Key = Key::surface(SURFACE, "pause");
pub(crate) const PAUSE_PENDING: Key = Key::surface(SURFACE, "pause-pending");
pub(crate) const PAUSED_NOTICE: Key = Key::surface(SURFACE, "paused-notice");
pub(crate) const RUNS_ON: Key = Key::surface(SURFACE, "runs-on");
pub(crate) const RUNTIME_LIMITS: Key = Key::surface(SURFACE, "runtime-limits");
/// The access card's *Selected pages* over its pages, and the folder card's
/// *Runs on this computer*: drawn, and published as words (finding 8).
pub(crate) const SELECTED_PAGES_TITLE: Key = Key::surface(SURFACE, "selected-pages-title");
pub(crate) const RUNS_HERE: Key = Key::surface(SURFACE, "runs-here");
/// The *Agent and model* card (S7) and its parts.
pub(crate) const AGENT_CARD: Key = Key::surface(SURFACE, "agent-card");
pub(crate) const MODEL_CHIP: Key = Key::surface(SURFACE, "model-chip");
pub(crate) const MODE_CHIP: Key = Key::surface(SURFACE, "mode-chip");
pub(crate) const SAVE_MODEL: Key = Key::surface(SURFACE, "save-model");
pub(crate) const MODEL_PENDING: Key = Key::surface(SURFACE, "model-pending");
pub(crate) const MODEL_NOTE: Key = Key::surface(SURFACE, "model-note");
pub(crate) const AGENT_READINESS: Key = Key::surface(SURFACE, "agent-readiness");

/// The part a selected page's row and its *Remove* are keyed by, under the
/// page's own document id — never its place in the list.
pub(crate) const PAGE_PART: &str = "selected-page";
pub(crate) const REMOVE_PART: &str = "remove-page";

pub(crate) fn page_key(page: &BotPage) -> Key {
    Key::model(page.document.as_uuid().as_u128(), PAGE_PART)
}

pub(crate) fn remove_key(page: &BotPage) -> Key {
    Key::model(page.document.as_uuid().as_u128(), REMOVE_PART)
}

/// The two approved access choices, in the order the track shows them
/// (ROADMAP §8, X8): the default, then any page in the home workspace.
pub(crate) const CHOICES: [BotAccess; 2] = [BotAccess::Default, BotAccess::Workspace];

/// What a press or a key on one of the tab's controls asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    ToggleSettings,
    SaveName,
    OpenRole,
    Choose(BotAccess),
    AddPage,
    RemovePage(BotPage),
    SaveAccess,
    ChangeFolder,
    TogglePause,
    /// *New conversation* (S8).
    NewConversation,
    /// *Show more* under the conversation list (S8).
    MoreConversations,
    /// The header's ⋯: the Bot's lifecycle menu, which the window opens (S14).
    Actions,
    /// The *Agent and model* card's model chip, its mode chip and *Save*
    /// (S7).
    OpenModelPicker,
    OpenModes,
    SaveModel,
    /// A parked operation's *Continue* and *Cancel* (S14).
    ContinueOperation,
    CancelOperation,
    /// An archived Bot's *Unarchive*, a trashed Bot's *Restore* (S14).
    Unarchive,
    Restore,
    /// The create form's (S8): *Create bot*, a stopped creation's *Retry*
    /// and *Abandon*, an example's wording, where the role comes from, the
    /// existing role page's picker, *Change access*, the folder dialog, the
    /// managed folder again, and a page a creation left behind.
    Create,
    Retry,
    Abandon,
    /// *Start over* after Koru could not confirm whether the bot was made.
    StartOver,
    Example(crate::bots::creation::Example),
    ChooseRole(crate::bots::creation::RoleKind),
    ChooseRolePage,
    ChangeAccess,
    ChooseFolder,
    UseManagedFolder,
    OpenLeftPage,
}

/// One control as the accessibility tree publishes it, in the tab's
/// coordinates and in the keyboard's order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control {
    pub key: Key,
    pub rect: Rect,
    pub role: ControlRole,
    pub label: String,
    /// The field's text.
    pub value: Option<String>,
    pub selected: Option<bool>,
    pub disabled: bool,
    pub focused: bool,
    /// What nests under it: a card's lines and controls, the choice's
    /// segments.
    pub children: Vec<Control>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControlRole {
    Group,
    Button,
    /// A button that is on or off — Fast, the longer window, a ☆.
    Toggle,
    TextInput,
    Radio,
    Label,
    /// The conversation list (S8), and one of its rows.
    List,
    ListItem,
}

/// A card's quiet title above it, in the koru_ui `Card`'s words: a meta
/// line in hint ink.
pub(super) fn card_title(words: &str) -> El {
    text(words.to_owned())
        .named("Bot settings title")
        .role(roles::META)
        .ink(Ink::Hint)
        .truncate()
}

/// A card: the settings rows that belong together, on the raised plate.
pub(super) fn card(key: Key, name: Words) -> El {
    col("Bot settings card")
        .id(key)
        .bg(Surface::Raised)
        .radius(Radius::L)
        .pad(Space::S3)
        .gap(Space::S3)
        .semantics(Role::Group)
        .name(name)
}

/// A sentence in a card or below the cards: meta, wrapped.
pub(super) fn line(name: &'static str, key: Key, words: String, ink: Ink) -> El {
    text(words)
        .named(name)
        .id(key)
        .role(roles::META)
        .ink(ink)
        .wrap()
}

impl BotView {
    /// The words the tab says in its language.
    pub(super) fn words(&self, key: Words) -> String {
        t(self.lang, key).to_owned()
    }

    /// **The agent the Bot runs on**, by its id's word after `agent-`,
    /// capitalized — *Codex* for `agent-codex`, *Claude* for `agent-claude`.
    /// Any runtime may run a Bot (QUESTIONS O33, the owner's answer,
    /// 2026-11-10), so the disclosures name the Bot's own agent and claim no
    /// other. The tab holds no agent listing to read a display name from.
    pub(crate) fn agent_name(&self) -> String {
        let provider = self
            .agent_provider()
            .map(|provider| provider.as_str().to_owned())
            .unwrap_or_default();
        let word = provider.strip_prefix("agent-").unwrap_or(&provider);
        let mut chars = word.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    /// The home workspace as the access sentences name it.
    pub(super) fn home_words(&self) -> String {
        self.home_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.words(Words::BotsHomeWorkspace))
    }

    /// A selected page's title, as the window last said; *Untitled* until
    /// it has.
    pub(crate) fn page_title(&self, page: &BotPage) -> String {
        self.page_titles
            .get(&page.document)
            .filter(|title| !title.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.words(Words::DocUntitled))
    }

    pub(super) fn button(
        &self,
        engine: &mut TextEngine,
        name: &'static str,
        key: Key,
        label: String,
        style: ActionStyle,
        disabled: bool,
    ) -> El {
        self.button_led(engine, name, key, label, style, disabled, None)
    }

    /// [`Self::button`] led by a mark in an ink of its own (the mode chip,
    /// P1.8 §20). Its name stays its words: the mark and its color add
    /// nothing to what a screen reader hears.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn button_led(
        &self,
        engine: &mut TextEngine,
        name: &'static str,
        key: Key,
        label: String,
        style: ActionStyle,
        disabled: bool,
        lead: Option<Lead>,
    ) -> El {
        let focused = self.focus == Some(key);
        leaf(
            name,
            ButtonLeaf::led(engine, label.clone(), style, disabled, focused, lead),
        )
        .semantic()
        .id(key)
        .hit()
        .focusable()
        .disabled(disabled)
        .semantics(Role::Button)
        .name_text(label)
    }

    /// The header's *Settings*: the one control the header has. Selected
    /// while the region is open.
    pub(super) fn settings_button(&self, engine: &mut TextEngine) -> El {
        self.button(
            engine,
            "Bot settings button",
            SETTINGS_BUTTON,
            self.words(Words::BotsSettings),
            ActionStyle::Ghost,
            false,
        )
        .selected(self.settings_open)
    }

    /// **The region as elements.** Its cards only once the tab knows the
    /// Bot as the owner holds it; before that, the header's notice says the
    /// Bot is being read.
    pub(super) fn region(&self, engine: &mut TextEngine) -> El {
        let mut content = col("Bot settings content")
            .pad_x(alias::NAV_PAD)
            .pad_y(Space::S3)
            .gap(Space::S4);
        let settings = &self.settings;
        if let Some(words) = settings.notice_words() {
            content = content.child(line(
                "Bot settings notice",
                SETTINGS_NOTICE,
                self.words(words),
                Ink::Accent,
            ));
        }
        // A change still being delivered to what runs the Bot: which one,
        // and why the controls that would be refused are inert.
        if let Some(words) = settings.delivering_line() {
            content = content.child(line(
                "Bot settings delivering",
                SETTINGS_DELIVERING,
                self.words(words),
                Ink::Secondary,
            ));
        }
        if let Some(bot) = settings.bot() {
            content = content
                .child(self.about_card(engine))
                .child(card_title(t(self.lang, Words::BotsAgent)))
                .child(self.agent_card(engine))
                .child(card_title(t(self.lang, Words::BotsAccess)))
                .child(self.access_card(
                    engine,
                    settings.is_pending(Change::Access),
                    Some(settings.can_save_access()),
                ))
                .child(card_title(t(self.lang, Words::BotsFolder)))
                .child(self.folder_card(engine, &bot.config.working_folder))
                .child(self.pause_card(engine, bot.paused))
                .child(self.disclosures());
        }
        scroll_y("Bot settings")
            .id(SETTINGS)
            .offset(self.scroll)
            .scrollbar()
            .fill()
            .semantics(Role::Group)
            .name(Words::BotsSettings)
            .child(content)
    }

    fn about_card(&self, engine: &mut TextEngine) -> El {
        let input = self.name_input_with(engine);
        let name = row("Bot name row")
            .gap(Space::S2)
            .items_center()
            .child(
                text(self.words(Words::BotsName))
                    .named("Bot name label")
                    .role(roles::UI)
                    .ink(Ink::Secondary),
            )
            .child(
                leaf(
                    "Bot name field",
                    FieldLeaf {
                        input,
                        placeholder: self.words(Words::BotsName),
                        focused: self.focus == Some(NAME_FIELD),
                    },
                )
                .semantic()
                .fill()
                .id(NAME_FIELD)
                .hit()
                .focusable()
                .name(Words::BotsName),
            )
            .child(if self.settings.is_pending(Change::Name) {
                line(
                    "Bot name pending",
                    NAME_PENDING,
                    self.words(Words::BotsSaving),
                    Ink::Hint,
                )
            } else {
                self.button(
                    engine,
                    "Bot save name",
                    SAVE_NAME,
                    self.words(Words::BotsSave),
                    ActionStyle::Primary,
                    !self.settings.can_save_name(),
                )
            });
        let role = row("Bot role row")
            .gap(Space::S2)
            .items_center()
            .child(
                text(self.words(Words::BotsRole))
                    .named("Bot role label")
                    .role(roles::UI)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            )
            .child(self.button(
                engine,
                "Bot open role",
                OPEN_ROLE,
                self.words(Words::BotsOpenRole),
                ActionStyle::Secondary,
                false,
            ));
        card(ABOUT_CARD, Words::BotsSettings)
            .child(name)
            .child(role)
    }

    /// **What the access choice shows**: the settings' draft over what the
    /// owner holds, or — in create mode (S8) — the new Bot's draft.
    pub(super) fn access_shown(&self) -> BotAccess {
        match self.create.as_deref() {
            Some(create) => create.form.access_shown(),
            None => self.settings.access_shown(),
        }
    }

    /// The selected pages the access card lists, from the same draft.
    pub(super) fn pages_shown(&self) -> &[BotPage] {
        match self.create.as_deref() {
            Some(create) => create.form.pages_shown(),
            None => self.settings.pages_shown(),
        }
    }

    /// What the shown access choice allows, naming the home workspace.
    pub(super) fn access_summary(&self) -> String {
        let home = self.home_words();
        match self.access_shown() {
            BotAccess::Default => koru_i18n::fill(
                t(self.lang, Words::BotsAccessDefaultSummary),
                &[home.as_str()],
            ),
            BotAccess::Workspace => koru_i18n::fill(
                t(self.lang, Words::BotsAccessWorkspaceSummary),
                &[home.as_str()],
            ),
        }
    }

    /// **The access card** — the summary, the two approved choices, the
    /// selected pages with *Add a page…* — for the settings and, the same
    /// card, for the create form (S8). `save` is the settings' *Save*
    /// (whether it can send); the create form has none, its access going
    /// with *Create bot*. `pending` holds every control still.
    pub(super) fn access_card(
        &self,
        engine: &mut TextEngine,
        pending: bool,
        save: Option<bool>,
    ) -> El {
        let summary = self.access_summary();
        let selected = CHOICES
            .iter()
            .position(|choice| *choice == self.access_shown())
            .unwrap_or(0);
        let hovered = (self.hot == Some(ACCESS_CHOICE))
            .then_some(self.hot_segment)
            .flatten();
        let choice = leaf(
            "Bot access choice",
            ChoiceLeaf::new(engine, self.choice_labels(), selected, hovered, pending),
        )
        .semantic()
        .id(ACCESS_CHOICE)
        .hit()
        .focusable()
        .disabled(pending)
        .name(Words::BotsAccess);
        let mut card = card(ACCESS_CARD, Words::BotsAccess)
            .child(line(
                "Bot access summary",
                ACCESS_SUMMARY,
                summary,
                Ink::Primary,
            ))
            .child(row("Bot access row").child(choice).child(spacer()))
            .child(card_title(t(self.lang, Words::BotsSelectedPages)).id(SELECTED_PAGES_TITLE));
        for page in self.pages_shown() {
            let title = self.page_title(page);
            card = card.child(
                row("Bot selected page")
                    .gap(Space::S2)
                    .items_center()
                    .id(page_key(page))
                    .child(
                        text(title.clone())
                            .named("Bot selected page title")
                            .role(roles::ROW)
                            .fill()
                            .truncate(),
                    )
                    .child(self.button(
                        engine,
                        "Bot remove page",
                        remove_key(page),
                        koru_i18n::fill(t(self.lang, Words::BotsRemovePage), &[title.as_str()]),
                        ActionStyle::Ghost,
                        pending,
                    )),
            );
        }
        let mut actions = row("Bot access actions")
            .gap(Space::S2)
            .items_center()
            .child(self.button(
                engine,
                "Bot add page",
                ADD_PAGE,
                self.words(Words::BotsAddPage),
                ActionStyle::Secondary,
                pending,
            ))
            .child(spacer());
        actions = match save {
            None => actions,
            Some(_) if pending => actions.child(line(
                "Bot access pending",
                ACCESS_PENDING,
                self.words(Words::BotsSaving),
                Ink::Hint,
            )),
            Some(can_save) => actions.child(self.button(
                engine,
                "Bot save access",
                SAVE_ACCESS,
                self.words(Words::BotsSave),
                ActionStyle::Primary,
                !can_save,
            )),
        };
        card.child(actions)
    }

    fn folder_card(&self, engine: &mut TextEngine, folder: &str) -> El {
        let settings = &self.settings;
        let mut actions = row("Bot folder actions").gap(Space::S2).items_center();
        actions = if settings.is_pending(Change::Folder) {
            actions.child(line(
                "Bot folder pending",
                FOLDER_PENDING,
                self.words(Words::BotsSaving),
                Ink::Hint,
            ))
        } else if settings.folder_changeable() {
            // Inert, not hidden, while a change of the Bot is still being
            // delivered: the owner would refuse it.
            actions.child(self.button(
                engine,
                "Bot change folder",
                CHANGE_FOLDER,
                self.words(Words::BotsChangeFolder),
                ActionStyle::Secondary,
                !settings.can_change_folder(),
            ))
        } else {
            actions.child(
                line(
                    "Bot folder fixed",
                    FOLDER_FIXED,
                    self.words(Words::BotsFolderFixed),
                    Ink::Hint,
                )
                .fill(),
            )
        };
        card(FOLDER_CARD, Words::BotsFolder)
            .child(
                text(self.words(Words::BotsRunsOnThisComputer))
                    .named("Bot runs here")
                    .id(RUNS_HERE)
                    .role(roles::UI)
                    .ink(Ink::Primary),
            )
            .child(line(
                "Bot folder path",
                FOLDER_PATH,
                folder.to_owned(),
                Ink::Secondary,
            ))
            .child(actions)
    }

    fn pause_card(&self, engine: &mut TextEngine, paused: bool) -> El {
        let settings = &self.settings;
        let mut pause = row("Bot pause row").gap(Space::S2).items_center();
        pause = if paused {
            pause.child(
                line(
                    "Bot paused notice",
                    PAUSED_NOTICE,
                    self.words(Words::BotsPausedNotice),
                    Ink::Secondary,
                )
                .fill(),
            )
        } else {
            pause.child(spacer())
        };
        let waiting = if settings.is_pending(Change::Pause) {
            Some(Words::BotsPausing)
        } else if settings.is_pending(Change::Resume) {
            Some(Words::BotsResuming)
        } else {
            None
        };
        pause = match waiting {
            Some(words) => pause.child(line(
                "Bot pause pending",
                PAUSE_PENDING,
                self.words(words),
                Ink::Hint,
            )),
            None => pause.child(self.button(
                engine,
                "Bot pause",
                PAUSE,
                self.words(if paused {
                    Words::BotsResume
                } else {
                    Words::BotsPause
                }),
                ActionStyle::Secondary,
                // Inert while a change of the Bot is still being delivered.
                !settings.can_toggle_pause(),
            )),
        };
        card(PAUSE_CARD, Words::BotsPause).child(pause)
    }

    /// **The *Agent and model* card** (S7): the model's chip — the agent,
    /// the model and its level, as the chat's chip reads them — which opens
    /// the model picker; the mode's chip, which opens the approval-mode
    /// list; what the choice applies to (QUESTIONS O23); the agent's
    /// readiness when it is not ready; *Save*, or *Saving…* while the owner
    /// has not answered.
    ///
    /// **The create form has the same card** (S8) less its *Save* and its
    /// note: the choice goes with *Create bot*, and the card says instead
    /// that the agent is chosen once (QUESTIONS O42).
    pub(super) fn agent_card(&self, engine: &mut TextEngine) -> El {
        let creating = self.create.as_deref();
        let pending = match creating {
            Some(create) => !create.form.editable(),
            None => self.settings.is_pending(Change::Model),
        };
        let model = row("Bot model row").gap(Space::S2).items_center().child(
            self.button(
                engine,
                "Bot model chip",
                MODEL_CHIP,
                self.model_chip_words(),
                ActionStyle::Secondary,
                pending,
            )
            .fill(),
        );
        let mut card = card(AGENT_CARD, Words::BotsAgent).child(model);
        if let Some(mode) = self.mode_chip_row() {
            // **The mode chip wears the mode's color** (P1.8 §20, the
            // owner's decision): the mode's icon in the inks the list's row
            // for it is drawn in, from the one decision a chat's send button
            // wears (`modes::tone_of`, carried by the row).
            let inks = modes::elements::mode_inks(mode.tone, mode.full_access);
            let lead = Lead {
                glyph: mode.icon,
                ink: inks.icon,
                label: Some(inks.title),
            };
            card = card.child(
                row("Bot mode row")
                    .gap(Space::S2)
                    .items_center()
                    .child(
                        text(self.words(Words::AiAgentAsks))
                            .named("Bot mode label")
                            .role(roles::UI)
                            .ink(Ink::Secondary),
                    )
                    .child(
                        self.button_led(
                            engine,
                            "Bot mode chip",
                            MODE_CHIP,
                            mode.title,
                            ActionStyle::Secondary,
                            pending,
                            Some(lead),
                        )
                        .fill(),
                    ),
            );
        }
        if let Some(words) = self.agent_readiness_words() {
            card = card.child(line(
                "Bot agent readiness",
                AGENT_READINESS,
                words,
                Ink::Accent,
            ));
        }
        if creating.is_some() {
            return card.child(line(
                "Bot agent fixed",
                super::create::AGENT_FIXED_NOTE,
                self.words(Words::BotsAgentFixed),
                Ink::Secondary,
            ));
        }
        let save = row("Bot model save row")
            .gap(Space::S2)
            .items_center()
            .child(
                line(
                    "Bot model note",
                    MODEL_NOTE,
                    self.words(Words::BotsAppliesToNewConversations),
                    Ink::Secondary,
                )
                .fill(),
            )
            .child(if pending {
                line(
                    "Bot model pending",
                    MODEL_PENDING,
                    self.words(Words::BotsSaving),
                    Ink::Hint,
                )
            } else {
                self.button(
                    engine,
                    "Bot save model",
                    SAVE_MODEL,
                    self.words(Words::BotsSave),
                    ActionStyle::Primary,
                    !self.settings.can_save_model(),
                )
            });
        card.child(save)
    }

    /// The agent's name as the catalog has it, else its id's word.
    pub(super) fn agent_label(&self) -> String {
        self.agent_provider()
            .and_then(|provider| {
                self.catalog
                    .agents()
                    .iter()
                    .find(|agent| agent.id == provider)
            })
            .map_or_else(|| self.agent_name(), |agent| agent.label.clone())
    }

    /// **The model's chip**: *Codex, GPT-5, High* — the agent, the model as
    /// the picker names it (its *Default* by the agent's own word), and the
    /// level when one is chosen.
    pub(super) fn model_chip_words(&self) -> String {
        use koru_ai_ui::agent_catalog::config::picker::{DEFAULT_MODEL, inputs};
        let Some(draft) = self.agent_draft() else {
            // The create form before the catalog has offered an agent.
            return self.words(Words::BotsAgent);
        };
        let id = draft.model.requested_model.as_str();
        let listing = self.agent_listing();
        let model = match listing {
            Some(listing) if id == DEFAULT_MODEL => {
                inputs::default_model_label(&listing.records(), self.lang)
            }
            Some(listing) => match listing.find(id) {
                Some((listed, _)) => listed
                    .name
                    .clone()
                    .unwrap_or_else(|| inputs::fallback_label(&listed.id, &listed.group)),
                None => inputs::fallback_label(id, ""),
            },
            None if id == DEFAULT_MODEL => self.words(Words::AiModelAgentDefault),
            None => inputs::fallback_label(id, ""),
        };
        let with = t(self.lang, Words::AiPickerRowWith);
        let mut words = koru_i18n::fill(with, &[self.agent_label().as_str(), model.as_str()]);
        let effort = koru_ai_ui::agent_catalog::effort(draft.model.default_reasoning);
        if effort != koru_ai_ui::agent_catalog::config::effort::Effort::Default {
            let level = koru_ai_ui::agent_catalog::config::effort::word(self.lang, effort);
            words = koru_i18n::fill(with, &[words.as_str(), level.as_str()]);
        }
        words
    }

    /// **The mode's chip**, while the agent's modes are known: the mode the
    /// draft names, else the one the agent says it starts in, else (a mode
    /// the agent no longer lists) its first — its row, in Koru's words, with
    /// its icon and its color.
    pub(super) fn mode_chip_row(&self) -> Option<modes::ModeRow> {
        let provider = self.agent_provider()?;
        let listing = self.agent_listing()?;
        let rows = modes::rows(self.lang, provider.as_str(), &listing.offers());
        let shown = self.mode_shown();
        let at = rows
            .iter()
            .position(|row| Some(row.id.as_str()) == shown.as_deref())
            .unwrap_or(0);
        rows.into_iter().nth(at)
    }

    /// **The agent's readiness, when it is not ready**: said by its name,
    /// never replaced by another agent — not ready as the service said;
    /// still being checked, while the catalog asks; could not be told,
    /// once it stopped (`catalog::CHECKING_LIMIT`). Nothing before the
    /// agents are listed, and nothing for a ready one.
    pub(super) fn agent_readiness_words(&self) -> Option<String> {
        use crate::bots::catalog::Standing;
        let provider = self.agent_provider()?;
        if self.catalog.agents().is_empty() {
            return None;
        }
        let words = match self.catalog.standing(&provider) {
            Standing::Ready => return None,
            Standing::Checking => Words::BotsAgentChecking,
            Standing::NotReady(_) => Words::BotsAgentNotReady,
            Standing::Unknown => Words::BotsAgentReadinessUnknown,
        };
        Some(koru_i18n::fill(
            t(self.lang, words),
            &[self.agent_label().as_str()],
        ))
    }

    /// **The honest disclosures**, as plain sentences: where the Bot runs
    /// and whose account applies, what Koru's access does not reach, and the
    /// first version's agent.
    pub(super) fn disclosures(&self) -> El {
        let agent = self.agent_name();
        col("Bot disclosures")
            .gap(Space::S2)
            .child(line(
                "Bot runs on",
                RUNS_ON,
                koru_i18n::fill(t(self.lang, Words::BotsRunsOn), &[agent.as_str()]),
                Ink::Secondary,
            ))
            .child(line(
                "Bot runtime limits",
                RUNTIME_LIMITS,
                koru_i18n::fill(
                    t(self.lang, Words::BotsRuntimeLimits),
                    &[agent.as_str(), agent.as_str()],
                ),
                Ink::Secondary,
            ))
    }

    pub(super) fn choice_labels(&self) -> [String; 2] {
        [
            self.words(Words::BotsAccessChoiceDefault),
            self.words(Words::BotsAccessChoiceWorkspace),
        ]
    }

    /// The name field, made the first time the region asks for it, on the
    /// engine the caller holds (a layout is already inside the thread's
    /// input engine).
    pub(super) fn name_input_with(&self, engine: &mut TextEngine) -> Rc<RefCell<TextInput>> {
        self.name_field
            .borrow_mut()
            .get_or_insert_with(|| {
                Rc::new(RefCell::new(TextInput::with_text(
                    engine,
                    koru_draw::theme::role::row(),
                    self.settings.name_shown(),
                )))
            })
            .clone()
    }

    /// The name field, outside a layout.
    pub(super) fn name_input(&self) -> Rc<RefCell<TextInput>> {
        crate::input_engine::with(|engine| self.name_input_with(engine))
    }

    /// Put the owner's name in the field when the person has no edit of
    /// their own in it: a read or a saved name moves it, an edit never.
    pub(super) fn sync_name_field(&mut self) {
        // The create form's field holds the draft's name, which only the
        // person's typing moves (S8).
        if self.settings.name_edited() || self.create.is_some() {
            return;
        }
        let shown = self.settings.name_shown().to_owned();
        if let Some(input) = self.name_field.borrow().as_ref() {
            let mut input = input.borrow_mut();
            if input.raw_text() != shown {
                crate::input_engine::with(|engine| input.set_text(engine, &shown));
            }
        }
    }

    // ----------------------------------------------------------- acting

    /// What pressing `key` asks for, if anything.
    pub(super) fn act_for(&self, key: Key) -> Option<Act> {
        if let Some(act) = self.create_act_for(key) {
            return Some(act);
        }
        let act = match key {
            k if k == SETTINGS_BUTTON => Act::ToggleSettings,
            k if k == SAVE_NAME => Act::SaveName,
            k if k == OPEN_ROLE => Act::OpenRole,
            k if k == ADD_PAGE => Act::AddPage,
            k if k == SAVE_ACCESS => Act::SaveAccess,
            k if k == CHANGE_FOLDER => Act::ChangeFolder,
            k if k == PAUSE => Act::TogglePause,
            k if k == super::list::NEW_CONVERSATION => Act::NewConversation,
            k if k == super::list::MORE_CONVERSATIONS => Act::MoreConversations,
            k if k == super::actions::ACTIONS_BUTTON => Act::Actions,
            k if k == MODEL_CHIP => Act::OpenModelPicker,
            k if k == MODE_CHIP => Act::OpenModes,
            k if k == SAVE_MODEL => Act::SaveModel,
            k if k == super::actions::CONTINUE => Act::ContinueOperation,
            k if k == super::actions::CANCEL_OPERATION => Act::CancelOperation,
            k if k == super::actions::UNARCHIVE => Act::Unarchive,
            k if k == super::actions::RESTORE => Act::Restore,
            k if k.part == REMOVE_PART => Act::RemovePage(
                self.pages_shown()
                    .iter()
                    .find(|page| remove_key(page) == k)?
                    .clone(),
            ),
            _ => return None,
        };
        Some(act)
    }

    /// **Do what `act` asks.** A write goes to the link and is shown as
    /// pending until the owner answers; a request for the window (the role
    /// page, the folder dialog, the page picker) is left for it to take.
    /// `true` when anything changed.
    pub(crate) fn perform(&mut self, act: Act) -> bool {
        // The create form's acts, and the access card's while it is the
        // create form's (S8).
        if self.create.is_some() {
            return self.perform_create(act);
        }
        let command = match act {
            Act::ToggleSettings => {
                self.set_settings_open(!self.settings_open);
                return true;
            }
            Act::OpenRole => {
                let Some(bot) = self.settings.bot() else {
                    return false;
                };
                self.role_request = Some(bot.config.role.clone());
                return true;
            }
            Act::AddPage => {
                if self.settings.is_pending(Change::Access) || self.settings.bot().is_none() {
                    return false;
                }
                self.open_page_picker();
                return true;
            }
            Act::ChangeFolder => {
                if !self.settings.folder_changeable() {
                    return false;
                }
                let start = self
                    .settings
                    .bot()
                    .map(|bot| std::path::PathBuf::from(&bot.config.working_folder));
                self.folder_request = Some(start);
                return true;
            }
            Act::Choose(access) => {
                self.settings.choose_access(access);
                return true;
            }
            Act::RemovePage(page) => {
                let removed = self.settings.remove_page(&page);
                if removed && self.focus == Some(remove_key(&page)) {
                    // The row is gone: the keyboard stays in the card.
                    self.focus = Some(ADD_PAGE);
                }
                return removed;
            }
            Act::NewConversation => return self.new_conversation(),
            Act::MoreConversations => return self.more_conversations(),
            Act::Actions => {
                let Some(at) = self.actions_anchor() else {
                    return false;
                };
                // The keyboard comes back here when the menu, or the
                // dialog it leads to, closes.
                self.return_focus = self.focus;
                self.actions_request = Some(at);
                return true;
            }
            Act::OpenModelPicker => return self.open_agent_picker(),
            Act::OpenModes => return self.open_agent_modes(),
            Act::ContinueOperation => return self.continue_lifecycle(),
            Act::CancelOperation => return self.cancel_lifecycle(),
            Act::Unarchive => {
                return self.begin_lifecycle(crate::bots::lifecycle::Ask::Unarchive);
            }
            Act::Restore => return self.begin_lifecycle(crate::bots::lifecycle::Ask::Restore),
            Act::SaveName => self.settings.save_name(),
            Act::SaveAccess => self.settings.save_access(),
            Act::SaveModel => self.settings.save_model(),
            Act::TogglePause => self.settings.toggle_pause(),
            // Only the create form offers these.
            Act::Create
            | Act::Retry
            | Act::Abandon
            | Act::StartOver
            | Act::Example(_)
            | Act::ChooseRole(_)
            | Act::ChooseRolePage
            | Act::ChangeAccess
            | Act::ChooseFolder
            | Act::UseManagedFolder
            | Act::OpenLeftPage => return false,
        };
        match command {
            Some(command) => {
                self.send_write(command);
                true
            }
            None => false,
        }
    }

    /// Open or close the region. Opening it before the tab holds the Bot as
    /// the owner does asks for it. **Closing it gives the keyboard back to
    /// where it came from**: to *Settings* when the keyboard opened the
    /// region from there (`F6`, then Enter), and to the conversation
    /// otherwise — a pointer's press opened it, and the keyboard was the
    /// conversation's. (Opening always puts the keyboard on *Settings*, so
    /// where it is at the close says nothing about where it came from.)
    pub(crate) fn set_settings_open(&mut self, open: bool) {
        let from_header = self.focus == Some(SETTINGS_BUTTON);
        self.settings_open = open;
        if open {
            if self.settings.bot().is_none() {
                self.settings.owe_read();
            }
            self.sync_name_field();
            self.settings_return = from_header;
            self.focus = Some(SETTINGS_BUTTON);
            // The agents-and-models catalog, read while the settings show
            // (S6): the agents, their readiness, the Bot's agent's list.
            self.catalog.shown();
            if let Some(facts) = &self.facts {
                self.catalog.want(&facts.provider);
            }
        } else {
            let back = std::mem::take(&mut self.settings_return);
            self.focus = back.then_some(SETTINGS_BUTTON);
            self.hot = None;
            self.picker = None;
            self.agent = None;
            if let Some(input) = self.name_field.borrow().as_ref() {
                crate::input_engine::with(|engine| input.borrow_mut().blur(engine));
            }
        }
    }

    /// The control the keyboard is on: inside the *Agent and model* card's
    /// popover while one is up (S7), else the tab's own.
    pub(crate) fn focus(&self) -> Option<Key> {
        self.agent_semantics()
            .and_then(|(_, focus, _)| focus)
            .map(super::agent::published_key)
            .or(self.focus)
    }

    /// **The keyboard's order**: every focusable, enabled control in the
    /// frame, in tree order — the header's *Settings*, then the region's.
    pub(crate) fn focus_order(&self) -> Vec<Key> {
        self.with_frame(|_, frame| focus_order(frame))
    }

    fn step_focus(&mut self, back: bool) -> bool {
        let order = self.focus_order();
        if order.is_empty() {
            return false;
        }
        let at = self
            .focus
            .and_then(|key| order.iter().position(|k| *k == key));
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => order.len() - 1,
            (Some(i), false) => (i + 1) % order.len(),
            (Some(i), true) => (i + order.len() - 1) % order.len(),
        };
        self.focus = Some(order[next]);
        if self.focus == Some(super::list::CONVERSATION_LIST) {
            self.focus_list();
        }
        self.blur_fields_but(self.focus);
        self.reveal_focus();
        true
    }

    /// Scroll the region so the focused control shows.
    fn reveal_focus(&mut self) {
        let Some(key) = self.focus else {
            return;
        };
        let reveal = self.with_frame(|_, frame| {
            let viewport = frame.node(SETTINGS)?.viewport?;
            let rect = frame.node(key)?.rect;
            Some((viewport, rect))
        });
        if let Some((viewport, rect)) = reveal {
            if rect.y < viewport.y {
                self.scroll = (self.scroll - (viewport.y - rect.y)).max(0.0);
            } else if rect.y + rect.h > viewport.y + viewport.h {
                self.scroll += rect.y + rect.h - (viewport.y + viewport.h);
            }
        }
    }

    /// **A key while the region is open, or while the header's *Settings*
    /// has the keyboard.** `None` when the key is not the tab's: it goes to
    /// the conversation.
    pub(super) fn region_key(
        &mut self,
        key: &WinitKey,
        typed: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if !self.form_open() && self.focus.is_none() {
            return None;
        }
        // Tab walks the region's controls while it is open, and the
        // header's and the conversation column's once one of them has the
        // keyboard (`F6` landed on *Settings*). The create form is a region
        // that is always open (S8).
        if let WinitKey::Named(NamedKey::Tab) = key
            && (self.form_open() || self.focus.is_some())
        {
            return Some(self.step_focus(mods.shift_key()));
        }
        if let WinitKey::Named(NamedKey::Escape) = key {
            if self.settings_open {
                self.set_settings_open(false);
            } else {
                self.release_keyboard();
            }
            return Some(true);
        }
        let focus = self.focus?;
        if let Some(handled) = self.create_field_key(focus, key, typed, mods) {
            return Some(handled);
        }
        if focus == NAME_FIELD {
            if let WinitKey::Named(NamedKey::Enter) = key {
                return Some(self.perform(Act::SaveName));
            }
            let input = self.name_input();
            let handled = crate::input_engine::with(|engine| {
                input.borrow_mut().key(engine, key, typed, mods)
            });
            let text = input.borrow().raw_text().to_owned();
            self.settings.edit_name(&text);
            return Some(matches!(handled, Typed::Handled));
        }
        let activate =
            matches!(key, WinitKey::Named(NamedKey::Enter | NamedKey::Space)) || typed == Some(" ");
        if focus == ACCESS_CHOICE {
            let step = match key {
                WinitKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => true,
                _ => activate,
            };
            if step {
                let next = match self.access_shown() {
                    BotAccess::Default => BotAccess::Workspace,
                    BotAccess::Workspace => BotAccess::Default,
                };
                return Some(self.perform(Act::Choose(next)));
            }
            return Some(false);
        }
        if focus == super::create::ROLE_CHOICE {
            let step = match key {
                WinitKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => true,
                _ => activate,
            };
            return Some(step && self.perform(Act::ChooseRole(self.other_role())));
        }
        if activate && let Some(act) = self.act_for(focus) {
            return Some(self.perform(act));
        }
        if !self.form_open() {
            // *Settings* had the keyboard and the key is the conversation's.
            self.focus = None;
            return None;
        }
        Some(false)
    }

    /// **A press on the tab's own controls**, in the tab's coordinates.
    /// `None` when the press is not on one.
    pub(super) fn region_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let hit = self.with_frame(|_, frame| {
            let key = frame.hit_at(x, y)?;
            Some((key, frame.node(key).map(|node| node.rect)))
        });
        let Some((key, rect)) = hit else {
            if self.form_open() {
                self.release_keyboard();
                return Some(false);
            }
            return None;
        };
        self.pressed = Some(key);
        if let Some(handled) = self.create_field_press(key, rect, x, y) {
            return Some(handled);
        }
        if key == NAME_FIELD {
            self.focus = Some(NAME_FIELD);
            let input = self.name_input();
            if let Some(rect) = rect {
                crate::input_engine::with(|engine| input.borrow_mut().click(engine, rect, x, y));
            }
            return Some(true);
        }
        self.blur_fields();
        // A pointer press does not put the keyboard's ring on a button.
        self.focus = None;
        if key == super::create::ROLE_CHOICE {
            let labels = self.role_labels();
            let segment = rect.and_then(|rect| {
                crate::input_engine::with(|engine| {
                    super::controls::segment_at(engine, &labels, rect, x, y)
                })
            });
            return Some(match segment.and_then(|i| super::create::ROLES.get(i)) {
                Some(role) => self.perform(Act::ChooseRole(*role)),
                None => false,
            });
        }
        if key == ACCESS_CHOICE {
            let labels = self.choice_labels();
            let segment = rect.and_then(|rect| {
                crate::input_engine::with(|engine| {
                    super::controls::segment_at(engine, &labels, rect, x, y)
                })
            });
            return Some(match segment.and_then(|i| CHOICES.get(i)) {
                Some(access) => self.perform(Act::Choose(*access)),
                None => false,
            });
        }
        if key == SETTINGS {
            // The region's scrollbar gutter, or its empty space.
            return Some(false);
        }
        let disabled =
            self.with_frame(|_, frame| frame.node(key).is_some_and(|node| node.disabled));
        if disabled {
            return Some(false);
        }
        Some(match self.act_for(key) {
            Some(act) => self.perform(act),
            None => false,
        })
    }

    /// The pointer over the tab's controls: what is lit, and a drag in the
    /// name field. `true` when what is lit changed.
    pub(super) fn region_move(&mut self, x: f32, y: f32) -> bool {
        let (hot, rect) = self.with_frame(|_, frame| {
            let key = frame.hit_at(x, y);
            (
                key,
                key.and_then(|key| frame.node(key).map(|node| node.rect)),
            )
        });
        let segment = match (hot, rect) {
            (Some(key), Some(rect)) if key == ACCESS_CHOICE => {
                let labels = self.choice_labels();
                crate::input_engine::with(|engine| {
                    super::controls::segment_at(engine, &labels, rect, x, y)
                })
            }
            (Some(key), Some(rect)) if key == super::create::ROLE_CHOICE => {
                let labels = self.role_labels();
                crate::input_engine::with(|engine| {
                    super::controls::segment_at(engine, &labels, rect, x, y)
                })
            }
            _ => None,
        };
        self.create_field_drag(x, y);
        if self.pressed == Some(NAME_FIELD)
            && let Some(field) = self.with_frame(|_, frame| frame.node(NAME_FIELD).map(|n| n.rect))
        {
            let input = self.name_input();
            crate::input_engine::with(|engine| input.borrow_mut().drag(engine, field, x));
        }
        let changed = self.hot != hot || self.hot_segment != segment;
        self.hot = hot;
        self.hot_segment = segment;
        changed
    }

    pub(super) fn region_release(&mut self) {
        self.pressed = None;
        if let Some(input) = self.name_field.borrow().as_ref() {
            input.borrow_mut().release();
        }
        self.create_field_release();
    }

    /// Scroll the region by `dy` logical units; layout clamps it.
    pub(super) fn scroll_region(&mut self, dy: f32) {
        let max = self.with_frame(|_, frame| {
            frame
                .node(SETTINGS)
                .and_then(|node| node.scroll)
                .zip(frame.node(SETTINGS).and_then(|node| node.viewport))
                .map_or(0.0, |((extent, _), viewport)| {
                    (extent - viewport.h).max(0.0)
                })
        });
        self.scroll = (self.scroll + dy).clamp(0.0, max);
        // The person moved the form: a press no longer owes keeping its
        // actions in view.
        self.create_scrolled();
    }

    // ------------------------------------------------- the window's asks

    /// The Role page the person asked to open, for the window to open as an
    /// ordinary page tab. Taken once.
    pub(crate) fn take_role_request(&mut self) -> Option<BotPage> {
        self.role_request.take()
    }

    /// The folder dialog the person asked for — where it starts — for the
    /// window's existing folder dialog. Taken once.
    pub(crate) fn take_folder_request(&mut self) -> Option<Option<std::path::PathBuf>> {
        self.folder_request.take()
    }

    /// The folder dialog answered: the change goes to the owner, and is
    /// shown as pending until it answers. The keyboard stays where it was.
    pub(crate) fn folder_chosen(&mut self, folder: &std::path::Path) -> bool {
        match self.settings.folder_chosen(folder) {
            Some(command) => {
                self.send_write(command);
                true
            }
            None => false,
        }
    }

    /// The titles of the pages the region lists, as the window knows them.
    pub(crate) fn set_page_titles(
        &mut self,
        titles: std::collections::HashMap<koru_ai_core::DocumentId, String>,
    ) -> bool {
        if self.page_titles == titles {
            return false;
        }
        self.page_titles = titles;
        true
    }

    /// The pages the region lists: what the window looks titles up for.
    pub(crate) fn listed_pages(&self) -> Vec<BotPage> {
        let mut pages = self.pages_shown().to_vec();
        // The create form's existing role page too (S8), so its title shows.
        if let Some(page) = self
            .create
            .as_deref()
            .and_then(|create| create.form.draft().existing.clone())
        {
            pages.push(page);
        }
        pages
    }

    // ---------------------------------------------------- accessibility

    /// **The tab's controls for the accessibility tree**: the header's
    /// *Settings*, then — while the region is open — the region as a group,
    /// each card a group, and in each its lines and controls, in the
    /// keyboard's order. The field publishes its text; the access choice
    /// publishes its two segments as radio buttons, the chosen one
    /// selected.
    pub(crate) fn controls(&self) -> Vec<Control> {
        let labels = self.choice_labels();
        // Laid out on its own first: the list's rows come from the shared
        // list's geometry, which reads the tab's frame itself.
        let list = if self.form_open() {
            Vec::new()
        } else {
            self.list_controls()
        };
        self.with_frame(|_, frame| {
            // Each published node, and the published node it nests under:
            // its nearest published ancestor in the frame.
            let mut arena: Vec<(Control, Option<usize>)> = Vec::new();
            let mut published: Vec<Option<usize>> = vec![None; frame.nodes.len()];
            let label = |name: &Option<koru_draw::el::Name>| match name {
                Some(koru_draw::el::Name::Key(words)) => t(self.lang, *words).to_owned(),
                Some(koru_draw::el::Name::Text(content)) => content.clone(),
                None => String::new(),
            };
            for (index, node) in frame.nodes.iter().enumerate() {
                let Some(key) = node.key else {
                    continue;
                };
                if !self.form_open()
                    && key != SETTINGS_BUTTON
                    && key != super::list::NEW_CONVERSATION
                    && key != super::list::MORE_CONVERSATIONS
                    && key != super::list::TRASH_NOTICE
                    && !Self::is_lifecycle_control(key)
                {
                    continue;
                }
                let role = if key == NAME_FIELD || super::create::FIELDS.contains(&key) {
                    ControlRole::TextInput
                } else if key == ACCESS_CHOICE || key == super::create::ROLE_CHOICE {
                    ControlRole::Group
                } else {
                    match node.role {
                        Some(Role::Button) => ControlRole::Button,
                        Some(Role::Group) => ControlRole::Group,
                        _ if node.lines().is_some() => ControlRole::Label,
                        _ => continue,
                    }
                };
                let words = match role {
                    ControlRole::Label => self.line_words(key),
                    _ => Some(label(&node.label)),
                };
                let Some(words) = words else {
                    continue;
                };
                let mut parent = node.parent;
                let mut nests = None;
                while let Some(at) = parent {
                    if let Some(slot) = published[at] {
                        nests = Some(slot);
                        break;
                    }
                    parent = frame.nodes[at].parent;
                }
                let focused = self.focus == Some(key);
                published[index] = Some(arena.len());
                let slot = arena.len();
                arena.push((
                    Control {
                        key,
                        rect: node.rect,
                        role,
                        label: words,
                        value: self.field_value(key),
                        selected: node.selected,
                        disabled: node.disabled,
                        focused,
                        children: Vec::new(),
                    },
                    nests,
                ));
                if key == ACCESS_CHOICE {
                    let rects = crate::input_engine::with(|engine| {
                        super::controls::segments(engine, &labels, node.rect)
                    });
                    for (((rect, words), choice), segment) in rects
                        .into_iter()
                        .zip(&labels)
                        .zip(CHOICES)
                        .zip(ACCESS_SEGMENTS)
                    {
                        arena.push((
                            Control {
                                key: segment,
                                rect,
                                role: ControlRole::Radio,
                                label: words.clone(),
                                value: None,
                                selected: Some(self.access_shown() == choice),
                                disabled: node.disabled,
                                focused,
                                children: Vec::new(),
                            },
                            Some(slot),
                        ));
                    }
                }
                // Where the new Bot's role comes from (S8): its two
                // segments as radio buttons, the chosen one selected.
                if key == super::create::ROLE_CHOICE {
                    let labels = self.role_labels();
                    let rects = crate::input_engine::with(|engine| {
                        super::controls::segments(engine, &labels, node.rect)
                    });
                    let shown = self.role_shown();
                    for (((rect, words), role), segment) in rects
                        .into_iter()
                        .zip(&labels)
                        .zip(super::create::ROLES)
                        .zip(super::create::ROLE_SEGMENTS)
                    {
                        arena.push((
                            Control {
                                key: segment,
                                rect,
                                role: ControlRole::Radio,
                                label: words.clone(),
                                value: None,
                                selected: Some(shown == Some(role)),
                                disabled: node.disabled,
                                focused,
                                children: Vec::new(),
                            },
                            Some(slot),
                        ));
                    }
                }
            }
            // Nest back to front, so a child is complete before it moves.
            let mut roots = Vec::new();
            let mut built: Vec<Option<Control>> = Vec::with_capacity(arena.len());
            let parents: Vec<Option<usize>> = arena.iter().map(|(_, parent)| *parent).collect();
            built.extend(arena.into_iter().map(|(control, _)| Some(control)));
            for index in (0..built.len()).rev() {
                let control = built[index].take().expect("each once");
                match parents[index] {
                    Some(parent) => built[parent]
                        .as_mut()
                        .expect("a parent precedes its children")
                        .children
                        .insert(0, control),
                    None => roots.insert(0, control),
                }
            }
            // The card's popover while one is up (S7): the control's own
            // nodes, under the chip that opened it.
            if let Some((nodes, focus, search)) = self.agent_semantics() {
                let lang = self.lang;
                roots.extend(
                    nodes
                        .iter()
                        .map(|node| super::agent::control_of(node, focus, search.as_deref(), lang)),
                );
            }
            if !self.form_open() {
                // The conversation list, after *New conversation* and before
                // *Show more*, in the keyboard's order (S8).
                let more = roots
                    .iter()
                    .position(|control| control.key == super::list::MORE_CONVERSATIONS)
                    .unwrap_or(roots.len());
                roots.splice(more..more, list);
            }
            roots
        })
    }

    /// The words of a sentence in the region, by its key.
    fn line_words(&self, key: Key) -> Option<String> {
        if key == super::list::TRASH_NOTICE {
            return self.list_notice().map(|words| self.words(words));
        }
        if self.create.is_some() {
            return self.create_line_words(key);
        }
        let settings = &self.settings;
        let bot = settings.bot()?;
        let agent = self.agent_name();
        Some(match key {
            k if k == SETTINGS_NOTICE => self.words(settings.notice_words()?),
            k if k == SETTINGS_DELIVERING => self.words(settings.delivering_line()?),
            k if k == ACCESS_SUMMARY => self.access_summary(),
            k if k == SELECTED_PAGES_TITLE => self.words(Words::BotsSelectedPages),
            k if k == RUNS_HERE => self.words(Words::BotsRunsOnThisComputer),
            k if k == MODEL_NOTE => self.words(Words::BotsAppliesToNewConversations),
            k if k == AGENT_READINESS => return self.agent_readiness_words(),
            k if k == NAME_PENDING
                || k == ACCESS_PENDING
                || k == FOLDER_PENDING
                || k == MODEL_PENDING =>
            {
                self.words(Words::BotsSaving)
            }
            k if k == PAUSE_PENDING => self.words(if settings.is_pending(Change::Resume) {
                Words::BotsResuming
            } else {
                Words::BotsPausing
            }),
            k if k == FOLDER_PATH => bot.config.working_folder.clone(),
            k if k == FOLDER_FIXED => self.words(Words::BotsFolderFixed),
            k if k == PAUSED_NOTICE => self.words(Words::BotsPausedNotice),
            k if k == RUNS_ON => {
                koru_i18n::fill(t(self.lang, Words::BotsRunsOn), &[agent.as_str()])
            }
            k if k == RUNTIME_LIMITS => koru_i18n::fill(
                t(self.lang, Words::BotsRuntimeLimits),
                &[agent.as_str(), agent.as_str()],
            ),
            _ => return None,
        })
    }

    /// Do what an assistive technology asked of the control `key`.
    pub(crate) fn accessibility_action(&mut self, key: Key, click: bool) -> bool {
        // **An inert control is inert to an assistive technology too**, as
        // it is to the pointer (`region_press`) — checked before any path
        // acts: a segment by its choice (the access choice is held still
        // while a save of it is on its way), the list's and the popovers'
        // controls by their own node.
        if click && self.inert_to_assistive(key) {
            return false;
        }
        if let Some(done) = self.agent_accessibility_action(key, click) {
            return done;
        }
        if let Some(done) = self.list_action(key, click) {
            return done;
        }
        if let Some(index) = ACCESS_SEGMENTS.iter().position(|segment| *segment == key) {
            if !self.with_frame(|_, frame| frame.contains(ACCESS_CHOICE)) {
                return false;
            }
            self.focus = Some(ACCESS_CHOICE);
            return !click || self.perform(Act::Choose(CHOICES[index]));
        }
        if let Some(index) = super::create::ROLE_SEGMENTS
            .iter()
            .position(|segment| *segment == key)
        {
            if !self.with_frame(|_, frame| frame.contains(super::create::ROLE_CHOICE)) {
                return false;
            }
            self.focus = Some(super::create::ROLE_CHOICE);
            return !click || self.perform(Act::ChooseRole(super::create::ROLES[index]));
        }
        if !self.with_frame(|_, frame| frame.contains(key)) {
            return false;
        }
        if !click {
            self.focus = Some(key);
            return true;
        }
        if key == super::create::ROLE_CHOICE {
            return self.perform(Act::ChooseRole(self.other_role()));
        }
        if key == ACCESS_CHOICE {
            let next = match self.access_shown() {
                BotAccess::Default => BotAccess::Workspace,
                BotAccess::Workspace => BotAccess::Default,
            };
            return self.perform(Act::Choose(next));
        }
        match self.act_for(key) {
            Some(act) => self.perform(act),
            None => false,
        }
    }
}

impl BotView {
    /// Whether `key` is drawn inert: its own node, or — for a segment of
    /// the access or role choice — the choice it belongs to.
    pub(super) fn inert_to_assistive(&self, key: Key) -> bool {
        let owner = if ACCESS_SEGMENTS.contains(&key) {
            Some(ACCESS_CHOICE)
        } else if super::create::ROLE_SEGMENTS.contains(&key) {
            Some(super::create::ROLE_CHOICE)
        } else {
            None
        };
        self.with_frame(|_, frame| {
            [Some(key), owner]
                .into_iter()
                .flatten()
                .any(|key| frame.node(key).is_some_and(|node| node.disabled))
        })
    }
}

/// Every focusable, enabled, keyed element of `frame`, in tree order.
pub(crate) fn focus_order(frame: &Frame) -> Vec<Key> {
    frame
        .nodes
        .iter()
        .filter(|node| node.focusable && !node.disabled)
        .filter_map(|node| node.key)
        .collect()
}
