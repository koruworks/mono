// SPDX-License-Identifier: MPL-2.0

//! **The Bots roster**: the person's own Bots whose home is the window's
//! workspace, one row each, in the order the service creates them.
//!
//! ## What a row says
//!
//! The Bot's **name**, and **Paused** when its owner says it is paused.
//!
//! **Under it, its running and Needs-you conversations** (Bots P1.8 S10,
//! E3), from the owner's activity read (`ListActivity`): one sub-row each,
//! wearing the Chats list's own mark and words (`ChatStatus`), so what it
//! means is never said by color alone. Pressing one opens that conversation
//! in the Bot's tab. **A Bot with no sub-row is not claimed idle**: the read
//! cannot tell queued from executing, does not count a reply being settled
//! after an AI restart or a task started through the API without a
//! conversation, and stops at 200 conversations — when it stopped, the
//! roster says more work exists than it lists.
//!
//! ## What it says when it has no rows
//!
//! - **Asked, nothing back yet**: *Loading bots…* — never a zero claimed early.
//! - **The service did not answer**: it says so, **over what it showed
//!   before** — a failed read is never *you have no Bots*.
//! - **The service answered with none**: one short line, *No bots yet*
//!   (QUESTIONS O155). What a Bot is and the *New bot* button are the main
//!   area's while there is none (`bots::empty`), and the navigator heading
//!   keeps its `+`.
//!
//! ## Unfinished creations (S8)
//!
//! Beside every listing the link reads the workspace's **unfinished
//! creations** (`ListBotCreations`, the same request's second answer), so a
//! creation a restart interrupted is never invisible: a small *Unfinished*
//! group under the Bots, one row each — its name, where it stands in words,
//! **Retry** when Retry can resolve it, and **Abandon**, which the window
//! asks about first. Both work from the creation's request id alone. An
//! abandoned one's answer names what it left: its role page, with *Open role
//! page*, and its folder.
//!
//! ## How it is built
//!
//! An element tree (`docs/features/koruui/ELEMENTS-GUIDE.md`): one layout makes
//! one frame, and paint, the pointer, the keyboard's reveal, the accessibility
//! tree and the F12 inspector all read it. Rows and the list are named
//! elements with token padding and gaps; nothing is a bare number.
//!
//! ## When it works
//!
//! **Only while it is shown.** The host says when that changes
//! ([`set_shown`](BotsRoster::set_shown)); a hidden roster sends nothing,
//! folds nothing and lays nothing out. Being hidden marks it stale, so the
//! next showing asks once — nothing polls on a timer.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use koru_ai_core::bots::{
    Bot, BotActivity, BotActivityPage, BotActivityState, BotCommand, BotCreation, BotCreationStage,
    BotId, BotLifecycle, BotLifecycleKind, BotLifecycleStage, BotPage, BotRequestId, BotResponse,
};
use koru_chats_ui::ChatStatus;
use koru_draw::el::{
    Action, El, Frame, FrameCx, Interaction, Key, Look, Ring, Role, StateRecipe, TextCache, col,
    icon, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Radius, Space, Surface, alias, metrics, roles};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;
use vello::Scene;
use vello::kurbo::Affine;

use super::link::{Answer, BotsLink, Command, Failure, Reread};

/// The surface every key below that names no Bot belongs to.
pub(crate) const SURFACE: &str = "bots";
/// A Bot row's part. The row is the Bot's model key with this part.
pub(crate) const ROW_PART: &str = "bot";
/// The list viewport.
pub(crate) const LIST: Key = Key::surface(SURFACE, "list");
/// The line over the list while it loads or the service is not answering.
pub(crate) const NOTICE: Key = Key::surface(SURFACE, "notice");
/// The empty roster's sentence.
pub(crate) const EMPTY: Key = Key::surface(SURFACE, "empty");
/// The sentence saying the activity read stopped before the end.
pub(crate) const MORE_WORK: Key = Key::surface(SURFACE, "more-work");
/// An activity sub-row's part. The sub-row is the conversation's model key
/// with this part.
pub(crate) const ACTIVITY_PART: &str = "bot-activity";
/// **How long an activity read counts as what is running now** for the
/// question before an Archive or a Trash (`bots::stops`). Work a window
/// does not see start — a task started through the API, another device's
/// reply — moves nothing here, so an older read is not trusted to say
/// nothing runs, and the question then defaults to Cancel.
pub(crate) const ACTIVITY_FRESH: std::time::Duration = std::time::Duration::from_secs(30);
/// The bounded clock an unanswered listing is asked again on: 250 ms,
/// doubling to two seconds, the lifecycle UI's own.
const LIST_FIRST: std::time::Duration = std::time::Duration::from_millis(250);
const LIST_MAX: std::time::Duration = std::time::Duration::from_secs(2);
/// The Archived filter's *Show archived bots* / *Hide archived bots* (S14).
pub(crate) const ARCHIVED_TOGGLE: Key = Key::surface(SURFACE, "archived-toggle");
/// The Archived filter's heading.
pub(crate) const ARCHIVED_HEADING: Key = Key::surface(SURFACE, "archived-heading");
/// The Archived filter's sentence: loading, none, not answering, or that a
/// Bot whose Archive the owner does not name cannot be unarchived from here.
pub(crate) const ARCHIVED_NOTICE: Key = Key::surface(SURFACE, "archived-notice");
/// An archived row's part, under the Bot's own id.
pub(crate) const ARCHIVED_PART: &str = "bot-archived";
/// An archived row's *Unarchive*, under the Bot's own id.
pub(crate) const UNARCHIVE_PART: &str = "bot-unarchive";

/// The empty roster's *Create a bot* (S8).
pub(crate) const CREATE: Key = Key::surface(SURFACE, "create");
/// The Unfinished group's heading (S8).
pub(crate) const UNFINISHED_HEADING: Key = Key::surface(SURFACE, "unfinished-heading");
/// Why the last Retry or Abandon from here did not happen.
pub(crate) const UNFINISHED_NOTICE: Key = Key::surface(SURFACE, "unfinished-notice");
/// Under the Unfinished rows when the owner held more than were listed.
pub(crate) const UNFINISHED_MORE: Key = Key::surface(SURFACE, "unfinished-more");
/// An abandoned creation's sentences — what happened, what it left — and
/// its role page's *Open role page*.
pub(crate) const ABANDONED_LINES: [Key; 3] = [
    Key::surface(SURFACE, "abandoned"),
    Key::surface(SURFACE, "abandoned-more"),
    Key::surface(SURFACE, "abandoned-last"),
];
pub(crate) const OPEN_LEFT_PAGE: Key = Key::surface(SURFACE, "open-left-page");
/// An unfinished creation's row, its Retry and its Abandon, under the
/// creation's request id.
pub(crate) const CREATION_PART: &str = "bot-creation";
pub(crate) const CREATION_RETRY_PART: &str = "bot-creation-retry";
pub(crate) const CREATION_ABANDON_PART: &str = "bot-creation-abandon";

/// An unfinished creation's row key: its request's own id.
pub(crate) fn creation_key(request: BotRequestId) -> Key {
    Key::model(request.as_uuid().as_u128(), CREATION_PART)
}

pub(crate) fn creation_retry_key(request: BotRequestId) -> Key {
    Key::model(request.as_uuid().as_u128(), CREATION_RETRY_PART)
}

pub(crate) fn creation_abandon_key(request: BotRequestId) -> Key {
    Key::model(request.as_uuid().as_u128(), CREATION_ABANDON_PART)
}

/// An unfinished creation's state line, under its row.
pub(crate) const CREATION_STATE_PART: &str = "bot-creation-state";

pub(crate) fn creation_state_key(request: BotRequestId) -> Key {
    Key::model(request.as_uuid().as_u128(), CREATION_STATE_PART)
}

/// An archived row's key.
pub(crate) fn archived_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), ARCHIVED_PART)
}

/// An archived row's *Unarchive*.
pub(crate) fn unarchive_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), UNARCHIVE_PART)
}

/// An activity sub-row's key: its conversation's own id.
pub(crate) fn activity_key(chat: koru_ai_core::ChatId) -> Key {
    Key::model(chat.as_uuid().as_u128(), ACTIVITY_PART)
}

/// The mark a sub-row wears and the status it says, from the owner's
/// state: the Chats list's own vocabulary.
pub(crate) fn activity_status(state: BotActivityState) -> ChatStatus {
    match state {
        BotActivityState::Running => ChatStatus::Working,
        BotActivityState::NeedsYou => ChatStatus::NeedsYou,
    }
}

/// A row's key: the Bot's own id, never its place in the list.
pub(crate) fn row_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), ROW_PART)
}

/// A row's *Runs on this computer* mark (QUESTIONS O155), under the Bot's
/// own id.
pub(crate) const RUNS_HERE_PART: &str = "bot-runs-here";

pub(crate) fn runs_here_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), RUNS_HERE_PART)
}

/// A Bot row's treatments, the library's: the open Bot on the card, the
/// pointer's row tinted, the keyboard's ringed.
static BOT_ROW_STATES: StateRecipe = StateRecipe {
    name: "bot row",
    base: Look::NONE,
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::bg(Surface::Selected),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// Where the roster stands.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) enum Load {
    /// Never asked, or about another workspace than the one it now shows.
    #[default]
    Cold,
    /// Asked, nothing back yet.
    Loading,
    Ready,
    /// The service did not answer. **Not an empty roster**: what was on
    /// screen stays.
    Unavailable,
    /// **Koru AI answered, refusing this window access to Bots** past the
    /// while a starting Koru takes to vouch for it ([`super::link::Vouching`]).
    /// What was on screen stays; the sentence says why, not that Koru AI is
    /// silent.
    Denied,
}

