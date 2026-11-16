// SPDX-License-Identifier: MPL-2.0

//! **A Bot's quick setup, drawn over its empty main conversation**
//! (QUESTIONS O155): a greeting in the Bot's voice and a two-step card.
//!
//! ```text
//! scroll_y  Bot setup                      the room the chat hands over, the shared scrollbar
//!   col  Bot setup content                 S3 above and below, S4 between
//!     row  Bot setup greeting              the Bot's face, its name, the greeting
//!     col  Bot setup card                  on the raised plate
//!       row  Bot setup header              *Set up {name}*, *Skip setup*
//!       row  Bot setup steps               a list: *Explain its role*, *Add context*
//!       col  Bot setup role                the text, what Save does, examples, Save, Skip
//!        or  Bot setup context             what a context page is, the pages, Browse, Done, Skip
//!        or  Bot setup finished            *{name} is ready…*, *Back to setup*
//!     Bot setup hint                       *Or skip it and just start typing…*
//! ```
//!
//! **It is a presentation, never a message.** The shared chat hands the tab
//! the room where an empty chat's greeting would be
//! (`koru_ai_ui::owned`, *A host-drawn empty transcript*) and the tab paints
//! this tree there. Nothing of it reaches the transcript, the stored chat
//! or a request; the composer under it is the ordinary composer and keeps
//! the keyboard until a control here is focused.
//!
//! **Where it shows**: the Bot's main conversation, read and empty, on an
//! active Bot, while the card is not over on this install
//! ([`crate::bots::onboard`]). A first message ends it for good.
//!
//! **What it writes**, both through what exists: step 1's Save leaves one
//! [`RoleWrite`] for the window, which replaces the Role page's text off
//! its thread; step 2's Browse opens the tab's page picker and a page
//! picked is added to Knowledge and saved by the Library's own
//! `SetKnowledge` at once.
//!
//! Every control is a shared component hosted as a leaf ([`super::controls`])
//! or an element; every size and color is a token.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use koru_ai_core::bots::library::BOT_KNOWLEDGE_LIMIT;
use koru_ai_core::bots::{BotLifecycle, BotPage};
use koru_draw::el::{
    Action, El, ElState, Frame, FrameCx, Key, Leaf, Look, Object, Ring, Role, StateRecipe, col,
    divider, icon, leaf, row, scroll_y, spacer, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Metric, Radius, Space, Surface, metrics, roles};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::button::ActionStyle;
use koru_ui::chip::Chip;
use koru_ui::text_field::{TextArea, Typed};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::controls::AreaLeaf;
use super::picker::Destination;
use super::region::{Control, ControlRole, line};
use super::{BotView, SURFACE};
use crate::bots::faces::{self, FaceLeaf};
use crate::bots::onboard::{
    Example, Mark, News, RoleRead, RoleSave, RoleWrite, STEPS, Setup, Stage,
};

/// The whole presentation: the scroll viewport over the chat's room.
pub(crate) const SETUP: Key = Key::surface(SURFACE, "setup");
pub(crate) const GREETING: Key = Key::surface(SURFACE, "setup-greeting");
pub(crate) const CARD: Key = Key::surface(SURFACE, "setup-card");
pub(crate) const SKIP_ALL: Key = Key::surface(SURFACE, "setup-skip-all");
pub(crate) const STEPS_LIST: Key = Key::surface(SURFACE, "setup-steps");
pub(crate) const STEP_ROLE: Key = Key::surface(SURFACE, "setup-step-role");
pub(crate) const STEP_CONTEXT: Key = Key::surface(SURFACE, "setup-step-context");
pub(crate) const ROLE_FIELD: Key = Key::surface(SURFACE, "setup-role");
pub(crate) const ROLE_NOTE: Key = Key::surface(SURFACE, "setup-role-note");
pub(crate) const TRY: Key = Key::surface(SURFACE, "setup-try");
/// The three examples, in [`Example::ALL`]'s order.
pub(crate) const EXAMPLES: [Key; 3] = [
    Key::surface(SURFACE, "setup-example-digest"),
    Key::surface(SURFACE, "setup-example-tasks"),
    Key::surface(SURFACE, "setup-example-specs"),
];
/// *Saving…*, the failure's sentence, or *Saved*.
pub(crate) const ROLE_STATUS: Key = Key::surface(SURFACE, "setup-role-status");
pub(crate) const SAVE_ROLE: Key = Key::surface(SURFACE, "setup-save-role");
pub(crate) const SKIP_ROLE: Key = Key::surface(SURFACE, "setup-skip-role");
pub(crate) const CONTEXT_HINT: Key = Key::surface(SURFACE, "setup-context-hint");
pub(crate) const CONTEXT_STATUS: Key = Key::surface(SURFACE, "setup-context-status");
pub(crate) const BROWSE: Key = Key::surface(SURFACE, "setup-browse");
pub(crate) const RETRY_CONTEXT: Key = Key::surface(SURFACE, "setup-retry-context");
pub(crate) const DONE: Key = Key::surface(SURFACE, "setup-done");
pub(crate) const SKIP_CONTEXT: Key = Key::surface(SURFACE, "setup-skip-context");
pub(crate) const READY: Key = Key::surface(SURFACE, "setup-ready");
pub(crate) const AGAIN: Key = Key::surface(SURFACE, "setup-again");
pub(crate) const HINT: Key = Key::surface(SURFACE, "setup-hint");
/// The part a context page's title is keyed by, under the page's own
/// document id.
pub(crate) const PAGE_PART: &str = "setup-page";

