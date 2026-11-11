// SPDX-License-Identifier: MPL-2.0

//! **A Bot's tab** (Bots P1.8 S4): a header about the Bot over the Bot's
//! conversation, which is the shared chat itself.
//!
//! ## What it shows
//!
//! The header says what the Bot service said, and nothing it did not: the
//! Bot's **name**, **Paused** when its owner says so, and *Saves results in
//! {workspace}* with the first version's limit — a Bot saves only in its home
//! workspace (QUESTIONS O8). Below it the Bot's main conversation, in an
//! [`AiChatView`] whose surface the Bot owns (`koru_ai_ui::OwnedConversation`,
//! S3): its turns carry the home workspace as their destination, its picker
//! offers only the Bot's agent, the controls the owner refuses are absent,
//! and a conversation that cannot be read says so instead of becoming an
//! ordinary chat.
//!
//! **What it does not show, deliberately.** No settings, no lifecycle menu,
//! no *New conversation* and no conversation list: each is a later slice with
//! its own dependency, and a control that leads nowhere is a promise the tab
//! cannot keep.
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

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use koru_ai_core::bots::{Bot, BotId};
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

use super::link::{Answer, BotsLink, Command, Failure};
use super::roster::RosterBot;
use crate::ai_chat_view::AiChatView;

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
/// *Saves results in {workspace}*.
pub(crate) const SAVES: Key = Key::surface(SURFACE, "saves");
/// The first version's home-only limit.
pub(crate) const LIMIT: Key = Key::surface(SURFACE, "limit");
/// The line saying the Bot is being read, or could not be.
pub(crate) const NOTICE: Key = Key::surface(SURFACE, "notice");
/// Where the conversation sits.
pub(crate) const CONVERSATION: Key = Key::surface(SURFACE, "conversation");

/// **What a Bot's conversation offers beyond the conversation itself.**
/// Files and folders on this computer attached by their path: the Bot owner
/// delivers them to the agent as an ordinary chat's are (QUESTIONS O30), and
/// a path reference grants nothing Koru controls. Everything else the owner
/// refuses — rename, pin, privacy, the chat's own working folder and
/// workspace access — stays absent.
pub(crate) const BOT_OFFERS: koru_ai_ui::OwnedOffers = koru_ai_ui::OwnedOffers {
    rename: false,
    pin: false,
    privacy: false,
    working_folder: false,
    tool_access: false,
    path_references: true,
};

/// **What the tab knows about its Bot**: what the service said, by the Bot
/// service's own ids.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BotFacts {
    pub id: BotId,
    pub name: String,
    pub paused: bool,
    pub home: WorkspaceId,
    pub provider: AiProviderId,
    pub main_chat: ChatId,
}

impl From<&RosterBot> for BotFacts {
    fn from(bot: &RosterBot) -> Self {
        Self {
            id: bot.id,
            name: bot.name.clone(),
            paused: bot.paused,
            home: bot.home,
            provider: bot.provider.clone(),
            main_chat: bot.main_chat,
        }
    }
}

impl From<Bot> for BotFacts {
    fn from(bot: Bot) -> Self {
        Self {
            id: bot.id,
            name: bot.config.name,
            paused: bot.paused,
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
}

/// What the tab has done, so a test can assert a count rather than a clock.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Work {
    /// Requests handed to the link.
    pub commands: usize,
    /// Element layouts made.
    pub layouts: usize,
}

pub(crate) struct BotView {
    bot: BotId,
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
}

impl BotView {
    fn blank(bot: BotId, chat: ChatId) -> Self {
        Self {
            bot,
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
        }
    }

    /// **A Bot opened from the roster**: the roster's answer is fresh, so
    /// the tab knows its Bot and binds its conversation at once; it asks
    /// the Bot service nothing more until it is opened again.
    pub(crate) fn opened(bot: &RosterBot) -> Self {
        let mut view = Self::blank(bot.id, bot.main_chat);
        view.asked = true;
        view.know(BotFacts::from(bot));
        view
    }

    /// **A Bot tab put back from the session**: it knows which Bot and which
    /// conversation, and nothing else until it is shown and the Bot service
    /// answers. Its conversation is not started before then — never as an
    /// ordinary chat in its place.
    pub(crate) fn restored(bot: BotId, chat: ChatId) -> Self {
        Self::blank(bot, chat)
    }

    /// A tab over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(bot: BotId, chat: ChatId, link: BotsLink) -> Self {
        let mut view = Self::blank(bot, chat);
        view.started_link = true;
        view.link = Some(link);
        view
    }

    pub(crate) fn bot(&self) -> BotId {
        self.bot
    }