/// One row: what the service said about one Bot that the roster draws, and
/// what opening it needs — its main conversation, its home and its agent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RosterBot {
    pub id: BotId,
    pub name: String,
    pub paused: bool,
    /// The conversation its tab opens on, and the one id a Bot tab is
    /// found by wherever else that conversation is opened from.
    pub main_chat: koru_ai_core::ChatId,
    pub home: WorkspaceId,
    pub provider: koru_ai_core::AiProviderId,
    /// **A row of the Archived filter** (`list_lifecycle`): the Bot is
    /// archived, and the tab it opens says so from the start — never as
    /// active until its own read (the fourth window run's finding A).
    /// `false` on the roster's own rows, which list active Bots only.
    pub archived: bool,
    /// The finished Archive or Trash the owner names as holding the Bot
    /// (`held_by`): what the tab's Unarchive names before its own read.
    pub held_by: Option<koru_ai_core::bots::BotLifecycleHolder>,
    /// **The face the Bot wears** (QUESTIONS O155): the one the person
    /// picked, else the one derived from its id, as the Bot itself says
    /// (`Bot::avatar`). An index into `koru_draw::face::ROWS`.
    pub avatar: u8,
}

impl From<Bot> for RosterBot {
    fn from(bot: Bot) -> Self {
        Self {
            avatar: bot.avatar(),
            id: bot.id,
            name: bot.config.name,
            paused: bot.paused,
            main_chat: bot.main_chat,
            home: bot.home_workspace,
            provider: bot.config.model.provider,
            archived: bot.lifecycle == BotLifecycle::Archived,
            held_by: bot.held_by,
        }
    }
}

/// What the roster has done, so a test can assert a count rather than a
/// clock. Cumulative.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Work {
    /// Requests handed to the link.
    pub commands: usize,
    /// Element layouts made.
    pub layouts: usize,
}

/// A key the roster answers while it holds the keyboard. Its own vocabulary,
/// the Chats view's keys less the ones that would act on something the
/// roster does not offer yet: no Delete, no menu.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Press {
    Up,
    Down,
    Home,
    End,
    /// Enter or Space: ask for the Bot under the keyboard.
    Open,
    Escape,
}

impl Press {
    /// The keys the roster claims, and only those: everything else falls
    /// through, so a focused list never makes the window feel stuck.
    pub(crate) fn of(key: &winit::keyboard::Key) -> Option<Self> {
        use winit::keyboard::{Key, NamedKey};
        match key {
            Key::Named(NamedKey::ArrowUp) => Some(Self::Up),
            Key::Named(NamedKey::ArrowDown) => Some(Self::Down),
            Key::Named(NamedKey::Home) => Some(Self::Home),
            Key::Named(NamedKey::End) => Some(Self::End),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => Some(Self::Open),
            Key::Character(text) if text == " " => Some(Self::Open),
            Key::Named(NamedKey::Escape) => Some(Self::Escape),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct BotsRoster {
    lang: Lang,
    load: Load,
    /// In the service's order: by creation, so a row never moves under the
    /// person between two listings.
    bots: Vec<RosterBot>,
    workspace: Option<WorkspaceId>,
    shown: bool,
    /// The service has been asked since the roster was last shown, or its
    /// workspace last changed. `false` — the default — means what is on
    /// screen may be older than the service: the next pass that shows it asks.
    fresh: bool,
    seq: u64,
    in_flight: bool,
    /// The Bot last asked for: it wears the card. By id, so a refresh that
    /// reorders or adds rows keeps it.
    selected: Option<BotId>,
    /// The control the keyboard is on, by its element key — never a place
    /// in the list. `Some` doubles as *the roster holds the keyboard*.
    focus: Option<Key>,
    hover: Option<BotId>,
    /// The activity sub-row under the pointer, by its conversation.
    hover_activity: Option<koru_ai_core::ChatId>,
    /// The list's offset, as the last layout clamped it.
    scroll: f32,
    /// The keyboard moved; the next draw brings its row into view.
    reveal_owed: bool,
    wants_open: Option<BotId>,
    /// **The owner's running and Needs-you conversations** for the roster's
    /// workspace (S10), by Bot then conversation; and whether the read
    /// stopped before the end.
    activity: Vec<BotActivity>,
    activity_truncated: bool,
    /// An activity read is owed: shown again, the roster listed, or the
    /// window said a Bot conversation's work moved. Coalesced: one read on
    /// its way at a time, and one more after it if more was owed.
    activity_owed: bool,
    activity_seq: u64,
    activity_in_flight: bool,
    /// **When the activity shown was answered**, while it is still what the
    /// window knows of running work: cleared by a failed read, by the
    /// roster being hidden (nothing is read while hidden) and by a change
    /// of workspace.
    activity_read_at: Option<std::time::Instant>,
    /// A sub-row asked for: the Bot and the conversation, for the window.
    wants_open_chat: Option<(BotId, koru_ai_core::ChatId)>,
    work: Cell<Work>,
    cache: RefCell<TextCache>,
    started: bool,
    link: Option<BotsLink>,
    // ------------------------------------- the Archived filter (S14)
    /// The filter is open: the workspace's archived Bots are listed under
    /// the active ones, from the owner's `ListLifecycle`.
    archived_open: bool,
    archived: Vec<Bot>,
    archived_load: Load,
    archived_seq: u64,
    archived_in_flight: bool,
    /// The archived list was read since it was opened or the roster was
    /// last shown; `false` means the next shown pass asks.
    archived_fresh: bool,
    /// Unarchives on their way, by their request.
    unarchiving: std::collections::HashMap<BotRequestId, (BotId, String)>,
    finished: Vec<super::lifecycle::Finished>,
    /// Why the last Unarchive did not happen.
    unarchive_refused: Option<Words>,
    hover_archived: Option<Key>,
    // ---------------------------------------- unfinished creations (S8)
    /// The workspace's unfinished creations, oldest first, as the link last
    /// read them beside the list.
    creations: Vec<BotCreation>,
    /// The listing's second answer is still owed.
    creations_in_flight: bool,
    /// The owner held more unfinished creations than one listing reads:
    /// the group says so under its rows.
    creations_more: bool,
    /// A Retry or an Abandon on its way, by the creation's request.
    creations_acting: std::collections::HashSet<BotRequestId>,
    /// **When a moving creation is listed again**: the create form's
    /// bounded clock (250 ms doubling to two seconds), only while shown and
    /// only while one of them moves, so a creation retried from here leaves
    /// the group when it finishes and its Bot is listed.
    creations_next: Option<std::time::Instant>,
    creations_backoff: std::time::Duration,
    /// **When a listing Koru AI did not answer is asked again**: the same
    /// bounded clock, only while shown. A roster shown right after a restart
    /// asks before the service is up; without this it said *Koru AI isn't
    /// answering…* until it was hidden and shown again (the first window
    /// run's finding 4).
    list_next: Option<std::time::Instant>,
    list_backoff: std::time::Duration,
    /// **Since when the listing is refused as unvouched**: asked again on
    /// the same clock for a bounded while, said as loading meanwhile (the
    /// second window run: after every restart the first listing was refused
    /// `forbidden` 3 ms after the first frame, and the roster said *Koru AI
    /// isn't answering…* until it was hidden and shown again).
    vouching: super::link::Vouching,
    /// Why the last Retry or Abandon from here did not happen.
    creation_refused: Option<Words>,
    /// The creation abandoned from here last: what it left, said once.
    abandoned: Option<BotCreation>,
    /// The listing's sequence when [`Self::abandoned`] was said: the next
    /// listing asked after it takes the note away (the third window run: it
    /// stayed under an empty *Unfinished* heading after a Bot of that name
    /// was made).
    abandoned_at: u64,
    /// *Create a bot* was pressed: the window opens the form. Taken once.
    wants_create: bool,
    /// An Abandon pressed: the request and the name, for the window to ask
    /// about first. Taken once.
    abandon_ask: Option<(BotRequestId, String)>,
    /// A page an abandoned creation left, to open. Taken once.
    open_page: Option<BotPage>,
    /// Creations a Retry or an Abandon from here answered for, by request:
    /// a create tab holding one reads it again. Taken once.
    creations_moved: Vec<BotRequestId>,
    /// Conversation titles as open Bot tabs show them (`know_titles`).
    titles: std::collections::HashMap<koru_ai_core::ChatId, String>,
    // ------------------------------------------------ faces (O155)
    /// **A copy of the window's one face clock** and the Bots it says are
    /// working, handed in by the window (`set_faces`): what a row's face is
    /// posed from. The roster owns no clock and asks for no frame.
    face_clock: koru_draw::face::FaceClock,
    face_working: Vec<BotId>,
}

impl BotsRoster {
    /// Test only: the window's paths never need it.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Give the roster its link, once, the first time it is shown.
    /// Nothing under test: [`BotsLink::start`] answers `None` there.
    pub(crate) fn start(&mut self, root: Option<PathBuf>) {
        if self.started {
            return;
        }
        self.started = true;
        self.link = BotsLink::start(root);
    }

    /// A roster that already listed `bots`, as a window test needs it.
    #[cfg(test)]
    pub(crate) fn listed_for_test(&mut self, bots: Vec<RosterBot>) {
        self.bots = bots;
        self.load = Load::Ready;
    }

    /// A roster over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(link: BotsLink) -> Self {
        Self {
            started: true,
            link: Some(link),
            ..Self::new()
        }
    }

    pub(crate) fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
    }

    pub(crate) fn load(&self) -> Load {
        self.load
    }

    pub(crate) fn bots(&self) -> &[RosterBot] {
        &self.bots
    }

    /// What the roster has done, for a test to count.
    #[cfg(test)]
    pub(crate) fn work(&self) -> Work {
        self.work.get()
    }

    /// **The window's face clock, and the Bots that are working now**
    /// (QUESTIONS O155): a working Bot's face moves, every other is still.
    /// The window decides both from what it already knows
    /// (`MainView::sync_bot_faces`).
    pub(crate) fn set_faces(&mut self, clock: &koru_draw::face::FaceClock, working: &[BotId]) {
        self.face_clock = clock.clone();
        if self.face_working != working {
            self.face_working = working.to_vec();
        }
    }

    /// The Bots the activity read says have a reply running now: one of the
    /// two things the window's face clock is told (the other is a Bot tab's
    /// own conversation).
    pub(crate) fn running_bots(&self) -> impl Iterator<Item = BotId> + '_ {
        self.activity
            .iter()
            .filter(|entry| entry.state == BotActivityState::Running)
            .map(|entry| entry.bot_id)
    }

    /// The pose `bot`'s face is drawn in now.
    fn face_pose(&self, bot: &RosterBot) -> koru_draw::face::FacePose {
        self.face_clock.pose(
            bot.avatar,
            self.face_working.contains(&bot.id),
            std::time::Instant::now(),
        )
    }