/// A step's round mark: its number, or a tick once it was saved.
const STEP_MARK: Metric = Metric::new("BOT_SETUP_STEP_MARK", 22.0);

fn page_key(page: &BotPage) -> Key {
    Key::model(page.document.as_uuid().as_u128(), PAGE_PART)
}

fn step_key(step: Stage) -> Key {
    match step {
        Stage::Role => STEP_ROLE,
        _ => STEP_CONTEXT,
    }
}

fn step_words(step: Stage) -> Words {
    match step {
        Stage::Role => Words::BotsSetupStepRole,
        _ => Words::BotsSetupStepContext,
    }
}

/// Whether `key` is one of the setup's own.
pub(crate) fn is_setup_key(key: Key) -> bool {
    match key.object {
        Object::Surface(SURFACE) => key.part == "setup" || key.part.starts_with("setup-"),
        Object::Model(_) => key.part == PAGE_PART,
        _ => false,
    }
}

/// A step: lit under the pointer, ringed under the keyboard. Which step is
/// the current one is said by its mark, its weight and its name, never by
/// this plate.
static STEP_STATES: StateRecipe = StateRecipe {
    name: "bot setup step",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Hover),
    selected_pressed: Look::bg(Surface::Hover),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// An example: the shared `Chip`, as wide as its words.
struct ChipLeaf {
    label: String,
    /// Measured with the engine before the tree is built.
    width: f32,
}

impl ChipLeaf {
    fn new(engine: &mut TextEngine, label: String) -> Self {
        let width = Chip::new(&label, false).width(engine);
        Self { label, width }
    }
}

impl Leaf for ChipLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, Chip::H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        Chip::new(&self.label, false).draw(scene, root, engine, at, state.hovered, state.focused);
    }
}

/// **The setup's place in a Bot tab**: the card's state, step 1's field
/// and where the chat last put the room. Boxed in the tab, which is moved
/// by value and kept small.
#[derive(Default)]
pub(crate) struct Card {
    pub(crate) setup: Setup,
    /// Step 1's text, made the first time it is drawn.
    field: RefCell<Option<Rc<RefCell<TextArea>>>>,
    /// The presentation's scroll offset; layout clamps it.
    scroll: Cell<f32>,
    /// The room the chat gave at the last draw, in the tab's coordinates:
    /// `None` whenever the setup is not what shows.
    area: Cell<Option<Rect>>,
    /// The main conversation was seen read and empty while the setup was
    /// on offer: the first message after that ends the setup.
    seen_empty: bool,
    /// The keyboard moved: the next layout brings its control into view.
    pub(super) reveal: Cell<bool>,
}

impl BotView {
    // --------------------------------------------------- when it shows

    /// The name the setup says: the Bot's.
    fn setup_name(&self) -> String {
        self.facts
            .as_ref()
            .map(|facts| facts.name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.words(Words::BotsTabUnnamed))
    }

    /// **Whether the tab asks the chat for the empty transcript's room**:
    /// an active Bot's main conversation, shown, with the card not over on
    /// this install. The chat then gives the room only while the
    /// conversation is read and holds nothing.
    pub(super) fn setup_wanted(&self) -> bool {
        let Some(facts) = &self.facts else {
            return false;
        };
        self.create.is_none()
            && !facts.trashed
            && !facts.archived
            && facts.main_chat == self.chat
            && self.shows_conversation()
            && !self.settings_open
            && !self.list_covers()
            && self.card.setup.active()
    }

    /// Whether the setup is what shows in the conversation's room, as of
    /// the last draw.
    pub(crate) fn setup_shown(&self) -> bool {
        self.card.area.get().is_some() && self.setup_wanted()
    }

    /// Whether step 2 is up and reads the Bot's Library.
    pub(super) fn setup_reads_library(&self) -> bool {
        self.card.setup.stage() == Stage::Context && self.setup_wanted()
    }

    /// **The setup's part of a shown tab's pass**: tell the chat whether
    /// the tab draws its empty transcript, and end the setup once the main
    /// conversation, seen empty, holds a message. `true` when anything
    /// changed.
    pub(super) fn setup_pass(&mut self) -> bool {
        let wanted = self.setup_wanted();
        let on_main = self
            .facts
            .as_ref()
            .is_some_and(|facts| facts.main_chat == self.chat);
        // Whether the Role page is empty, asked once: the card is offered
        // only for a Role nobody has written.
        let mut asked = false;
        if on_main
            && self.create.is_none()
            && let Some(bot) = self.settings.bot()
            && bot.lifecycle == BotLifecycle::Active
        {
            asked = self.card.setup.read_role(bot.id, bot.config.role.clone());
        }
        let Some(conversation) = self.conversation_mut() else {
            return asked;
        };
        let mut changed = conversation.host_empty_transcript(wanted) || asked;
        let empty = conversation.transcript_is_empty();
        if !on_main {
            return changed;
        }
        match empty {
            // Only while the setup is on offer: a conversation that was
            // empty before the window said so ends nothing.
            Some(true) => self.card.seen_empty = wanted,
            Some(false) if self.card.seen_empty => {
                self.card.seen_empty = false;
                if self.card.setup.sent() {
                    if let Some(conversation) = self.conversation_mut() {
                        conversation.host_empty_transcript(false);
                    }
                    changed = true;
                }
            }
            _ => {}
        }
        changed
    }

