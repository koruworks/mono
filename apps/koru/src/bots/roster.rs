// SPDX-License-Identifier: MPL-2.0

//! **The Bots roster**: the person's own Bots whose home is the window's
//! workspace, one row each, in the order the service creates them.
//!
//! ## What a row says
//!
//! The Bot's **name**, and **Paused** when its owner says it is paused. That
//! is all the service's roster read says for certain, so it is all a row
//! claims. Running and *Needs you* work is not shown here yet: the window can
//! only see the conversations it has open, so a roster built from them would
//! read *idle* for a Bot working with no tab open — an incomplete picture
//! presented as a complete one. It joins the row when the Bot owner can be
//! asked for it (Bots P1.8 §2, B5).
//!
//! ## What it says when it has no rows
//!
//! - **Asked, nothing back yet**: *Loading bots…* — never a zero claimed early.
//! - **The service did not answer**: it says so, **over what it showed
//!   before** — a failed read is never *you have no Bots*.
//! - **The service answered with none**: the approved empty sentence. It names
//!   no button, because creating a Bot is not in this build yet, and a button
//!   that leads nowhere is a promise the roster cannot keep.
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

use koru_ai_core::bots::{Bot, BotId};
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

use super::link::{Answer, BotsLink, Command, Failure};

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

/// A row's key: the Bot's own id, never its place in the list.
pub(crate) fn row_key(bot: BotId) -> Key {
    Key::model(bot.as_uuid().as_u128(), ROW_PART)
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
}

