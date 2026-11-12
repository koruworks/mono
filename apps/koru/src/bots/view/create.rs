// SPDX-License-Identifier: MPL-2.0

//! **Creating a Bot in its tab** (Bots P1.8 S8): *Create a bot* — from the
//! roster's heading or its empty state — opens a Bot tab in create mode, a
//! form in the conversation's place, built from the settings' own cards.
//!
//! ```text
//! scroll_y  Bot create                         the region (keyed as the settings' viewport)
//!   col  Bot create content                    NAV_PAD at the sides, S3 above and below, S4 between
//!     col  Bot settings card  (Name)           the name
//!     Bot settings title  (Standing role)
//!     col  Bot settings card  (Standing role)  write a role | use an existing page; the title,
//!                                              the text, the examples and what they do — or the page
//!     Bot settings title  (Agent and model)
//!     col  Bot settings card  (Agent and model)  the settings' card: chips, readiness, *fixed once made*
//!     Bot settings title  (Access)
//!     col  Bot settings card  (Access)         the default in words and *Change access*, which
//!                                              reveals the settings' own access card
//!     Bot settings title  (Working folder)
//!     col  Bot settings card  (Working folder) managed in words, or the chosen path; what the
//!                                              owner bound; *Choose…*, *Use a managed folder*
//!     col  Bot disclosures                     the settings' own
//!     col  Bot create ended                    a rejected or abandoned creation, and what it left
//!     col  Bot create progress                 a creation the owner holds: where it stands,
//!                                              the unconfirmed role page, Retry, Abandon
//!     Bot create notice                        a refusal's sentence, by its code
//!     row  Bot create unconfirmed actions      *Start over*
//!     row  Bot create actions                  what the form refuses, in words; *Create bot*
//! ```
//!
//! **Everything a press comes to is drawn where it was pressed**, at the
//! form's foot beside *Create bot*, and the form keeps it and the actions
//! row in view as answers arrive (until the person scrolls).
//!
//! **The draft is [`CreateForm`]'s** ([`crate::bots::creation`]); this file
//! draws it, takes the person's keys and presses into it, and carries its
//! commands to the tab's own link thread. **Reused, not copied**: the access
//! card, the *Agent and model* card and its popovers, the page picker, the
//! keyboard's walk, the accessibility tree and F12's records are the
//! settings' own.
//!
//! **When it finishes** the tab becomes the Bot's tab on its main
//! conversation, in place — the same tab — from the Bot the owner returned
//! ([`BotView::become_created`]), never an identity made up here.

use std::cell::RefCell;
use std::rc::Rc;

use koru_ai_core::bots::{Bot, BotCreation, BotCreationStage, BotId};
use koru_draw::el::{El, Key, Role, col, leaf, row, scroll_y, spacer, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, alias, roles};
use koru_i18n::{Key as Words, t};
use koru_store_core::ids::WorkspaceId;
use koru_ui::button::ActionStyle;
use koru_ui::text_field::{TextArea, Typed};
use koru_ui::text_input::TextInput;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::controls::{AreaLeaf, ChoiceLeaf, FieldLeaf};
use super::region::{
    ABOUT_CARD, ACCESS_CARD, ACCESS_SUMMARY, Act, FOLDER_CARD, FOLDER_PATH, NAME_FIELD, SETTINGS,
    card, card_title, line,
};
use super::{BotView, SURFACE};
use crate::bots::creation::{CreateForm, Example, NEW_BOT_PERMISSION, RoleKind, Sent};

/// The form's sentence over its cards: a refusal, by its code.
pub(crate) const CREATE_NOTICE: Key = Key::surface(SURFACE, "create-notice");
pub(crate) const ROLE_CARD: Key = Key::surface(SURFACE, "role-card");
/// Where the role comes from, and its two segments.
pub(crate) const ROLE_CHOICE: Key = Key::surface(SURFACE, "role-choice");
pub(crate) const ROLE_SEGMENTS: [Key; 2] = [
    Key::surface(SURFACE, "role-write"),
    Key::surface(SURFACE, "role-existing"),
];
/// The two role choices, in the order the track shows them.
pub(crate) const ROLES: [RoleKind; 2] = [RoleKind::Authored, RoleKind::Existing];
pub(crate) const TITLE_FIELD: Key = Key::surface(SURFACE, "role-title");
pub(crate) const TEXT_FIELD: Key = Key::surface(SURFACE, "role-text");
/// The form's text fields beside the name: what takes typing.
pub(crate) const FIELDS: [Key; 2] = [TITLE_FIELD, TEXT_FIELD];
pub(crate) const ROLE_HINT: Key = Key::surface(SURFACE, "role-hint");
pub(crate) const EXAMPLE_OWN: Key = Key::surface(SURFACE, "example-own");
pub(crate) const EXAMPLE_RESEARCH: Key = Key::surface(SURFACE, "example-research");
pub(crate) const EXAMPLE_ASSESSMENT: Key = Key::surface(SURFACE, "example-assessment");
pub(crate) const EXAMPLES_NOTE: Key = Key::surface(SURFACE, "examples-note");
pub(crate) const CHOOSE_ROLE_PAGE: Key = Key::surface(SURFACE, "choose-role-page");
pub(crate) const ROLE_PAGE: Key = Key::surface(SURFACE, "role-page");
/// The *Agent and model* card's sentence that the agent is chosen once.
pub(crate) const AGENT_FIXED_NOTE: Key = Key::surface(SURFACE, "agent-fixed");
pub(crate) const CHANGE_ACCESS: Key = Key::surface(SURFACE, "change-access");
pub(crate) const CHOOSE_FOLDER: Key = Key::surface(SURFACE, "choose-folder");
pub(crate) const USE_MANAGED_FOLDER: Key = Key::surface(SURFACE, "use-managed-folder");
pub(crate) const FOLDER_RESOLVED: Key = Key::surface(SURFACE, "folder-resolved");
pub(crate) const CREATE_BUTTON: Key = Key::surface(SURFACE, "create");
/// The form's last row: what it refuses, and *Create bot* or *Creating…*.
pub(crate) const CREATE_ACTIONS: Key = Key::surface(SURFACE, "create-actions");
pub(crate) const CREATING: Key = Key::surface(SURFACE, "creating");
/// What the form refuses before anything is sent, in words.
pub(crate) const CREATE_PROBLEM: Key = Key::surface(SURFACE, "create-problem");
/// Where a creation the owner holds stands: its reason, then what Retry
/// or Abandon can do.
pub(crate) const STAGE_LINES: [Key; 2] = [
    Key::surface(SURFACE, "create-stage"),
    Key::surface(SURFACE, "create-stage-more"),
];
pub(crate) const ROLE_UNCONFIRMED: Key = Key::surface(SURFACE, "role-unconfirmed");
pub(crate) const RETRY: Key = Key::surface(SURFACE, "retry");
/// *Start over*, once Koru could not confirm whether the bot was made.
pub(crate) const START_OVER: Key = Key::surface(SURFACE, "start-over");
pub(crate) const ABANDON: Key = Key::surface(SURFACE, "abandon");
/// A rejected or abandoned creation's sentences, in order: what happened,
/// then what it left — the role page, the folder — or that it left nothing.
pub(crate) const ENDED_LINES: [Key; 3] = [
    Key::surface(SURFACE, "create-ended"),
    Key::surface(SURFACE, "create-ended-more"),
    Key::surface(SURFACE, "create-ended-last"),
];
pub(crate) const OPEN_LEFT_PAGE: Key = Key::surface(SURFACE, "open-left-page");

