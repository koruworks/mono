// SPDX-License-Identifier: MPL-2.0

//! **A Bot's tab** (Bots P1.8 S4): a header about the Bot over the Bot's
//! conversation, which is the shared chat itself.
//!
//! ## What it shows
//!
//! The header says what the Bot service said, and nothing it did not: the
//! Bot's **name**, **Paused** when its owner says so, **Archived** when the
//! Bot's own `lifecycle` says so (P1.7: its history reads, nothing about it
//! changes), and *Saves results in
//! {workspace}* with the first version's limit — a Bot saves only in its home
//! workspace (QUESTIONS O8). Below it the Bot's main conversation, in an
//! [`AiChatView`] whose surface the Bot owns (`koru_ai_ui::OwnedConversation`,
//! S3): its turns carry the home workspace as their destination, its picker
//! offers only the Bot's agent, the controls the owner refuses are absent,
//! and a conversation that cannot be read says so instead of becoming an
//! ordinary chat.
//!
//! **Its settings** (S5): the header's one control, *Settings*, opens the
//! Bot's settings in the conversation's place ([`region`]) — the name, the
//! Role page, access, the working folder, pause and resume, and what Koru's
//! access does not cover. Each change is the owner's to apply and is shown
//! as pending until it answers ([`crate::bots::settings`]).
//!
//! **Its conversations** (S8, E3): a column beside the conversation lists
//! the Bot's conversations as its owner holds them, main first, with *New
//! conversation*; choosing one shows it in the tab ([`list`],
//! [`conversations`]). The header says when the conversation shown is the
//! Bot's main one.
//!
//! **Its lifecycle** (S14): the header's ⋯ — Archive and Move to Trash, or
//! Unarchive — a conversation row's Move to Trash, each asked about by the
//! window first, the operation's progress, a parked one's Continue and
//! Cancel, and what is true when it is done ([`actions`]).
//!
//! **Creating one** (S8): a Bot tab in create mode is a form in the
//! conversation's place ([`create`]), found by no conversation until the
//! owner has made the Bot; then it becomes that Bot's tab, in place, on its
//! main conversation.
//!
//! **What it does not show, deliberately.** No Rename or Pin on a
//! conversation, and no permanent delete: each is a later slice with its own
//! dependency, and a control that leads nowhere is a promise the tab cannot
//! keep.
//!
//! ## One conversation, one container
//!
//! The tab is found by its conversation's [`ChatId`] — the same id the window
//! asks every tab for (`TabView::chat_id`) — so the Bot's conversation opened
//! from the roster and from anywhere else that knows it is a Bot's lands on
//! this one tab. A tab restored from the session knows that id before the
//! Bot service has answered anything.
//!
//! ## When it works
//!
//! **Only while it is shown.** The window draws and maintains only the tab in
//! front, and that is where this tab asks the Bot service (through its own
//! [`BotsLink`] thread — never the window thread) and lays its header out. A
//! tab behind another sends nothing and lays nothing out; its conversation is
//! not started until the tab is first shown. Only the latest request's answer
//! about this Bot is taken, and a failed read keeps what was shown.
//!
//! **Hidden, then shown again, it reads its Bot once.** The window tells a
//! tab that is no longer the one in front of its group ([`BotView::hidden`]);
//! the next showing asks the Bot service once, so a name, a pause or access
//! changed elsewhere meanwhile is what the header and the settings show.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use koru_ai_core::bots::{Bot, BotId, BotLifecycle, BotRequestId, BotResponse};
use koru_ai_core::{AiProviderId, ChatId};
use koru_core::schema::WorkspaceDoc;
use koru_draw::el::{El, Frame, FrameCx, Key, TextCache, boxed, col, divider, icon, row, text};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::scene;
use koru_draw::theme::{IconSize, Ink, Line, Space, alias, roles};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, ModifiersState};
use winit::window::CursorIcon;

use super::lifecycle::Lifecycle;
use super::link::{Answer, BotsLink, Command, Failure, Reread};
use super::roster::RosterBot;
use super::settings::Settings;
use crate::ai_chat_view::AiChatView;

mod actions;
pub(crate) mod agent;
pub(crate) mod context_library;
pub(crate) mod context_routines;
pub(crate) mod controls;
pub(crate) mod conversations;
pub(crate) mod copy;
pub(crate) mod create;
mod library;
pub(crate) mod list;
pub(crate) mod memory;
pub(crate) mod new_bot;
pub(crate) mod onboard;
mod picker;
pub(crate) mod region;
pub(crate) use region::{Control, ControlRole};

/// The surface every key below belongs to.
pub(crate) const SURFACE: &str = "bot-tab";
/// The whole tab.
pub(crate) const ROOT: Key = Key::surface(SURFACE, "root");
/// The header over the conversation.
pub(crate) const HEADER: Key = Key::surface(SURFACE, "header");
/// The Bot's name.
pub(crate) const NAME: Key = Key::surface(SURFACE, "name");
/// *Paused*, when the owner says so.
pub(crate) const PAUSED: Key = Key::surface(SURFACE, "paused");
/// *Archived*, when the Bot's lifecycle says so.
pub(crate) const ARCHIVED: Key = Key::surface(SURFACE, "archived");
/// *Saves results in {workspace}*.
pub(crate) const SAVES: Key = Key::surface(SURFACE, "saves");
/// The first version's home-only limit.
pub(crate) const LIMIT: Key = Key::surface(SURFACE, "limit");
/// The line saying the Bot is being read, or could not be.
pub(crate) const NOTICE: Key = Key::surface(SURFACE, "notice");
/// Where the conversation sits.
pub(crate) const CONVERSATION: Key = Key::surface(SURFACE, "conversation");
/// *Main conversation*, while the conversation shown is the Bot's main one.
pub(crate) const MAIN_MARK: Key = Key::surface(SURFACE, "main-conversation");

/// **What a Bot's conversation offers beyond the conversation itself.**
/// Files and folders on this computer attached by their path: the Bot owner
/// delivers them to the agent as an ordinary chat's are (QUESTIONS O30), and
/// a path reference grants nothing Koru controls. Rename, as an AI chat's.
/// Everything else the owner refuses — pin, privacy, the chat's own working
/// folder and workspace access — stays absent.
pub(crate) const BOT_OFFERS: koru_ai_ui::OwnedOffers = koru_ai_ui::OwnedOffers {
    // **Renamed as an AI chat is** — by hand, and from its first message
    // while it still carries the new-chat placeholder (the owner's decision
    // for Bots conversations, "same as an AI chat"; the engine admits
    // `RenameChat` on a Bot conversation).
    rename: true,
    pin: false,
    privacy: false,
    working_folder: false,
    tool_access: false,
    path_references: true,
};

/// **The Bot owner's refusals the chat says in the Bot's words**
/// (`koru_ai_ui::OwnedConversation::refusals`): its lifecycle holds —
/// archived, a conversation in Trash, the whole Bot in Trash, an operation
/// moving or parked — and a change still being delivered to what runs it.
/// The shared chat knows none of these codes; this is the Bot adapter's
/// data, handed in when the conversation is bound.
pub(crate) const BOT_REFUSALS: koru_ai_ui::OwnedRefusals = &[
    (super::settings::BOT_ARCHIVED, Words::BotsErrArchived),
    (super::settings::BOT_TRASHED, Words::BotsErrTrashed),
    (super::settings::BOT_TRASHED_BOT, Words::BotsErrBotTrashed),
    (super::settings::BOT_PENDING, Words::BotsErrPending),
    (
        super::settings::BOT_OPERATION_PENDING,
        Words::BotsErrOperationPending,
    ),
    (
        super::settings::ROUTINE_RETRY_UNSUPPORTED,
        Words::BotsErrRoutineRetryUnsupported,
    ),
    // Every Bot is paused (M8): a message to a Bot that is not working is
    // refused, and says what to do about it.
    (super::team_pause::TEAM_PAUSED, Words::BotsTeamPausedSend),
];

/// The bounded clock a Bot read left owed is asked again on: the lifecycle
/// UI's own pace (`bots::lifecycle`), reused rather than invented.
const BOT_READ_FIRST: std::time::Duration = std::time::Duration::from_millis(250);
const BOT_READ_MAX: std::time::Duration = std::time::Duration::from_secs(2);

/// **What the tab knows about its Bot**: what the service said, by the Bot
/// service's own ids.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BotFacts {
    pub id: BotId,
    pub name: String,
    pub paused: bool,
    /// Archived (P1.7): its history reads and every change is refused with
    /// `bot_archived` until it is unarchived (S14).
    pub archived: bool,
    /// In Trash (P1.7): the Bot and its conversations are hidden; nothing
    /// else moved. Its tab says so and offers Restore (S14).
    pub trashed: bool,
    pub home: WorkspaceId,
    pub provider: AiProviderId,
    pub main_chat: ChatId,
    /// The face the Bot wears (QUESTIONS O155): its stored choice, else the
    /// default one (`Bot::avatar`).
    pub avatar: u8,
    /// **The other computer that runs the Bot** (M8, P8.3), when the tab
    /// knows it is one: the tab is then a saved copy's ([`copy`]). `None`
    /// for a Bot this computer runs, and until the tab knows.
    pub owner: Option<koru_ai_core::bots::DeviceId>,
}

impl From<&RosterBot> for BotFacts {
    fn from(bot: &RosterBot) -> Self {
        Self {
            owner: bot.owner,
            avatar: bot.avatar,
            id: bot.id,
            name: bot.name.clone(),
            paused: bot.paused,
            // The roster's own rows are active Bots (`BotCommand::List`); a
            // row of its Archived filter says it is archived.
            archived: bot.archived,
            trashed: false,
            home: bot.home,
            provider: bot.provider.clone(),
            main_chat: bot.main_chat,
        }
    }
}

impl From<Bot> for BotFacts {
    fn from(bot: Bot) -> Self {
        Self {
            // A Bot alone does not say which device this is: the tab
            // keeps what it knew, or works it out ([`BotView::know`]).
            owner: None,
            avatar: bot.avatar(),
            id: bot.id,
            name: bot.config.name,
            paused: bot.paused,
            archived: bot.lifecycle == BotLifecycle::Archived,
            trashed: bot.lifecycle == BotLifecycle::Trashed,
            home: bot.home_workspace,
            provider: bot.config.model.provider,
            main_chat: bot.main_chat,
        }
    }
}

/// Where the tab's read of its Bot stands.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) enum Load {
    /// Not asked yet.
    #[default]
    Cold,
    Loading,
    Ready,
    /// The service did not answer. **What was shown stays**.
    Unavailable,
    /// **Koru AI refused this window access to bots** past the while a
    /// starting Koru takes to vouch for it ([`super::link::Vouching`]).
    Denied,
}

/// What the tab has done, so a test can assert a count rather than a clock.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Work {
    /// Requests handed to the link.
    pub commands: usize,
    /// Element layouts made.
    pub layouts: usize,
}

/// **Where a tab that does not yet know its Bot stands** (Bots P1.8, B1):
/// the window asked whose the conversation is and shows this tab, neutral,
/// until it answers.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) enum Opening {
    #[default]
    Asking,
    /// The read did not answer: the tab says the conversation cannot be
    /// opened right now, and is asked about again when it is next shown.
    Failed,
    /// The owner says the conversation is in Trash (S14): the tab says so,
    /// and is not asked about again on its own.
    Trashed,
}