impl From<Bot> for RosterBot {
    fn from(bot: Bot) -> Self {
        Self {
            id: bot.id,
            name: bot.config.name,
            paused: bot.paused,
            main_chat: bot.main_chat,
            home: bot.home_workspace,
            provider: bot.config.model.provider,
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
    /// `Some` doubles as *the roster holds the keyboard*.
    focus: Option<BotId>,
    hover: Option<BotId>,
    /// The list's offset, as the last layout clamped it.
    scroll: f32,
    /// The keyboard moved; the next draw brings its row into view.
    reveal_owed: bool,
    wants_open: Option<BotId>,
    work: Cell<Work>,
    cache: RefCell<TextCache>,
    started: bool,
    link: Option<BotsLink>,
}

impl BotsRoster {
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

    pub(crate) fn work(&self) -> Work {
        self.work.get()
    }

    pub(crate) fn selected(&self) -> Option<BotId> {
        self.selected
    }

    pub(crate) fn focus(&self) -> Option<BotId> {
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
        true
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
            self.focus = None;
            self.hover = None;
        }
        true
    }

    /// The pass the window runs on its wake path. A hidden roster returns
    /// at once: no request, no answer folded. `true` when a repaint is owed.
    pub(crate) fn maintain(&mut self) -> bool {
        if !self.shown {
            return false;
        }
        let asked = !self.fresh && self.refresh();
        let answered = self.poll();
        asked || answered
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
    /// older reply can never replace newer state.
    fn poll(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let mut changed = false;
        for answer in link.drain() {
            // The roster asks only for lists; any other answer is not its.
            let Answer::Listed {
                seq,
                workspace,
                result,
            } = answer
            else {
                continue;
            };
            if seq != self.seq || Some(workspace) != self.workspace {
                continue;
            }
            self.in_flight = false;
            self.take(result);
            changed = true;
        }
        changed
    }

    fn take(&mut self, result: Result<Vec<Bot>, Failure>) {
        match result {
            Ok(bots) => {
                self.bots = bots.into_iter().map(RosterBot::from).collect();
                self.load = Load::Ready;
                // Kept by id; a Bot the answer no longer lists lets go.
                let selected = self.selected.filter(|id| self.has(*id));
                let hover = self.hover.filter(|id| self.has(*id));
                // The keyboard stays in the list: on its Bot, else the first.
                let focus = self.focus.map(|id| {
                    Some(id)
                        .filter(|id| self.has(*id))
                        .or_else(|| self.bots.first().map(|bot| bot.id))
                });
                self.selected = selected;
                self.hover = hover;
                self.focus = focus.flatten();
            }
            // What was shown stays; the line over it says why it may be old.
            Err(_) => self.load = Load::Unavailable,
        }
    }

    fn has(&self, id: BotId) -> bool {
        self.bots.iter().any(|bot| bot.id == id)
    }

    /// Whether an answer is owed — the one reason the window keeps a wake
    /// clock for the roster.
    pub(crate) fn awaiting(&self) -> bool {
        self.shown && self.in_flight
    }

    /// The Bot a row, a key or an accessibility action asked for, taken
    /// once, with what its tab needs: the window opens or focuses it.
    pub(crate) fn take_wants_open(&mut self) -> Option<RosterBot> {
        let bot = self.wants_open.take()?;
        self.bots.iter().find(|row| row.id == bot).cloned()
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

    /// **Focus enters on open**: on the Bot last asked for when the list
    /// holds it, else the first. `false` with no rows to land on.
    pub(crate) fn focus_first(&mut self) -> bool {
        let landing = self
            .selected
            .filter(|id| self.has(*id))
            .or_else(|| self.bots.first().map(|bot| bot.id));
        self.focus = landing;
        self.reveal_owed = landing.is_some();
        landing.is_some()
    }

    pub(crate) fn set_focus(&mut self, bot: BotId) -> bool {
        if !self.has(bot) {
            return false;
        }
        self.focus = Some(bot);
        self.reveal_owed = true;
        true
    }

    /// Give the keyboard back. `true` when the roster had it.
    pub(crate) fn clear_focus(&mut self) -> bool {
        self.focus.take().is_some()
    }

    /// A key while the roster holds the keyboard. `false` for a key it does
    /// not claim, which falls through.
    pub(crate) fn key(&mut self, press: Press) -> bool {
        let Some(index) = self
            .focus
            .and_then(|id| self.bots.iter().position(|bot| bot.id == id))
        else {
            return false;
        };
        let last = self.bots.len().saturating_sub(1);
        let to = match press {
            Press::Up => index.saturating_sub(1),
            Press::Down => (index + 1).min(last),
            Press::Home => 0,
            Press::End => last,
            Press::Open => {
                let bot = self.bots[index].id;
                return self.ask_open(bot);
            }
            Press::Escape => {
                self.focus = None;
                return true;
            }
        };
        self.focus = Some(self.bots[to].id);
        self.reveal_owed = true;
        true
    }

    // ----------------------------------------------------------- pointer

    /// A press in the roster's band. Always the roster's: a press on its
    /// background must not fall through to the view beside the column.
    pub(crate) fn mouse_down(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        if let Some(bot) = self.bot_at(area, top, x, y) {
            self.ask_open(bot);
        }
        true
    }

    /// The pointer moved. `true` when the row under it changed.
    pub(crate) fn mouse_move(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let hover = self.bot_at(area, top, x, y);
        std::mem::replace(&mut self.hover, hover) != hover
    }

    pub(crate) fn clear_hover(&mut self) -> bool {
        self.hover.take().is_some()
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

    /// The line over the list, when there is one to say.
    fn notice(&self) -> Option<Words> {
        match self.load {
            Load::Loading => Some(Words::BotsLoading),
            Load::Unavailable => Some(Words::BotsUnavailable),
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
            .min_h(metrics::NAV_ROW_MIN)
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
            .child(icon(icons::ROBOT, IconSize::S).decorative())
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
        // The margins press the row's primary, so the whole band is the row.
        row("Bot row")
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
            return root.child(Self::sentence(
                "Bots empty",
                EMPTY,
                t(self.lang, Words::BotsEmpty),
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
                .children(self.bots.iter().map(|bot| self.bot_row(bot))),
        )
    }

    fn interaction(&self) -> Interaction {
        Interaction {
            hot: self.hover.map(row_key),
            pressed: None,
            focused: self.focus.map(row_key),
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
            && let Some((_, offset)) = self.focus.and_then(|bot| frame.reveal(row_key(bot)))
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

#[cfg(test)]
mod tests;