    /// **Take the room the chat gives**, once the conversation is placed:
    /// from the tab's draw, which is the only place the setup is laid out.
    /// A control of the setup that had the keyboard gives it back when the
    /// setup no longer shows.
    pub(super) fn place_setup(&mut self) {
        let wanted = self.setup_wanted();
        let area = match self.conversation_mut() {
            Some(conversation) if wanted => conversation.hosted_empty_area(),
            _ => None,
        };
        self.card.area.set(area);
        if area.is_none() {
            if self.focus.is_some_and(is_setup_key) {
                self.focus = None;
            }
            if self.hot.is_some_and(is_setup_key) {
                self.hot = None;
            }
        }
        let held = area.is_some() && self.focus.is_some_and(is_setup_key);
        if let Some(conversation) = self.conversation_mut() {
            conversation.set_host_keyboard(held);
        }
    }

    /// Lay the conversation and the setup out as a draw does, without
    /// drawing: what input reads between two draws under test.
    #[cfg(test)]
    pub(crate) fn settle_setup(&mut self) {
        let area = self.with_frame(|_, frame| self.conversation_area(frame));
        self.place_conversation(area);
        self.setup_pass();
        self.place_setup();
    }

    // ------------------------------------------------- the window's side

    /// The window's word on whether this Bot's setup was ended on this
    /// install. `true` when what shows may have changed.
    pub(crate) fn tell_setup(&mut self, dismissed: bool) -> bool {
        self.card.setup.tell(dismissed)
    }

    /// The setup was ended, or opened again: for the window's book. Taken
    /// once.
    pub(crate) fn take_setup_news(&mut self) -> Option<News> {
        self.card.setup.take_news()
    }

    /// The Role write step 1's Save left, for the window's worker. Taken
    /// once.
    pub(crate) fn take_role_write(&mut self) -> Option<RoleWrite> {
        self.card.setup.take_write()
    }

    /// **The window's worker answered a Role write.** `true` when it was
    /// the write on its way.
    pub(crate) fn role_written(&mut self, serial: u64, ok: bool) -> bool {
        let answered = self.card.setup.written(serial, ok);
        if answered && self.card.setup.stage() == Stage::Context {
            self.library.owe_read();
        }
        answered
    }

    /// Whether a Role write or a Role read is with the window's worker.
    pub(crate) fn role_saving(&self) -> bool {
        self.card.setup.role_save() == RoleSave::Saving || self.card.setup.reading_role()
    }

    /// The Role read the setup left, for the window's worker. Taken once.
    pub(crate) fn take_role_read(&mut self) -> Option<RoleRead> {
        self.card.setup.take_read()
    }

    /// **The window's worker answered a Role read**: whether the page has
    /// nothing under its title, or `None` when the read failed.
    pub(crate) fn role_read(&mut self, serial: u64, blank: Option<bool>) -> bool {
        self.card.setup.role_read(serial, blank)
    }

    /// **The worker found the Role page written** when it went to save:
    /// nothing was written and the setup is over for this Bot.
    pub(crate) fn role_found_written(&mut self, serial: u64) -> bool {
        self.card.setup.found_written(serial)
    }

    /// The tab was hidden: a Role read that failed is asked again when it
    /// is next shown.
    pub(super) fn setup_hidden(&mut self) {
        self.card.setup.retry_role_read();
    }

    /// The card's state. Test only.
    #[cfg(test)]
    pub(crate) fn setup(&self) -> &Setup {
        &self.card.setup
    }

    // ----------------------------------------------------------- frame

    /// Step 1's field, on the engine the caller holds.
    fn setup_field_with(&self, engine: &mut TextEngine) -> Rc<RefCell<TextArea>> {
        self.card
            .field
            .borrow_mut()
            .get_or_insert_with(|| {
                let mut area = TextArea::new(engine, koru_draw::theme::role::row());
                area.set_text(engine, self.card.setup.text());
                Rc::new(RefCell::new(area))
            })
            .clone()
    }

    /// Step 1's field, outside a layout.
    fn setup_field(&self) -> Rc<RefCell<TextArea>> {
        crate::input_engine::with(|engine| self.setup_field_with(engine))
    }

    /// Put `words` in step 1, in the field and in the card's state.
    fn setup_fill(&mut self, words: &str) -> bool {
        let input = self.setup_field();
        crate::input_engine::with(|engine| input.borrow_mut().set_text(engine, words));
        self.card.setup.edit(words)
    }

    /// Step 1's line under its examples: a write on its way, a failure, or
    /// what was saved.
    fn setup_role_status(&self) -> Option<(String, Ink)> {
        match self.card.setup.role_save() {
            RoleSave::Idle => None,
            RoleSave::Saving => Some((self.words(Words::BotsSaving), Ink::Hint)),
            RoleSave::Saved => Some((self.words(Words::BotsSaved), Ink::Hint)),
            RoleSave::Failed => Some((self.words(Words::BotsSetupRoleFailed), Ink::Accent)),
        }
    }