    pub(crate) fn selected(&self) -> Option<BotId> {
        self.selected
    }

    /// The control the keyboard is on, by its element key: a Bot row, an
    /// Unfinished row's Retry or Abandon, the Archived toggle, an archived
    /// row or its Unarchive.
    pub(crate) fn focus(&self) -> Option<Key> {
        self.focus
    }

    /// **Which workspace the roster is about**: the window's. Another
    /// workspace's Bots are not this one's, so a change clears the rows and
    /// any answer still on its way for the old one; it asks again only once
    /// it is shown. `true` when it changed.
    pub(crate) fn set_workspace(&mut self, workspace: Option<WorkspaceId>) -> bool {
        if self.workspace == workspace {
            return false;
        }
        self.workspace = workspace;
        self.bots.clear();
        self.load = Load::Cold;
        self.selected = None;
        self.focus = None;
        self.hover = None;
        self.scroll = 0.0;
        self.wants_open = None;
        self.in_flight = false;
        // An answer for the old workspace is never this one's.
        self.seq = self.seq.wrapping_add(1);
        self.fresh = false;
        self.activity.clear();
        self.activity_truncated = false;
        self.activity_in_flight = false;
        self.activity_read_at = None;
        self.activity_seq = self.activity_seq.wrapping_add(1);
        self.wants_open_chat = None;
        self.archived.clear();
        self.archived_load = Load::Cold;
        self.archived_fresh = false;
        self.archived_in_flight = false;
        self.archived_seq = self.archived_seq.wrapping_add(1);
        self.creations.clear();
        self.creations_in_flight = false;
        self.creation_refused = None;
        self.abandoned = None;
        self.creations_more = false;
        // An answer still on its way is the old workspace's, never this one's.
        self.unarchiving.clear();
        self.creations_acting.clear();
        self.creations_next = None;
        self.list_next = None;
        self.list_backoff = LIST_FIRST;
        self.vouching.reset();
        true
    }

    /// The unanswered listing's clock, come due now: a test standing in
    /// for the time passing. `false` when no clock runs.
    #[cfg(test)]
    pub(crate) fn list_retry_due_for_test(&mut self) -> bool {
        match &mut self.list_next {
            Some(at) => {
                *at = std::time::Instant::now();
                true
            }
            None => false,
        }
    }

    /// **Whether the roster is what the navigator shows.** Leaving marks it
    /// stale — nothing tells a hidden roster what changed — so coming back
    /// asks once. `true` when it changed.
    pub(crate) fn set_shown(&mut self, shown: bool) -> bool {
        if self.shown == shown {
            return false;
        }
        self.shown = shown;
        if !shown {
            self.fresh = false;
            self.archived_fresh = false;
            // Nothing tells a hidden roster that work started or stopped.
            self.activity_read_at = None;
            self.focus = None;
            self.hover = None;
            // Nothing is listed while hidden: shown again, it lists once.
            self.creations_next = None;
            self.list_next = None;
            // Shown again, an unvouched window has the whole while again.
            self.vouching.reset();
        }
        true
    }

    /// The pass the window runs on its wake path. A hidden roster returns
    /// at once: no request, no answer folded. `true` when a repaint is owed.
    pub(crate) fn maintain(&mut self) -> bool {
        if !self.shown {
            return false;
        }
        // A moving creation's clock came due: list again.
        if self
            .creations_next
            .is_some_and(|at| at <= std::time::Instant::now())
        {
            self.creations_next = None;
            self.fresh = false;
        }
        // An unanswered listing's clock came due: ask again.
        if self
            .list_next
            .is_some_and(|at| at <= std::time::Instant::now())
        {
            self.list_next = None;
            self.fresh = false;
        }
        let asked = !self.fresh && self.refresh();
        let archived = self.archived_open && !self.archived_fresh && self.ask_archived();
        let answered = self.poll();
        let activity = self.ask_activity();
        asked || answered || activity || archived
    }

    // ------------------------------------------- the Archived filter (S14)

    /// Whether the Archived filter is open.
    pub(crate) fn archived_open(&self) -> bool {
        self.archived_open
    }

    /// The archived Bots, as the owner last listed them.
    pub(crate) fn archived(&self) -> &[Bot] {
        &self.archived
    }

    pub(crate) fn archived_load(&self) -> Load {
        self.archived_load
    }

    /// **Open or close the Archived filter.** Opening asks the owner once;
    /// closing keeps nothing on its way.
    pub(crate) fn toggle_archived(&mut self) -> bool {
        self.archived_open = !self.archived_open;
        if self.archived_open {
            self.archived_fresh = false;
        }
        self.unarchive_refused = None;
        true
    }

    /// **The Archive that holds `bot`**, as the Archived filter's entry
    /// names it (`held_by`, `list_lifecycle`): what its Unarchive names,
    /// after a restart too.
    fn archive_source(&self, bot: BotId) -> Option<BotRequestId> {
        self.archived
            .iter()
            .find(|row| row.id == bot)?
            .held_by
            .as_ref()
            .filter(|holder| holder.kind == BotLifecycleKind::Archive)
            .map(|holder| holder.request_id)
    }

    /// Whether `bot`'s row offers *Unarchive*: the owner names its Archive.
    ///
    /// Not while another operation of the Bot is open (`open_operation`):
    /// the owner would refuse it, and that operation is the tab's to follow.
    pub(crate) fn can_unarchive(&self, bot: BotId) -> bool {
        self.archive_source(bot).is_some()
            && self
                .archived
                .iter()
                .find(|row| row.id == bot)
                .is_some_and(|row| row.open_operation.is_none())
    }

    /// **A Bot tab's own answer says what one of the listed Bots is now** —
    /// renamed, paused or resumed there: its row says so at once, shown or
    /// not, rather than when the roster next lists. A Bot the roster does
    /// not list is left to the list. `true` when a row changed.
    pub(crate) fn know_bot(&mut self, bot: RosterBot) -> bool {
        match self.bots.iter_mut().find(|row| row.id == bot.id) {
            Some(row) if *row != bot => {
                *row = bot;
                true
            }
            _ => false,
        }
    }

    /// **A create tab's own answer says where a creation stands** — it
    /// stopped unfinished, it is still being made, or it ended: the
    /// Unfinished group says so at once, shown or not, rather than when the
    /// roster next lists (the second window run: the group appeared only
    /// after the roster was shown again). Another workspace's creation is
    /// not this roster's. `true` when the group changed.
    pub(crate) fn know_creation(&mut self, creation: BotCreation) -> bool {
        if Some(creation.home_workspace) != self.workspace {
            return false;
        }
        let at = self
            .creations
            .iter()
            .position(|row| row.request_id == creation.request_id);
        let unfinished = matches!(
            creation.stage,
            BotCreationStage::Pending | BotCreationStage::Incomplete
        );
        let changed = match (at, unfinished) {
            (Some(at), true) if self.creations[at] != creation => {
                self.creations[at] = creation;
                true
            }
            (None, true) => {
                self.creations.push(creation);
                true
            }
            (Some(at), false) => {
                self.creations.remove(at);
                true
            }
            _ => false,
        };
        if changed {
            self.keep_focus();
        }
        changed
    }

    /// **The creations a Retry or an Abandon from here moved**, by their
    /// request, for the window to tell a create tab holding one: it reads
    /// its creation again rather than go on offering what was done here.
    /// Taken once.
    pub(crate) fn take_creations_moved(&mut self) -> Vec<BotRequestId> {
        std::mem::take(&mut self.creations_moved)
    }

    /// **Another surface of the window changed a Bot's lifecycle**: both
    /// lists are read again on the next shown pass.
    pub(crate) fn owe_lifecycle_refresh(&mut self) {
        self.fresh = false;
        self.archived_fresh = false;
    }

    fn ask_archived(&mut self) -> bool {
        let (Some(workspace), Some(link)) = (self.workspace, &self.link) else {
            return false;
        };
        let seq = self.archived_seq.wrapping_add(1);
        if !link.send(Command::Lifecycle {
            seq,
            workspace,
            lifecycle: BotLifecycle::Archived,
        }) {
            return false;
        }
        self.archived_seq = seq;
        self.archived_fresh = true;
        self.archived_in_flight = true;
        if self.archived_load == Load::Cold {
            self.archived_load = Load::Loading;
        }
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        true
    }

    /// **Unarchive `bot`**: the Restore naming its Archive, at the revision
    /// the Archived filter listed. Nothing is shown as unarchived until the
    /// owner says so. `false` when nothing was sent.
    pub(crate) fn unarchive(&mut self, bot: BotId) -> bool {
        if !self.can_unarchive(bot) {
            return false;
        }
        let (Some(listed), Some(source), Some(link)) = (
            self.archived.iter().find(|row| row.id == bot),
            self.archive_source(bot),
            &self.link,
        ) else {
            return false;
        };
        if self.unarchiving.values().any(|(id, _)| *id == bot) {
            return false;
        }
        let request = BotRequestId::mint();
        let command = BotCommand::Restore {
            request_id: request,
            bot_id: bot,
            expected_revision: listed.revision,
            source_request_id: source,
        };
        let name = listed.config.name.clone();
        if !link.send(Command::Write {
            bot,
            request,
            command: Box::new(command),
        }) {
            self.unarchive_refused = Some(Words::BotsErrUnreachable);
            return true;
        }
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        self.unarchive_refused = None;
        self.unarchiving.insert(request, (bot, name));
        true
    }

    /// Whether `bot`'s Unarchive is on its way.
    pub(crate) fn unarchiving(&self, bot: BotId) -> bool {
        self.unarchiving.values().any(|(id, _)| *id == bot)
    }

    /// The Unarchives that finished, for the window to tell the Bot's tabs
    /// and the Trash. Taken once.
    pub(crate) fn take_finished(&mut self) -> Vec<super::lifecycle::Finished> {
        std::mem::take(&mut self.finished)
    }