pub(crate) struct BotView {
    /// The Bot, once known. `None` only on a tab the window opened on a
    /// conversation before it knew whose it was ([`Self::resolving`]).
    bot: Option<BotId>,
    /// Where finding out whose the conversation is stands, while `bot` is
    /// `None`.
    opening: Opening,
    /// The name the conversation's Bot label carried, for the tab's name
    /// until the Bot is read.
    label_name: Option<String>,
    /// The face the roster lists for this Bot, for the tab strip until the
    /// Bot is read (`know_face`).
    listed_face: Option<u8>,
    /// The message block a link or a search hit named, shown once the
    /// conversation is bound.
    pending_block: Option<koru_ai_ui::MessageBlockId>,
    /// The conversation this tab shows: what it is found by.
    chat: ChatId,
    facts: Option<BotFacts>,
    /// The home workspace's name, as the window last said.
    home_name: Option<String>,
    load: Load,
    /// The conversation, once the tab knows enough to bind it: the Bot's
    /// home and agent come from the Bot service.
    conversation: Option<AiChatView>,
    /// The conversation has been started: its reader dials the AI service.
    started_chat: bool,
    link: Option<BotsLink>,
    started_link: bool,
    seq: u64,
    in_flight: bool,
    /// The Bot service has been asked since the tab was made: the next
    /// showing need not ask again.
    asked: bool,
    size: (f32, f32),
    lang: Lang,
    work: Cell<Work>,
    cache: RefCell<TextCache>,
    /// The tab was hidden after it had been shown: the next showing reads
    /// the Bot once.
    stale: bool,
    /// The header's bottom edge as last drawn, so the pointer over the
    /// conversation costs no layout of the tab's own.
    header_bottom: Cell<f32>,
    // -------------------------------------------- the settings (S5)
    settings: Box<Settings>,
    library: Box<super::library::Library>,
    pub(crate) routines: Box<super::routines::Routines>,
    /// The Routine whose timing dialog was asked for: the window opens it.
    routine_timing_request: Option<super::routines::control::TimingSeed>,
    memory: Box<super::memory::Memory>,
    #[cfg(test)]
    memory_test_available: bool,
    memory_ask: Option<Box<memory::Ask>>,
    memory_target: Option<Box<koru_editor::trigger::DocumentTarget>>,
    settings_open: bool,
    context_library_visible: bool,
    context_instance: u64,
    /// **The conversation list, opened from the header while the tab is
    /// compact**: it takes the conversation's place until a row is chosen
    /// or it is closed.
    list_open: bool,
    /// The control the keyboard is on: the header's *Settings*, or one of
    /// the region's. `None` gives the keyboard to the conversation.
    focus: Option<Key>,
    /// The keyboard opened the settings from *Settings*: closing them gives
    /// it back there rather than to the conversation.
    settings_return: bool,
    /// The control under the pointer, and the access segment it is over.
    hot: Option<Key>,
    hot_segment: Option<usize>,
    pressed: Option<Key>,
    /// The name field, made the first time the region shows it.
    name_field: RefCell<Option<Rc<RefCell<koru_ui::text_input::TextInput>>>>,
    /// The region's scroll offset; layout clamps it.
    scroll: f32,
    /// What the person asked the window for: the Role page, the folder
    /// dialog (where it starts), the page picker.
    role_request: Option<koru_ai_core::bots::BotPage>,
    folder_request: Option<Option<PathBuf>>,
    /// *Add a page…*'s picker while it is up, and the row under the pointer.
    /// Boxed, as the settings are: a tab is moved by value on the window
    /// thread and stays small (`a_tab_stays_small_enough_to_move_on_the_window_thread`).
    picker: Option<Box<koru_editor::components::entity_picker::EntityPicker>>,
    picker_hover: Option<usize>,
    /// Selected pages' titles, as the window knows them.
    page_titles: HashMap<koru_ai_core::DocumentId, String>,
    // ---------------------------------------- the conversations (S8)
    /// Boxed for the reason the settings are.
    conversations: Box<conversations::Conversations>,
    /// The conversation column's right edge as last drawn, so the pointer
    /// over the conversation costs no layout of the tab's own.
    column_right: Cell<f32>,
    /// Where the pointer last was, in the tab's coordinates: the wheel says
    /// nothing about where it is turned.
    pointer: Cell<(f32, f32)>,
    /// The conversation's status when the window last asked, so it can tell
    /// the roster that a conversation's work moved (S10).
    seen_status: koru_chats_ui::ChatStatus,
    // ------------------------------------------- the lifecycle (S14)
    /// Archive, Trash, Unarchive, Restore and a conversation's Trash: what
    /// is on its way and what the owner last said. Boxed for the reason the
    /// settings are.
    lifecycle: Box<Lifecycle>,
    /// The trashed Bot as the owner's Trash filter lists it — its revision
    /// is what a Restore carries, and its read answers *not found*.
    trashed_bot: Option<Box<Bot>>,
    /// **A Restore answered and the Bot's read has not**: a Trash reversed
    /// gives the Bot back to what it was before — active or archived — and
    /// only the read says which, so the ⋯ and the rows' Trash offer nothing
    /// until it answers rather than something the owner would refuse.
    lifecycle_awaits_read: bool,
    /// The last Restore or Unarchive left Routines that may still be on, or
    /// whose access must be chosen again.
    restored_routines_need_a_look: bool,
    /// **When the Bot's read is asked again** after one that did not answer
    /// while [`Self::lifecycle_awaits_read`] holds, on the bounded clock
    /// (250 ms doubling to 2 s), only while shown.
    bot_read_next: Option<std::time::Instant>,
    bot_read_backoff: std::time::Duration,
    /// **Since when the Bot's read is refused as unvouched**: asked again on
    /// the same clock for a bounded while, as a tab restored at startup is
    /// (the second window run: the first read after a restart is refused
    /// `forbidden` until the Agent vouches for the window).
    vouching: super::link::Vouching,
    /// A Trash filter read is owed: the Bot went to Trash, or its read said
    /// it is not found.
    trash_read_owed: bool,
    trash_in_flight: bool,
    trash_seq: u64,
    /// The header's ⋯ was pressed: its menu, at its anchor, for the window.
    actions_request: Option<(f32, f32)>,
    /// A conversation row's menu, at its anchor, or its Delete: for the
    /// window, which asks before anything is sent.
    conversation_menu: Option<(ChatId, (f32, f32))>,
    conversation_trash: Option<ChatId>,
    /// Where the keyboard goes back when the window's menu or dialog that a
    /// control of this tab opened closes.
    return_focus: Option<Key>,
    /// The conversation the tab showed went to Trash and the Bot listed no
    /// other to show: the tab says so and offers *New conversation*.
    chat_in_trash: bool,
    /// Why the list did not act on a row's Trash: several picked, or another
    /// operation is still finishing.
    list_notice: Option<Words>,
    // ------------------------------------------ agent and model (S6)
    /// The agents-and-models catalog the settings' card reads, on a link of
    /// its own, asked only while the settings are shown. Boxed for the
    /// reason the settings are.
    catalog: Box<super::catalog::Catalog>,
    /// The person's own choices about agents, as the window holds them.
    agent_settings: Box<super::catalog::AgentSettings>,
    /// The *Agent and model* card's popover while one is up (S7). Boxed
    /// for the reason the settings are.
    agent: Option<Box<agent::AgentPopover>>,
    /// The starred models after a ☆ in the card's picker, for the window
    /// to store.
    favorites_request: Option<Vec<koru_ai_ui::FavoriteModel>>,
    // ------------------------------------------------ creating one (S8)
    /// **The create form**, while this tab creates a Bot: the draft, its
    /// fields and what the window owes it. `None` on every other Bot tab.
    /// Boxed for the reason the settings are.
    create: Option<Box<create::CreateTab>>,
    /// Which draft receives the home-workspace page picker choice.
    picker_destination: picker::Destination,
    /// The tab just became a new Bot's tab: the window lists it.
    created: bool,
    /// **What the roster shows of this Bot moved**, as one of the tab's own
    /// answers says — a rename saved, a pause or a resume the owner took, a
    /// read that shows either — for the window to hand the roster. Taken
    /// once (`take_roster_news`).
    roster_news: Option<Bot>,
    /// **Where this create tab's creation stands, as its own answer just
    /// said** — stopped unfinished, still being made, or ended — for the
    /// window to hand the roster's Unfinished group at once. Taken once
    /// (`take_creation_news`).
    /// Boxed: the tab is moved by value and kept small.
    creation_news: Option<Box<koru_ai_core::bots::BotCreation>>,
    /// **The pose the header's face is drawn in** (QUESTIONS O155), as the
    /// window last read it from its one face clock
    /// (`MainView::sync_bot_faces`): still unless this Bot is working. The
    /// tab owns no clock and asks for no frame for it.
    face_pose: koru_draw::face::FacePose,
    /// **The quick setup over the Bot's empty main conversation** (QUESTIONS
    /// O155): the card's state, its field and the room the chat gives it
    /// ([`onboard`]). Boxed for the reason the settings are.
    card: Box<onboard::Card>,
    // ----------------------------------- one Bot asks another (M6)
    /// **The requests the conversation shown made of other Bots, and who
    /// asked for it** (contract 7), read on a link of their own while the
    /// tab is shown ([`crate::bots::children`]). Boxed for the reason the
    /// settings are.
    children: Box<crate::bots::children::Children>,
    /// A conversation a request card or the origin line asked to open: the
    /// window opens it in that Bot's tab. Taken once.
    open_request: Option<crate::bots::children::OpenRequest>,
    // ------------------------------- another computer's Bot (M8)
    /// **What the tab holds while another computer runs its Bot** ([`copy`]):
    /// that computer, its copy's state and what the last command came to.
    /// Empty for a Bot this computer runs. Boxed for the reason the
    /// settings are.
    copy: Box<copy::Copy>,
    // ------------------------------------ mentioning another Bot
    /// **The Bots the conversation's `@` offers**, and how fresh the list
    /// is ([`crate::bots::mentions`]). Boxed for the reason the settings
    /// are.
    mentions: Box<crate::bots::mentions::Mentions>,
}

impl BotView {
    fn blank(bot: Option<BotId>, chat: ChatId) -> Self {
        Self {
            bot,
            opening: Opening::Asking,
            label_name: None,
            listed_face: None,
            pending_block: None,
            chat,
            facts: None,
            home_name: None,
            load: Load::Cold,
            conversation: None,
            started_chat: false,
            link: None,
            started_link: false,
            seq: 0,
            in_flight: false,
            asked: false,
            size: (0.0, 0.0),
            lang: crate::format::lang(),
            work: Cell::new(Work::default()),
            cache: RefCell::new(TextCache::default()),
            stale: false,
            header_bottom: Cell::new(f32::INFINITY),
            settings: Box::default(),
            library: Box::default(),
            routines: Box::default(),
            routine_timing_request: None,
            memory: Box::default(),
            #[cfg(test)]
            memory_test_available: false,
            memory_ask: None,
            memory_target: None,
            settings_open: false,
            context_library_visible: false,
            context_instance: context_library::mint_instance(),
            list_open: false,
            focus: None,
            settings_return: false,
            hot: None,
            hot_segment: None,
            pressed: None,
            name_field: RefCell::new(None),
            scroll: 0.0,
            role_request: None,
            folder_request: None,
            picker: None,
            picker_hover: None,
            page_titles: HashMap::new(),
            conversations: Box::default(),
            column_right: Cell::new(f32::NEG_INFINITY),
            pointer: Cell::new((0.0, 0.0)),
            seen_status: koru_chats_ui::ChatStatus::Idle,
            lifecycle: Box::default(),
            trashed_bot: None,
            lifecycle_awaits_read: false,
            restored_routines_need_a_look: false,
            bot_read_next: None,
            bot_read_backoff: BOT_READ_FIRST,
            vouching: super::link::Vouching::default(),
            trash_read_owed: false,
            trash_in_flight: false,
            trash_seq: 0,
            actions_request: None,
            conversation_menu: None,
            conversation_trash: None,
            return_focus: None,
            chat_in_trash: false,
            list_notice: None,
            catalog: Box::default(),
            agent_settings: Box::default(),
            agent: None,
            favorites_request: None,
            create: None,
            picker_destination: picker::Destination::Access,
            created: false,
            roster_news: None,
            creation_news: None,
            face_pose: koru_draw::face::FacePose::STILL,
            card: Box::default(),
            children: Box::default(),
            open_request: None,
            copy: Box::default(),
            mentions: Box::default(),
        }
    }