/// **A create tab's own state**: the draft's model, its two text fields
/// beside the name (the settings' name field serves both), whether *Change
/// access* has revealed the choices, and what the window owes the tab.
/// Boxed in the tab, which is moved by value.
pub(crate) struct CreateTab {
    pub(crate) form: CreateForm,
    title_field: RefCell<Option<Rc<RefCell<TextInput>>>>,
    text_field: RefCell<Option<Rc<RefCell<TextArea>>>>,
    access_open: bool,
    /// The person pressed Abandon: the window asks before anything is sent.
    abandon_ask: bool,
    /// **The person pressed one of the form's own actions** — *Create bot*,
    /// *Retry*, *Start over*, a confirmed *Abandon* — and what it comes to
    /// is drawn beside it: the form keeps that and the actions row in view
    /// as answers arrive, until the person scrolls it themselves.
    reveal_owed: bool,
}

impl CreateTab {
    fn new(home: WorkspaceId) -> Self {
        Self {
            form: CreateForm::new(home),
            title_field: RefCell::new(None),
            text_field: RefCell::new(None),
            access_open: false,
            abandon_ask: false,
            reveal_owed: false,
        }
    }
}

impl BotView {
    /// **A tab that creates a Bot** in `home`, named `home_name` by the
    /// window: the form, a fresh draft, and nothing sent. Its conversation
    /// slot holds a placeholder no one is told: the window finds a create
    /// tab by its draft's BotId, and [`Self::conversation_id`] answers none.
    pub(crate) fn creating(home: WorkspaceId, home_name: Option<String>) -> Self {
        let mut view = Self::blank(None, koru_ai_core::ChatId::new());
        view.create = Some(Box::new(CreateTab::new(home)));
        view.home_name = home_name;
        view
    }

    /// Whether this tab is a create form.
    pub(crate) fn is_creating(&self) -> bool {
        self.create.is_some()
    }

    /// The BotId a create tab's draft reserves.
    pub(crate) fn create_bot(&self) -> Option<BotId> {
        Some(self.create.as_deref()?.form.bot())
    }

    /// A create tab with nothing sent yet: the one the window brings
    /// forward rather than open a second beside it.
    pub(crate) fn create_untouched(&self) -> bool {
        self.create
            .as_deref()
            .is_some_and(|create| create.form.untouched())
    }

    /// The form being drawn in the conversation's place: the settings, or
    /// the create form, which is always open.
    pub(crate) fn form_open(&self) -> bool {
        self.settings_open || self.create.is_some()
    }

    /// The home workspace: the Bot's, or the create form's.
    pub(crate) fn home(&self) -> Option<WorkspaceId> {
        match self.create.as_deref() {
            Some(create) => Some(create.form.home()),
            None => self.facts.as_ref().map(|facts| facts.home),
        }
    }

    /// What the tab strip calls a create tab: the name typed so far, else
    /// *New bot*.
    pub(super) fn create_label(&self) -> Option<String> {
        let create = self.create.as_deref()?;
        let name = create.form.draft().name.trim();
        Some(if name.is_empty() {
            self.words(Words::BotsNewBot)
        } else {
            name.to_owned()
        })
    }

    pub(super) fn role_labels(&self) -> [String; 2] {
        [
            self.words(Words::BotsRoleWrite),
            self.words(Words::BotsRoleExisting),
        ]
    }

    pub(super) fn role_shown(&self) -> Option<RoleKind> {
        Some(self.create.as_deref()?.form.draft().role)
    }

    pub(super) fn other_role(&self) -> RoleKind {
        match self.role_shown() {
            Some(RoleKind::Authored) => RoleKind::Existing,
            _ => RoleKind::Authored,
        }
    }

    /// **The mode a new Bot starts in**: its agent's asking mode
    /// ([`NEW_BOT_PERMISSION`]), from the agent's own listed modes, never
    /// the person's default permission. `None` while the modes are not
    /// known, which leaves the runtime's own start.
    pub(super) fn asking_mode(&self) -> Option<String> {
        use koru_ai_ui::agent_catalog::config::modes;
        let provider = self.agent_provider()?;
        let listing = self.agent_listing()?;
        modes::default_mode(
            provider.as_str(),
            &listing.offers(),
            listing.default_mode.as_deref(),
            NEW_BOT_PERMISSION,
            NEW_BOT_PERMISSION,
        )
    }

    /// A text field's text, for the accessibility tree.
    pub(super) fn field_value(&self, key: Key) -> Option<String> {
        if let Some(create) = self.create.as_deref() {
            let draft = create.form.draft();
            return match key {
                k if k == NAME_FIELD => Some(draft.name.clone()),
                k if k == TITLE_FIELD => Some(create.form.title_shown()),
                k if k == TEXT_FIELD => Some(draft.text.clone()),
                _ => None,
            };
        }
        (key == NAME_FIELD).then(|| self.settings.name_shown().to_owned())
    }

    fn title_input_with(&self, engine: &mut TextEngine) -> Option<Rc<RefCell<TextInput>>> {
        let create = self.create.as_deref()?;
        let title = create.form.title_shown();
        Some(
            create
                .title_field
                .borrow_mut()
                .get_or_insert_with(|| {
                    Rc::new(RefCell::new(TextInput::with_text(
                        engine,
                        koru_draw::theme::role::row(),
                        &title,
                    )))
                })
                .clone(),
        )
    }

    fn text_input_with(&self, engine: &mut TextEngine) -> Option<Rc<RefCell<TextArea>>> {
        let create = self.create.as_deref()?;
        let text = create.form.draft().text.clone();
        Some(
            create
                .text_field
                .borrow_mut()
                .get_or_insert_with(|| {
                    let mut area = TextArea::new(engine, koru_draw::theme::role::row());
                    area.set_text(engine, &text);
                    Rc::new(RefCell::new(area))
                })
                .clone(),
        )
    }

    /// Put the draft's title in its field when the person is not typing
    /// there: while it follows the name, the name moves it.
    fn sync_title_field(&self) {
        let Some(create) = self.create.as_deref() else {
            return;
        };
        if self.focus == Some(TITLE_FIELD) {
            return;
        }
        let shown = create.form.title_shown();
        if let Some(input) = create.title_field.borrow().as_ref() {
            let mut input = input.borrow_mut();
            if input.raw_text() != shown {
                crate::input_engine::with(|engine| input.set_text(engine, &shown));
            }
        }
    }