    /// Fold an Unarchive's answer. `true` when it was one of this roster's.
    fn take_unarchived(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
    ) -> bool {
        let Some((bot, name)) = self.unarchiving.remove(&request) else {
            return false;
        };
        match result {
            Ok(BotResponse::Lifecycle { operation })
                if operation.request_id == request
                    && operation.stage == BotLifecycleStage::Completed =>
            {
                // **Out of the Archived filter the moment it answered**, not
                // when the re-list does, so a late second press sends
                // nothing.
                self.archived.retain(|row| row.id != bot);
                self.finished.push(super::lifecycle::Finished {
                    bot,
                    name,
                    home: self.workspace,
                    kind: BotLifecycleKind::Restore,
                    request,
                    reverses: Some(BotLifecycleKind::Archive),
                    chat: None,
                    at_ms: 0,
                });
            }
            // The outcome is unknown: never said as not unarchived. The
            // lists read again show which.
            Ok(_) | Err(Failure::Unanswered) => {
                self.unarchive_refused = Some(Words::BotsErrUnreachable);
            }
            // Unarchived already, or the Bot moved since: the re-list says
            // what it is now (`restore_refusal_words`).
            Err(Failure::Refused(code)) => {
                self.unarchive_refused = Some(super::lifecycle::restore_refusal_words(&code));
            }
        }
        // Either way the lists are the owner's to say again.
        self.fresh = false;
        self.archived_fresh = false;
        true
    }

    /// The archived Bot a row asked for, as the window opens it: its tab
    /// reads it and shows it as archived.
    fn archived_row(&self, bot: BotId) -> Option<RosterBot> {
        self.archived
            .iter()
            .find(|row| row.id == bot)
            .cloned()
            .map(RosterBot::from)
    }

    /// **An activity read is owed** — the window saw a Bot conversation's
    /// work start, stop or ask (S10). Sent on the next pass while the
    /// roster is shown, never from a hidden one and never on a timer.
    pub(crate) fn owe_activity(&mut self) {
        self.activity_owed = true;
    }

    /// Ask for the workspace's activity when it is owed and none is on its
    /// way. `true` when a request went.
    fn ask_activity(&mut self) -> bool {
        if !self.activity_owed || self.activity_in_flight {
            return false;
        }
        let (Some(workspace), Some(link)) = (self.workspace, &self.link) else {
            return false;
        };
        let seq = self.activity_seq.wrapping_add(1);
        if !link.send(Command::Activity { seq, workspace }) {
            return false;
        }
        self.activity_seq = seq;
        self.activity_owed = false;
        self.activity_in_flight = true;
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        true
    }

    /// The owner's running and Needs-you conversations, as last read.
    pub(crate) fn activity(&self) -> &[BotActivity] {
        &self.activity
    }

    /// Whether the activity read stopped before the end.
    pub(crate) fn activity_truncated(&self) -> bool {
        self.activity_truncated
    }

    /// **The activity read, while it is what is running now** — for the
    /// question before an Archive or a Trash (`bots::stops`), which sends
    /// nothing to find out. Its entries and whether it stopped before the
    /// end, only when it answered for this workspace, nothing was owed or on
    /// its way since (a Bot tab's work moving owes a read), the roster has
    /// been shown throughout, and it is no older than [`ACTIVITY_FRESH`].
    /// `None` otherwise: the window does not know.
    pub(crate) fn activity_now(&self, now: std::time::Instant) -> Option<(&[BotActivity], bool)> {
        let at = self.activity_read_at?;
        let fresh = now.saturating_duration_since(at) <= ACTIVITY_FRESH
            && !self.activity_owed
            && !self.activity_in_flight;
        fresh.then_some((self.activity.as_slice(), self.activity_truncated))
    }

    /// One Bot's running and Needs-you conversations, as the rows' marks.
    pub(crate) fn activity_of(
        &self,
        bot: BotId,
    ) -> std::collections::HashMap<koru_ai_core::ChatId, ChatStatus> {
        self.activity
            .iter()
            .filter(|entry| entry.bot_id == bot)
            .map(|entry| (entry.chat_id, activity_status(entry.state)))
            .collect()
    }

    /// A sub-row a press, a key or an accessibility action asked for, taken
    /// once, with what the Bot's tab needs.
    pub(crate) fn take_wants_open_chat(&mut self) -> Option<(RosterBot, koru_ai_core::ChatId)> {
        let (bot, chat) = self.wants_open_chat.take()?;
        let row = self.bots.iter().find(|row| row.id == bot)?.clone();
        Some((row, chat))
    }

    /// Ask for one of a Bot's running or Needs-you conversations.
    pub(crate) fn ask_open_chat(&mut self, chat: koru_ai_core::ChatId) -> bool {
        let Some(entry) = self.activity.iter().find(|entry| entry.chat_id == chat) else {
            return false;
        };
        if !self.has(entry.bot_id) {
            return false;
        }
        self.selected = Some(entry.bot_id);
        self.wants_open_chat = Some((entry.bot_id, chat));
        true
    }

    /// A sub-row's accessible name: which conversation, then its state's
    /// words — the Chats list's.
    pub(crate) fn activity_name(&self, bot: &RosterBot, entry: &BotActivity) -> String {
        let which = self.conversation_words(bot, entry.chat_id);
        match activity_status(entry.state).spoken(self.lang) {
            Some(state) => format!("{which}, {state}"),
            None => which.to_owned(),
        }
    }

    /// **Which conversation an activity row is**, in the words its Bot
    /// tab's list titles it with (`conversations::shown_title`), when an
    /// open tab has listed it; else the main one by that name and another
    /// by what it is — the activity read names no title.
    fn conversation_words(&self, bot: &RosterBot, chat: koru_ai_core::ChatId) -> String {
        if let Some(title) = self.titles.get(&chat) {
            return title.clone();
        }
        t(
            self.lang,
            if chat == bot.main_chat {
                Words::BotsMainConversation
            } else {
                Words::BotsOtherConversation
            },
        )
        .to_owned()
    }

    /// The title an open Bot tab shows for `chat`, when one has listed it.
    pub(crate) fn known_title(&self, chat: koru_ai_core::ChatId) -> Option<&str> {
        self.titles.get(&chat).map(String::as_str)
    }

    /// **The titles open Bot tabs show for their conversations**, so an
    /// activity row says the same words as the conversation's row. `true`
    /// when one moved.
    pub(crate) fn know_titles(
        &mut self,
        titles: impl IntoIterator<Item = (koru_ai_core::ChatId, String)>,
    ) -> bool {
        let mut moved = false;
        for (chat, title) in titles {
            if self.titles.get(&chat) != Some(&title) {
                self.titles.insert(chat, title);
                moved = true;
            }
        }
        moved
    }

    /// Ask the service again, when there is a workspace to ask about and a
    /// link to ask through. A full queue leaves it unasked, to ask on a later
    /// pass. `true` when a request went.
    fn refresh(&mut self) -> bool {
        let (Some(workspace), Some(link)) = (self.workspace, &self.link) else {
            return false;
        };
        let seq = self.seq.wrapping_add(1);
        if !link.send(Command::List { seq, workspace }) {
            return false;
        }
        self.seq = seq;
        self.fresh = true;
        self.in_flight = true;
        self.creations_in_flight = true;
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        // A refresh over rows already shown does not blank them: they stay
        // until the answer replaces them, and an unanswered service keeps
        // saying so until one does.
        if self.load == Load::Cold {
            self.load = Load::Loading;
        }
        true
    }