    /// **The pose the header's face is drawn in**, from the window's clock.
    /// `true` when it moved, and a repaint is owed.
    pub(crate) fn set_face_pose(&mut self, pose: koru_draw::face::FacePose) -> bool {
        let changed = self.face_pose != pose;
        self.face_pose = pose;
        changed
    }

    /// The pose the header's face is drawn in. Test only.
    #[cfg(test)]
    pub(crate) fn face_pose(&self) -> koru_draw::face::FacePose {
        self.face_pose
    }

    /// The face this tab's Bot wears, once the tab knows its Bot.
    pub(crate) fn avatar(&self) -> Option<u8> {
        self.facts.as_ref().map(|facts| facts.avatar)
    }

    /// **Whether the conversation this tab shows has a reply running**, as
    /// the window's chat status pass last set it (`maintain_chat_status`):
    /// one of the two things that say a Bot is working (the other is the
    /// roster's activity read).
    pub(crate) fn conversation_working(&self) -> bool {
        self.conversation()
            .is_some_and(|view| view.status() == koru_chats_ui::ChatStatus::Working)
    }

    /// **Where this create tab's creation stands**, when one of its own
    /// answers just moved it: the window puts it in the roster's
    /// Unfinished group at once (the second window run's finding 3a).
    /// Taken once.
    pub(crate) fn take_creation_news(&mut self) -> Option<koru_ai_core::bots::BotCreation> {
        self.creation_news.take().map(|news| *news)
    }

    /// **The Bot as one of this tab's answers left it, when what the roster
    /// shows of it moved** — its name or its pause. Taken once: the window
    /// puts it in the roster's row at once and has the roster read its list
    /// again, so neither waits for the roster to be shown again (the first
    /// window run's finding 4).
    pub(crate) fn take_roster_news(&mut self) -> Option<Bot> {
        self.roster_news.take()
    }

    /// Note `bot` as roster news when its name or its pause is not what the
    /// tab showed before.
    fn note_roster_news(&mut self, bot: &Bot) {
        // A tab's first read changes nothing the roster listed.
        let moved = self
            .facts
            .as_ref()
            .is_some_and(|facts| facts.name != bot.config.name || facts.paused != bot.paused);
        if moved {
            self.roster_news = Some(bot.clone());
        }
    }

    /// **The person's own choices about agents** — turned off, their order,
    /// the starred models, the default agent — as the window holds them.
    /// `true` when they changed.
    pub(crate) fn set_agent_settings(&mut self, settings: &super::catalog::AgentSettings) -> bool {
        if *self.agent_settings == *settings {
            return false;
        }
        *self.agent_settings = settings.clone();
        true
    }

    /// **A Bot opened from the roster**: the roster's row is fresh, so the
    /// tab knows its Bot and binds its conversation at once; the first time
    /// it is shown it reads the Bot once, as every tab does — the row does
    /// not carry the Bot's open operation, its holders or its settings.
    /// Test only: the window opens a Bot on a chosen conversation
    /// ([`Self::opened_on`]).
    #[cfg(test)]
    pub(crate) fn opened(bot: &RosterBot) -> Self {
        Self::opened_on(bot, bot.main_chat, None)
    }