    /// The conversation this tab is: found by it from anywhere.
    pub(crate) fn chat_id(&self) -> ChatId {
        self.chat
    }

    pub(crate) fn facts(&self) -> Option<&BotFacts> {
        self.facts.as_ref()
    }

    pub(crate) fn load(&self) -> Load {
        self.load
    }

    pub(crate) fn work(&self) -> Work {
        self.work.get()
    }

    /// The embedded chat, once bound: the window's chat passes reach it
    /// through `TabView::chat_view`.
    pub(crate) fn conversation(&self) -> Option<&AiChatView> {
        self.conversation.as_ref()
    }

    pub(crate) fn conversation_mut(&mut self) -> Option<&mut AiChatView> {
        self.conversation.as_mut()
    }

    /// What the tab strip calls it: the Bot's name, or *Bot* until the Bot
    /// service has said.
    pub(crate) fn label(&self) -> String {
        match &self.facts {
            Some(facts) if !facts.name.trim().is_empty() => facts.name.trim().to_owned(),
            _ => t(self.lang, Words::BotsTabUnnamed).to_owned(),
        }
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
    pub(crate) fn refresh_from(&mut self, bot: &RosterBot) -> bool {
        if bot.id != self.bot {
            return false;
        }
        let facts = BotFacts::from(bot);
        if self.facts.as_ref() == Some(&facts) {
            return false;
        }
        self.know(facts);
        true
    }

    /// Take `facts` as what the tab knows, and bind the conversation to the
    /// Bot's — the main one, unless this tab is on another of its chats.
    fn know(&mut self, facts: BotFacts) {
        let conversation = koru_ai_ui::OwnedConversation {
            chat: self.chat,
            destination: facts.home,
            provider: facts.provider.clone(),
            offers: BOT_OFFERS,
        };
        match &mut self.conversation {
            Some(view) => {
                view.bind_owned(conversation);
            }
            None => self.conversation = Some(AiChatView::owned(conversation)),
        }
        self.facts = Some(facts);
        self.load = Load::Ready;
    }

    /// **The pass a shown tab runs** — from its draw and its maintenance,
    /// which the window gives only the tab in front. Starts the link and the
    /// conversation the first time, asks the Bot service once, and folds
    /// whatever it answered. `true` when a repaint is owed.
    pub(crate) fn shown_pass(&mut self, root: Option<PathBuf>) -> bool {
        if !self.started_link {
            self.started_link = true;
            self.link = BotsLink::start(root);
        }
        let asked = !self.asked && self.ask();
        let answered = self.poll();
        if !self.started_chat
            && let Some(conversation) = &mut self.conversation
        {
            self.started_chat = true;
            conversation.activate();
        }
        asked || answered
    }

    /// Ask for the Bot. A full queue leaves it unasked, for a later pass.
    fn ask(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let seq = self.seq.wrapping_add(1);
        if !link.send(Command::Read { seq, bot: self.bot }) {
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
    /// this Bot**, so an older reply never replaces newer state.
    fn poll(&mut self) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let mut taken = None;
        for answer in link.drain() {
            let Answer::Read { seq, bot, result } = answer else {
                continue;
            };
            if seq != self.seq || bot != self.bot {
                continue;
            }
            taken = Some(result);
        }
        let Some(result) = taken else {
            return false;
        };
        self.in_flight = false;
        self.take(result);
        true
    }

    fn take(&mut self, result: Result<Bot, Failure>) {
        match result {
            // A Bot that says it is another is not this tab's.
            Ok(bot) if bot.id == self.bot => self.know(BotFacts::from(bot)),
            Ok(_) | Err(_) => self.load = Load::Unavailable,
        }
    }

    // ------------------------------------------------------------ frame

    /// The line under the header's facts, when there is one to say.
    fn notice(&self) -> Option<Words> {
        match (self.load, self.facts.is_some()) {
            (Load::Cold | Load::Loading, false) => Some(Words::BotsBotLoading),
            (Load::Unavailable, _) => Some(Words::BotsBotUnavailable),
            _ => None,
        }
    }

    /// *Saves results in {workspace}*, naming the home by its name when the
    /// window knows it.
    pub(crate) fn saves_in(&self) -> Option<String> {
        self.facts.as_ref()?;
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
    fn header(&self) -> El {
        let mut header = col("Bot header")
            .id(HEADER)
            .pad_x(alias::NAV_PAD)
            .pad_top(Space::S3)
            .gap(Space::S1);
        if let Some(facts) = &self.facts {
            let mut title = row("Bot title")
                .gap(Space::S2)
                .items_center()
                .child(icon(icons::ROBOT, IconSize::M).decorative())
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
            header = header.child(title);
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
    pub(crate) fn tree(&self) -> El {
        col("Bot tab")
            .id(ROOT)
            .child(self.header())
            .child(boxed("Bot conversation").id(CONVERSATION).fill())
    }

    fn area(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size.0.max(0.0), self.size.1.max(0.0))
    }

    fn layout_with(&self, engine: &mut koru_draw::text::TextEngine) -> (El, Frame) {
        let tree = self.tree();
        let frame = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            self.area(),
            FrameCx::root(),
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
        self.conversation
            .as_mut()
            .is_some_and(|conversation| conversation.set_area(area))
    }

    /// Whether `(x, y)` is on the conversation, in the tab's coordinates —
    /// where the chat answers input; the header has no control.
    fn on_conversation(&self, y: f32) -> bool {
        self.with_frame(|_, frame| y >= self.conversation_area(frame).y)
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
            line(NAME, self.facts.as_ref().map(|facts| facts.name.clone()));
            line(
                PAUSED,
                self.facts
                    .as_ref()
                    .filter(|facts| facts.paused)
                    .map(|_| t(self.lang, Words::BotsPaused).to_owned()),
            );
            line(SAVES, self.saves_in());
            line(
                LIMIT,
                self.facts
                    .as_ref()
                    .map(|_| t(self.lang, Words::BotsHomeOnly).to_owned()),
            );
            line(
                NOTICE,
                self.notice().map(|notice| t(self.lang, notice).to_owned()),
            );
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
        rev.byte(self.load as u8).uuid(self.bot.as_uuid());
        match &self.facts {
            Some(facts) => {
                rev.byte(1).str(&facts.name).bool(facts.paused);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.str(self.home_name.as_deref().unwrap_or(""))
            .f32(self.size.0)
            .f32(self.size.1);
    }
}

impl View for BotView {
    fn set_mods(&mut self, mods: ModifiersState) {
        if let Some(conversation) = &mut self.conversation {
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
        // The header has no control: every key is the conversation's.
        self.conversation
            .as_mut()
            .is_some_and(|conversation| View::key(conversation, ws, key, text, mods))
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        if !self.on_conversation(y) {
            return false;
        }
        self.conversation
            .as_mut()
            .is_some_and(|conversation| View::mouse_down(conversation, ws, x, y, ctrl))
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.conversation
            .as_mut()
            .is_some_and(|conversation| View::mouse_move(conversation, x, y))
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.conversation
            .as_mut()
            .is_some_and(|conversation| View::mouse_up(conversation, ws))
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if !self.on_conversation(y) {
            return false;
        }
        self.conversation
            .as_mut()
            .is_some_and(|conversation| View::mouse_double_click(conversation, ws, x, y))
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if let Some(conversation) = &mut self.conversation {
            View::scroll_by(conversation, delta);
        }
    }

    /// Only the size is kept: nothing is laid out until the tab is drawn,
    /// so a tab sized behind another costs nothing.
    fn set_viewport(&mut self, w: f32, h: f32) {
        self.size = (w, h);
    }

    fn cursor_icon(&self) -> CursorIcon {
        self.conversation
            .as_ref()
            .map_or(CursorIcon::Default, View::cursor_icon)
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        if let Some(conversation) = &mut self.conversation {
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
        let (header, header_frame, conversation_area) = crate::input_engine::with(|engine| {
            let (_, frame) = self.layout_with(engine);
            let header_rect = frame
                .node(HEADER)
                .map_or(Rect::new(0.0, 0.0, 0.0, 0.0), |node| node.rect);
            let conversation_area = self.conversation_area(&frame);
            // The header painted from a frame of its own at its own rect, so
            // its inspector record is the header's size, inside the tab's.
            let header = self.header();
            let header_frame = header.layout(
                engine,
                &mut self.cache.borrow_mut(),
                header_rect,
                FrameCx::root(),
            );
            let mut work = self.work.get();
            work.layouts += 1;
            self.work.set(work);
            (header, header_frame, conversation_area)
        });
        self.place_conversation(conversation_area);
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
            View::draw(
                conversation,
                scene_out,
                workspace,
                images,
                root,
                caret_on,
                pointer,
            );
        }
    }

    fn animating(&self) -> bool {
        self.conversation.as_ref().is_some_and(View::animating)
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        let mine = self.in_flight.then(|| {
            std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(50))
                .unwrap_or_else(std::time::Instant::now)
        });
        let chat = self
            .conversation
            .as_ref()
            .and_then(View::maintenance_deadline);
        match (mine, chat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        let folded = self.in_flight && self.poll();
        let chat = self
            .conversation
            .as_mut()
            .is_some_and(|conversation| View::maintain(conversation, ws, now));
        folded || chat
    }
}

#[cfg(test)]
mod tests;