    /// Fold in what the link answered. **The only place an answer is
    /// accepted**, and only the latest request's for this workspace: an
    /// older reply can never replace newer state. **In the order the link
    /// answered** — it is one thread, first in first out — so a listing
    /// sent before an Unarchive is never folded after the Unarchive's answer.
    fn poll(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let answers = link.drain();
        let mut changed = false;
        for answer in answers {
            // The roster asks for lists, for activity, for its Archived
            // filter and its Unarchives; any other answer is not its.
            match answer {
                Answer::Listed {
                    seq,
                    workspace,
                    result,
                } => {
                    if seq != self.seq || Some(workspace) != self.workspace {
                        continue;
                    }
                    self.in_flight = false;
                    self.take(result);
                    changed = true;
                }
                Answer::Activity {
                    seq,
                    workspace,
                    result,
                } => changed |= self.take_activity(seq, workspace, result),
                Answer::Lifecycle {
                    seq,
                    workspace,
                    lifecycle: BotLifecycle::Archived,
                    result,
                } => {
                    if seq != self.archived_seq || Some(workspace) != self.workspace {
                        continue;
                    }
                    self.archived_in_flight = false;
                    match result {
                        Ok(bots) => {
                            self.archived = bots;
                            self.archived_load = Load::Ready;
                        }
                        // What was listed stays; the sentence says it may be
                        // old.
                        Err(_) => self.archived_load = Load::Unavailable,
                    }
                    changed = true;
                }
                Answer::Wrote {
                    request, result, ..
                } => {
                    changed |= if self.unarchiving.contains_key(&request) {
                        self.take_unarchived(request, result)
                    } else {
                        self.take_creation(request, result)
                    };
                }
                Answer::Creations {
                    seq,
                    workspace,
                    result,
                } => {
                    if seq != self.seq || Some(workspace) != self.workspace {
                        continue;
                    }
                    self.creations_in_flight = false;
                    // **What was abandoned here was said once**: a listing
                    // asked after it lists the workspace without it, and the
                    // note goes with it.
                    if seq != self.abandoned_at {
                        self.abandoned = None;
                    }
                    // A failed read keeps what was listed.
                    if let Ok(listed) = result {
                        self.creations_more = listed.more;
                        self.creations = listed
                            .items
                            .into_iter()
                            .filter(|creation| {
                                matches!(
                                    creation.stage,
                                    BotCreationStage::Pending | BotCreationStage::Incomplete
                                )
                            })
                            .collect();
                    }
                    self.schedule_creations();
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.keep_focus();
        }
        changed
    }

    /// **The keyboard stays in the roster** when what it was on leaves — a
    /// Bot no longer listed, a Retry that is acting, an Unarchive done: on
    /// the first control there is, or nowhere when there is none.
    fn keep_focus(&mut self) {
        if let Some(focus) = self.focus {
            let stops = self.keyboard_stops();
            if !stops.contains(&focus) {
                self.focus = stops.first().copied();
            }
        }
    }

    fn take(&mut self, result: Result<Vec<Bot>, Failure>) {
        match result {
            Ok(bots) => {
                self.bots = bots.into_iter().map(RosterBot::from).collect();
                self.load = Load::Ready;
                // A listing is the roster's own command: its activity is read
                // after it, once.
                self.activity_owed = true;
                // Kept by id; a Bot the answer no longer lists lets go.
                let selected = self.selected.filter(|id| self.has(*id));
                let hover = self.hover.filter(|id| self.has(*id));
                self.selected = selected;
                self.hover = hover;
                self.list_next = None;
                self.list_backoff = LIST_FIRST;
                self.vouching.reset();
            }
            // What was shown stays; the line over it says why it may be old.
            // Asked again on the bounded clock while shown when asking
            // again may help — never for a refusal that would only repeat.
            Err(failure) => {
                let now = std::time::Instant::now();
                let again = match self.vouching.read_failed(&failure, now) {
                    Reread::Again => {
                        self.load = Load::Unavailable;
                        true
                    }
                    // Koru AI answered: the window is not vouched for yet.
                    // Still loading, not *isn't answering*; rows shown stay.
                    Reread::Unvouched => {
                        if self.load != Load::Ready {
                            self.load = Load::Loading;
                        }
                        true
                    }
                    Reread::Denied => {
                        self.load = Load::Denied;
                        false
                    }
                    Reread::Final => {
                        self.load = Load::Unavailable;
                        false
                    }
                };
                if again {
                    let wait = self.list_backoff.max(LIST_FIRST);
                    self.list_next = Some(now.checked_add(wait).unwrap_or(now));
                    self.list_backoff = (wait * 2).min(LIST_MAX);
                }
            }
        }
    }

    /// Fold an activity answer: only the latest read's, for this workspace.
    /// A failed read keeps what was shown. `true` when it was this roster's.
    pub(crate) fn take_activity(
        &mut self,
        seq: u64,
        workspace: WorkspaceId,
        result: Result<BotActivityPage, Failure>,
    ) -> bool {
        if seq != self.activity_seq || Some(workspace) != self.workspace {
            return false;
        }
        self.activity_in_flight = false;
        match result {
            Ok(page) => {
                self.activity = page.entries;
                self.activity_truncated = page.truncated;
                self.activity_read_at = Some(std::time::Instant::now());
            }
            // What was shown stays on the rows, but it is not what the
            // window knows is running now.
            Err(_) => self.activity_read_at = None,
        }
        true
    }

    /// The activity as a read answered `at`, the roster shown: a test
    /// standing in for `ListActivity`'s answer at a time of its choosing.
    #[cfg(test)]
    pub(crate) fn activity_read_for_test(
        &mut self,
        entries: Vec<BotActivity>,
        truncated: bool,
        at: std::time::Instant,
    ) {
        self.shown = true;
        self.activity = entries;
        self.activity_truncated = truncated;
        self.activity_owed = false;
        self.activity_in_flight = false;
        self.activity_read_at = Some(at);
    }

    fn has(&self, id: BotId) -> bool {
        self.bots.iter().any(|bot| bot.id == id)
    }

    /// Whether an answer is owed — the one reason the window keeps a wake
    /// clock for the roster.
    pub(crate) fn awaiting(&self) -> bool {
        self.shown
            && (self.in_flight
                || self.activity_in_flight
                || self.archived_in_flight
                || self.creations_in_flight
                || self.creations_next.is_some()
                || self.list_next.is_some()
                || !self.unarchiving.is_empty()
                || !self.creations_acting.is_empty())
    }

    /// The Bot a row, a key or an accessibility action asked for, taken
    /// once, with what its tab needs: the window opens or focuses it. An
    /// archived row opens too: its conversations stay readable.
    pub(crate) fn take_wants_open(&mut self) -> Option<RosterBot> {
        let bot = self.wants_open.take()?;
        self.bots
            .iter()
            .find(|row| row.id == bot)
            .cloned()
            .or_else(|| self.archived_row(bot))
    }

    /// Ask for an archived Bot's tab — its row's press.
    pub(crate) fn ask_open_archived(&mut self, bot: BotId) -> bool {
        if !self.archived.iter().any(|row| row.id == bot) {
            return false;
        }
        self.selected = Some(bot);
        self.wants_open = Some(bot);
        true
    }

    /// **The Bot whose main conversation is `chat`**, as far as this roster
    /// knows — the one association the window has before the engine labels
    /// a chat with its Bot. `None` is *not known*, never *not a Bot's*.
    pub(crate) fn bot_for_chat(&self, chat: koru_ai_core::ChatId) -> Option<&RosterBot> {
        self.bots.iter().find(|bot| bot.main_chat == chat)
    }

    /// Ask for `bot`: it takes the card and is recorded for the host.
    pub(crate) fn ask_open(&mut self, bot: BotId) -> bool {
        if !self.has(bot) {
            return false;
        }
        self.selected = Some(bot);
        self.wants_open = Some(bot);
        true
    }

    // ---------------------------------------------------------- keyboard

    pub(crate) fn has_keyboard(&self) -> bool {
        self.focus.is_some()
    }

    /// **What the arrow keys walk, in the order the roster draws it**: the
    /// Bot rows, each followed by its running and Needs-you sub-rows; the
    /// Unfinished group's
    /// *Open role page* and each row's Retry and Abandon; the Archived
    /// toggle; and while it is open each archived row and its Unarchive.
    /// Exactly the controls drawn now, so the keyboard never lands on one
    /// that is not there.
    pub(crate) fn keyboard_stops(&self) -> Vec<Key> {
        // Each Bot row, then its running and Needs-you sub-rows under it,
        // as `bot_row` draws them: the pointer and an assistive technology
        // press those too.
        let mut stops: Vec<Key> = Vec::new();
        for bot in &self.bots {
            stops.push(row_key(bot.id));
            stops.extend(
                self.activity
                    .iter()
                    .filter(|entry| entry.bot_id == bot.id)
                    .map(|entry| activity_key(entry.chat_id)),
            );
        }
        if self
            .abandoned
            .as_ref()
            .and_then(|abandoned| abandoned.left_behind.as_ref())
            .is_some_and(|left| left.role_page.is_some())
        {
            stops.push(OPEN_LEFT_PAGE);
        }
        for creation in &self.creations {
            if self.creations_acting.contains(&creation.request_id) {
                continue;
            }
            if creation.retryable {
                stops.push(creation_retry_key(creation.request_id));
            }
            stops.push(creation_abandon_key(creation.request_id));
        }
        stops.push(ARCHIVED_TOGGLE);
        if self.archived_open {
            for bot in &self.archived {
                stops.push(archived_key(bot.id));
                if !self.unarchiving(bot.id) && self.can_unarchive(bot.id) {
                    stops.push(unarchive_key(bot.id));
                }
            }
        }
        stops
    }

    /// **Focus enters on open**: on the Bot last asked for when the list
    /// holds it, else the first control there is. `false` with none.
    pub(crate) fn focus_first(&mut self) -> bool {
        let landing = self
            .selected
            .filter(|id| self.has(*id))
            .map(row_key)
            .or_else(|| self.keyboard_stops().first().copied());
        self.focus = landing;
        self.reveal_owed = landing.is_some();
        landing.is_some()
    }

    /// Put the keyboard on `bot`'s row.
    pub(crate) fn set_focus(&mut self, bot: BotId) -> bool {
        if !self.has(bot) {
            return false;
        }
        self.focus = Some(row_key(bot));
        self.reveal_owed = true;
        true
    }

    /// Give the keyboard back. `true` when the roster had it.
    pub(crate) fn clear_focus(&mut self) -> bool {
        self.focus.take().is_some()
    }

    /// A key while the roster holds the keyboard. `false` for a key it does
    /// not claim, which falls through. **Up and Down walk every control the
    /// roster draws** ([`Self::keyboard_stops`]); Enter does what a press of
    /// the focused control does, through the same functions the pointer and
    /// an assistive technology use.
    pub(crate) fn key(&mut self, press: Press) -> bool {
        let Some(focus) = self.focus else {
            return false;
        };
        let stops = self.keyboard_stops();
        let Some(index) = stops.iter().position(|stop| *stop == focus) else {
            // What it was on left: land on the first control there is.
            self.focus = stops.first().copied();
            self.reveal_owed = self.focus.is_some();
            return true;
        };
        let last = stops.len().saturating_sub(1);
        let to = match press {
            Press::Up => index.saturating_sub(1),
            Press::Down => (index + 1).min(last),
            Press::Home => 0,
            Press::End => last,
            Press::Open => return self.press_key(focus),
            Press::Escape => {
                self.focus = None;
                return true;
            }
        };
        self.focus = Some(stops[to]);
        self.reveal_owed = true;
        true
    }

    /// **Enter on a control**: a Bot row opens its tab; the Unfinished and
    /// Archived controls go to their own press functions.
    fn press_key(&mut self, key: Key) -> bool {
        if key.part == ROW_PART
            && let koru_draw::el::Object::Model(id) = key.object
            && let Some(bot) = self
                .bots
                .iter()
                .map(|bot| bot.id)
                .find(|bot| bot.as_uuid().as_u128() == id)
        {
            return self.ask_open(bot);
        }
        // A running or Needs-you sub-row opens its conversation, as a
        // press of it does.
        if key.part == ACTIVITY_PART
            && let Some(chat) = self
                .activity
                .iter()
                .map(|entry| entry.chat_id)
                .find(|chat| activity_key(*chat) == key)
        {
            return self.ask_open_chat(chat);
        }
        let pressed =
            if key == ARCHIVED_TOGGLE || key.part == ARCHIVED_PART || key.part == UNARCHIVE_PART {
                self.press_archived(key)
            } else {
                self.press_unfinished(key)
            };
        if pressed {
            self.keep_focus();
        }
        pressed
    }

    // ----------------------------------------------------------- pointer

    /// A press in the roster's band. Always the roster's: a press on its
    /// background must not fall through to the view beside the column.
    pub(crate) fn mouse_down(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        if let Some(key) = self.unfinished_at(area, top, x, y) {
            self.press_unfinished(key);
            return true;
        }
        if let Some(key) = self.archived_at(area, top, x, y) {
            self.press_archived(key);
            return true;
        }
        if let Some(chat) = self.activity_at(area, top, x, y) {
            self.ask_open_chat(chat);
        } else if let Some(bot) = self.bot_at(area, top, x, y) {
            self.ask_open(bot);
        }
        true
    }

    /// The activity sub-row under a point, by its conversation.
    fn activity_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<koru_ai_core::ChatId> {
        if self.activity.is_empty() {
            return None;
        }
        let key = self.with_input_frame(area, top, |_, frame| frame.hit_at(x, y))?;
        self.activity
            .iter()
            .map(|entry| entry.chat_id)
            .find(|chat| activity_key(*chat) == key)
    }

    /// The Archived filter's control under a point: its toggle, an archived
    /// row, or a row's *Unarchive*.
    fn archived_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<Key> {
        let key = self.with_input_frame(area, top, |_, frame| frame.hit_at(x, y))?;
        (key == ARCHIVED_TOGGLE || key.part == ARCHIVED_PART || key.part == UNARCHIVE_PART)
            .then_some(key)
    }

    /// **Do what an Archived filter control asks** — the pointer's press and
    /// an assistive technology's click alike. `true` when it was one.
    pub(crate) fn press_archived(&mut self, key: Key) -> bool {
        if key == ARCHIVED_TOGGLE {
            return self.toggle_archived();
        }
        let koru_draw::el::Object::Model(id) = key.object else {
            return false;
        };
        let Some(bot) = self
            .archived
            .iter()
            .map(|row| row.id)
            .find(|bot| bot.as_uuid().as_u128() == id)
        else {
            return false;
        };
        if key.part == UNARCHIVE_PART {
            return self.unarchive(bot);
        }
        self.ask_open_archived(bot)
    }

    /// The pointer moved. `true` when the row under it changed.
    pub(crate) fn mouse_move(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let archived = self
            .unfinished_at(area, top, x, y)
            .or_else(|| self.archived_at(area, top, x, y));
        let activity = if archived.is_some() {
            None
        } else {
            self.activity_at(area, top, x, y)
        };
        let hover = if activity.is_some() || archived.is_some() {
            None
        } else {
            self.bot_at(area, top, x, y)
        };
        let moved = std::mem::replace(&mut self.hover_activity, activity) != activity;
        let lit = std::mem::replace(&mut self.hover_archived, archived) != archived;
        std::mem::replace(&mut self.hover, hover) != hover || moved || lit
    }

    pub(crate) fn clear_hover(&mut self) -> bool {
        let activity = self.hover_activity.take().is_some();
        let archived = self.hover_archived.take().is_some();
        self.hover.take().is_some() || activity || archived
    }

    /// The list's offset, as the last layout clamped it: what the published
    /// rows' clipped bounds move with.
    pub(crate) fn scroll_offset(&self) -> f32 {
        self.scroll
    }

    pub(crate) fn scroll_by(&mut self, dy: f32) {
        // Clamped at the next layout, which knows the content's extent.
        self.scroll = (self.scroll - dy).max(0.0);
    }

    fn bot_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<BotId> {
        if self.bots.is_empty() {
            return None;
        }
        let key = self.with_input_frame(area, top, |_, frame| frame.hit_at(x, y))?;
        self.bots
            .iter()
            .map(|bot| bot.id)
            .find(|bot| row_key(*bot) == key)
    }

    // ------------------------------------------------------------ frame

    /// The band the roster owns: the column below its heading.
    pub(crate) fn band(area: Rect, top: f32) -> Rect {
        Rect::new(area.x, top, area.w, (area.y + area.h - top).max(0.0))
    }

    /// The line over the list, when there is one to say — what the roster
    /// draws and what it publishes, from this one place.
    pub(crate) fn notice(&self) -> Option<Words> {
        match self.load {
            Load::Loading => Some(Words::BotsLoading),
            Load::Unavailable => Some(Words::BotsUnavailable),
            Load::Denied => Some(Words::BotsNoAccess),
            Load::Cold | Load::Ready => None,
        }
    }

    /// A row's accessible name: the Bot's name, and that it is paused.
    pub(crate) fn accessible_name(&self, bot: &RosterBot) -> String {
        if bot.paused {
            koru_i18n::fill(t(self.lang, Words::BotsRowPaused), &[bot.name.as_str()])
        } else {
            bot.name.clone()
        }
    }

    fn bot_row(&self, bot: &RosterBot) -> El {
        let key = row_key(bot.id);
        let selected = self.selected == Some(bot.id);
        let mut primary = row("Bot primary")
            .id(key)
            .fill()
            .min_h(super::faces::ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .gap(Space::S2)
            .items_center()
            .radius(Radius::M)
            .states(&BOT_ROW_STATES)
            .hit()
            .focusable()
            .action(Action::Click)
            .action(Action::Focus)
            .semantics(Role::ListItem)
            .name_text(self.accessible_name(bot))
            .selected(selected)
            // Its face (O155): the one it stores, else the one its id
            // gives it; moving only while it works.
            .child(
                koru_draw::el::leaf(
                    "Bot face",
                    super::faces::FaceLeaf {
                        face: bot.avatar,
                        name: bot.name.clone(),
                        pose: self.face_pose(bot),
                        size: super::faces::ROW_FACE,
                    },
                )
                .decorative(),
            )
            .child(
                text(&bot.name)
                    .named("Bot name")
                    .role(roles::ROW)
                    .ink(if selected {
                        Ink::Primary
                    } else {
                        Ink::Secondary
                    })
                    .emphasis(selected)
                    .fill()
                    .truncate(),
            );
        if bot.paused {
            primary = primary.child(
                text(t(self.lang, Words::BotsPaused))
                    .named("Bot paused")
                    .role(roles::META)
                    .ink(Ink::Hint),
            );
        }
        // Where it runs, as a mark with its own words (O155): every Bot
        // runs on the computer that owns it, which is this one.
        primary = primary.child(
            icon(icons::DESKTOP, IconSize::S)
                .alt(Words::BotsRunsOnThisComputer)
                .id(runs_here_key(bot.id))
                .ink(Ink::Hint),
        );
        // The margins press the row's primary, so the whole band is the row.
        let mut block = col("Bot block").child(
            row("Bot row")
                .pad_x(alias::ROW_PLATE_INSET)
                .hit_as(key)
                .child(primary),
        );
        for entry in self.activity.iter().filter(|entry| entry.bot_id == bot.id) {
            block = block.child(self.activity_row(bot, entry));
        }
        block
    }

    /// **One running or Needs-you conversation under its Bot** (S10): the
    /// Chats list's mark and its words, then which conversation — the main
    /// one by name; another by what it is, the read naming no title.
    fn activity_row(&self, bot: &RosterBot, entry: &BotActivity) -> El {
        let key = activity_key(entry.chat_id);
        let status = activity_status(entry.state);
        let which = self.conversation_words(bot, entry.chat_id);
        let primary = row("Bot activity")
            .id(key)
            .fill()
            .min_h(metrics::NAV_ROW_MIN)
            .pad_start(Space::S6)
            .pad_end(alias::ROW_PLATE_INSET)
            .gap(Space::S2)
            .items_center()
            .radius(Radius::M)
            .states(&BOT_ROW_STATES)
            .hit()
            .focusable()
            .action(Action::Click)
            .semantics(Role::ListItem)
            .name_text(self.activity_name(bot, entry))
            .child(
                koru_draw::el::leaf("Bot activity mark", MarkLeaf(status))
                    .decorative()
                    .w(metrics::TRAILING_LANE_W),
            )
            .child(
                text(which)
                    .named("Bot activity conversation")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            )
            .child(
                text(status.spoken(self.lang).unwrap_or_default())
                    .named("Bot activity state")
                    .role(roles::META)
                    .ink(Ink::Hint),
            );
        row("Bot activity row")
            .pad_x(alias::ROW_PLATE_INSET)
            .hit_as(key)
            .child(primary)
    }

    fn sentence(name: &'static str, part: Key, words: &str) -> El {
        row(name)
            .id(part)
            .pad_x(alias::NAV_PAD)
            .pad_y(Space::S1)
            .child(
                text(words)
                    .named("Bots sentence")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap()
                    .fill(),
            )
    }

    /// The roster as elements.
    pub(crate) fn tree(&self) -> El {
        let mut root = col("Bots roster")
            .id(Key::surface(SURFACE, "roster"))
            .pad_top(Space::S1)
            .gap(Space::S1);
        if let Some(notice) = self.notice() {
            root = root.child(Self::sentence("Bots notice", NOTICE, t(self.lang, notice)));
        }
        if self.load == Load::Ready && self.bots.is_empty() {
            // No active Bot: one short line (O155: the pitch and *New bot*
            // are the main area's, and the heading keeps its `+`), what has
            // not finished, and the Archived filter (S14).
            return root
                .child(Self::sentence(
                    "Bots empty",
                    EMPTY,
                    t(self.lang, Words::BotsNoneYet),
                ))
                .children(self.unfinished_section())
                .child(self.archived_section());
        }
        if self.activity_truncated {
            root = root.child(Self::sentence(
                "Bots more work",
                MORE_WORK,
                t(self.lang, Words::BotsMoreWork),
            ));
        }
        root.child(
            scroll_y("Bots list")
                .id(LIST)
                .fill()
                .offset(self.scroll)
                .scrollbar()
                .semantics(Role::List)
                .name(Words::BotsTitle)
                .children(self.bots.iter().map(|bot| self.bot_row(bot)))
                .children(self.unfinished_section())
                .child(self.archived_section()),
        )
    }

    /// A quiet text action in the roster's own row treatment: the empty
    /// state's *Create a bot*, an Unfinished row's *Retry* and *Abandon*.
    fn text_button(&self, name: &'static str, key: Key, words: Words, label: String) -> El {
        row(name)
            .id(key)
            .pad_x(Space::S2)
            .min_h(metrics::NAV_ROW_MIN)
            .items_center()
            .radius(Radius::M)
            .states(&BOT_ROW_STATES)
            .hit()
            .action(Action::Click)
            .semantics(Role::Button)
            .name_text(label)
            .child(
                text(t(self.lang, words))
                    .named("Bots action label")
                    .role(roles::META)
                    .ink(Ink::Accent),
            )
    }

    /// **The Unfinished group** (S8): its heading and one row per creation
    /// that has not finished — its name, where it stands, *Retry* when Retry
    /// can resolve it, *Abandon* — and what an Abandon from here left. Absent
    /// while there is nothing to say.
    ///
    /// **Absent, not empty**: an empty group still took its top padding,
    /// an 8 px element the inspector showed under every roster (the first
    /// window run's inspector finding).
    fn unfinished_section(&self) -> Option<El> {
        if self.creations.is_empty() && self.abandoned.is_none() && self.creation_refused.is_none()
        {
            return None;
        }
        let mut section = col("Bots unfinished")
            .pad_x(alias::ROW_PLATE_INSET)
            .pad_top(Space::S2)
            .gap(Space::S1);
        // **The heading heads unfinished creations only**: a group holding
        // none — only what an Abandon left, or a Retry's word — draws no
        // *Unfinished* over it (the third window run: an empty heading over
        // an abandoned note).
        if !self.creations.is_empty() {
            section = section.child(
                text(t(self.lang, Words::BotsUnfinished))
                    .named("Bots unfinished heading")
                    .id(UNFINISHED_HEADING)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .truncate(),
            );
        }
        if let Some(words) = self.creation_refused {
            section = section.child(
                text(t(self.lang, words))
                    .named("Bots unfinished notice")
                    .id(UNFINISHED_NOTICE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        if let Some(abandoned) = &self.abandoned {
            for (words, key) in crate::bots::view::create::abandoned_lines(self.lang, abandoned)
                .into_iter()
                .zip(ABANDONED_LINES)
            {
                section = section.child(
                    text(words)
                        .named("Bots abandoned")
                        .id(key)
                        .role(roles::META)
                        .ink(Ink::Hint)
                        .wrap(),
                );
            }
            if abandoned
                .left_behind
                .as_ref()
                .is_some_and(|left| left.role_page.is_some())
            {
                section = section.child(self.text_button(
                    "Bots open left page",
                    OPEN_LEFT_PAGE,
                    Words::BotsOpenRole,
                    t(self.lang, Words::BotsOpenRole).to_owned(),
                ));
            }
        }
        for creation in &self.creations {
            section = section.child(self.creation_row(creation));
        }
        // **Never a silent cut**: the owner held more than one listing reads.
        if self.creations_more {
            section = section.child(
                text(t(self.lang, Words::BotsUnfinishedMore))
                    .named("Bots unfinished more")
                    .id(UNFINISHED_MORE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        Some(section)
    }

    /// Whether the owner held more unfinished creations than were listed.
    pub(crate) fn creations_more(&self) -> bool {
        self.creations_more
    }

    /// **One unfinished creation**: a head row — its name, *Retry*,
    /// *Abandon* — and under it where it stands, on its own line with the
    /// row's whole width.
    ///
    /// **The state is never squeezed beside the buttons**: drawn in a column
    /// between the name and the buttons, a sentence like *The working folder
    /// couldn't be made or read.* wrapped into a column about 90 px wide
    /// at the navigator's width, and the row, measured as one line, was
    /// overlapped by *Show archived bots* (the second window run's finding
    /// 3b, `scr-b12-04`).
    fn creation_row(&self, creation: &BotCreation) -> El {
        let name = self.creation_name(creation);
        let mut head = row("Bot creation head")
            .min_h(metrics::NAV_ROW_MIN)
            .gap(Space::S2)
            .items_center()
            .child(icon(icons::ROBOT, IconSize::S).decorative())
            .child(
                text(name.clone())
                    .named("Bot creation name")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            );
        let acting = self.creations_acting.contains(&creation.request_id);
        if acting {
            head = head.child(
                text(t(self.lang, Words::BotsSaving))
                    .named("Bot creation acting")
                    .role(roles::META)
                    .ink(Ink::Hint),
            );
        } else {
            if creation.retryable {
                head = head.child(self.text_button(
                    "Bot creation retry",
                    creation_retry_key(creation.request_id),
                    Words::BotsRetry,
                    koru_i18n::fill(t(self.lang, Words::BotsRetryNamed), &[name.as_str()]),
                ));
            }
            head = head.child(self.text_button(
                "Bot creation abandon",
                creation_abandon_key(creation.request_id),
                Words::BotsAbandon,
                koru_i18n::fill(t(self.lang, Words::BotsAbandonNamed), &[name.as_str()]),
            ));
        }
        // **No `fill` down the group's column**: in a column it shares out
        // the height, so the row was given one line's height whatever its
        // words needed — what let *Show archived bots* draw over them.
        col("Bot creation row")
            .id(creation_key(creation.request_id))
            .pad_x(alias::ROW_PLATE_INSET)
            .child(head)
            .child(
                row("Bot creation state row")
                    .id(creation_state_key(creation.request_id))
                    .pad_bottom(Space::S1)
                    .child(
                        text(self.creation_state(creation))
                            .named("Bot creation state")
                            .role(roles::META)
                            .ink(Ink::Hint)
                            .wrap()
                            .fill(),
                    ),
            )
    }

    /// An unfinished creation's name, or *New bot* when it has none.
    pub(crate) fn creation_name(&self, creation: &BotCreation) -> String {
        if creation.name.trim().is_empty() {
            t(self.lang, Words::BotsNewBot).to_owned()
        } else {
            creation.name.clone()
        }
    }

    /// Where an unfinished creation stands, in its first sentence: why it
    /// stopped, or that it is still being made.
    pub(crate) fn creation_state(&self, creation: &BotCreation) -> String {
        crate::bots::creation::stage_words(creation)
            .first()
            .map(|words| t(self.lang, *words).to_owned())
            .unwrap_or_default()
    }

    /// An unfinished creation's accessible name: its name, not finished.
    pub(crate) fn creation_accessible_name(&self, creation: &BotCreation) -> String {
        koru_i18n::fill(
            t(self.lang, Words::BotsUnfinishedRow),
            &[self.creation_name(creation).as_str()],
        )
    }

    // ---------------------------------------- unfinished creations (S8)

    /// The workspace's unfinished creations, as last listed.
    pub(crate) fn creations(&self) -> &[BotCreation] {
        &self.creations
    }

    /// The creation abandoned from here last, while the roster says so.
    pub(crate) fn abandoned(&self) -> Option<&BotCreation> {
        self.abandoned.as_ref()
    }

    /// Why the last Retry or Abandon from here did not happen.
    pub(crate) fn creation_refused(&self) -> Option<Words> {
        self.creation_refused
    }

    /// Whether `request`'s Retry or Abandon is on its way.
    pub(crate) fn creation_acting(&self, request: BotRequestId) -> bool {
        self.creations_acting.contains(&request)
    }

    /// **Create a bot** was asked for: the empty state's button, or an
    /// assistive technology's click on it.
    pub(crate) fn ask_create(&mut self) -> bool {
        self.wants_create = true;
        true
    }

    /// *Create a bot*, for the window to open. Taken once.
    pub(crate) fn take_wants_create(&mut self) -> bool {
        std::mem::take(&mut self.wants_create)
    }

    /// An Abandon pressed, for the window to ask about. Taken once.
    pub(crate) fn take_abandon_ask(&mut self) -> Option<(BotRequestId, String)> {
        self.abandon_ask.take()
    }

    /// A page an abandoned creation left, to open. Taken once.
    pub(crate) fn take_open_page(&mut self) -> Option<BotPage> {
        self.open_page.take()
    }

    /// **Send a creation command by its request alone**: Retry, or an
    /// Abandon the person confirmed. `false` when nothing was sent.
    fn act_on_creation(&mut self, request: BotRequestId, command: BotCommand) -> bool {
        let (Some(creation), Some(link)) = (
            self.creations.iter().find(|c| c.request_id == request),
            &self.link,
        ) else {
            return false;
        };
        if self.creations_acting.contains(&request) {
            return false;
        }
        if !link.send(Command::Write {
            bot: creation.bot_id,
            request,
            command: Box::new(command),
        }) {
            self.creation_refused = Some(Words::BotsErrUnreachable);
            return true;
        }
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        self.creation_refused = None;
        self.creations_acting.insert(request);
        true
    }

    /// **Read one creation** (`ReadBotCreation`, by its request alone), its
    /// answer folded as a Retry's is. `false` when nothing was sent.
    fn read_creation(&mut self, request: BotRequestId) -> bool {
        let (Some(creation), Some(link)) = (
            self.creations.iter().find(|c| c.request_id == request),
            &self.link,
        ) else {
            return false;
        };
        if !link.send(Command::Write {
            bot: creation.bot_id,
            request,
            command: Box::new(BotCommand::ReadBotCreation {
                request_id: request,
            }),
        }) {
            return false;
        }
        let mut work = self.work.get();
        work.commands += 1;
        self.work.set(work);
        self.creations_acting.insert(request);
        true
    }

    /// **Retry** an unfinished creation, when Retry can resolve it.
    pub(crate) fn retry(&mut self, request: BotRequestId) -> bool {
        if !self
            .creations
            .iter()
            .any(|creation| creation.request_id == request && creation.retryable)
        {
            return false;
        }
        self.act_on_creation(
            request,
            BotCommand::RetryBotCreation {
                request_id: request,
            },
        )
    }

    /// **Abandon** an unfinished creation, once the person confirmed it.
    pub(crate) fn abandon(&mut self, request: BotRequestId) -> bool {
        self.act_on_creation(
            request,
            BotCommand::AbandonBotCreation {
                request_id: request,
            },
        )
    }

    /// Fold a Retry's or an Abandon's answer. `true` when it was one of
    /// this roster's.
    /// **Keep the clock while a listed creation moves**: the next listing
    /// at the current step, doubling to two seconds; none once none moves.
    fn schedule_creations(&mut self) {
        const FIRST: std::time::Duration = std::time::Duration::from_millis(250);
        const MAX: std::time::Duration = std::time::Duration::from_secs(2);
        let moving = self
            .creations
            .iter()
            .any(|creation| creation.stage == BotCreationStage::Pending);
        if !moving {
            self.creations_next = None;
            self.creations_backoff = FIRST;
            return;
        }
        let wait = self.creations_backoff.max(FIRST);
        let now = std::time::Instant::now();
        self.creations_next = Some(now.checked_add(wait).unwrap_or(now));
        self.creations_backoff = (wait * 2).min(MAX);
    }

    fn take_creation(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
    ) -> bool {
        if !self.creations_acting.remove(&request) {
            return false;
        }
        // Whatever it came to, a create tab holding this creation reads it
        // again: it must not go on offering Retry and Abandon for a
        // creation finished or abandoned here.
        self.creations_moved.push(request);
        match result {
            Ok(BotResponse::Creation { creation }) if creation.request_id == request => {
                match creation.stage {
                    BotCreationStage::Pending | BotCreationStage::Incomplete => {
                        if let Some(row) = self
                            .creations
                            .iter_mut()
                            .find(|row| row.request_id == request)
                        {
                            *row = creation;
                        }
                    }
                    BotCreationStage::Abandoned => {
                        self.creations.retain(|row| row.request_id != request);
                        self.abandoned = Some(creation);
                        self.abandoned_at = self.seq;
                    }
                    // The owner's stage speaks: made, or refused before it
                    // made anything.
                    BotCreationStage::Completed => {
                        self.creations.retain(|row| row.request_id != request);
                        self.creation_refused = Some(Words::BotsCreateCreated);
                    }
                    BotCreationStage::Rejected => {
                        self.creations.retain(|row| row.request_id != request);
                        self.creation_refused = Some(Words::BotsUnfinishedRejected);
                    }
                }
            }
            // The outcome is unknown: never said as refused. The listing
            // read again shows whether it is still unfinished.
            Ok(_) | Err(Failure::Unanswered) => {
                self.creation_refused = Some(Words::BotsErrUnreachable);
            }
            // **`request_conflict` says the creation is not where the
            // command found it**: Abandon met one that finished — made, or
            // rejected (`abandon_creation`) — or the record moved between
            // its read and its write (`abandon_bot_creation`,
            // `advance_bot_creation`). Which one is the owner's to say: the
            // creation is read, and its stage speaks.
            Err(Failure::Refused(code)) if code == "request_conflict" => {
                if !self.read_creation(request) {
                    self.creation_refused = Some(Words::BotsErrUnreachable);
                }
            }
            Err(Failure::Refused(code)) => {
                self.creation_refused = Some(super::settings::general_refusal_words(&code));
            }
        }
        // Either way the lists are the owner's to say again: a finished
        // creation's Bot is listed now.
        self.fresh = false;
        true
    }

    /// **Do what an Unfinished control asks** — the pointer's press and an
    /// assistive technology's click alike. `true` when it was one.
    pub(crate) fn press_unfinished(&mut self, key: Key) -> bool {
        if key == CREATE {
            return self.ask_create();
        }
        if key == OPEN_LEFT_PAGE {
            self.open_page = self
                .abandoned
                .as_ref()
                .and_then(|abandoned| abandoned.left_behind.as_ref())
                .and_then(|left| left.role_page.clone());
            return self.open_page.is_some();
        }
        let koru_draw::el::Object::Model(id) = key.object else {
            return false;
        };
        let Some(creation) = self
            .creations
            .iter()
            .find(|creation| creation.request_id.as_uuid().as_u128() == id)
        else {
            return false;
        };
        let request = creation.request_id;
        match key.part {
            CREATION_RETRY_PART => self.retry(request),
            CREATION_ABANDON_PART => {
                if self.creations_acting.contains(&request) {
                    return false;
                }
                self.abandon_ask = Some((request, self.creation_name(creation)));
                true
            }
            _ => false,
        }
    }

    /// The Unfinished control under a point.
    fn unfinished_at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<Key> {
        let key = self.with_input_frame(area, top, |_, frame| frame.hit_at(x, y))?;
        (key == CREATE
            || key == OPEN_LEFT_PAGE
            || key.part == CREATION_RETRY_PART
            || key.part == CREATION_ABANDON_PART)
            .then_some(key)
    }

    /// **The Archived filter** (S14; EXPERIENCE: archived Bots stay "in an
    /// Archived filter outside the active roster"), under the active rows:
    /// its toggle, and while open its heading, its sentence and one row per
    /// archived Bot, each opening the Bot's tab — its conversations stay
    /// readable — with *Unarchive* when the owner names the Archive.
    fn archived_section(&self) -> El {
        let toggle_words = if self.archived_open {
            Words::BotsHideArchived
        } else {
            Words::BotsShowArchived
        };
        let toggle = row("Bots archived toggle")
            .id(ARCHIVED_TOGGLE)
            .min_h(metrics::NAV_ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .items_center()
            .radius(Radius::M)
            .states(&BOT_ROW_STATES)
            .hit()
            .action(Action::Click)
            .semantics(Role::Button)
            .name(toggle_words)
            .child(
                text(t(self.lang, toggle_words))
                    .named("Bots archived toggle label")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .fill()
                    .truncate(),
            );
        let mut section = col("Bots archived")
            .pad_x(alias::ROW_PLATE_INSET)
            .pad_top(Space::S2)
            .gap(Space::S1)
            .child(toggle);
        if !self.archived_open {
            return section;
        }
        section = section.child(
            text(t(self.lang, Words::BotsArchived))
                .named("Bots archived heading")
                .id(ARCHIVED_HEADING)
                .role(roles::META)
                .ink(Ink::Secondary)
                .truncate(),
        );
        if let Some(words) = self.archived_notice() {
            section = section.child(
                text(t(self.lang, words))
                    .named("Bots archived notice")
                    .id(ARCHIVED_NOTICE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        for bot in &self.archived {
            section = section.child(self.archived_bot_row(bot));
        }
        section
    }

    /// **The Archived filter's sentence**, while it is open: loading, none,
    /// not answering, why an Unarchive did not happen — or that a Bot whose
    /// Archive the owner does not name (`held_by`) cannot be unarchived from
    /// here, said once rather than as a dead button.
    pub(crate) fn archived_notice(&self) -> Option<Words> {
        if !self.archived_open {
            return None;
        }
        match self.archived_load {
            Load::Cold | Load::Loading if self.archived.is_empty() => {
                Some(Words::BotsArchivedLoading)
            }
            Load::Unavailable => Some(Words::BotsArchivedUnavailable),
            Load::Ready if self.archived.is_empty() => Some(Words::BotsArchivedEmpty),
            _ if self.unarchive_refused.is_some() => self.unarchive_refused,
            _ if self.archived.iter().any(|bot| !self.can_unarchive(bot.id)) => {
                Some(Words::BotsUndoUnavailable)
            }
            _ => None,
        }
    }

    /// One archived Bot: its name, opening its tab, and *Unarchive*.
    fn archived_bot_row(&self, bot: &Bot) -> El {
        let key = archived_key(bot.id);
        let mut line = row("Bot archived row")
            .id(key)
            .fill()
            .min_h(metrics::NAV_ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .gap(Space::S2)
            .items_center()
            .radius(Radius::M)
            .states(&BOT_ROW_STATES)
            .hit()
            .action(Action::Click)
            .semantics(Role::ListItem)
            .name_text(bot.config.name.clone())
            .selected(self.selected == Some(bot.id))
            .child(icon(icons::ROBOT, IconSize::S).decorative())
            .child(
                text(&bot.config.name)
                    .named("Bot archived name")
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate(),
            );
        if self.unarchiving(bot.id) {
            line = line.child(
                text(t(self.lang, Words::BotsRestoring))
                    .named("Bot unarchiving")
                    .role(roles::META)
                    .ink(Ink::Hint),
            );
        } else if self.can_unarchive(bot.id) {
            let words = t(self.lang, Words::BotsUnarchive);
            line = line.child(
                row("Bot unarchive")
                    .id(unarchive_key(bot.id))
                    .pad_x(Space::S2)
                    .min_h(metrics::NAV_ROW_MIN)
                    .items_center()
                    .radius(Radius::M)
                    .states(&BOT_ROW_STATES)
                    .hit()
                    .action(Action::Click)
                    .semantics(Role::Button)
                    .name_text(koru_i18n::fill(
                        t(self.lang, Words::BotsUnarchiveNamed),
                        &[bot.config.name.as_str()],
                    ))
                    .child(
                        text(words)
                            .named("Bot unarchive label")
                            .role(roles::META)
                            .ink(Ink::Accent),
                    ),
            );
        }
        line
    }

    fn interaction(&self) -> Interaction {
        Interaction {
            hot: self
                .hover_archived
                .or(self.hover_activity.map(activity_key))
                .or(self.hover.map(row_key)),
            pressed: None,
            focused: self.focus,
        }
    }

    fn layout(&self, engine: &mut TextEngine, area: Rect, top: f32) -> (El, Frame) {
        let tree = self.tree();
        let frame = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            Self::band(area, top),
            FrameCx::root().with_interaction(self.interaction()),
        );
        let mut work = self.work.get();
        work.layouts += 1;
        self.work.set(work);
        (tree, frame)
    }

    /// The frame as input and accessibility read it, laid out with the
    /// thread's input engine: no paint is running when they ask.
    pub(crate) fn with_input_frame<R>(
        &self,
        area: Rect,
        top: f32,
        read: impl FnOnce(&El, &Frame) -> R,
    ) -> R {
        let (tree, frame) = crate::input_engine::with(|engine| self.layout(engine, area, top));
        read(&tree, &frame)
    }

    /// Paint the roster below its host's heading, from `top` down.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
    ) {
        let (mut tree, mut frame) = self.layout(engine, area, top);
        if std::mem::take(&mut self.reveal_owed)
            && let Some((_, offset)) = self.focus.and_then(|key| frame.reveal(key))
        {
            self.scroll = offset;
            (tree, frame) = self.layout(engine, area, top);
        }
        if let Some((_, offset)) = frame.node(LIST).and_then(|node| node.scroll) {
            self.scroll = offset;
        }
        frame.paint(&tree, scene, root, engine);
    }
}

/// **The Chats list's status mark**, as an element leaf: the ring of a
/// running conversation, held still — the roster asks for no frames — and
/// the question of one that needs the person. Its words are the row's.
struct MarkLeaf(ChatStatus);

impl koru_draw::el::Leaf for MarkLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (metrics::TRAILING_LANE_W.value, metrics::NAV_ROW_MIN.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: koru_draw::el::ElState,
    ) {
        if let Some(mark) = self.0.mark(None) {
            koru_shell_chrome::status_mark::draw(scene, root, engine, at, mark);
        }
    }
}

#[cfg(test)]
mod tests;