    /// Whether Save would write: there are words, no write is on its way,
    /// and the tab holds the Bot, active, as its owner does.
    fn setup_can_save_role(&self) -> bool {
        self.card.setup.can_save()
            && self
                .settings
                .bot()
                .is_some_and(|bot| bot.lifecycle == BotLifecycle::Active)
    }

    /// Step 2's line: what the Library last said, a save or a read on its
    /// way, what was saved, or that there is no page yet.
    fn setup_context_status(&self) -> Option<(String, Ink)> {
        let library = &self.library;
        if let Some(words) = library.notice() {
            return Some((self.words(words), Ink::Accent));
        }
        if library.saving() {
            return Some((self.words(Words::BotsSaving), Ink::Hint));
        }
        if library.loading() || library.held().is_none() {
            return Some((self.words(Words::BotsLoading), Ink::Hint));
        }
        if library.saved() {
            return Some((self.words(Words::BotsSaved), Ink::Hint));
        }
        library
            .pages()
            .is_empty()
            .then(|| (self.words(Words::BotsKnowledgeEmpty), Ink::Secondary))
    }

    /// Whether a page can be added now: the Library is read at the Bot's
    /// revision, nothing of it is on its way, and there is room.
    fn setup_can_browse(&self) -> bool {
        self.settings.bot().is_some_and(|bot| {
            self.library.can_edit(bot)
                && !self.settings.delivering()
                && self.library.pages().len() < BOT_KNOWLEDGE_LIMIT
        })
    }