    /// **A Bot the roster knows, on one of its conversations** — its main
    /// one or another the window learned is the Bot's — at `block` when a
    /// link named one.
    pub(crate) fn opened_on(
        bot: &RosterBot,
        chat: ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> Self {
        let mut view = Self::blank(Some(bot.id), chat);
        view.pending_block = block;
        // Not `asked`: the first showing reads the Bot, so a parked or
        // moving operation, an archived Bot's holder and the settings are
        // the owner's from the start, not from the roster's row.
        view.know(BotFacts::from(bot));
        view.know_listed_holder(bot);
        view
    }

    /// **The Archive an Archived-filter row names as holding the Bot**, for
    /// a tab that shows it archived: its Unarchive is offered before the
    /// tab's own read answers. Nothing for an active row.
    fn know_listed_holder(&mut self, bot: &RosterBot) {
        let archived = self.facts.as_ref().is_some_and(|facts| facts.archived);
        if let (true, Some(holder)) = (archived && bot.archived, &bot.held_by) {
            self.lifecycle.know_listed_archive(holder);
        }
    }

    /// **A conversation the window learned is `bot`'s**, from its label,
    /// before the roster has listed that Bot: the tab knows which Bot and
    /// reads it when shown. `name` is the label's, for the tab's name.
    pub(crate) fn labelled(
        bot: BotId,
        chat: ChatId,
        name: &str,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> Self {
        let mut view = Self::blank(Some(bot), chat);
        view.label_name = Some(name.to_owned()).filter(|name| !name.trim().is_empty());
        view.pending_block = block;
        view
    }

    /// **A conversation whose Bot, if any, is not known yet** (B1): a link,
    /// a search hit or a restored chat tab the window could not place. The
    /// tab shows *Opening this conversation…* and nothing else — no
    /// conversation, no link, no read of its own — until the window learns
    /// whose it is and either names its Bot ([`Self::resolved`]) or puts an
    /// ordinary chat tab in its place.
    pub(crate) fn resolving(chat: ChatId, block: Option<koru_ai_ui::MessageBlockId>) -> Self {
        let mut view = Self::blank(None, chat);
        view.pending_block = block;
        view
    }

    /// **The window learned the conversation is `bot`'s**: the tab becomes
    /// that Bot's, on this conversation, and reads the Bot when shown.
    pub(crate) fn resolved(&mut self, bot: BotId, name: &str) {
        if self.bot.is_some() {
            return;
        }
        self.bot = Some(bot);
        self.label_name = Some(name.to_owned()).filter(|name| !name.trim().is_empty());
    }

    /// **The window knows this Bot's name** — the roster lists it — while
    /// the tab has read nothing yet: a tab restored behind another wears
    /// the Bot's name rather than *Bot* until it is opened (the second
    /// window run). The tab's own read wins once it has one. `true` when
    /// the label moved.
    pub(crate) fn know_name(&mut self, name: &str) -> bool {
        if self.facts.is_some() || self.create.is_some() || name.trim().is_empty() {
            return false;
        }
        if self.label_name.as_deref() == Some(name) {
            return false;
        }
        self.label_name = Some(name.to_owned());
        true
    }

    /// **The window knows this Bot's face** — the roster lists it — while
    /// the tab has read nothing yet: a tab restored behind another wears
    /// its Bot's face in the strip rather than the Bots mark until it is
    /// opened. The tab's own read wins once it has one. `true` when the
    /// strip's icon moved.
    pub(crate) fn know_face(&mut self, face: u8) -> bool {
        if self.facts.is_some() || self.create.is_some() {
            return false;
        }
        let moved = self.listed_face != Some(face);
        self.listed_face = Some(face);
        moved
    }

    /// **The face the tab strip draws for this tab**, and the name whose
    /// initial it wears: the Bot's as the tab read it; before that, the
    /// one the roster lists (`know_face`); for a create tab, the face
    /// picked in the *New bot* dialog, as its header shows it. `None` while
    /// none of those is known (a tab restored before the roster answered,
    /// a conversation whose Bot is still being found, the full create form
    /// with no face picked): the strip draws the Bots mark until it is.
    pub(crate) fn tab_face(&self) -> Option<(u8, &str)> {
        if let Some(facts) = &self.facts {
            return Some((facts.avatar, facts.name.trim()));
        }
        if let Some(create) = self.create.as_deref() {
            let draft = create.form.draft();
            return Some((draft.avatar?, draft.name.trim()));
        }
        Some((
            self.listed_face?,
            self.label_name.as_deref().unwrap_or_default().trim(),
        ))
    }

    /// Whether this tab is still finding out whose its conversation is.
    pub(crate) fn opening(&self) -> Option<Opening> {
        (self.bot.is_none() && self.create.is_none()).then_some(self.opening)
    }

    /// The window's read of whose the conversation is did not answer.
    pub(crate) fn opening_failed(&mut self) {
        if self.bot.is_none() {
            self.opening = Opening::Failed;
        }
    }

    /// The owner says the conversation is in Trash (S14).
    pub(crate) fn opening_in_trash(&mut self) {
        if self.bot.is_none() {
            self.opening = Opening::Trashed;
        }
    }

    /// The block a link named, for the ordinary tab that takes this one's
    /// place.
    pub(crate) fn pending_block(&self) -> Option<koru_ai_ui::MessageBlockId> {
        self.pending_block
    }

    /// **A Bot tab put back from the session**: it knows which Bot and which
    /// conversation, and nothing else until it is shown and the Bot service
    /// answers. Its conversation is not started before then — never as an
    /// ordinary chat in its place.
    pub(crate) fn restored(bot: BotId, chat: ChatId) -> Self {
        Self::blank(Some(bot), chat)
    }

    /// A tab over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(bot: BotId, chat: ChatId, link: BotsLink) -> Self {
        let mut view = Self::blank(Some(bot), chat);
        view.started_link = true;
        view.link = Some(link);
        view
    }

    /// The Bot as its owner holds it, handed in by a test where a read
    /// would have answered.
    #[cfg(test)]
    pub(crate) fn know_bot_for_test(&mut self, bot: Bot) {
        self.settings.know(bot.clone());
        self.know(BotFacts::from(bot));
        self.sync_name_field();
    }

    /// The Bot, once known: `None` while the tab is still finding out whose
    /// its conversation is.
    pub(crate) fn bot(&self) -> Option<BotId> {
        self.bot
    }

    /// The conversation this tab is: found by it from anywhere.
    pub(crate) fn chat_id(&self) -> ChatId {
        self.chat
    }

    /// **The conversation the window may find this tab by**: none while it
    /// creates a Bot, whose conversation does not exist yet (S8).
    pub(crate) fn conversation_id(&self) -> Option<ChatId> {
        self.create.is_none().then_some(self.chat)
    }

    /// What the tab knows of its Bot. Test only: the window reads a tab's
    /// home through [`Self::home`], which a create tab answers too (S8).
    #[cfg(test)]
    pub(crate) fn facts(&self) -> Option<&BotFacts> {
        self.facts.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn load(&self) -> Load {
        self.load
    }

    #[cfg(test)]
    pub(crate) fn work(&self) -> Work {
        self.work.get()
    }

    /// **A change of the Bot is still being delivered to what runs it**, as
    /// its last read said: the window dims the ⋯'s Archive and Move to
    /// Trash, which the owner would refuse (`bot_operation_pending`).
    pub(crate) fn delivering(&self) -> bool {
        self.settings.delivering()
    }

    /// The settings as they stand: what the owner holds, the person's
    /// edits and the changes on their way. Test only.
    #[cfg(test)]
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    /// **The window's word that this tab is no longer in front of its
    /// group.** A tab that has been shown reads its Bot once when it is
    /// shown again; one never shown has nothing stale to refresh. Nothing is
    /// sent now: a hidden tab sends nothing.
    pub(crate) fn hidden(&mut self) {
        self.routines.show(self.bot, false);
        self.memory.show(false);
        self.children.show(false);
        self.setup_hidden();
        if self.create.is_some() {
            self.create_hidden();
            return;
        }
        if (self.started_link && self.asked) || self.opening() == Some(Opening::Failed) {
            self.stale = true;
        }
        // No clock while hidden: shown again, it reads once, and an
        // unvouched window has the whole while again.
        self.bot_read_next = None;
        self.bot_read_backoff = BOT_READ_FIRST;
        self.vouching.reset();
        self.copy_hidden();
    }

    /// **Shown again after a read of whose the conversation is failed**:
    /// `true` once, and the tab says it is opening again — the window asks
    /// once more.
    pub(crate) fn take_opening_retry(&mut self) -> bool {
        if self.opening() != Some(Opening::Failed) || !self.stale {
            return false;
        }
        self.stale = false;
        self.opening = Opening::Asking;
        true
    }

    /// The embedded chat, once bound: the window's chat passes reach it
    /// through `TabView::chat_view`.
    /// `None` while the conversation is let go (its Bot in Trash): the
    /// window's passes never reach a chat kept only for its draft.
    pub(crate) fn conversation(&self) -> Option<&AiChatView> {
        self.conversation
            .as_ref()
            .filter(|_| self.conversation_live())
    }

    pub(crate) fn conversation_mut(&mut self) -> Option<&mut AiChatView> {
        let live = self.conversation_live();
        self.conversation.as_mut().filter(|_| live)
    }

    /// What the tab strip calls it: the Bot's name, or *Bot* until the Bot
    /// service has said.
    pub(crate) fn label(&self) -> String {
        if let Some(label) = self.create_label() {
            return label;
        }
        match (&self.facts, &self.label_name) {
            (Some(facts), _) if !facts.name.trim().is_empty() => facts.name.trim().to_owned(),
            (None, Some(name)) => name.trim().to_owned(),
            // Not known to be a Bot's: the name an ordinary chat tab wears
            // before its chat is read, never *Bot*.
            _ if self.bot.is_none() => t(self.lang, Words::ChatsTabNew).to_owned(),
            _ => t(self.lang, Words::BotsTabUnnamed).to_owned(),
        }
    }

    /// The home workspace's name as the window's catalog last gave it.
    pub(crate) fn home_name(&self) -> Option<String> {
        self.home_name.clone()
    }

    /// The home workspace's name, from the window's catalog. `true` when it
    /// changed what the header says.
    pub(crate) fn set_home_name(&mut self, name: Option<String>) -> bool {
        if self.home_name == name {
            return false;
        }
        self.home_name = name;
        true
    }

    /// The Bot service's newer word about this Bot — the roster's, opened
    /// again. `true` when the label may have moved.
    ///
    /// **A row never makes the tab less true than what it read** (the fourth
    /// window run's finding A: an archived Bot's tab, brought forward from
    /// its Archived row, lost *Archived* and *Unarchive* and offered Rename
    /// again). The row brings the name, the pause, the home and the agent;
    /// whether the Bot is archived or in Trash stays what the tab knows, and
    /// a row that says otherwise has the tab read its Bot again — the owner
    /// says which is current, not whichever list was opened last.
    pub(crate) fn refresh_from(&mut self, bot: &RosterBot) -> bool {
        if Some(bot.id) != self.bot {
            return false;
        }
        let mut facts = BotFacts::from(bot);
        if let Some(known) = &self.facts {
            if known.archived != facts.archived || known.trashed {
                self.asked = false;
            }
            facts.archived = known.archived;
            facts.trashed = known.trashed;
        }
        let changed = self.facts.as_ref() != Some(&facts);
        if changed {
            self.know(facts);
        }
        self.know_listed_holder(bot);
        changed
    }

    /// Take `facts` as what the tab knows, and bind the conversation to the
    /// Bot's — the main one, unless this tab is on another of its chats.
    fn know(&mut self, facts: BotFacts) {
        // **Which computer runs the Bot stays what the tab knew** (M8): a
        // read or a row that does not say keeps it, as it keeps whether the
        // Bot is archived.
        let mut facts = facts;
        if facts.owner.is_none() {
            facts.owner = self.facts.as_ref().and_then(|known| known.owner);
        }
        self.set_copy_owner(facts.owner);
        self.conversations.set_bot(facts.id);
        self.lifecycle.set_bot(facts.id);
        self.lifecycle_awaits_read = false;
        // A conversation's Trash is offered only while the Bot is active:
        // the owner refuses it for an archived Bot (`bot_archived`).
        self.conversations
            .offer_trash(!facts.archived && !facts.trashed);
        self.conversations
            .surface
            .set_delete_protected(std::collections::HashSet::from([facts.main_chat]));
        if facts.trashed {
            // Hidden with its conversations: nothing of it is bound or
            // read, and the settings it would refuse are closed.
            if self.settings_open {
                self.set_settings_open(false);
            }
            self.release_conversation();
            self.facts = Some(facts);
            self.load = Load::Ready;
            return;
        }
        let conversation = koru_ai_ui::OwnedConversation {
            chat: self.chat,
            destination: facts.home,
            provider: facts.provider.clone(),
            offers: BOT_OFFERS,
            refusals: BOT_REFUSALS,
        };
        match &mut self.conversation {
            Some(view) => {
                view.bind_owned(conversation);
            }
            None => {
                let mut view = AiChatView::owned(conversation);
                if let Some(block) = self.pending_block.take() {
                    view.open_link(self.chat, Some(block));
                }
                self.conversation = Some(view);
            }
        }
        self.facts = Some(facts);
        self.load = Load::Ready;
        // A conversation just made is told what its copy lets it do.
        self.push_saved_copy();
    }

    /// **A trashed Bot's conversation is let go, and its draft kept**: the
    /// owner refuses every read of it (`bot_trashed`), so the embedded
    /// chat's client is stopped ([`AiChatView::deactivate`]) and it is
    /// neither maintained nor drawn behind a header that no longer shows it
    /// ([`Self::conversation_live`]). The view itself — the person's unsent
    /// draft and staged attachments, which live only in memory — is kept,
    /// whoever did the Trash: a Restore's read binds it again and starts
    /// its client ([`Self::know`]).
    pub(super) fn release_conversation(&mut self) {
        if let Some(conversation) = &mut self.conversation {
            conversation.deactivate();
        }
        self.started_chat = false;
    }

    /// Whether the embedded chat is in use: bound, and its Bot not in
    /// Trash. A let-go conversation is kept for its draft and nothing else.
    fn conversation_live(&self) -> bool {
        !self.facts.as_ref().is_some_and(|facts| facts.trashed)
    }

    /// **The pass a shown tab runs** — from its draw and its maintenance,
    /// which the window gives only the tab in front. Starts the link and the
    /// conversation the first time, asks the Bot service once, and folds
    /// whatever it answered. `true` when a repaint is owed.
    pub(crate) fn shown_pass(&mut self, root: Option<PathBuf>) -> bool {
        // Creating a Bot (S8): the form's own pass.
        if self.create.is_some() {
            return self.create_pass(root);
        }
        // Not known to be a Bot's yet: nothing of the tab's own to ask.
        if self.bot.is_none() {
            return false;
        }
        if !self.started_link {
            self.started_link = true;
            self.link = BotsLink::start(root.clone());
        }
        if self.stale && !self.in_flight {
            self.stale = false;
            self.asked = false;
            // Changed elsewhere meanwhile, maybe: the list reads once too,
            // and an operation on its way is read now (S14).
            self.conversations.owe_read();
            self.lifecycle_read_now();
            if self.facts.as_ref().is_some_and(|facts| facts.trashed) {
                self.trash_read_owed = true;
            }
            // The agents and their readiness may have moved too.
            self.catalog.shown();
            self.library.owe_read();
        }
        let lifecycle = self.lifecycle_pass(std::time::Instant::now());
        if self.settings.take_read_owed() {
            self.asked = false;
        }
        self.bot_read_due(std::time::Instant::now());
        let asked = !self.asked && !self.in_flight && self.ask();
        let answered = self.poll();
        let polled = self.send_polls();
        if !self.started_chat
            && self.conversation_live()
            && let Some(conversation) = &mut self.conversation
        {
            self.started_chat = true;
            conversation.activate();
        }
        // A trashed Bot lists nothing: its list would be refused.
        let listed = !self.facts.as_ref().is_some_and(|facts| facts.trashed) && self.list_pass();
        // The conversation's requests to other Bots, and who asked for it
        // (M6), on their own link.
        let children = self.children_pass(root.clone());
        // The catalog only while the settings are shown (S6).
        let catalog = self.settings_open && self.catalog.pass(root);
        self.settle_agent_refreshes();
        let library = self.library_pass();
        let memory = self.memory_pass();
        let routines = self.routines_pass();
        // Whether the tab draws the conversation's empty transcript (O155).
        let setup = self.setup_pass();
        // Another computer's Bot (M8): its copy's state, on this link.
        let copy = self.copy_pass();
        asked
            || copy
            || setup
            || answered
            || polled
            || listed
            || children
            || lifecycle
            || catalog
            || library
            || memory
            || routines
    }

    /// **The pass of the conversation's requests to other Bots** (M6): once
    /// the tab knows its Bot, and never for a Bot in Trash or a create tab.
    /// `true` when what the band says changed.
    fn children_pass(&mut self, root: Option<PathBuf>) -> bool {
        if self
            .conversation
            .as_ref()
            .is_some_and(|c| c.is_engine_owned())
        {
            let changed = self.children.shows();
            self.children.show(false);
            return changed;
        }
        if self.create.is_some() {
            return false;
        }
        let Some(facts) = self.facts.as_ref().filter(|facts| !facts.trashed) else {
            return false;
        };
        let (bot, main, name) = (facts.id, facts.main_chat, facts.name.clone());
        self.children.start(root);
        let before = self.children.shows();
        let moved = self.children.set_subject(bot, self.chat, main);
        let worded = self.children.set_words(self.lang, &name);
        self.children.show(true);
        let answered = self.children.pass(std::time::Instant::now());
        // A child conversation is labelled in the list by who asked for it.
        if let Some((chat, asker)) = self.children.take_origin_news() {
            self.conversations.know_asked_by(chat, &asker);
        }
        answered || (moved && before) || (worded && self.children.shows())
    }

    // ------------------------------------ mentioning another Bot

    /// **The Bots the window's roster lists**, for the conversation's `@`
    /// until the tab's own read answers (`bots::mentions`). `true` when the
    /// list changed.
    pub(crate) fn know_mention_roster(&mut self, rows: &[crate::bots::roster::RosterBot]) -> bool {
        self.mentions
            .seed(rows.iter().map(crate::bots::mentions::Listed::from))
    }

    /// **The Bot whose conversation this is, when its `@` may offer Bots**:
    /// asking is open in this build, the owner's own read of the Bot is
    /// held (it says the setting), the Bot is active, and this computer
    /// runs it. `None` otherwise: the picker then lists pages only.
    fn mention_asker(&self) -> Option<crate::bots::mentions::Asker> {
        if !crate::bots::children::ASKING_OPEN || self.create.is_some() || self.copy().is_some() {
            return None;
        }
        let bot = self.settings.bot()?;
        (bot.lifecycle == BotLifecycle::Active).then_some(crate::bots::mentions::Asker {
            id: bot.id,
            home: bot.home_workspace,
            may_ask: bot.may_ask,
        })
    }

    /// **Feed the conversation's `@` and keep its list fresh**: the askable
    /// Bots and the names sent mentions read as go to the conversation; and
    /// while its picker is open and what is held is old, the workspace's
    /// Bots are asked for on the tab's own link (queued, never waited on).
    /// `true` when what is drawn changed.
    fn mentions_pass(&mut self, now: std::time::Instant) -> bool {
        let asker = self.mention_asker();
        let lang = self.lang;
        if !self.conversation_live() {
            return false;
        }
        let Some(conversation) = self.conversation.as_mut() else {
            return false;
        };
        let (section, names) = self.mentions.feed(lang, asker);
        let mut changed = conversation.set_mention_choices(section);
        changed |= conversation.set_mention_names(names);
        let open = conversation.mention_picker_open();
        if let Some(asker) = asker.filter(|asker| open && asker.may_ask)
            && self.mentions.due(now)
            && let Some(link) = &self.link
        {
            let seq = self.mentions.asking(now);
            if link.send(Command::List {
                seq,
                workspace: asker.home,
            }) {
                let mut work = self.work.get();
                work.commands += 1;
                self.work.set(work);
            } else {
                self.mentions.unsent();
            }
        }
        changed
    }

    /// The requests' pass on the window's maintenance: what the link
    /// answered, and a read come due — for a tab already shown, which only
    /// its draw makes it.
    fn children_maintain(&mut self, now: std::time::Instant) -> bool {
        if self
            .conversation
            .as_ref()
            .is_some_and(|c| c.is_engine_owned())
        {
            let changed = self.children.shows();
            self.children.show(false);
            return changed;
        }
        let answered = self.children.pass(now);
        if let Some((chat, asker)) = self.children.take_origin_news() {
            self.conversations.know_asked_by(chat, &asker);
        }
        answered
    }

    /// **The conversation a request card or the origin line asked to open**
    /// (M6, contract 7.5): the Bot, its name for the tab until it is read,
    /// and the conversation. Taken once by the window.
    pub(crate) fn take_open_request(&mut self) -> Option<crate::bots::children::OpenRequest> {
        if let Some((bot, name, chat)) = self
            .conversation
            .as_mut()
            .and_then(|c| c.take_open_bot_conversation())
        {
            return Some(crate::bots::children::OpenRequest { bot, name, chat });
        }
        self.open_request.take()
    }

    /// **The faces the window's roster lists**, for the request cards'
    /// leads. `true` when a card's face moved.
    pub(crate) fn know_faces(&mut self, faces: impl IntoIterator<Item = (BotId, u8)>) -> bool {
        self.children.set_faces(faces) && self.children.shows()
    }

    /// The conversation's requests and origin, as the tab holds them. Test
    /// only.
    #[cfg(test)]
    pub(crate) fn children(&self) -> &crate::bots::children::Children {
        &self.children
    }

    /// The requests' link, for a test that answers it.
    #[cfg(test)]
    pub(crate) fn children_with_link(&mut self, link: BotsLink) {
        *self.children = crate::bots::children::Children::with_link(link);
    }

    /// **The band over the conversation** (M6): the origin line, the line
    /// said while the conversation waits, and the request cards, each link
    /// one of the tab's own buttons. `None` when it has nothing to say.
    pub(super) fn child_band(&self, engine: &mut koru_draw::text::TextEngine) -> Option<El> {
        let mut button = |name: &'static str, key: Key, label: String| {
            self.button(
                engine,
                name,
                key,
                label,
                koru_ui::button::ActionStyle::Ghost,
                false,
            )
        };
        self.children.band(&mut button)
    }

    /// The `ReadOperation`s the settings owe now. `true` when any went.
    fn send_polls(&mut self) -> bool {
        let polls = self.settings.polls(std::time::Instant::now());
        let any = !polls.is_empty();
        for poll in polls {
            self.send_write(poll);
        }
        any
    }

    /// Hand a settings change to the link; one it will not take is shown as
    /// not sent, never as on its way.
    fn send_write(&mut self, command: Command) {
        let Command::Write { request, .. } = &command else {
            return;
        };
        let request = *request;
        let sent = self.link.as_ref().is_some_and(|link| link.send(command));
        let mut work = self.work.get();
        work.commands += usize::from(sent);
        self.work.set(work);
        if !sent {
            self.settings.unsent(request);
            self.library.unsent(request);
            self.lifecycle.unsent(request, std::time::Instant::now());
        }
    }

    /// Ask for the Bot. A full queue leaves it unasked, for a later pass.
    fn ask(&mut self) -> bool {
        let (Some(link), Some(bot)) = (&self.link, self.bot) else {
            return false;
        };
        let seq = self.seq.wrapping_add(1);
        if !link.send(Command::Read { seq, bot }) {
            return false;
        }
        self.seq = seq;
        self.asked = true;
        self.in_flight = true;
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        if self.facts.is_none() {
            self.load = Load::Loading;
        }
        true
    }

    /// Fold what the link answered: **only the latest request's answer about
    /// this Bot**, so an older reply never replaces newer state — and **in
    /// the order the link answered**. The link is one thread that answers
    /// first in first out, so a Bot read sent before the person confirmed
    /// an Archive is answered before the Archive is; folded in that order,
    /// the read lands while the Archive is still on its way (and is not
    /// allowed to drop it), never after the Archive's answer, which it
    /// would then contradict with an older state.
    fn poll(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let answers = link.drain();
        let creating = self.create_bot();
        let now = std::time::Instant::now();
        let mut changed = false;
        for answer in answers {
            match answer {
                Answer::RoutineDetail {
                    seq,
                    bot,
                    schedule,
                    result,
                } => {
                    changed |= self.routines.detail_answer(seq, bot, schedule, result);
                }
                Answer::Routines { seq, bot, result } => {
                    changed |= self.routines.answer(seq, bot, result);
                }
                // The owner's answer to one Routine control: confirmed, or
                // refused in a sentence. Never folded for another control.
                Answer::RoutineControl {
                    seq,
                    bot,
                    schedule,
                    result,
                } => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
                        .unwrap_or(0);
                    changed |= self
                        .routines
                        .control_answer(seq, bot, schedule, result, now_ms);
                }
                Answer::RoutineLabel {
                    seq,
                    bot,
                    chat,
                    result,
                } => {
                    changed |= self.routines.labelled(seq, bot, chat, result);
                }
                Answer::Memory { request, result } => {
                    if let Some(bot) = self.settings.bot() {
                        let folded =
                            self.memory
                                .answer(self.context_instance, bot, request, result);
                        if folded {
                            if self.memory.known_revision() > bot.revision {
                                self.asked = false;
                                self.library.owe_read();
                            }
                            changed = true;
                        }
                    }
                }
                Answer::Library { seq, bot, result } if Some(bot) == self.bot => {
                    if let Some(bot) = self.settings.bot() {
                        changed |= self.library.answer_read(seq, bot, result);
                        if self.library.known_revision() > bot.revision {
                            self.asked = false;
                        }
                    }
                }
                // Why a write was refused for its agent: noted before its
                // refusal is folded, so the refusal names why.
                Answer::Reason {
                    bot,
                    request,
                    reason,
                } => {
                    if Some(bot) == creating {
                        if let Some(create) = self.create.as_deref_mut() {
                            create.form.note_reason(request, reason);
                        }
                    } else if Some(bot) == self.bot {
                        self.settings.note_reason(request, reason);
                    }
                }
                // The create form's (S8): by its draft's reserved BotId.
                Answer::Wrote {
                    bot,
                    request,
                    result,
                } if Some(bot) == creating => changed |= self.create_answer(request, result),
                Answer::Chats { seq, bot, result } => {
                    changed |= self.conversations.answer(seq, bot, result);
                }
                // The workspace's Bots, asked for when the conversation's
                // `@` opened: who it may offer (`bots::mentions`).
                Answer::Listed {
                    seq,
                    workspace,
                    result,
                } => {
                    let home = self.settings.bot().map(|bot| bot.home_workspace);
                    changed |= self
                        .mentions
                        .answer(seq, home, workspace, result, self.here(), now);
                }
                Answer::Read { seq, bot, result } if seq == self.seq && Some(bot) == self.bot => {
                    self.in_flight = false;
                    self.take(result);
                    changed = true;
                }
                Answer::Wrote {
                    bot,
                    request,
                    result,
                } if Some(bot) == self.bot => {
                    // What a command for another computer came to (M8),
                    // beside whatever the write's own surface says.
                    changed |= self.copy_wrote(request, &result);
                    changed |= self.take_written(request, result, now);
                }
                // Another computer's Bot (M8): its copy's state, and how
                // far a refused command got on its way there.
                Answer::LinkedOwner { seq, bot, result } => {
                    changed |= self.copy_answer(seq, bot, result);
                }
                Answer::Delivery {
                    bot,
                    request,
                    delivery,
                } => self.copy_delivery(bot, request, delivery),
                Answer::Lifecycle {
                    seq,
                    lifecycle: BotLifecycle::Trashed,
                    result,
                    ..
                } if seq == self.trash_seq => {
                    self.trash_in_flight = false;
                    self.take_trash_listing(result);
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.sync_name_field();
        }
        changed
    }

    /// **What the owner said to one of this tab's writes**: a lifecycle
    /// operation's, *New conversation*'s, or a settings change's. `true`
    /// when it was this tab's.
    fn take_written(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
        now: std::time::Instant,
    ) -> bool {
        if self.library.owns(request) {
            self.library.answer_save(request, result);
            self.asked = false;
            return true;
        }
        // A lifecycle operation's answer is the lifecycle's (S14).
        if self.lifecycle.owns(request) {
            self.lifecycle.answer(request, result, now);
            self.after_lifecycle();
            return true;
        }
        // *New conversation*'s answer is the list's, not the settings'.
        if self.conversations.wrote(request, &result) {
            // Archived meanwhile: read the Bot again, so the header says so
            // beside the refusal.
            let archived = super::settings::BOT_ARCHIVED;
            if matches!(&result, Err(Failure::Refused(code)) if code == archived) {
                self.asked = false;
            }
            return true;
        }
        let settled = self.settings.answer(request, result, now);
        if let Some(bot) = settled.bot {
            self.library.know_bot(&bot);
            self.note_roster_news(&bot);
            // A write's answer: it never drops or replaces the operation the
            // tab follows — only a read does.
            self.lifecycle.know_written(&bot);
            self.know(BotFacts::from(bot));
        }
        settled.matched
    }

    fn take(&mut self, result: Result<Bot, Failure>) {
        if self.routines.held.is_some()
            && result
                .as_ref()
                .ok()
                .is_none_or(|bot| self.settings.bot() != Some(bot))
        {
            self.routines.invalidate();
        }
        if matches!(&result, Ok(bot) if Some(bot.id) == self.bot &&
            (bot.revision < self.library.known_revision()
                || bot.revision < self.memory.known_revision()
                || self.settings.bot().is_some_and(|known| bot.revision < known.revision)))
        {
            self.asked = false;
            return;
        }
        match result {
            // A Bot that says it is another is not this tab's.
            Ok(bot) if Some(bot.id) == self.bot => {
                self.vouching.reset();
                // **The list read after a refused start is read again** with
                // the Bot: refused for the same reason, it said only that it
                // may be old.
                if self.conversations.load() == conversations::Load::Unavailable {
                    self.conversations.owe_read();
                }
                // **A change still being delivered is read again** on the
                // bounded clock while the tab is shown (never while hidden),
                // so its sentence and the controls it holds inert go by
                // themselves once what runs the Bot takes it. The owner
                // emits nothing when it does.
                if bot.pending_change.is_some() {
                    self.bot_read_again(std::time::Instant::now());
                } else {
                    self.bot_read_answered();
                }
                self.note_roster_news(&bot);
                self.library.know_bot(&bot);
                self.memory.know(&bot);
                self.settings.know(bot.clone());
                if bot.lifecycle != BotLifecycle::Trashed {
                    self.trashed_bot = None;
                }
                // **The lifecycle knows its Bot before it reads the open
                // operation**: `know` gives it the Bot, so the first read of
                // a tab restored at startup or opened from a link adopts a
                // parked or moving operation rather than finding no Bot.
                // Another device's Bot is that computer's (M8): the tab is
                // a saved copy's from here on.
                let owner = self.remote_owner(&bot);
                let mut facts = BotFacts::from(bot.clone());
                facts.owner = owner;
                self.know(facts);
                self.lifecycle.know(&bot);
                // A read may finish the operation followed — one the owner
                // completed by itself — which the tab acts on as on an
                // answer.
                self.after_lifecycle();
            }
            // A trashed Bot's read answers *not found* (P1.7): its home's
            // Trash filter says whether that is where it is.
            Err(Failure::Refused(code)) if code == "not_found" && self.facts.is_some() => {
                self.bot_read_answered();
                self.trash_read_owed = true;
            }
            Ok(_) => self.load = Load::Unavailable,
            Err(failure) => {
                let now = std::time::Instant::now();
                // **A read a Restore left owed is asked again** on the
                // bounded clock while the tab is shown — until it answers,
                // nothing else is offered — when asking again may help. So
                // is one a settled `rejected` waits on, or one that last
                // said a change was still being delivered, and **a tab that
                // has never had its Bot** — restored at startup, before
                // Koru AI or its vouching is up.
                let owed = self.lifecycle_awaits_read
                    || self.settings.awaits_read()
                    || self.settings.delivering()
                    || self.facts.is_none();
                match self.vouching.read_failed(&failure, now) {
                    Reread::Again => {
                        self.load = Load::Unavailable;
                        if owed {
                            self.bot_read_again(now);
                        }
                    }
                    // Koru AI answered: the window is not vouched for yet.
                    // Still loading, not *isn't answering*.
                    Reread::Unvouched => {
                        if self.facts.is_none() {
                            self.load = Load::Loading;
                        }
                        self.bot_read_again(now);
                    }
                    Reread::Denied => {
                        self.load = Load::Denied;
                        self.settings.read_refused();
                    }
                    Reread::Final => {
                        self.load = Load::Unavailable;
                        self.settings.read_refused();
                    }
                }
            }
        }
    }

    /// The Bot's read answered: no retry of it is owed.
    fn bot_read_answered(&mut self) {
        self.bot_read_next = None;
        self.bot_read_backoff = BOT_READ_FIRST;
    }

    /// The Bot's read is owed again at the next step of the bounded clock.
    fn bot_read_again(&mut self, now: std::time::Instant) {
        let wait = self.bot_read_backoff;
        self.bot_read_next = Some(now.checked_add(wait).unwrap_or(now));
        self.bot_read_backoff = (wait * 2).min(BOT_READ_MAX);
    }

    /// The Bot's read is due again: asked on the next shown pass.
    fn bot_read_due(&mut self, now: std::time::Instant) -> bool {
        if self.in_flight || !self.bot_read_next.is_some_and(|at| at <= now) {
            return false;
        }
        self.bot_read_next = None;
        self.asked = false;
        true
    }

    // ------------------------------------------------------------ frame

    /// The line under the header's facts, when there is one to say.
    ///
    /// A create tab says nothing here: its form says where it stands (S8).
    fn notice(&self) -> Option<Words> {
        // A create tab reads no Bot: *Loading this bot…* stood under *New
        // bot* for the form's whole life (the first window run's finding 6).
        if self.create.is_some() {
            return None;
        }
        match self.opening() {
            Some(Opening::Asking) => return Some(Words::BotsChatOpening),
            Some(Opening::Failed) => return Some(Words::BotsConversationUnavailable),
            Some(Opening::Trashed) => return Some(Words::BotsLinkInTrash),
            None => {}
        }
        match (self.load, self.facts.is_some()) {
            (Load::Cold | Load::Loading, false) => Some(Words::BotsBotLoading),
            (Load::Unavailable, _) => Some(Words::BotsBotUnavailable),
            (Load::Denied, _) => Some(Words::BotsBotNoAccess),
            _ => None,
        }
    }

    /// *Saves results in {workspace}*, naming the home by its name when the
    /// window knows it.
    pub(crate) fn saves_in(&self) -> Option<String> {
        if self.facts.is_none() && self.create.is_none() {
            return None;
        }
        let name = self
            .home_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| t(self.lang, Words::BotsHomeWorkspace).to_owned());
        Some(koru_i18n::fill(
            t(self.lang, Words::BotsSavesTo),
            &[name.as_str()],
        ))
    }

    /// The header as elements: the Bot's name and its pause, where its work
    /// goes and the first version's limit, a notice while the Bot is read
    /// or could not be, and the divider that closes it.
    fn header(&self, engine: &mut koru_draw::text::TextEngine) -> El {
        let mut header = col("Bot header")
            .id(HEADER)
            .pad_x(alias::NAV_PAD)
            .pad_top(Space::S3)
            .gap(Space::S1);
        if let Some(facts) = &self.facts {
            let mut title = row("Bot title")
                .gap(Space::S2)
                .items_center()
                // Its face (O155), beside its name: decorative, the name
                // says who it is. Moving only while the Bot works.
                .child(
                    koru_draw::el::leaf(
                        "Bot face",
                        super::faces::FaceLeaf {
                            face: facts.avatar,
                            name: facts.name.clone(),
                            pose: self.face_pose,
                            size: super::faces::HEADER_FACE,
                        },
                    )
                    .decorative(),
                )
                .child(
                    text(&facts.name)
                        .named("Bot name")
                        .id(NAME)
                        .role(roles::POPOVER_HEADING)
                        .emphasis(true)
                        .fill()
                        .truncate(),
                );
            if facts.paused {
                title = title.child(
                    text(t(self.lang, Words::BotsPaused))
                        .named("Bot paused")
                        .id(PAUSED)
                        .role(roles::META)
                        .ink(Ink::Hint),
                );
            }
            // In words, beside the name: never color alone.
            if facts.archived {
                title = title.child(
                    text(t(self.lang, Words::BotsArchived))
                        .named("Bot archived")
                        .id(ARCHIVED)
                        .role(roles::META)
                        .ink(Ink::Hint),
                );
            }
            // The lifecycle's ⋯ (S14), while it has a row to offer, and
            // *Settings*, which a trashed Bot does not have.
            if self.header_shows_actions() {
                title = title.child(self.actions_button());
            }
            if !facts.trashed {
                // Compact, the conversation column has left: its list is
                // one press away (EXPERIENCE: a reachable disclosure).
                if self.compact() {
                    title = title.child(self.conversations_button(engine));
                }
                title = title.child(self.settings_button(engine));
            }
            header = header.child(title);
            if self.chat == facts.main_chat {
                header = header.child(
                    text(t(self.lang, Words::BotsMainConversation))
                        .named("Bot main conversation")
                        .id(MAIN_MARK)
                        .role(roles::META)
                        .ink(Ink::Secondary),
                );
            }
            if let Some(saves) = self.saves_in() {
                header = header.child(
                    text(saves)
                        .named("Bot destination")
                        .id(SAVES)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap(),
                );
            }
            header = header.child(
                text(t(self.lang, Words::BotsHomeOnly))
                    .named("Bot destination limit")
                    .id(LIMIT)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
            if let Some(block) = self.lifecycle_block(engine) {
                header = header.child(block);
            }
            // Another computer's Bot (M8): that this is a saved copy and
            // as of when, and what the last command came to.
            if let Some(banner) = self.copy_banner() {
                header = header.child(
                    text(banner)
                        .named("Bot saved copy")
                        .id(copy::COPY_BANNER)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap(),
                );
            }
            if let Some(line) = self.copy_line() {
                header = header.child(
                    text(line)
                        .named("Bot copy command")
                        .id(copy::COPY_LINE)
                        .role(roles::META)
                        .ink(Ink::Hint)
                        .wrap(),
                );
            }
        } else if let Some(label) = self.create_label() {
            // A Bot being created (S8): its name so far, where its results
            // will go, and the home-only limit.
            header = header
                .child(
                    row("Bot title")
                        .gap(Space::S2)
                        .items_center()
                        // The face picked in the *New bot* dialog, when the
                        // draft came from it (O155); else the Bots mark.
                        .child(match self.create_face() {
                            Some((face, name)) => koru_draw::el::leaf(
                                "Bot face",
                                super::faces::FaceLeaf::still(
                                    face,
                                    &name,
                                    super::faces::HEADER_FACE,
                                ),
                            )
                            .decorative(),
                            None => icon(icons::ROBOT, IconSize::M).decorative(),
                        })
                        .child(
                            text(label)
                                .named("Bot name")
                                .id(NAME)
                                .role(roles::POPOVER_HEADING)
                                .emphasis(true)
                                .fill()
                                .truncate(),
                        ),
                )
                .child(
                    text(self.saves_in().unwrap_or_default())
                        .named("Bot destination")
                        .id(SAVES)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap(),
                )
                .child(
                    text(t(self.lang, Words::BotsHomeOnly))
                        .named("Bot destination limit")
                        .id(LIMIT)
                        .role(roles::META)
                        .ink(Ink::Hint)
                        .wrap(),
                );
        }
        if let Some(notice) = self.notice() {
            header = header.child(
                text(t(self.lang, notice))
                    .named("Bot notice")
                    .id(NOTICE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        header.child(
            boxed("Bot header end")
                .pad_top(Space::S2)
                .child(divider(Line::Divider)),
        )
    }

    /// The tab as elements: the header, then the conversation's place,
    /// which takes what is left.
    pub(crate) fn tree(&self, engine: &mut koru_draw::text::TextEngine) -> El {
        let tab = col("Bot tab").id(ROOT).child(self.header(engine));
        // Creating a Bot (S8): the form in the conversation's place.
        if self.create.is_some() {
            return tab.child(self.create_region(engine));
        }
        // A trashed Bot is its header alone: what it says, and Restore.
        if self.facts.as_ref().is_some_and(|facts| facts.trashed) {
            return tab;
        }
        let conversation = boxed("Bot conversation").id(CONVERSATION).fill();
        // The conversation's requests to other Bots and who asked for it
        // (M6), over the conversation and beside the column.
        let conversation = match (!self.settings_open && !self.list_covers())
            .then(|| self.child_band(engine))
            .flatten()
        {
            Some(band) => col("Bot conversation side")
                .fill()
                .child(band)
                .child(conversation),
            None => conversation,
        };
        if self.settings_open {
            tab.child(self.region(engine))
        } else if self.list_covers() {
            // Compact, the list opened: it takes the conversation's place.
            tab.child(
                koru_draw::el::row("Bot body")
                    .fill()
                    .child(self.conversation_column(engine)),
            )
        } else if self.shows_column() {
            // The Bot's conversations beside the one shown (S8).
            tab.child(
                koru_draw::el::row("Bot body")
                    .fill()
                    .child(self.conversation_column(engine))
                    .child(conversation),
            )
        } else {
            tab.child(conversation)
        }
    }

    fn area(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size.0.max(0.0), self.size.1.max(0.0))
    }

    /// The pointer's and the keyboard's state, for the states recipes.
    fn interaction(&self) -> koru_draw::el::Interaction {
        koru_draw::el::Interaction {
            hot: self.hot,
            pressed: self.pressed,
            focused: self.focus,
        }
    }

    fn layout_with(&self, engine: &mut koru_draw::text::TextEngine) -> (El, Frame) {
        let tree = self.tree(engine);
        let frame = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            self.area(),
            FrameCx::root().with_interaction(self.interaction()),
        );
        let mut work = self.work.get();
        work.layouts += 1;
        self.work.set(work);
        (tree, frame)
    }

    /// The frame as input and accessibility read it, laid out with the
    /// thread's input engine.
    pub(crate) fn with_frame<R>(&self, read: impl FnOnce(&El, &Frame) -> R) -> R {
        let (tree, frame) = crate::input_engine::with(|engine| self.layout_with(engine));
        read(&tree, &frame)
    }

    /// Where the conversation sits in the tab, from the tab's own frame.
    fn conversation_area(&self, frame: &Frame) -> Rect {
        frame
            .node(CONVERSATION)
            .map(|node| node.rect)
            .unwrap_or_else(|| self.area())
    }

    /// Lay the conversation out where the tab's frame puts it. `true` when
    /// it moved.
    fn place_conversation(&mut self, area: Rect) -> bool {
        self.conversation_mut()
            .is_some_and(|conversation| conversation.set_area(area))
    }

    /// Whether `(x, y)` is on the conversation, in the tab's coordinates —
    /// where the chat answers input; the header and the conversation column
    /// are the tab's own.
    fn on_conversation(&self, x: f32, y: f32) -> bool {
        self.with_frame(|_, frame| {
            let area = self.conversation_area(frame);
            y >= area.y && x >= area.x
        })
    }

    /// The header's nodes for the accessibility tree, in the tab's
    /// coordinates: a group named by the Bot, with its lines as labels.
    pub(crate) fn header_lines(&self) -> Vec<(Key, Rect, String)> {
        self.with_frame(|_, frame| {
            let mut out = Vec::new();
            let mut line = |key: Key, words: Option<String>| {
                if let (Some(node), Some(words)) = (frame.node(key), words) {
                    out.push((key, node.rect, words));
                }
            };
            line(
                NAME,
                self.facts
                    .as_ref()
                    .map(|facts| facts.name.clone())
                    .or_else(|| self.create_label()),
            );
            line(
                PAUSED,
                self.facts
                    .as_ref()
                    .filter(|facts| facts.paused)
                    .map(|_| t(self.lang, Words::BotsPaused).to_owned()),
            );
            line(
                ARCHIVED,
                self.facts
                    .as_ref()
                    .filter(|facts| facts.archived)
                    .map(|_| t(self.lang, Words::BotsArchived).to_owned()),
            );
            line(
                MAIN_MARK,
                self.facts
                    .as_ref()
                    .filter(|facts| facts.main_chat == self.chat)
                    .map(|_| t(self.lang, Words::BotsMainConversation).to_owned()),
            );
            line(SAVES, self.saves_in());
            line(
                LIMIT,
                (self.facts.is_some() || self.create.is_some())
                    .then(|| t(self.lang, Words::BotsHomeOnly).to_owned()),
            );
            line(
                NOTICE,
                self.notice().map(|notice| t(self.lang, notice).to_owned()),
            );
            // What the lifecycle says (S14): an operation on its way, what
            // holds the Bot, what an operation just did.
            line(
                actions::LIFECYCLE_NOTICE,
                self.lifecycle_said()
                    .map(|said| t(self.lang, said.words).to_owned()),
            );
            // Another computer's Bot (M8).
            line(copy::COPY_BANNER, self.copy_banner());
            line(copy::COPY_LINE, self.copy_line());
            out
        })
    }

    /// The header's own rect, in the tab's coordinates.
    pub(crate) fn header_rect(&self) -> Rect {
        self.with_frame(|_, frame| {
            frame
                .node(HEADER)
                .map_or(Rect::new(0.0, 0.0, 0.0, 0.0), |node| node.rect)
        })
    }

    /// Fold what changes the published tree into the chrome's fingerprint.
    pub(crate) fn revise(&self, rev: &mut koru_workspace_host::accessibility::Revision) {
        rev.byte(self.load as u8)
            .uuid(self.bot.map(|bot| bot.as_uuid()).unwrap_or_default())
            .byte(self.opening as u8);
        match &self.facts {
            Some(facts) => {
                rev.byte(1)
                    .str(&facts.name)
                    .bool(facts.paused)
                    .bool(facts.archived);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.str(self.home_name.as_deref().unwrap_or(""))
            .f32(self.size.0)
            .f32(self.size.1);
        // Another computer's Bot (M8): the header's two lines.
        rev.str(self.copy_banner().as_deref().unwrap_or(""))
            .str(self.copy_line().as_deref().unwrap_or(""));
        // A create tab's name so far (S8); its fields, lines and controls
        // are folded with the controls below.
        rev.str(self.create_label().as_deref().unwrap_or(""));
        if let Some(facts) = &self.facts {
            rev.bool(facts.trashed);
        }
        self.revise_lifecycle(rev);
        // The conversation shown, and the list beside it (S8).
        rev.uuid(self.chat.as_uuid())
            .byte(self.conversations.load() as u8)
            .bool(self.conversations.has_more())
            .bool(self.conversations.creating())
            .u64(self.conversations.listed().len() as u64);
        for chat in self.conversations.listed() {
            rev.uuid(chat.id.as_uuid())
                .str(&chat.title)
                .byte(self.conversations.surface.status(chat.id) as u8);
        }
        match self.conversations.surface.focused_chat() {
            Some(chat) => {
                rev.byte(1).uuid(chat.as_uuid());
            }
            None => {
                rev.byte(0);
            }
        }
        // The controls and the keyboard's place among them: opening the
        // settings, an edit, a pending change or a focus move each change
        // what is published. Asked only while a client is attached.
        rev.bool(self.settings_open);
        match self.focus {
            Some(key) => {
                rev.byte(1).str(key.part);
                if let koru_draw::el::Object::Model(id) = key.object {
                    rev.u64(id as u64).u64((id >> 64) as u64);
                }
            }
            None => {
                rev.byte(0);
            }
        }
        fn fold(rev: &mut koru_workspace_host::accessibility::Revision, controls: &[Control]) {
            for control in controls {
                rev.str(control.key.part)
                    .str(&control.label)
                    .str(control.value.as_deref().unwrap_or(""))
                    .byte(match control.selected {
                        None => 0,
                        Some(false) => 1,
                        Some(true) => 2,
                    })
                    .bool(control.disabled)
                    .bool(control.focused)
                    // Where it is: a scroll of the form or a popover's list
                    // moves every rect and nothing else, and the published
                    // bounds went stale after a wheel (finding 8).
                    .f32(control.rect.x)
                    .f32(control.rect.y)
                    .f32(control.rect.w)
                    .f32(control.rect.h)
                    .u64(control.children.len() as u64);
                fold(rev, &control.children);
            }
        }
        fold(rev, &self.controls());
    }

    // ------------------------------------------------- the window's F6

    /// Whether the settings are open in the conversation's place. Test only.
    #[cfg(test)]
    pub(crate) fn settings_shown(&self) -> bool {
        self.settings_open
    }

    /// Whether the header has a control the window's `F6` rotation can land
    /// on: *Settings*, which the header draws once it knows its Bot.
    pub(crate) fn header_stop(&self) -> bool {
        self.facts.is_some() || self.create.is_some()
    }

    /// Whether one of the tab's own controls has the keyboard — *Settings*
    /// or one of the region's — rather than the conversation.
    pub(crate) fn holds_keyboard(&self) -> bool {
        self.focus.is_some()
    }

    /// **`F6` landed on this tab**: the keyboard goes to the header's
    /// *Settings*, where Enter or Space opens the settings and Escape gives
    /// it back to the conversation. `false` when the header has no control.
    pub(crate) fn focus_header(&mut self) -> bool {
        if !self.header_stop() {
            return false;
        }
        self.blur_name_field();
        // A create tab has no *Settings*: the form's first control, its
        // name, is where the keyboard lands (S8).
        if self.create.is_some() {
            self.focus = self.focus_order().first().copied();
            return self.focus.is_some();
        }
        // A trashed Bot has no *Settings*: its first control is the
        // keyboard's landing, when it has one.
        let trashed = self.facts.as_ref().is_some_and(|facts| facts.trashed);
        self.focus = if trashed {
            self.focus_order().first().copied()
        } else {
            Some(region::SETTINGS_BUTTON)
        };
        self.focus.is_some()
    }

    /// **`F6` moved on, or a press landed outside the tab**: none of the
    /// tab's controls has the keyboard any more. The settings stay as they
    /// were. `true` when one had it.
    pub(crate) fn release_keyboard(&mut self) -> bool {
        let had = self.focus.take().is_some();
        if had {
            self.blur_name_field();
        }
        had
    }

    fn blur_name_field(&self) {
        self.blur_fields();
    }
}

impl View for BotView {
    fn set_mods(&mut self, mods: ModifiersState) {
        if let Some(conversation) = self.conversation_mut() {
            View::set_mods(conversation, mods);
        }
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // The page picker's while it is up; the region's while it is open,
        // or *Settings*'s while it has the keyboard; every other key is the
        // conversation's.
        if self.picker.is_some() {
            return self.picker_key(key, text, mods);
        }
        if self.agent_popover_open() {
            return self.agent_key(key, text, mods);
        }
        if let Some(handled) = self.list_key(key, mods) {
            return handled;
        }
        // The quick setup's own controls while one has the keyboard (O155).
        if let Some(handled) = self.setup_key(key, text, mods) {
            return handled;
        }
        if let Some(handled) = self.region_key(key, text, mods) {
            return handled;
        }
        // **A form that covers the conversation keeps every key from it**:
        // with the settings open and nothing of theirs focused, typing went
        // to the hidden composer (the second window run: a message sent
        // doubled). The conversation takes keys only while it is shown.
        if self.form_open() || self.list_covers() || !self.shows_conversation() {
            return false;
        }
        self.conversation_mut()
            .is_some_and(|conversation| View::key(conversation, ws, key, text, mods))
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        if self.picker.is_some() {
            return self.picker_press(x, y);
        }
        if self.agent_popover_open() {
            return self.agent_press(x, y);
        }
        if !self.form_open()
            && let Some(handled) = self.list_press(x, y)
        {
            return handled;
        }
        if let Some(handled) = self.region_press(x, y) {
            return handled;
        }
        if !self.shows_conversation() || self.list_covers() || !self.on_conversation(x, y) {
            return false;
        }
        self.focus = None;
        let handled = self
            .conversation_mut()
            .is_some_and(|conversation| View::mouse_down(conversation, ws, x, y, ctrl));
        // A press the chat handed back: on the quick setup the tab draws in
        // its empty transcript, with nothing of the chat's over it (O155).
        match self
            .conversation_mut()
            .and_then(AiChatView::take_hosted_press)
        {
            Some((x, y)) => self.setup_press(x, y) || handled,
            None => handled,
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.picker.is_some() {
            return self.picker_move(x, y);
        }
        if self.agent_popover_open() {
            return self.agent_move(x, y);
        }
        self.pointer.set((x, y));
        // Over the quick setup (O155): its controls light, and the chat
        // still hears where the pointer is.
        let (in_setup, setup_lit) = self.setup_move(x, y);
        if in_setup {
            let chat = self
                .conversation
                .as_mut()
                .is_some_and(|conversation| View::mouse_move(conversation, x, y));
            return setup_lit || chat;
        }
        // Over the conversation with nothing lit there is nothing of the
        // tab's to light: no layout.
        let over_column = x <= self.column_right.get();
        let over_tab =
            self.form_open() || y <= self.header_bottom.get() || over_column || self.hot.is_some();
        let lit =
            (self.facts.is_some() || self.create.is_some()) && over_tab && self.region_move(x, y);
        if self.form_open() {
            return lit || setup_lit;
        }
        let listed =
            (over_column || self.conversations.surface.hover().is_some()) && self.list_move(x, y);
        let lit = lit || listed || setup_lit;
        if over_column || !self.shows_conversation() {
            return lit;
        }
        let chat = self
            .conversation
            .as_mut()
            .is_some_and(|conversation| View::mouse_move(conversation, x, y));
        lit || chat
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.picker.is_some() {
            return self.picker_release();
        }
        if self.agent_popover_open() {
            return self.agent_release();
        }
        self.region_release();
        self.setup_release();
        if self.form_open() {
            return false;
        }
        self.conversation_mut()
            .is_some_and(|conversation| View::mouse_up(conversation, ws))
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if self.form_open() || !self.shows_conversation() || !self.on_conversation(x, y) {
            return false;
        }
        self.conversation_mut()
            .is_some_and(|conversation| View::mouse_double_click(conversation, ws, x, y))
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        // The wheel over the picker scrolls its list, and nothing under it.
        if self.picker.is_some() {
            self.picker_scroll(delta);
            return;
        }
        if self.agent_popover_open() {
            self.agent_scroll(delta);
            return;
        }
        if self.form_open() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    -lines * koru_draw::theme::metrics::ROW_MIN.value
                }
                MouseScrollDelta::PixelDelta(position) => -(position.y as f32),
            };
            self.scroll_region(dy);
            return;
        }
        // The wheel over the conversation column scrolls its list.
        let (x, y) = self.pointer.get();
        if x <= self.column_right.get() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    lines * koru_draw::theme::metrics::ROW_MIN.value
                }
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            self.list_scroll(x, y, dy);
            return;
        }
        if !self.shows_conversation() {
            return;
        }
        // The wheel over the quick setup scrolls it (O155).
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, lines) => {
                -lines * koru_draw::theme::metrics::ROW_MIN.value
            }
            MouseScrollDelta::PixelDelta(position) => -(position.y as f32),
        };
        if self.setup_scroll(x, y, dy) {
            return;
        }
        if let Some(conversation) = self.conversation_mut() {
            View::scroll_by(conversation, delta);
        }
    }

    /// Only the size is kept: nothing is laid out until the tab is drawn,
    /// so a tab sized behind another costs nothing.
    fn set_viewport(&mut self, w: f32, h: f32) {
        self.size = (w, h);
    }

    fn cursor_icon(&self) -> CursorIcon {
        if self.hot == Some(region::NAME_FIELD)
            || self.hot.is_some_and(|hot| create::FIELDS.contains(&hot))
        {
            return CursorIcon::Text;
        }
        // The I-beam over the card's picker's search field (S7).
        if let Some(Some(cursor)) = self.agent.as_deref().and_then(|popover| {
            let (x, y) = popover.pointer;
            self.agent_picker_read(|picker| picker.cursor(x, y))
        }) {
            return cursor;
        }
        if self.form_open() {
            return CursorIcon::Default;
        }
        // Over the quick setup: the I-beam in its field, the arrow on the
        // rest (O155).
        if let Some(hot) = self.hot.filter(|hot| onboard::is_setup_key(*hot)) {
            return if hot == onboard::ROLE_FIELD {
                CursorIcon::Text
            } else {
                CursorIcon::Default
            };
        }
        self.conversation()
            .map_or(CursorIcon::Default, View::cursor_icon)
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        if let Some(conversation) = self.conversation_mut() {
            View::invalidate(conversation);
        }
    }

    fn draw(
        &mut self,
        scene_out: &mut Scene,
        workspace: &Workspace,
        images: &koru_draw::ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        self.shown_pass(crate::search::gateway_root());
        let bounds = self.area();
        // **One frame for the tab**: the header's place and the
        // conversation's come from it, as input and accessibility read them.
        let (header, header_frame, region, column, band, conversation_area) =
            crate::input_engine::with(|engine| {
                let (_, frame) = self.layout_with(engine);
                let header_rect = frame
                    .node(HEADER)
                    .map_or(Rect::new(0.0, 0.0, 0.0, 0.0), |node| node.rect);
                // The band over the conversation (M6) is the tab's own too:
                // the pointer over it lights its links.
                let band_rect = frame
                    .node(crate::bots::children::BAND)
                    .map(|node| node.rect);
                self.header_bottom
                    .set(band_rect.map_or(header_rect.y + header_rect.h, |rect| {
                        (rect.y + rect.h).max(header_rect.y + header_rect.h)
                    }));
                let conversation_area = self.conversation_area(&frame);
                let column_rect = frame.node(list::CONVERSATIONS).map(|node| node.rect);
                self.column_right
                    .set(column_rect.map_or(f32::NEG_INFINITY, |rect| rect.x + rect.w));
                let list_area = Self::list_area(&frame);
                let cx = FrameCx::root().with_interaction(self.interaction());
                // The header painted from a frame of its own at its own rect,
                // so its inspector record is the header's size, inside the
                // tab's; the region likewise, at the conversation's place.
                let header = self.header(engine);
                let header_frame =
                    header.layout(engine, &mut self.cache.borrow_mut(), header_rect, cx);
                let region = self.form_open().then(|| {
                    let area = frame
                        .node(region::SETTINGS)
                        .map_or(conversation_area, |node| node.rect);
                    let tree = if self.create.is_some() {
                        self.create_region(engine)
                    } else {
                        self.region(engine)
                    };
                    let laid = tree.layout(engine, &mut self.cache.borrow_mut(), area, cx);
                    (tree, laid)
                });
                // The column likewise, at its own rect, and the list's rows
                // at the place the tab's frame gives them.
                let column = column_rect.map(|rect| {
                    let tree = self.conversation_column(engine);
                    let laid = tree.layout(engine, &mut self.cache.borrow_mut(), rect, cx);
                    (tree, laid, list_area)
                });
                // The band likewise, at its own rect.
                let band = band_rect.and_then(|rect| {
                    let tree = self.child_band(engine)?;
                    let laid = tree.layout(engine, &mut self.cache.borrow_mut(), rect, cx);
                    Some((tree, laid))
                });
                let mut work = self.work.get();
                work.layouts += 1;
                self.work.set(work);
                (
                    header,
                    header_frame,
                    region,
                    column,
                    band,
                    conversation_area,
                )
            });
        self.place_conversation(conversation_area);
        // The room the chat gives the quick setup, now the chat is placed.
        self.place_setup();
        // The `@` typed since the last pass finds its Bots already there,
        // and an old list is asked for again while the picker is open.
        self.mentions_pass(std::time::Instant::now());
        let p = koru_draw::theme::p();
        scene::fill(
            scene_out,
            root,
            p.bg,
            &scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        );
        // **Every primitive inside its parent** for F12: the tab's record
        // holds the header's elements and the conversation's own records.
        let _tab = koru_draw::inspect::scope(
            "Bot tab",
            "container",
            root,
            scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
            |record| {
                record.property(
                    "layout",
                    "header over the Bot's conversation",
                    "bots::view::BotView::tree",
                );
            },
        );
        crate::input_engine::with(|engine| header_frame.paint(&header, scene_out, root, engine));
        if let Some((tree, frame, list_area)) = &column {
            crate::input_engine::with(|engine| frame.paint(tree, scene_out, root, engine));
            if let Some(area) = list_area {
                self.paint_list(scene_out, root, *area);
            }
        }
        if let Some((tree, frame)) = &band {
            crate::input_engine::with(|engine| frame.paint(tree, scene_out, root, engine));
        }
        if let Some((tree, frame)) = &region {
            crate::input_engine::with(|engine| frame.paint(tree, scene_out, root, engine));
            self.paint_picker(scene_out, root);
            self.paint_agent_popover(scene_out, root);
            return;
        }
        // Nothing of a conversation the tab cannot show: a trashed Bot's, or
        // one that went to Trash with nothing to take its place (S14) — nor
        // one the compact list covers.
        if !self.shows_conversation() || self.list_covers() {
            return;
        }
        // The quick setup, laid out before the chat is borrowed to draw:
        // the chat calls its painter where its own greeting would be.
        let setup = crate::input_engine::with(|engine| self.setup_layout(engine));
        if let Some(conversation) = &mut self.conversation {
            let a = conversation_area;
            let _conversation = koru_draw::inspect::scope(
                "Bot conversation",
                "container",
                root,
                scene::rect(a.x, a.y, a.w, a.h),
                |record| {
                    record.property(
                        "surface",
                        "koru_ai_ui::ChatSurface, owned by the Bot",
                        "ChatSurface::bind_owned",
                    );
                },
            );
            match &setup {
                Some((tree, frame)) => conversation.draw_hosting(
                    scene_out,
                    workspace,
                    root,
                    &mut |canvas, root, room| {
                        let _setup = koru_draw::inspect::scope(
                            "Bot setup",
                            "container",
                            root,
                            scene::rect(room.x, room.y, room.w, room.h),
                            |record| {
                                record.property(
                                    "layout",
                                    "a greeting and a two-step card over an empty conversation",
                                    "bots::view::onboard::BotView::setup_tree",
                                );
                            },
                        );
                        crate::input_engine::with(|engine| {
                            frame.paint(tree, canvas, root, engine);
                        });
                    },
                ),
                None => View::draw(
                    conversation,
                    scene_out,
                    workspace,
                    images,
                    root,
                    caret_on,
                    pointer,
                ),
            }
        }
        // *Browse workspace…*'s picker floats over the conversation.
        self.paint_picker(scene_out, root);
    }

    fn animating(&self) -> bool {
        self.conversation().is_some_and(View::animating)
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        let now = std::time::Instant::now();
        let read = self.in_flight.then(|| {
            now.checked_add(std::time::Duration::from_millis(50))
                .unwrap_or(now)
        });
        // A change on its way, or an operation the owner holds as pending:
        // asked about only while the tab is shown, which is the only time the
        // window asks this. The list's page on its way too (S8).
        let read = read.or_else(|| {
            (self.conversations.awaiting()
                || self.trash_awaiting()
                || (!self.stale
                    && (self.settings_open
                        || self.context_library_visible
                        || self.setup_reads_library())
                    && self.library.awaiting())
                // A Role read or write with the window's worker (O155),
                // shown only.
                || (!self.stale && self.role_saving())
                || (self.memory_available()
                    && !self.stale
                    && self.settings_open
                    && self.memory.busy())
                || (self.form_open() && self.catalog.waiting())
                // A Routine read or control on its way, shown only.
                || (!self.stale
                    && self.routines.visible()
                    && (self.routines.loading()
                        || self.routines.detail.loading()
                        || self.routines.control_busy())))
            .then(|| {
                now.checked_add(std::time::Duration::from_millis(50))
                    .unwrap_or(now)
            })
        });
        // A create tab's creation, moving and shown (S8).
        let creating = (!self.stale).then(|| self.create_deadline(now)).flatten();
        // A Bot read owed again on its clock, shown only.
        let reread = self.bot_read_next.filter(|_| !self.stale);
        // An agent's readiness still being checked, asked again on its
        // clock while the form that shows it is (S6, finding 3).
        let checking = (!self.stale && self.form_open())
            .then(|| self.catalog.deadline())
            .flatten();
        let mine = [
            read,
            // A Test's wait or minute ending, while its Routines are shown.
            (!self.stale).then(|| self.routines_deadline(now)).flatten(),
            // A request read on its way, owed, or come due (M6), shown only.
            (!self.stale).then(|| self.children.deadline(now)).flatten(),
            self.settings.deadline(now),
            self.lifecycle_deadline(now),
            creating,
            reread,
            checking,
            // Another computer's copy state, read again while shown (M8).
            (!self.stale).then(|| self.copy_deadline(now)).flatten(),
            // The Bots the `@` offers, asked for and not answered yet.
            (!self.stale && self.mentions.waiting(now)).then(|| {
                now.checked_add(std::time::Duration::from_millis(50))
                    .unwrap_or(now)
            }),
        ]
        .into_iter()
        .flatten()
        .min();
        let chat = self.conversation().and_then(View::maintenance_deadline);
        match (mine, chat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        let folded = self.poll();
        // Never from a tab the window has said is hidden.
        let polled = !self.stale && self.send_polls();
        let lifecycle = !self.stale && self.lifecycle_pass(now);
        // A read of the Bot an operation left owed goes on the next shown
        // pass, as every other read of it does.
        let listed = !self.stale
            && self.facts.as_ref().is_some_and(|facts| !facts.trashed)
            && self.list_pass();
        // A catalog answer on its way while the settings are shown (S6).
        let catalog =
            !self.stale && self.form_open() && self.catalog.pass(crate::search::gateway_root());
        // A create tab's read on its clock, never while hidden (S8).
        let read = !self.stale && self.send_create_read(now);
        let library = !self.stale && self.library_pass();
        let memory = self.memory_pass();
        let routines = !self.stale && self.routines_pass();
        let setup = !self.stale && self.setup_pass();
        // The requests to other Bots (M6): an answer, or a read come due.
        let children = !self.stale && self.children_maintain(now);
        // The Bot's read owed again: asked on the pass this repaint runs.
        let reread = !self.stale && self.bot_read_due(now);
        // The conversation's `@`: whom it offers, kept fresh while open.
        let mentions = !self.stale && self.mentions_pass(now);
        let folded = folded
            || mentions
            || children
            || polled
            || listed
            || lifecycle
            || catalog
            || read
            || reread
            || library
            || memory
            || routines
            || setup;
        let chat = self
            .conversation_mut()
            .is_some_and(|conversation| View::maintain(conversation, ws, now));
        folded || chat
    }
}

#[cfg(test)]
mod tests;