    /// Put the draft's text in its field: an example or *Start your own*
    /// filled it.
    fn sync_text_field(&self) {
        let Some(create) = self.create.as_deref() else {
            return;
        };
        let shown = create.form.draft().text.clone();
        if let Some(input) = create.text_field.borrow().as_ref() {
            let mut input = input.borrow_mut();
            if input.raw_text() != shown {
                crate::input_engine::with(|engine| input.set_text(engine, &shown));
            }
        }
    }

    /// No text field has the caret.
    pub(super) fn blur_fields(&self) {
        self.blur_fields_but(None);
    }

    /// Every text field but `keep` gives up its caret.
    pub(super) fn blur_fields_but(&self, keep: Option<Key>) {
        if keep != Some(NAME_FIELD)
            && let Some(input) = self.name_field.borrow().as_ref()
        {
            crate::input_engine::with(|engine| input.borrow_mut().blur(engine));
        }
        if keep != Some(TITLE_FIELD)
            && let Some(create) = self.create.as_deref()
            && let Some(input) = create.title_field.borrow().as_ref()
        {
            crate::input_engine::with(|engine| input.borrow_mut().blur(engine));
        }
    }

    // ----------------------------------------------------------- frame

    /// **The create form as elements**, in the conversation's place.
    pub(super) fn create_region(&self, engine: &mut TextEngine) -> El {
        let Some(create) = self.create.as_deref() else {
            return col("Bot create");
        };
        let form = &create.form;
        let locked = !form.editable();
        let mut content = col("Bot create content")
            .pad_x(alias::NAV_PAD)
            .pad_y(Space::S3)
            .gap(Space::S4);
        // The name, in the settings' own field.
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
                .disabled(locked)
                .name(Words::BotsName),
            );
        content = content
            .child(card(ABOUT_CARD, Words::BotsName).child(name))
            .child(card_title(t(self.lang, Words::BotsRole)))
            .child(self.role_card(engine, create))
            .child(card_title(t(self.lang, Words::BotsAgent)))
            .child(self.agent_card(engine))
            .child(card_title(t(self.lang, Words::BotsAccess)))
            .child(if create.access_open {
                self.access_card(engine, locked, None)
            } else {
                card(ACCESS_CARD, Words::BotsAccess)
                    .child(line(
                        "Bot access summary",
                        ACCESS_SUMMARY,
                        self.access_summary(),
                        Ink::Primary,
                    ))
                    .child(row("Bot access actions").child(self.button(
                        engine,
                        "Bot change access",
                        CHANGE_ACCESS,
                        self.words(Words::BotsChangeAccess),
                        ActionStyle::Secondary,
                        locked,
                    )))
            })
            .child(card_title(t(self.lang, Words::BotsFolder)))
            .child(self.create_folder_card(engine, create))
            .child(self.disclosures());
        // **What the last press came to, beside the button that was
        // pressed** — never only at the top of a form scrolled to its end
        // (the first window run's finding 3; the second run's finding 2: an
        // unfinished creation's reason, Retry and Abandon were drawn 226 px
        // above the view while the person was at *Create bot*): a creation
        // the owner ended or holds, its reason, Retry and Abandon; a
        // refusal's sentence; and *Start over* when Koru could not confirm
        // whether the bot was made.
        if let Some(ended) = form.ended() {
            content = content.child(self.ended_block(engine, ended));
        }
        if let Some(live) = form.live() {
            content = content.child(self.progress_block(engine, live));
        }
        if let Some(notice) = form.notice() {
            content = content.child(line(
                "Bot create notice",
                CREATE_NOTICE,
                self.create_notice_words(notice),
                Ink::Accent,
            ));
        }
        if form.can_start_over() {
            content = content.child(
                row("Bot create unconfirmed actions")
                    .gap(Space::S2)
                    .items_center()
                    .child(self.button(
                        engine,
                        "Bot start over",
                        START_OVER,
                        self.words(Words::BotsCreateStartOver),
                        ActionStyle::Secondary,
                        false,
                    ))
                    .child(spacer()),
            );
        }
        let mut actions = row("Bot create actions")
            .id(CREATE_ACTIONS)
            .gap(Space::S2)
            .items_center();
        actions = match self.create_problem() {
            Some(problem) if form.editable() => actions.child(
                line(
                    "Bot create problem",
                    CREATE_PROBLEM,
                    self.words(problem),
                    Ink::Secondary,
                )
                .fill(),
            ),
            _ => actions.child(spacer()),
        };
        actions = if form.in_flight() == Some(Sent::Create) {
            actions.child(line(
                "Bot creating",
                CREATING,
                self.words(Words::BotsCreating),
                Ink::Hint,
            ))
        } else if form.live().is_none() {
            actions.child(self.button(
                engine,
                "Bot create",
                CREATE_BUTTON,
                self.words(Words::BotsCreateAction),
                ActionStyle::Primary,
                !self.can_create(),
            ))
        } else {
            actions
        };
        scroll_y("Bot create")
            .id(SETTINGS)
            .offset(self.scroll)
            .scrollbar()
            .fill()
            .semantics(Role::Group)
            .name(Words::BotsCreate)
            .child(content.child(actions))
    }

    /// **The standing role**: write one — its title, its text, the
    /// examples that only fill the text in — or use a page that exists.
    fn role_card(&self, engine: &mut TextEngine, create: &CreateTab) -> El {
        let form = &create.form;
        let locked = !form.editable();
        let draft = form.draft();
        let selected = ROLES
            .iter()
            .position(|kind| *kind == draft.role)
            .unwrap_or(0);
        let hovered = (self.hot == Some(ROLE_CHOICE))
            .then_some(self.hot_segment)
            .flatten();
        let choice = leaf(
            "Bot role choice",
            ChoiceLeaf::new(engine, self.role_labels(), selected, hovered, locked),
        )
        .semantic()
        .id(ROLE_CHOICE)
        .hit()
        .focusable()
        .disabled(locked)
        .name(Words::BotsRole);
        let mut card = card(ROLE_CARD, Words::BotsRole)
            .child(row("Bot role choice row").child(choice).child(spacer()));
        match draft.role {
            RoleKind::Authored => {
                let title = row("Bot role title row")
                    .gap(Space::S2)
                    .items_center()
                    .child(
                        text(self.words(Words::BotsRoleTitle))
                            .named("Bot role title label")
                            .role(roles::UI)
                            .ink(Ink::Secondary),
                    );
                let title = match self.title_input_with(engine) {
                    Some(input) => title.child(
                        leaf(
                            "Bot role title field",
                            FieldLeaf {
                                input,
                                placeholder: self.words(Words::BotsRoleTitle),
                                focused: self.focus == Some(TITLE_FIELD),
                            },
                        )
                        .semantic()
                        .fill()
                        .id(TITLE_FIELD)
                        .hit()
                        .focusable()
                        .disabled(locked)
                        .name(Words::BotsRoleTitle),
                    ),
                    None => title,
                };
                card = card.child(title);
                if let Some(input) = self.text_input_with(engine) {
                    card = card.child(
                        leaf(
                            "Bot role text",
                            AreaLeaf {
                                input,
                                placeholder: self.words(Words::BotsRoleText),
                                focused: self.focus == Some(TEXT_FIELD),
                            },
                        )
                        .semantic()
                        .id(TEXT_FIELD)
                        .hit()
                        .focusable()
                        .disabled(locked)
                        .name(Words::BotsRoleText),
                    );
                }
                card = card
                    .child(line(
                        "Bot role hint",
                        ROLE_HINT,
                        self.words(Words::BotsRoleHint),
                        Ink::Secondary,
                    ))
                    .child(
                        row("Bot role examples")
                            .gap(Space::S2)
                            .items_center()
                            .child(self.button(
                                engine,
                                "Bot example own",
                                EXAMPLE_OWN,
                                self.words(Words::BotsStartYourOwn),
                                ActionStyle::Secondary,
                                locked,
                            ))
                            .child(self.button(
                                engine,
                                "Bot example research",
                                EXAMPLE_RESEARCH,
                                self.words(Words::BotsExampleResearch),
                                ActionStyle::Ghost,
                                locked,
                            ))
                            .child(self.button(
                                engine,
                                "Bot example assessment",
                                EXAMPLE_ASSESSMENT,
                                self.words(Words::BotsExampleAssessment),
                                ActionStyle::Ghost,
                                locked,
                            ))
                            .child(spacer()),
                    )
                    .child(line(
                        "Bot examples note",
                        EXAMPLES_NOTE,
                        self.words(Words::BotsExamplesNote),
                        Ink::Hint,
                    ));
            }
            RoleKind::Existing => {
                card = card.child(
                    row("Bot role page row")
                        .gap(Space::S2)
                        .items_center()
                        .child(
                            line(
                                "Bot role page",
                                ROLE_PAGE,
                                self.role_page_words(),
                                Ink::Primary,
                            )
                            .fill(),
                        )
                        .child(self.button(
                            engine,
                            "Bot choose role page",
                            CHOOSE_ROLE_PAGE,
                            self.words(Words::BotsChooseRolePage),
                            ActionStyle::Secondary,
                            locked,
                        )),
                );
            }
        }
        card
    }

    /// The existing role page by its title, or that none is chosen.
    fn role_page_words(&self) -> String {
        match self
            .create
            .as_deref()
            .and_then(|create| create.form.draft().existing.as_ref())
        {
            Some(page) => self.page_title(page),
            None => self.words(Words::BotsNoRolePage),
        }
    }

    /// **The working folder**: managed by default, said in words, or the
    /// chosen path; the folder the owner bound when it differs; *Choose…*
    /// through the window's folder dialog, and the managed one again.
    fn create_folder_card(&self, engine: &mut TextEngine, create: &CreateTab) -> El {
        let form = &create.form;
        let locked = !form.editable();
        let mut card = card(FOLDER_CARD, Words::BotsFolder)
            .child(
                text(self.words(Words::BotsRunsOnThisComputer))
                    .named("Bot runs here")
                    .id(super::region::RUNS_HERE)
                    .role(roles::UI)
                    .ink(Ink::Primary),
            )
            .child(line(
                "Bot folder path",
                FOLDER_PATH,
                self.folder_words(),
                Ink::Secondary,
            ));
        if let Some(resolved) = self.resolved_words() {
            card = card.child(line(
                "Bot folder resolved",
                FOLDER_RESOLVED,
                resolved,
                Ink::Secondary,
            ));
        }
        let mut actions = row("Bot folder actions")
            .gap(Space::S2)
            .items_center()
            .child(self.button(
                engine,
                "Bot choose folder",
                CHOOSE_FOLDER,
                self.words(Words::BotsFolderChoose),
                ActionStyle::Secondary,
                locked,
            ));
        if form.draft().folder.is_some() {
            actions = actions.child(self.button(
                engine,
                "Bot use managed folder",
                USE_MANAGED_FOLDER,
                self.words(Words::BotsFolderUseManaged),
                ActionStyle::Ghost,
                locked,
            ));
        }
        card.child(actions.child(spacer()))
    }

    fn folder_words(&self) -> String {
        match self
            .create
            .as_deref()
            .and_then(|create| create.form.draft().folder.clone())
        {
            Some(path) => path,
            None => self.words(Words::BotsFolderManaged),
        }
    }

    fn resolved_words(&self) -> Option<String> {
        let resolved = self.create.as_deref()?.form.resolved_folder()?;
        Some(koru_i18n::fill(
            t(self.lang, Words::BotsFolderResolved),
            &[resolved],
        ))
    }

    /// **Where a creation the owner holds stands**: its sentences, the
    /// role page while it is unconfirmed, *Retry* when Retry can resolve
    /// it, *Abandon* while it has not finished.
    fn progress_block(&self, engine: &mut TextEngine, live: &BotCreation) -> El {
        let mut block = col("Bot create progress").gap(Space::S2);
        for (words, key) in crate::bots::creation::stage_words(live)
            .into_iter()
            .zip(STAGE_LINES)
        {
            block = block.child(line(
                "Bot create stage",
                key,
                self.words(words),
                Ink::Accent,
            ));
        }
        if live.role_page.is_some() && live.role_page_unconfirmed {
            block = block.child(line(
                "Bot role unconfirmed",
                ROLE_UNCONFIRMED,
                self.words(Words::BotsRolePageUnconfirmed),
                Ink::Secondary,
            ));
        }
        let Some(create) = self.create.as_deref() else {
            return block;
        };
        let form = &create.form;
        let mut actions = row("Bot create progress actions")
            .gap(Space::S2)
            .items_center();
        if live.retryable {
            actions = actions.child(self.button(
                engine,
                "Bot retry",
                RETRY,
                self.words(Words::BotsRetry),
                ActionStyle::Primary,
                form.in_flight().is_some(),
            ));
        }
        if form.can_abandon() || form.in_flight() == Some(Sent::Abandon) {
            actions = actions.child(self.button(
                engine,
                "Bot abandon",
                ABANDON,
                self.words(Words::BotsAbandon),
                ActionStyle::Secondary,
                form.in_flight().is_some(),
            ));
        }
        block.child(actions.child(spacer()))
    }

    /// **A creation the owner ended**: refused before it made anything,
    /// with why; or abandoned, naming what it left — the role page (which
    /// may be unconfirmed) with *Open role page*, and the folder.
    fn ended_block(&self, engine: &mut TextEngine, ended: &BotCreation) -> El {
        let mut block = col("Bot create ended").gap(Space::S2);
        for (words, key) in self.ended_lines(ended).into_iter().zip(ENDED_LINES) {
            block = block.child(line("Bot create ended line", key, words, Ink::Secondary));
        }
        let left_page = ended
            .left_behind
            .as_ref()
            .and_then(|left| left.role_page.as_ref());
        if left_page.is_some() {
            block = block.child(row("Bot left page actions").child(self.button(
                engine,
                "Bot open left page",
                OPEN_LEFT_PAGE,
                self.words(Words::BotsOpenRole),
                ActionStyle::Secondary,
                false,
            )));
        }
        block
    }

    /// The ended creation's sentences, in order: what happened, then the
    /// page it left, then the folder.
    fn ended_lines(&self, ended: &BotCreation) -> Vec<String> {
        if ended.stage == BotCreationStage::Rejected {
            return crate::bots::creation::stage_words(ended)
                .into_iter()
                .map(|words| self.words(words))
                .collect();
        }
        abandoned_lines(self.lang, ended)
    }

    /// A refusal's sentence, the agent named where it is about the agent.
    pub(super) fn create_notice_words(&self, notice: Words) -> String {
        if crate::bots::creation::names_the_agent(notice) {
            // Every `{}` is the agent: one sentence names it twice.
            let agent = self.agent_label();
            koru_i18n::fill(t(self.lang, notice), &[agent.as_str(), agent.as_str()])
        } else {
            self.words(notice)
        }
    }

    /// **The first thing the form refuses, in words**: the draft's own
    /// limits, then *No agent is set up on this computer yet…* once every
    /// agent offered is known not to be able to run — the design's own
    /// refusal for it (DESIGN.md, *Creating a bot*), said before anything
    /// is sent rather than as the owner's refusal after it. One agent not
    /// ready while another can run is named on the card and not refused:
    /// the person may mean to set it up (the first window run's finding 3).
    pub(super) fn create_problem(&self) -> Option<Words> {
        let form = &self.create.as_deref()?.form;
        form.problem().or_else(|| {
            self.catalog
                .none_can_run(&self.agent_settings)
                .then_some(Words::BotsAgentRequired)
        })
    }

    /// Whether *Create bot* would send: nothing refused, nothing on its way.
    pub(super) fn can_create(&self) -> bool {
        self.create
            .as_deref()
            .is_some_and(|create| create.form.editable())
            && self.create_problem().is_none()
    }

    /// The words of one of the form's own lines, by its key.
    pub(super) fn create_line_words(&self, key: Key) -> Option<String> {
        let create = self.create.as_deref()?;
        let form = &create.form;
        if let Some(index) = STAGE_LINES.iter().position(|line| *line == key) {
            let words = crate::bots::creation::stage_words(form.live()?);
            return words.get(index).map(|words| self.words(*words));
        }
        if let Some(index) = ENDED_LINES.iter().position(|line| *line == key) {
            return self.ended_lines(form.ended()?).get(index).cloned();
        }
        let agent = self.agent_name();
        Some(match key {
            k if k == CREATE_NOTICE => self.create_notice_words(form.notice()?),
            k if k == CREATE_PROBLEM => self.words(self.create_problem()?),
            k if k == CREATING => self.words(Words::BotsCreating),
            k if k == ROLE_UNCONFIRMED => self.words(Words::BotsRolePageUnconfirmed),
            k if k == ROLE_HINT => self.words(Words::BotsRoleHint),
            k if k == EXAMPLES_NOTE => self.words(Words::BotsExamplesNote),
            k if k == ROLE_PAGE => self.role_page_words(),
            k if k == AGENT_FIXED_NOTE => self.words(Words::BotsAgentFixed),
            k if k == super::region::AGENT_READINESS => return self.agent_readiness_words(),
            k if k == ACCESS_SUMMARY => self.access_summary(),
            k if k == super::region::SELECTED_PAGES_TITLE => self.words(Words::BotsSelectedPages),
            k if k == super::region::RUNS_HERE => self.words(Words::BotsRunsOnThisComputer),
            k if k == FOLDER_PATH => self.folder_words(),
            k if k == FOLDER_RESOLVED => return self.resolved_words(),
            k if k == super::region::RUNS_ON => {
                koru_i18n::fill(t(self.lang, Words::BotsRunsOn), &[agent.as_str()])
            }
            k if k == super::region::RUNTIME_LIMITS => koru_i18n::fill(
                t(self.lang, Words::BotsRuntimeLimits),
                &[agent.as_str(), agent.as_str()],
            ),
            _ => return None,
        })
    }

    // ---------------------------------------------------------- acting

    /// What pressing one of the form's own controls asks for.
    pub(super) fn create_act_for(&self, key: Key) -> Option<Act> {
        self.create.as_ref()?;
        Some(match key {
            k if k == CREATE_BUTTON => Act::Create,
            k if k == RETRY => Act::Retry,
            k if k == ABANDON => Act::Abandon,
            k if k == START_OVER => Act::StartOver,
            k if k == EXAMPLE_OWN => Act::Example(Example::StartOwn),
            k if k == EXAMPLE_RESEARCH => Act::Example(Example::Research),
            k if k == EXAMPLE_ASSESSMENT => Act::Example(Example::Assessment),
            k if k == CHOOSE_ROLE_PAGE => Act::ChooseRolePage,
            k if k == CHANGE_ACCESS => Act::ChangeAccess,
            k if k == CHOOSE_FOLDER => Act::ChooseFolder,
            k if k == USE_MANAGED_FOLDER => Act::UseManagedFolder,
            k if k == OPEN_LEFT_PAGE => Act::OpenLeftPage,
            _ => return None,
        })
    }

    /// **Do what `act` asks of the create form.** A command goes to the
    /// tab's link and is shown as on its way until the owner answers; a
    /// request for the window — the folder dialog, the page picker, the
    /// Abandon question, a page to open — is left for it to take.
    pub(super) fn perform_create(&mut self, act: Act) -> bool {
        let lang = self.lang;
        let Some(create) = self.create.as_deref_mut() else {
            return false;
        };
        let command = match act {
            Act::Create => {
                // Inert while no agent offered can run: said beside it.
                if !self.can_create() {
                    return false;
                }
                let asking = self.asking_mode();
                let Some(create) = self.create.as_deref_mut() else {
                    return false;
                };
                create.form.create(asking)
            }
            Act::Retry => create.form.retry(),
            Act::StartOver => {
                let started = create.form.start_over();
                // The control is gone: the keyboard goes on to *Create bot*.
                if started && self.focus == Some(START_OVER) {
                    self.focus = Some(CREATE_BUTTON);
                }
                if started {
                    self.reveal_create_actions();
                }
                return started;
            }
            Act::Abandon => {
                if !create.form.can_abandon() {
                    return false;
                }
                create.abandon_ask = true;
                // The keyboard comes back here when the question closes.
                self.return_focus = self.focus;
                return true;
            }
            Act::Example(example) => {
                let words = example.text().map_or("", |words| t(lang, words));
                let filled = create.form.fill_text(words);
                self.sync_text_field();
                return filled;
            }
            Act::ChooseRole(role) => return create.form.choose_role(role),
            Act::ChooseRolePage => {
                if !create.form.editable() {
                    return false;
                }
                self.open_role_picker();
                return true;
            }
            Act::AddPage => {
                if !create.form.editable() {
                    return false;
                }
                self.open_page_picker();
                return true;
            }
            Act::Choose(access) => {
                create.form.choose_access(access);
                return true;
            }
            Act::RemovePage(page) => {
                let removed = create.form.remove_page(&page);
                if removed && self.focus == Some(super::region::remove_key(&page)) {
                    self.focus = Some(super::region::ADD_PAGE);
                }
                return removed;
            }
            Act::ChangeAccess => {
                if !create.form.editable() || create.access_open {
                    return false;
                }
                create.access_open = true;
                // The keyboard goes on to the choices it revealed.
                if self.focus == Some(CHANGE_ACCESS) {
                    self.focus = Some(super::region::ACCESS_CHOICE);
                }
                return true;
            }
            Act::ChooseFolder => {
                if !create.form.editable() {
                    return false;
                }
                let start = create.form.draft().folder.clone().map(Into::into);
                self.folder_request = Some(start);
                return true;
            }
            Act::UseManagedFolder => {
                let used = create.form.use_managed_folder();
                if used && self.focus == Some(USE_MANAGED_FOLDER) {
                    self.focus = Some(CHOOSE_FOLDER);
                }
                return used;
            }
            Act::OpenLeftPage => {
                let page = create
                    .form
                    .ended()
                    .and_then(|ended| ended.left_behind.as_ref())
                    .and_then(|left| left.role_page.clone());
                self.role_request = page;
                return self.role_request.is_some();
            }
            Act::OpenModelPicker => return self.open_agent_picker(),
            Act::OpenModes => return self.open_agent_modes(),
            // Nothing else is the create form's.
            _ => return false,
        };
        match command {
            Some(command) => {
                self.send_create(command);
                self.reveal_create_actions();
                true
            }
            None => false,
        }
    }

    /// **Keep what the last press came to, and the actions row, in view**
    /// from now on, as answers arrive (until the person scrolls).
    fn reveal_create_actions(&mut self) {
        if let Some(create) = self.create.as_deref_mut() {
            create.reveal_owed = true;
        }
        self.keep_create_actions_in_view();
    }

    /// The person scrolled the form: what they look at is theirs.
    pub(super) fn create_scrolled(&mut self) {
        if let Some(create) = self.create.as_deref_mut() {
            create.reveal_owed = false;
        }
    }

    /// **Scroll the form so the actions row shows**, and above it what the
    /// last press came to — the ended or unfinished creation's first line,
    /// the refusal — as much of it as fits with the actions row in view
    /// (the second window run: a sentence inserted above the actions row
    /// pushed *Create bot* below the clip, and the scroll stayed where it
    /// was). Only while a press of the person's owes it.
    fn keep_create_actions_in_view(&mut self) {
        if !self
            .create
            .as_deref()
            .is_some_and(|create| create.reveal_owed)
        {
            return;
        }
        let placed = self.with_frame(|_, frame| {
            let region = frame.node(SETTINGS)?;
            let viewport = region.viewport?;
            let extent = region.scroll.map_or(0.0, |(extent, _)| extent);
            let actions = frame.node(CREATE_ACTIONS)?.rect;
            let top = [ENDED_LINES[0], STAGE_LINES[0], CREATE_NOTICE]
                .into_iter()
                .filter_map(|key| frame.node(key).map(|node| node.rect.y))
                .fold(actions.y, f32::min);
            Some((viewport, extent, top, actions.y + actions.h))
        });
        let Some((viewport, extent, mut top, mut bottom)) = placed else {
            return;
        };
        let mut scroll = self.scroll;
        let below = bottom - (viewport.y + viewport.h);
        if below > 0.0 {
            scroll += below;
            top -= below;
            bottom -= below;
        }
        if top < viewport.y {
            let room = (viewport.y + viewport.h) - bottom;
            scroll -= (viewport.y - top).min(room).max(0.0);
        }
        self.scroll = scroll.clamp(0.0, (extent - viewport.h).max(0.0));
    }

    /// Hand a creation command to the link, by the draft's request and
    /// BotId; one the link will not take is shown as not sent.
    fn send_create(&mut self, command: koru_ai_core::bots::BotCommand) {
        let Some(create) = self.create.as_deref_mut() else {
            return;
        };
        let (bot, request) = (create.form.bot(), create.form.request());
        let sent = self.link.as_ref().is_some_and(|link| {
            link.send(super::super::link::Command::Write {
                bot,
                request,
                command: Box::new(command),
            })
        });
        let mut work = self.work.get();
        work.commands += usize::from(sent);
        self.work.set(work);
        if !sent && let Some(create) = self.create.as_deref_mut() {
            create.form.unsent();
        }
    }

    /// **The person confirmed Abandon** in the window's question: sent now.
    pub(crate) fn abandon_confirmed(&mut self) -> bool {
        let Some(command) = self
            .create
            .as_deref_mut()
            .and_then(|create| create.form.abandon())
        else {
            return false;
        };
        self.send_create(command);
        self.reveal_create_actions();
        true
    }

    /// The Abandon question the person asked for: the draft's request and
    /// the name it is asked by, for the window. Taken once.
    pub(crate) fn take_abandon_ask(&mut self) -> Option<String> {
        let create = self.create.as_deref_mut()?;
        if !std::mem::take(&mut create.abandon_ask) {
            return None;
        }
        Some(
            create
                .form
                .live()
                .map(|live| live.name.clone())
                .unwrap_or_else(|| create.form.draft().name.clone()),
        )
    }

    /// The folder dialog answered the create form: the folder joins the
    /// draft; nothing is sent. The keyboard stays where it was.
    pub(crate) fn create_folder_chosen(&mut self, folder: &std::path::Path) -> bool {
        self.create
            .as_deref_mut()
            .is_some_and(|create| create.form.choose_folder(folder))
    }

    // ----------------------------------------------------------- keys

    /// **A key on one of the form's text fields.** `None` when the
    /// keyboard is not on one of them, or this is not a create form.
    pub(super) fn create_field_key(
        &mut self,
        focus: Key,
        key: &WinitKey,
        typed: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let create = self.create.as_deref()?;
        if !create.form.editable() && (focus == NAME_FIELD || FIELDS.contains(&focus)) {
            return Some(false);
        }
        if focus == NAME_FIELD {
            // Enter does not create: a name is not the last thing to set.
            if let WinitKey::Named(NamedKey::Enter) = key {
                return Some(false);
            }
            let input = self.name_input();
            let handled = crate::input_engine::with(|engine| {
                input.borrow_mut().key(engine, key, typed, mods)
            });
            let text = input.borrow().raw_text().to_owned();
            if let Some(create) = self.create.as_deref_mut() {
                create.form.edit_name(&text);
            }
            // The title follows the name until the person types one.
            self.sync_title_field();
            return Some(matches!(handled, Typed::Handled));
        }
        if focus == TITLE_FIELD {
            if let WinitKey::Named(NamedKey::Enter) = key {
                return Some(false);
            }
            let input = crate::input_engine::with(|engine| self.title_input_with(engine))?;
            let handled = crate::input_engine::with(|engine| {
                input.borrow_mut().key(engine, key, typed, mods)
            });
            let text = input.borrow().raw_text().to_owned();
            if let Some(create) = self.create.as_deref_mut() {
                create.form.edit_title(&text);
            }
            return Some(matches!(handled, Typed::Handled));
        }
        if focus == TEXT_FIELD {
            let input = crate::input_engine::with(|engine| self.text_input_with(engine))?;
            let handled = crate::input_engine::with(|engine| {
                input.borrow_mut().key(engine, key, typed, mods)
            });
            let text = input.borrow().raw_text().to_owned();
            if let Some(create) = self.create.as_deref_mut() {
                create.form.edit_text(&text);
            }
            return Some(matches!(handled, Typed::Handled));
        }
        None
    }

    /// **A client set a text field's value** — the name (the create form's
    /// or the settings'), the role page's title or its text — as typing it
    /// would: the field shows it and the draft holds it. The fields are
    /// published with a value, so a client may send this whatever they
    /// advertise; they once ignored it (the first window run's finding 8).
    /// `false` for any other key, a field not drawn, or one held inert.
    pub(crate) fn accessibility_set_value(&mut self, key: Key, text: &str) -> bool {
        if (key != NAME_FIELD && !FIELDS.contains(&key))
            || self.inert_to_assistive(key)
            || !self.with_frame(|_, frame| frame.contains(key))
        {
            return false;
        }
        if let Some(create) = self.create.as_deref()
            && !create.form.editable()
        {
            return false;
        }
        if key == NAME_FIELD {
            let input = self.name_input();
            crate::input_engine::with(|engine| input.borrow_mut().set_text(engine, text));
            match self.create.as_deref_mut() {
                Some(create) => {
                    create.form.edit_name(text);
                    // The title follows the name until one is typed.
                    self.sync_title_field();
                }
                None => self.settings.edit_name(text),
            }
            return true;
        }
        if key == TITLE_FIELD {
            if let Some(input) = crate::input_engine::with(|engine| self.title_input_with(engine)) {
                crate::input_engine::with(|engine| input.borrow_mut().set_text(engine, text));
            }
            if let Some(create) = self.create.as_deref_mut() {
                create.form.edit_title(text);
            }
            return true;
        }
        if let Some(input) = crate::input_engine::with(|engine| self.text_input_with(engine)) {
            crate::input_engine::with(|engine| input.borrow_mut().set_text(engine, text));
        }
        if let Some(create) = self.create.as_deref_mut() {
            create.form.edit_text(text);
        }
        true
    }

    /// **A press on one of the form's text fields**: the caret goes where
    /// it landed. `None` when it is not one of them.
    pub(super) fn create_field_press(
        &mut self,
        key: Key,
        rect: Option<Rect>,
        x: f32,
        y: f32,
    ) -> Option<bool> {
        let create = self.create.as_deref()?;
        if !create.form.editable() && (key == NAME_FIELD || FIELDS.contains(&key)) {
            return Some(false);
        }
        let rect = rect?;
        if key == NAME_FIELD {
            self.blur_fields_but(Some(NAME_FIELD));
            self.focus = Some(NAME_FIELD);
            let input = self.name_input();
            crate::input_engine::with(|engine| input.borrow_mut().click(engine, rect, x, y));
            return Some(true);
        }
        if key == TITLE_FIELD {
            self.blur_fields_but(Some(TITLE_FIELD));
            self.focus = Some(TITLE_FIELD);
            let input = crate::input_engine::with(|engine| self.title_input_with(engine))?;
            crate::input_engine::with(|engine| input.borrow_mut().click(engine, rect, x, y));
            return Some(true);
        }
        if key == TEXT_FIELD {
            self.blur_fields_but(Some(TEXT_FIELD));
            self.focus = Some(TEXT_FIELD);
            let input = crate::input_engine::with(|engine| self.text_input_with(engine))?;
            crate::input_engine::with(|engine| input.borrow_mut().click(engine, rect, x, y));
            return Some(true);
        }
        None
    }

    /// A drag in the field the press landed in.
    pub(super) fn create_field_drag(&mut self, x: f32, y: f32) {
        let Some(pressed) = self.pressed.filter(|key| FIELDS.contains(key)) else {
            return;
        };
        if self.create.is_none() {
            return;
        }
        let Some(rect) = self.with_frame(|_, frame| frame.node(pressed).map(|node| node.rect))
        else {
            return;
        };
        crate::input_engine::with(|engine| {
            if pressed == TITLE_FIELD {
                if let Some(input) = self.title_input_with(engine) {
                    input.borrow_mut().drag(engine, rect, x);
                }
            } else if let Some(input) = self.text_input_with(engine) {
                input.borrow_mut().drag(engine, rect, x, y);
            }
        });
    }

    pub(super) fn create_field_release(&mut self) {
        let Some(create) = self.create.as_deref() else {
            return;
        };
        if let Some(input) = create.title_field.borrow().as_ref() {
            input.borrow_mut().release();
        }
        if let Some(input) = create.text_field.borrow().as_ref() {
            input.borrow_mut().release();
        }
    }

    // ----------------------------------------------------- the link side

    /// **The pass a shown create tab runs**: the link and the catalog the
    /// first time, an agent for the draft once the catalog offers one, the
    /// read the creation owes, and whatever came back. `true` when a
    /// repaint is owed.
    pub(super) fn create_pass(&mut self, root: Option<std::path::PathBuf>) -> bool {
        if !self.started_link {
            self.started_link = true;
            self.link = super::BotsLink::start(root.clone());
            self.catalog.shown();
            if let Some(create) = self.create.as_deref_mut() {
                create.form.shown();
            }
        }
        if std::mem::take(&mut self.stale) {
            self.catalog.shown();
            if let Some(create) = self.create.as_deref_mut() {
                create.form.shown();
            }
        }
        let catalog = self.catalog.pass(root);
        let seeded = self.seed_agent();
        let answered = self.poll();
        let read = self.send_create_read(std::time::Instant::now());
        self.settle_agent_refreshes();
        catalog || seeded || answered || read
    }

    /// **The agent a new Bot starts on**, once the catalog has said which
    /// runtimes there are: the person's default agent unless it is turned
    /// off, else the first ready one, else the first — shown with its
    /// readiness, never swapped behind the person's back. Its *Default*
    /// model, at its own level, mode and Fast.
    fn seed_agent(&mut self) -> bool {
        let Some(create) = self.create.as_deref() else {
            return false;
        };
        if create.form.model_shown().is_some() || !create.form.editable() {
            return false;
        }
        let Some(provider) = self.catalog.preferred(&self.agent_settings) else {
            return false;
        };
        self.catalog.want(&provider);
        let draft = crate::bots::settings::ModelDraft {
            model: koru_ai_core::ModelSelection {
                provider,
                requested_model: koru_ai_ui::agent_catalog::config::picker::DEFAULT_MODEL
                    .to_owned(),
                default_reasoning: koru_ai_core::ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        };
        if let Some(create) = self.create.as_deref_mut() {
            create.form.edit_model(draft);
        }
        true
    }

    /// The creation read owed by `now`, to the link. `true` when one went.
    pub(super) fn send_create_read(&mut self, now: std::time::Instant) -> bool {
        let Some(command) = self
            .create
            .as_deref_mut()
            .and_then(|create| create.form.read(now))
        else {
            return false;
        };
        self.send_create(command);
        true
    }

    /// **What the owner said about this draft's creation**. `true` when it
    /// was about it.
    pub(super) fn create_answer(
        &mut self,
        request: koru_ai_core::bots::BotRequestId,
        result: Result<koru_ai_core::bots::BotResponse, super::Failure>,
    ) -> bool {
        let now = std::time::Instant::now();
        let Some(create) = self.create.as_deref_mut() else {
            return false;
        };
        let before = (create.form.live().cloned(), create.form.ended().cloned());
        let settled = create.form.answer(request, result, now);
        // Where the creation stands now, when the answer moved it: the
        // roster's Unfinished group is told at once.
        let live = create.form.live().cloned();
        let ended = create.form.ended().cloned();
        if live.is_some() && live != before.0 {
            self.creation_news = live.map(Box::new);
        } else if ended.is_some() && ended != before.1 {
            self.creation_news = ended.map(Box::new);
        }
        let Some(create) = self.create.as_deref_mut() else {
            return settled.matched;
        };
        let finished = create
            .form
            .live()
            .is_some_and(|live| live.stage == BotCreationStage::Completed);
        match settled.created {
            Some(bot) => self.become_created(bot),
            // Made, but its Bot could not be read with the answer: the tab
            // says so, and the roster — which lists it — is read again.
            None if finished => self.created = true,
            None => {}
        }
        // *Create bot* left with the keyboard on it — the creation stopped
        // unfinished: the keyboard goes on to what replaced it, beside it.
        if self.focus == Some(CREATE_BUTTON)
            && let Some(live) = self.create.as_deref().and_then(|create| create.form.live())
            && live.stage == BotCreationStage::Incomplete
        {
            self.focus = Some(if live.retryable { RETRY } else { ABANDON });
        }
        // What it came to is drawn beside the press: kept in view.
        if settled.matched {
            self.keep_create_actions_in_view();
        }
        settled.matched
    }

    /// **The creation finished: this tab becomes the Bot's**, on its main
    /// conversation, from the Bot the owner returned — the same tab, its
    /// link kept, its catalog and the person's agent choices kept, and the
    /// window told once so the roster lists the new Bot.
    pub(super) fn become_created(&mut self, bot: Bot) {
        let roster = crate::bots::roster::RosterBot::from(bot.clone());
        let mut next = Self::opened_on(&roster, bot.main_chat, None);
        next.settings.know(bot);
        next.size = self.size;
        next.lang = self.lang;
        next.home_name = self.home_name.take();
        next.link = self.link.take();
        next.started_link = true;
        next.catalog = std::mem::take(&mut self.catalog);
        next.agent_settings = std::mem::take(&mut self.agent_settings);
        next.page_titles = std::mem::take(&mut self.page_titles);
        next.created = true;
        next.creation_news = self.creation_news.take();
        *self = next;
    }

    /// **The tab just became a new Bot's**: the window lists it in the
    /// roster. Taken once.
    pub(crate) fn take_created(&mut self) -> bool {
        std::mem::take(&mut self.created)
    }

    /// When a create tab next owes a pass: the creation's own clock or
    /// tick, and the catalog's while an answer is owed.
    pub(super) fn create_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let create = self.create.as_deref()?;
        let catalog = self.catalog.waiting().then(|| {
            now.checked_add(std::time::Duration::from_millis(50))
                .unwrap_or(now)
        });
        [create.form.deadline(now), catalog]
            .into_iter()
            .flatten()
            .min()
    }

    /// **The roster's Retry or Abandon answered for `request`**: when it is
    /// this tab's creation, the tab reads it on its next pass and follows
    /// what the owner says — never goes on offering Retry and Abandon for a
    /// creation finished or abandoned there. `true` when it was this tab's.
    pub(crate) fn creation_moved_elsewhere(
        &mut self,
        request: koru_ai_core::bots::BotRequestId,
    ) -> bool {
        match self.create.as_deref_mut() {
            Some(create) if create.form.request() == request => {
                create.form.moved_elsewhere();
                true
            }
            _ => false,
        }
    }

    /// A create tab behind another: its clock stops, and nothing is sent
    /// until it is shown again.
    pub(super) fn create_hidden(&mut self) {
        if let Some(create) = self.create.as_deref_mut() {
            create.form.hidden();
            self.stale = true;
        }
    }

    /// The selected or role page the window's picker chose, for a test.
    #[cfg(test)]
    pub(crate) fn create_form(&self) -> Option<&CreateForm> {
        Some(&self.create.as_deref()?.form)
    }

    /// The create form's draft, mutably, for a test to stand in for typing.
    #[cfg(test)]
    pub(crate) fn create_form_mut(&mut self) -> Option<&mut CreateForm> {
        Some(&mut self.create.as_deref_mut()?.form)
    }

    /// A create tab over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn creating_with_link(home: WorkspaceId, link: super::BotsLink) -> Self {
        let mut view = Self::creating(home, Some("Home".to_owned()));
        view.started_link = true;
        view.link = Some(link);
        view
    }

    /// A page the picker would have chosen, as the create form's role page.
    #[cfg(test)]
    pub(crate) fn choose_role_page_for_test(&mut self, page: koru_ai_core::bots::BotPage) -> bool {
        self.create
            .as_deref_mut()
            .is_some_and(|create| create.form.choose_existing(page))
    }
}