    fn setup_steps(&self) -> El {
        let setup = &self.card.setup;
        let added = !self.library.pages().is_empty();
        let total = koru_i18n::number::decimal(self.lang, STEPS.len() as u64);
        let mut list = row("Bot setup steps")
            .id(STEPS_LIST)
            .pad(Space::S2)
            .gap(Space::S2)
            .items_center()
            .semantics(Role::List)
            .name(Words::BotsSetupSteps);
        for (index, step) in STEPS.into_iter().enumerate() {
            let mark = setup.mark(step, added);
            let number = koru_i18n::number::decimal(self.lang, index as u64 + 1);
            let name = self.words(step_words(step));
            // In words, for a reader that sees no mark.
            let said = fill(
                t(
                    self.lang,
                    match mark {
                        Mark::Done => Words::BotsSetupStepIsDone,
                        Mark::Current => Words::BotsSetupStepIsCurrent,
                        Mark::Waiting => Words::BotsSetupStepIsWaiting,
                    },
                ),
                &[number.as_str(), total.as_str(), name.as_str()],
            );
            let (plate, ink) = match mark {
                Mark::Done => (Surface::Chip, Ink::Accent),
                Mark::Current => (Surface::Selected, Ink::Primary),
                Mark::Waiting => (Surface::Chip, Ink::Hint),
            };
            // A tick once saved, else the step's number: a shape, not a
            // color, says which.
            let glyph = match mark {
                Mark::Done => icon(icons::CHECK, IconSize::S).decorative().ink(ink),
                _ => text(number)
                    .named("Bot setup step number")
                    .role(roles::META)
                    .ink(ink)
                    .emphasis(mark == Mark::Current),
            };
            list = list.child(
                row("Bot setup step")
                    .id(step_key(step))
                    .fill()
                    .min_h(metrics::BUTTON_MIN)
                    .pad_x(Space::S2)
                    .gap(Space::S2)
                    .items_center()
                    .radius(Radius::M)
                    .states(&STEP_STATES)
                    .hit()
                    .focusable()
                    .selected(mark == Mark::Current)
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::ListItem)
                    .name_text(said)
                    .child(
                        row("Bot setup step mark")
                            .w(STEP_MARK)
                            .h(STEP_MARK)
                            .radius(Radius::Full)
                            .bg(plate)
                            .items_center()
                            .child(spacer())
                            .child(glyph)
                            .child(spacer()),
                    )
                    .child(
                        text(name)
                            .named("Bot setup step name")
                            .role(roles::UI)
                            .ink(if mark == Mark::Waiting {
                                Ink::Secondary
                            } else {
                                Ink::Primary
                            })
                            .emphasis(mark == Mark::Current)
                            .fill()
                            .truncate(),
                    ),
            );
        }
        list
    }

    fn setup_role_panel(&self, engine: &mut TextEngine, name: &str) -> El {
        let input = self.setup_field_with(engine);
        let mut examples = col("Bot setup examples")
            .gap(Space::S1)
            .items_start()
            .child(
                text(self.words(Words::BotsSetupTry))
                    .named("Bot setup try")
                    .id(TRY)
                    .role(roles::META)
                    .ink(Ink::Hint),
            );
        for (example, key) in Example::ALL.into_iter().zip(EXAMPLES) {
            let label = self.words(example.label());
            examples = examples.child(
                leaf("Bot setup example", ChipLeaf::new(engine, label.clone()))
                    .semantic()
                    .id(key)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::Button)
                    .name_text(label),
            );
        }
        let mut panel = col("Bot setup role")
            .pad(Space::S3)
            .gap(Space::S3)
            .child(
                leaf(
                    "Bot setup role text",
                    AreaLeaf {
                        input,
                        placeholder: self.words(Words::BotsSetupRolePlaceholder),
                        focused: self.focus == Some(ROLE_FIELD),
                    },
                )
                .semantic()
                .id(ROLE_FIELD)
                .hit()
                .focusable()
                .action(Action::Click)
                .action(Action::Focus)
                .semantics(Role::TextInput)
                .name_text(fill(t(self.lang, Words::BotsSetupRoleLabel), &[name])),
            )
            .child(line(
                "Bot setup role note",
                ROLE_NOTE,
                fill(t(self.lang, Words::BotsSetupRoleNote), &[name]),
                Ink::Secondary,
            ))
            .child(examples);
        if let Some((words, ink)) = self.setup_role_status() {
            panel = panel.child(line("Bot setup role status", ROLE_STATUS, words, ink));
        }
        let save = if self.card.setup.role_save() == RoleSave::Failed {
            Words::BotsRetry
        } else {
            Words::BotsSave
        };
        panel.child(
            row("Bot setup role actions")
                .gap(Space::S2)
                .items_center()
                .child(self.button(
                    engine,
                    "Bot setup save role",
                    SAVE_ROLE,
                    self.words(save),
                    ActionStyle::Primary,
                    !self.setup_can_save_role(),
                ))
                .child(self.button(
                    engine,
                    "Bot setup skip role",
                    SKIP_ROLE,
                    self.words(Words::BotsSetupSkip),
                    ActionStyle::Ghost,
                    false,
                )),
        )
    }

    fn setup_context_panel(&self, engine: &mut TextEngine, name: &str) -> El {
        let mut panel = col("Bot setup context")
            .pad(Space::S3)
            .gap(Space::S3)
            .child(line(
                "Bot setup context hint",
                CONTEXT_HINT,
                fill(t(self.lang, Words::BotsSetupContextHint), &[name]),
                Ink::Secondary,
            ));
        let pages = self.library.pages();
        if !pages.is_empty() {
            let mut list = col("Bot setup pages").gap(Space::S1);
            for page in pages {
                list = list.child(
                    row("Bot setup page")
                        .gap(Space::S2)
                        .items_center()
                        .child(
                            icon(icons::DOCUMENT, IconSize::S)
                                .decorative()
                                .ink(Ink::Secondary),
                        )
                        .child(
                            text(self.page_title(page))
                                .named("Bot setup page title")
                                .id(page_key(page))
                                .role(roles::ROW)
                                .fill()
                                .truncate(),
                        ),
                );
            }
            panel = panel.child(list);
        }
        if let Some((words, ink)) = self.setup_context_status() {
            panel = panel.child(line("Bot setup context status", CONTEXT_STATUS, words, ink));
        }
        let mut browse = row("Bot setup browse row")
            .gap(Space::S2)
            .items_center()
            .child(self.button(
                engine,
                "Bot setup browse",
                BROWSE,
                self.words(Words::BotsSetupBrowse),
                ActionStyle::Secondary,
                !self.setup_can_browse(),
            ));
        if self.library.retrying() {
            browse = browse.child(self.button(
                engine,
                "Bot setup retry context",
                RETRY_CONTEXT,
                self.words(Words::BotsRetry),
                ActionStyle::Secondary,
                self.settings.delivering(),
            ));
        }
        panel.child(browse).child(
            row("Bot setup context actions")
                .gap(Space::S2)
                .items_center()
                .child(self.button(
                    engine,
                    "Bot setup done",
                    DONE,
                    self.words(Words::BotsSetupDone),
                    ActionStyle::Primary,
                    false,
                ))
                .child(self.button(
                    engine,
                    "Bot setup skip context",
                    SKIP_CONTEXT,
                    self.words(Words::BotsSetupSkip),
                    ActionStyle::Ghost,
                    false,
                )),
        )
    }

    fn setup_done_panel(&self, engine: &mut TextEngine, name: &str) -> El {
        row("Bot setup finished")
            .pad(Space::S3)
            .gap(Space::S2)
            .items_center()
            .child(
                text(fill(t(self.lang, Words::BotsSetupReady), &[name]))
                    .named("Bot setup ready")
                    .id(READY)
                    .role(roles::UI)
                    .ink(Ink::Secondary)
                    .fill()
                    .wrap(),
            )
            .child(self.button(
                engine,
                "Bot setup again",
                AGAIN,
                self.words(Words::BotsSetupAgain),
                ActionStyle::Secondary,
                false,
            ))
    }

    /// **The setup as elements**, in the room the chat gives.
    pub(crate) fn setup_tree(&self, engine: &mut TextEngine) -> El {
        let name = self.setup_name();
        let stage = self.card.setup.stage();
        let title = fill(t(self.lang, Words::BotsSetupTitle), &[name.as_str()]);
        let mut header = row("Bot setup header")
            .pad_x(Space::S3)
            .pad_y(Space::S2)
            .gap(Space::S2)
            .items_center()
            .min_h(metrics::BUTTON_MIN)
            .child(
                text(title.clone())
                    .named("Bot setup title")
                    .role(roles::UI)
                    .emphasis(true)
                    .fill()
                    .truncate(),
            );
        if stage != Stage::Done {
            header = header.child(self.button(
                engine,
                "Bot setup skip",
                SKIP_ALL,
                self.words(Words::BotsSetupSkipAll),
                ActionStyle::Ghost,
                false,
            ));
        }
        let panel = match stage {
            Stage::Role => self.setup_role_panel(engine, &name),
            Stage::Context => self.setup_context_panel(engine, &name),
            Stage::Done => self.setup_done_panel(engine, &name),
        };
        let card = col("Bot setup card")
            .id(CARD)
            .bg(Surface::Raised)
            .radius(Radius::L)
            .child(header)
            .child(divider(Line::Divider))
            .child(self.setup_steps())
            .child(divider(Line::Divider))
            .child(panel);
        let face = self.facts.as_ref().map_or(0, |facts| facts.avatar);
        let mut content = col("Bot setup content")
            .pad_y(Space::S3)
            .gap(Space::S4)
            .child(
                row("Bot setup greeting")
                    .gap(Space::S3)
                    .items_start()
                    // Its face, decorative: the name beside it says who.
                    .child(
                        leaf(
                            "Bot face",
                            FaceLeaf {
                                face,
                                name: name.clone(),
                                pose: self.face_pose,
                                size: faces::ROW_FACE,
                            },
                        )
                        .decorative(),
                    )
                    .child(
                        col("Bot setup greeting words")
                            .fill()
                            .gap(Space::S1)
                            .child(
                                text(name.clone())
                                    .named("Bot setup greeting name")
                                    .role(roles::ROW)
                                    .emphasis(true)
                                    .truncate(),
                            )
                            .child(
                                text(fill(
                                    t(self.lang, Words::BotsSetupGreeting),
                                    &[name.as_str()],
                                ))
                                .named("Bot setup greeting text")
                                .id(GREETING)
                                .role(roles::UI)
                                .wrap(),
                            ),
                    ),
            )
            .child(card);
        if stage != Stage::Done {
            content = content.child(line(
                "Bot setup hint",
                HINT,
                fill(t(self.lang, Words::BotsSetupHint), &[name.as_str()]),
                Ink::Hint,
            ));
        }
        scroll_y("Bot setup")
            .id(SETUP)
            .offset(self.card.scroll.get())
            .scrollbar()
            .semantics(Role::Group)
            .name_text(title)
            .child(content)
    }

    /// The setup laid out in its room, when it shows: the one frame its
    /// paint, its pointer, its keyboard order and its published tree read.
    pub(super) fn setup_layout(&self, engine: &mut TextEngine) -> Option<(El, Frame)> {
        if !self.setup_shown() {
            return None;
        }
        let area = self.card.area.get()?;
        let lay = |engine: &mut TextEngine| {
            let tree = self.setup_tree(engine);
            let frame = tree.layout(
                engine,
                &mut self.cache.borrow_mut(),
                area,
                FrameCx::root().with_interaction(self.interaction()),
            );
            (tree, frame)
        };
        let mut laid = lay(engine);
        // The keyboard moved: its control comes into view.
        if self.card.reveal.replace(false)
            && let Some(focus) = self.focus.filter(|key| is_setup_key(*key))
            && let Some((_, offset)) = laid.1.reveal(focus)
            && offset != self.card.scroll.get()
        {
            self.card.scroll.set(offset);
            laid = lay(engine);
        }
        if let Some((_, offset)) = laid.1.node(SETUP).and_then(|node| node.scroll) {
            self.card.scroll.set(offset);
        }
        Some(laid)
    }

    /// The setup's frame, as input and accessibility read it.
    pub(crate) fn setup_frame<R>(&self, read: impl FnOnce(&Frame) -> R) -> Option<R> {
        crate::input_engine::with(|engine| self.setup_layout(engine)).map(|(_, frame)| read(&frame))
    }

    /// The setup's focusable, enabled controls, in the order they are
    /// drawn: where Tab goes after the tab's own.
    pub(super) fn setup_focus_order(&self) -> Vec<Key> {
        self.setup_frame(super::region::focus_order)
            .unwrap_or_default()
    }

    // ---------------------------------------------------------- acting

    /// The keyboard stays on a control that is drawn: when the one it was
    /// on left with its step, it goes to the step now shown, or to the
    /// finished card's way back.
    fn setup_keep_focus(&mut self) {
        let Some(focus) = self.focus.filter(|key| is_setup_key(*key)) else {
            return;
        };
        if self
            .setup_frame(|frame| frame.contains(focus))
            .unwrap_or(false)
        {
            return;
        }
        self.focus = Some(match self.card.setup.stage() {
            Stage::Role => STEP_ROLE,
            Stage::Context => STEP_CONTEXT,
            Stage::Done => AGAIN,
        });
        self.card.reveal.set(true);
    }

    /// Step 1's Save: one write for the window, or straight on when the
    /// Store already holds these words.
    fn setup_save_role(&mut self) -> bool {
        if !self.setup_can_save_role() {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        let (id, page, title) = (bot.id, bot.config.role.clone(), bot.config.name.clone());
        let before = self.words(Words::BotsSetupHistoryBefore);
        let after = self.words(Words::BotsSetupHistoryAfter);
        let moved = self.card.setup.save(id, page, title, before, after);
        if moved && self.card.setup.stage() == Stage::Context {
            self.library.owe_read();
        }
        moved
    }

    /// **Do what pressing `key` asks.** `true` when anything changed.
    pub(super) fn setup_act(&mut self, key: Key) -> bool {
        let done = if key == SKIP_ALL || key == DONE || key == SKIP_CONTEXT {
            self.card.setup.finish()
        } else if key == STEP_ROLE || key == AGAIN {
            self.card.setup.go(Stage::Role)
        } else if key == STEP_CONTEXT {
            let moved = self.card.setup.go(Stage::Context);
            if moved {
                self.library.owe_read();
            }
            moved
        } else if let Some(at) = EXAMPLES.iter().position(|example| *example == key) {
            let words = self.words(Example::ALL[at].text());
            self.setup_fill(&words);
            // The words are there to be made one's own.
            self.focus = Some(ROLE_FIELD);
            true
        } else if key == SAVE_ROLE {
            self.setup_save_role()
        } else if key == SKIP_ROLE {
            let moved = self.card.setup.skip_role();
            if moved {
                self.library.owe_read();
            }
            moved
        } else if key == BROWSE {
            if !self.setup_can_browse() {
                return false;
            }
            self.open_setup_picker();
            true
        } else if key == RETRY_CONTEXT {
            if self.settings.delivering() {
                return false;
            }
            let Some(command) = self.settings.bot().and_then(|bot| self.library.save(bot)) else {
                return false;
            };
            self.send_write(command);
            true
        } else {
            false
        };
        if done {
            self.card.scroll.set(0.0);
            self.setup_keep_focus();
        }
        done
    }

    /// **A page the picker gave step 2**: added to the Bot's Knowledge and
    /// saved at once, through the Library's own draft and `SetKnowledge`.
    /// Never the access card's selected pages: a context page grants
    /// nothing.
    pub(super) fn setup_add_page(&mut self, page: BotPage) -> bool {
        if self.settings.delivering() {
            return false;
        }
        let Some(bot) = self.settings.bot() else {
            return false;
        };
        if !self.library.add(bot, page) {
            return false;
        }
        let Some(command) = self.library.save(bot) else {
            return true;
        };
        self.send_write(command);
        true
    }

    // ----------------------------------------------------------- input

    fn setup_room(&self, x: f32, y: f32) -> bool {
        self.card.area.get().is_some_and(|area| area.contains(x, y))
    }

    /// **A press in the setup's room**, which the chat handed over because
    /// nothing of its own was over it. The field takes the caret; a button
    /// acts and, as everywhere in the tab, takes no keyboard ring from a
    /// pointer.
    pub(super) fn setup_press(&mut self, x: f32, y: f32) -> bool {
        let hit = self
            .setup_frame(|frame| {
                let key = frame.hit_at(x, y)?;
                let node = frame.node(key)?;
                Some((key, node.rect, node.disabled))
            })
            .flatten();
        let Some((key, rect, disabled)) = hit else {
            return false;
        };
        self.pressed = Some(key);
        if key == ROLE_FIELD {
            self.focus = Some(ROLE_FIELD);
            let input = self.setup_field();
            crate::input_engine::with(|engine| input.borrow_mut().click(engine, rect, x, y));
            return true;
        }
        if disabled || key == SETUP {
            return true;
        }
        self.setup_act(key);
        true
    }

    /// The pointer over the setup: what is lit, and a drag in step 1's
    /// field. `(in the setup's room, what is lit changed)`.
    pub(super) fn setup_move(&mut self, x: f32, y: f32) -> (bool, bool) {
        if !(self.setup_shown() && self.setup_room(x, y)) {
            let was = self.hot.is_some_and(is_setup_key);
            if was {
                self.hot = None;
            }
            return (false, was);
        }
        let (hot, field) = self
            .setup_frame(|frame| {
                (
                    frame.hit_at(x, y),
                    frame.node(ROLE_FIELD).map(|node| node.rect),
                )
            })
            .unwrap_or((None, None));
        let mut changed = self.hot != hot;
        self.hot = hot;
        if self.pressed == Some(ROLE_FIELD)
            && let Some(rect) = field
        {
            let input = self.setup_field();
            changed |=
                crate::input_engine::with(|engine| input.borrow_mut().drag(engine, rect, x, y));
        }
        (true, changed)
    }

    pub(super) fn setup_release(&mut self) {
        if let Some(input) = self.card.field.borrow().as_ref() {
            input.borrow_mut().release();
        }
    }

    /// The wheel over the setup scrolls it. `true` when it was over it.
    pub(super) fn setup_scroll(&mut self, x: f32, y: f32, dy: f32) -> bool {
        if !(self.setup_shown() && self.setup_room(x, y)) {
            return false;
        }
        // Layout clamps it to what there is to scroll.
        self.card.scroll.set((self.card.scroll.get() + dy).max(0.0));
        true
    }

    /// **A key while one of the setup's controls has the keyboard.** `None`
    /// when none has it, and for Tab and Escape, which are the tab's own
    /// (the walk, and giving the keyboard back to the conversation).
    pub(super) fn setup_key(
        &mut self,
        key: &WinitKey,
        typed: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let focus = self.focus.filter(|key| is_setup_key(*key))?;
        if matches!(key, WinitKey::Named(NamedKey::Tab | NamedKey::Escape)) {
            return None;
        }
        if focus == ROLE_FIELD {
            let input = self.setup_field();
            let handled = crate::input_engine::with(|engine| {
                input.borrow_mut().key(engine, key, typed, mods)
            });
            let text = input.borrow().raw_text().to_owned();
            self.card.setup.edit(&text);
            return Some(matches!(handled, Typed::Handled));
        }
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return Some(false);
        }
        let activate =
            matches!(key, WinitKey::Named(NamedKey::Enter | NamedKey::Space)) || typed == Some(" ");
        if activate {
            return Some(self.setup_act(focus));
        }
        // Any other key is the tab's to place: it gives the keyboard back
        // to the conversation.
        None
    }

    // ---------------------------------------------------- accessibility

    /// A sentence of the setup, by its key.
    fn setup_line(&self, key: Key) -> Option<String> {
        let name = self.setup_name();
        let filled = |words: Words| fill(t(self.lang, words), &[name.as_str()]);
        Some(if key == GREETING {
            filled(Words::BotsSetupGreeting)
        } else if key == ROLE_NOTE {
            filled(Words::BotsSetupRoleNote)
        } else if key == TRY {
            self.words(Words::BotsSetupTry)
        } else if key == ROLE_STATUS {
            self.setup_role_status()?.0
        } else if key == CONTEXT_HINT {
            filled(Words::BotsSetupContextHint)
        } else if key == CONTEXT_STATUS {
            self.setup_context_status()?.0
        } else if key == READY {
            filled(Words::BotsSetupReady)
        } else if key == HINT {
            filled(Words::BotsSetupHint)
        } else if key.part == PAGE_PART {
            let page = self
                .library
                .pages()
                .iter()
                .find(|page| page_key(page) == key)?;
            self.page_title(page)
        } else {
            return None;
        })
    }

    /// **The setup for the accessibility tree**: a group named *Set up
    /// {name}* holding the greeting, the steps as a list (each named with
    /// its number and whether it is done, current or not done, the current
    /// one selected), step 1's field with its text, and every sentence and
    /// control, in the keyboard's order. Nothing while it does not show.
    pub(super) fn setup_controls(&self) -> Vec<Control> {
        self.setup_frame(|frame| {
            let label = |name: &Option<koru_draw::el::Name>| match name {
                Some(koru_draw::el::Name::Key(words)) => t(self.lang, *words).to_owned(),
                Some(koru_draw::el::Name::Text(content)) => content.clone(),
                None => String::new(),
            };
            // Each published node, and the published node it nests under.
            let mut arena: Vec<(Control, Option<usize>)> = Vec::new();
            let mut published: Vec<Option<usize>> = vec![None; frame.nodes.len()];
            for (index, node) in frame.nodes.iter().enumerate() {
                let Some(key) = node.key else {
                    continue;
                };
                let (role, words) = match node.role {
                    Some(Role::Button) => (ControlRole::Button, label(&node.label)),
                    Some(Role::Group) => (ControlRole::Group, label(&node.label)),
                    Some(Role::List) => (ControlRole::List, label(&node.label)),
                    Some(Role::ListItem) => (ControlRole::ListItem, label(&node.label)),
                    Some(Role::TextInput) => (ControlRole::TextInput, label(&node.label)),
                    _ if node.lines().is_some() => match self.setup_line(key) {
                        Some(words) => (ControlRole::Label, words),
                        None => continue,
                    },
                    _ => continue,
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
                published[index] = Some(arena.len());
                arena.push((
                    Control {
                        key,
                        rect: node.rect,
                        role,
                        label: words,
                        value: (key == ROLE_FIELD).then(|| self.card.setup.text().to_owned()),
                        selected: node.selected,
                        disabled: node.disabled,
                        focused: self.focus == Some(key),
                        children: Vec::new(),
                    },
                    nests,
                ));
            }
            // Nest back to front, so a child is complete before it moves.
            let parents: Vec<Option<usize>> = arena.iter().map(|(_, parent)| *parent).collect();
            let mut built: Vec<Option<Control>> = arena
                .into_iter()
                .map(|(control, _)| Some(control))
                .collect();
            let mut roots = Vec::new();
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
            roots
        })
        .unwrap_or_default()
    }

    /// **What an assistive technology asked of one of the setup's
    /// controls.** `None` when `key` is not the setup's.
    pub(super) fn setup_accessibility_action(&mut self, key: Key, click: bool) -> Option<bool> {
        if !is_setup_key(key) {
            return None;
        }
        // An inert or absent control is inert to an assistive technology
        // too, as it is to the pointer.
        let live = self
            .setup_frame(|frame| frame.node(key).is_some_and(|node| !node.disabled))
            .unwrap_or(false);
        if !live {
            return Some(false);
        }
        if !click || key == ROLE_FIELD {
            self.focus = Some(key);
            self.card.reveal.set(true);
            return Some(true);
        }
        Some(self.setup_act(key))
    }

    /// **A client set step 1's text**, as typing it would. `None` when
    /// `key` is not that field.
    pub(super) fn setup_set_value(&mut self, key: Key, text: &str) -> Option<bool> {
        if key != ROLE_FIELD {
            return None;
        }
        if !self
            .setup_frame(|frame| frame.contains(ROLE_FIELD))
            .unwrap_or(false)
        {
            return Some(false);
        }
        self.setup_fill(text);
        Some(true)
    }

    // ----------------------------------------------------- the picker

    /// Open the tab's page picker under *Browse workspace…*.
    fn open_setup_picker(&mut self) {
        self.open_page_picker();
        self.picker_destination = Destination::Setup;
    }

    /// Where *Browse workspace…* is, for the picker that opens under it.
    pub(super) fn setup_browse_rect(&self) -> Option<Rect> {
        self.setup_frame(|frame| frame.node(BROWSE).map(|node| node.rect))
            .flatten()
    }
}