/// **What an abandoned creation left, in sentences**: that it was
/// abandoned and nothing was deleted; the role page, said as unconfirmed
/// when it may not exist; the managed folder by its path; or that it left
/// nothing.
pub(crate) fn abandoned_lines(lang: koru_i18n::Lang, ended: &BotCreation) -> Vec<String> {
    let mut lines = vec![koru_i18n::fill(
        t(lang, Words::BotsAbandoned),
        &[ended.name.as_str()],
    )];
    let left = ended
        .left_behind
        .clone()
        .unwrap_or(koru_ai_core::bots::BotCreationLeftovers {
            role_page: None,
            folder: None,
        });
    if left.role_page.is_some() {
        lines.push(
            t(
                lang,
                if ended.role_page_unconfirmed {
                    Words::BotsLeftPageUnconfirmed
                } else {
                    Words::BotsLeftPage
                },
            )
            .to_owned(),
        );
    }
    // **Where to look, asserting no more than the answer does.** The owner
    // names the managed folder only when it looked and found it there, or
    // when it may be there (a step was under way, or the look failed); the
    // wire does not tell the two apart (`BotCreationLeftovers`). The second
    // window run: *Its folder stays at C:\…\work.* for a folder that was
    // never made. A folder the answer does not name is not mentioned.
    if let Some(folder) = &left.folder {
        lines.push(koru_i18n::fill(
            t(lang, Words::BotsLeftFolder),
            &[folder.as_str()],
        ));
    }
    if left.role_page.is_none() && left.folder.is_none() {
        lines.push(t(lang, Words::BotsLeftNothing).to_owned());
    }
    lines
}
