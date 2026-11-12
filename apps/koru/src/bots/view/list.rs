// SPDX-License-Identifier: MPL-2.0

//! **The conversation column of a Bot's tab** (Bots P1.8 S8): its heading
//! and *New conversation*, the shared Chats list fed by the Bot owner
//! ([`super::conversations`]), *Show more* when the owner has another page,
//! and the sentences its states say.
//!
//! ```text
//! Bot body (row)
//! ├─ Bot conversations (col, PANEL_W)        CONVERSATIONS
//! │  ├─ Bot conversations heading (row)      "Conversations"  [New conversation]
//! │  ├─ Bot conversations notice             loading / unavailable / only the main one
//! │  ├─ Bot conversation list (fill)         CONVERSATION_LIST: ChatsSurface draws here
//! │  ├─ [Show more]                          MORE_CONVERSATIONS, only with a next page
//! │  └─ Bot new conversation refused         why New conversation made none
//! └─ Bot conversation (fill)                 the shared chat, owned by the Bot
//! ```
//!
//! The list's rows are the Chats view's own: drawn by `ChatsSurface` inside
//! its element's rect, within the tab's inspector record, so F12 nests them
//! under *Bot conversation list*. A press on a row, Enter on the keyboard's
//! row and an accessibility client's click all ask the surface to open it;
//! the tab then shows that conversation in place of the one it showed.

use std::collections::HashMap;

use koru_ai_core::ChatId;
use koru_chats_ui::{ChatStatus, Press};
use koru_draw::el::{El, Frame, Key, Role, boxed, col, row, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, alias, metrics, roles};
use koru_i18n::{Key as Words, t};
use koru_ui::button::ActionStyle;
use winit::keyboard::{Key as WinitKey, ModifiersState};

use super::region::{Control, ControlRole};
use super::{BotView, SURFACE};

/// The conversation column.
pub(crate) const CONVERSATIONS: Key = Key::surface(SURFACE, "conversations");
/// Where the shared list draws its rows: a keyboard stop of its own.
pub(crate) const CONVERSATION_LIST: Key = Key::surface(SURFACE, "conversation-list");
/// *New conversation*.
pub(crate) const NEW_CONVERSATION: Key = Key::surface(SURFACE, "new-conversation");
/// *Show more*.
pub(crate) const MORE_CONVERSATIONS: Key = Key::surface(SURFACE, "more-conversations");
/// The list's sentence: loading, unavailable, only the main one.
pub(crate) const CONVERSATIONS_NOTICE: Key = Key::surface(SURFACE, "conversations-notice");
/// Why *New conversation* made none.
pub(crate) const CREATE_REFUSED: Key = Key::surface(SURFACE, "create-refused");
/// Why a row's Move to Trash was not asked about (S14).
pub(crate) const TRASH_NOTICE: Key = Key::surface(SURFACE, "conversation-trash-notice");
/// A conversation row's part, under the conversation's own id.
pub(crate) const CONVERSATION_PART: &str = "bot-conversation";
/// The header's *Conversations*, while the tab is compact.
pub(crate) const CONVERSATIONS_BUTTON: Key = Key::surface(SURFACE, "conversations-button");

/// **The narrowest the conversation is comfortable at** beside the column:
/// its composer's placeholder and its model chip whole. At the default
/// window (940 x 620, a tab about 628 px wide) the column left the chat
/// about 350 px, its placeholder and chip cut (the second window run).
const CHAT_MIN_W: f32 = 480.0;

/// A conversation row's key, as the accessibility tree names it.
pub(crate) fn conversation_key(chat: ChatId) -> Key {
    Key::model(chat.as_uuid().as_u128(), CONVERSATION_PART)
}

impl BotView {
    /// Whether the conversation column is part of the tab: once the tab
    /// knows its Bot, with the settings closed.
    pub(super) fn shows_column(&self) -> bool {
        self.facts.is_some() && !self.settings_open && (!self.compact() || self.list_open)
    }

    /// **Compact, with the list opened from the header**: the list takes
    /// the conversation's place, which takes no keys or presses meanwhile.
    pub(super) fn list_covers(&self) -> bool {
        self.list_open && self.compact() && self.facts.is_some() && !self.settings_open
    }

    /// The header's *Conversations*: the list in the conversation's place,
    /// or the conversation back. Only while compact. `true` when it moved.
    pub(super) fn toggle_conversation_list(&mut self) -> bool {
        if !self.compact() {
            return false;
        }
        self.list_open = !self.list_open;
        if self.list_open {
            self.set_settings_open(false);
        }
        true
    }

    /// The header's *Conversations* button, selected while the list is
    /// open.
    pub(super) fn conversations_button(&self, engine: &mut TextEngine) -> El {
        self.button(
            engine,
            "Bot conversations button",
            CONVERSATIONS_BUTTON,
            t(self.lang, Words::BotsConversations).to_owned(),
            ActionStyle::Ghost,
            false,
        )
        .selected(self.list_covers())
    }

    /// **Narrower than a page's compact width**, the conversation column
    /// leaves and the conversation takes the tab — the rule the app's other
    /// two-column pages follow (`koru_ui::PageShell::compact`: Settings and
    /// Manage drop their nav column below `COMPACT_BELOW`). The column kept
    /// its 258 px at any width, and at 720 × 600 the chat was left about
    /// 120 px: a word per line, its composer clipped (the first window run's
    /// finding 9).
    ///
    /// **And sooner, where the column would squeeze the conversation**
    /// below [`CHAT_MIN_W`]: the column is kept only where both fit.
    pub(super) fn compact(&self) -> bool {
        self.size.0 < koru_ui::PageShell::COMPACT_BELOW.max(koru_draw::theme::PANEL_W + CHAT_MIN_W)
    }

    /// The column as elements.
    pub(super) fn conversation_column(&self, engine: &mut TextEngine) -> El {
        let list = &self.conversations;
        let new_label = if list.creating() {
            t(self.lang, Words::BotsConversationStarting).to_owned()
        } else {
            t(self.lang, Words::BotsNewConversation).to_owned()
        };
        let heading = row("Bot conversations heading")
            .gap(Space::S2)
            .items_center()
            .child(
                text(t(self.lang, Words::BotsConversations))
                    .named("Bot conversations title")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .fill()
                    .truncate(),
            )
            .child(self.list_button(
                engine,
                "Bot new conversation",
                NEW_CONVERSATION,
                new_label,
                list.creating(),
            ));
        let column = col("Bot conversations").id(CONVERSATIONS);
        // Compact and opened, the list takes the tab's width.
        let column = if self.list_covers() {
            column.fill()
        } else {
            column.w(metrics::PANEL_W)
        };
        let mut column = column
            .pad_x(alias::NAV_PAD)
            .pad_top(Space::S2)
            .gap(Space::S1)
            .semantics(Role::Group)
            .name_text(koru_i18n::fill(
                t(self.lang, Words::BotsConversationsA11y),
                &[self.label().as_str()],
            ))
            .child(heading);
        let notice = list.notice().map(|notice| {
            text(t(self.lang, notice))
                .named("Bot conversations notice")
                .id(CONVERSATIONS_NOTICE)
                .role(roles::META)
                .ink(Ink::Hint)
                .wrap()
        });
        // **Only the main conversation listed is said below its row**, where
        // the other conversations would be (the third window run: *No other
        // conversations yet* stood above the row). The list is then as tall
        // as its rows; loading and unavailable are said above, as before.
        let below = list.notice() == Some(Words::BotsConversationsOnlyMain) && list.shows_rows();
        let (above, after) = if below {
            (None, notice)
        } else {
            (notice, None)
        };
        if let Some(above) = above {
            column = column.child(above);
        }
        let area = boxed("Bot conversation list").id(CONVERSATION_LIST);
        let mut area = if below {
            area.h(koru_draw::el::Length::Runtime(
                self.rows_height(),
                "the conversation list's rows",
            ))
        } else {
            area.fill()
        };
        if list.shows_rows() {
            area = area.hit().focusable();
        }
        column = column.child(area);
        if let Some(after) = after {
            column = column.child(after);
        }
        if list.has_more() {
            column = column.child(self.list_button(
                engine,
                "Bot more conversations",
                MORE_CONVERSATIONS,
                t(self.lang, Words::BotsMoreConversations).to_owned(),
                false,
            ));
        }
        if let Some(refused) = list.create_refused() {
            column = column.child(
                text(t(self.lang, refused))
                    .named("Bot new conversation refused")
                    .id(CREATE_REFUSED)
                    .role(roles::META)
                    .ink(Ink::Accent)
                    .wrap(),
            );
        }
        // Why a row's Trash was not asked about (S14).
        if let Some(notice) = self.list_notice() {
            column = column.child(
                text(t(self.lang, notice))
                    .named("Bot conversation trash notice")
                    .id(TRASH_NOTICE)
                    .role(roles::META)
                    .ink(Ink::Accent)
                    .wrap(),
            );
        }
        column
    }

    /// **How tall the shared list's rows stand**, its footer's air
    /// included: the list's own geometry over a tall probe, so the column
    /// can end the list where its rows end.
    fn rows_height(&self) -> f32 {
        let probe = Rect::new(0.0, 0.0, koru_draw::theme::PANEL_W, 10_000.0);
        let surface = &self.conversations.surface;
        let air = probe.h - surface.band(probe, 0.0).h;
        surface
            .chat_rects(probe, 0.0)
            .last()
            .map_or(0.0, |(_, row)| row.y + row.h)
            + air
    }

    fn list_button(
        &self,
        engine: &mut TextEngine,
        name: &'static str,
        key: Key,
        label: String,
        disabled: bool,
    ) -> El {
        let focused = self.focus == Some(key);
        koru_draw::el::leaf(
            name,
            super::controls::ButtonLeaf::new(
                engine,
                label.clone(),
                ActionStyle::Ghost,
                disabled,
                focused,
            ),
        )
        .semantic()
        .id(key)
        .hit()
        .focusable()
        .disabled(disabled)
        .semantics(Role::Button)
        .name_text(label)
    }

    /// Where the list draws its rows, from the tab's frame.
    pub(super) fn list_area(frame: &Frame) -> Option<Rect> {
        frame.node(CONVERSATION_LIST).map(|node| node.rect)
    }

    /// **The list's own pass**: the read it owes and the row it was asked
    /// to open. `true` when anything changed.
    pub(super) fn list_pass(&mut self) -> bool {
        // The rows are titled in the tab's words, by the Bot's name.
        let name = self
            .facts
            .as_ref()
            .map(|facts| facts.name.clone())
            .unwrap_or_default();
        let mut changed = self.conversations.set_words(self.lang, &name);
        // A Branch in the conversation moves it to the branch: the tab is on
        // the branch now, and the list reads again to show it.
        if let Some(bound) = self
            .conversation
            .as_ref()
            .and_then(|view| view.owned_conversation())
            .map(|owned| owned.chat)
            && bound != self.chat
        {
            self.chat = bound;
            self.conversations.owe_read();
            changed = true;
        }
        // **What *New conversation* made is shown**, at any width: compact,
        // the list gives the conversation its place back, as choosing a row
        // does (the third window run: the list stayed up over it).
        if let Some(made) = self.conversations.take_made() {
            changed |= self.close_compact_list();
            changed |= self.show_conversation(made);
        }
        if let Some(chat) = self.conversations.surface.take_wants_open() {
            // A row chosen from the compact list: the conversation is back.
            changed |= self.close_compact_list();
            changed |= self.show_conversation(chat);
        }
        // **A rename the owner refused says why under the list** (the
        // fourth window run: refused in silence, the reason only in a
        // status line the list stood over). The row keeps the name the
        // owner holds — it never took the new one — and the Bot is read
        // again when the refusal says its lifecycle moved, so the header
        // says so beside the sentence.
        if let Some((_, code)) = self
            .conversation
            .as_mut()
            .and_then(|view| view.take_rename_refused())
        {
            self.list_notice = Some(super::super::settings::general_refusal_words(&code));
            if matches!(
                code.as_str(),
                super::super::settings::BOT_ARCHIVED | super::super::settings::BOT_TRASHED_BOT
            ) {
                self.asked = false;
            }
            self.conversations.owe_read();
            changed = true;
        }
        // A row's ⋯ or Delete asked for one conversation's Trash (S14).
        changed |= self.take_list_asks();
        // The tab is on the Bot's main conversation and the owner's list
        // says it is in Trash (O7): another one is shown, or the tab says so.
        if self.conversations.main_missing()
            && !self.chat_in_trash
            && self.facts.as_ref().map(|facts| facts.main_chat) == Some(self.chat)
            && !self
                .conversations
                .listed()
                .iter()
                .any(|chat| chat.id == self.chat)
        {
            self.move_off(self.chat);
            changed = true;
        }
        changed |= self.conversations.surface.set_current(Some(self.chat));
        if let Some(command) = self.conversations.read() {
            if self.send_list(command) {
                changed = true;
            } else {
                self.conversations.unsent();
            }
        }
        changed
    }

    /// **The compact list leaves for the conversation** — a row chosen, or
    /// one *New conversation* made — and the keyboard goes with it to the
    /// conversation, never to a list no longer drawn (the third window run:
    /// focus reported on the bare window, and Delete reaching nothing).
    ///
    /// **Compact and wide agree on what a row's keys act on**: the row the
    /// keyboard is on in a list that is shown. Wide, a click leaves the list
    /// shown with the keyboard on that row, so Delete asks for its Trash;
    /// compact, the click is *open* and the list leaves, so the keyboard is
    /// the conversation's and Delete is the composer's. A row's Trash at
    /// either width is its ⋯ or right-click menu, or Delete on the row the
    /// list's keyboard is on. `true` when the list was open.
    fn close_compact_list(&mut self) -> bool {
        let covering = self.list_covers();
        self.list_open = false;
        if !covering {
            return false;
        }
        if self.focus == Some(CONVERSATION_LIST) {
            self.conversations.surface.clear_focus();
            self.focus = None;
        }
        true
    }

    /// Hand a list read to the link. `false` when it would not take it.
    pub(super) fn send_list(&mut self, command: crate::bots::link::Command) -> bool {
        let sent = self.link.as_ref().is_some_and(|link| link.send(command));
        if sent {
            let mut work = self.work.get();
            work.commands += 1;
            self.work.set(work);
        }
        sent
    }

    /// **Show `chat` in this tab** — a row chosen, or the conversation *New
    /// conversation* made: the owned surface is bound to it, and the tab is
    /// found by it from then on. `true` when it moved.
    pub(crate) fn show_conversation(&mut self, chat: ChatId) -> bool {
        if chat == self.chat && !self.chat_in_trash {
            return false;
        }
        self.chat_in_trash = false;
        self.chat = chat;
        if let Some(facts) = self.facts.clone() {
            self.know(facts);
        }
        self.conversations.surface.set_current(Some(chat));
        true
    }

    /// *New conversation*: the owner's `CreateChat`.
    pub(super) fn new_conversation(&mut self) -> bool {
        let Some(command) = self.conversations.create() else {
            return false;
        };
        if !self.send_list(command) {
            self.conversations.create_unsent();
        }
        true
    }

    /// *Show more*: the next page.
    pub(super) fn more_conversations(&mut self) -> bool {
        let Some(command) = self.conversations.more() else {
            return false;
        };
        if !self.send_list(command) {
            self.conversations.unsent();
        }
        true
    }

    /// **The marks the rows wear**: the owner's running and Needs-you
    /// conversations of this Bot, as the roster's activity read last said,
    /// and the open conversation's own status, which the window knows
    /// first-hand. `true` when a mark changed.
    pub(crate) fn set_activity(&mut self, activity: &HashMap<ChatId, ChatStatus>) -> bool {
        let mut statuses = activity.clone();
        if let Some(view) = self.conversation() {
            match view.status() {
                ChatStatus::Idle => {}
                status => {
                    statuses.insert(self.chat, status);
                }
            }
        }
        self.conversations.set_statuses(statuses)
    }

    /// **Whether the conversation's work moved** since the window last
    /// asked — a reply started or ended, or it stopped to ask the person —
    /// so the roster reads its activity again (S10). Unread is not work.
    pub(crate) fn take_status_moved(&mut self) -> bool {
        let now = match self.conversation().map(|view| view.status()) {
            Some(ChatStatus::Unread) | None => ChatStatus::Idle,
            Some(status) => status,
        };
        let moved = std::mem::replace(&mut self.seen_status, now) != now;
        if moved {
            // A Stop the owner sent for an Archive or a Trash lands here
            // first: the operation is read again on the next shown pass.
            self.lifecycle_read_now();
        }
        moved
    }

    /// The list's band in the tab's coordinates, and where its rows start.
    fn list_band(&self) -> Option<(Rect, f32)> {
        self.with_frame(|_, frame| Self::list_area(frame))
            .filter(|_| self.conversations.shows_rows())
            .map(|area| (area, area.y))
    }

    /// A press on the list's rows. `None` when it is not on them.
    pub(super) fn list_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let (area, top) = self.list_band()?;
        if !area.contains(x, y) {
            return None;
        }
        // **A press on a row gives the list the keyboard, on that row**, as
        // the keyboard's way in does (`F6`, then Tab): Delete then asks for
        // that conversation's Trash rather than reaching the composer (the
        // second window run). Typing still falls through to the
        // conversation: the list claims only its own keys.
        let pressed = self.conversations.surface.chat_pressed_at(area, top, x, y);
        let handled = self.conversations.surface.mouse_down(area, top, x, y);
        match pressed {
            Some(chat) => {
                self.focus = Some(CONVERSATION_LIST);
                self.conversations
                    .surface
                    .set_focus(koru_chats_ui::Focus::Chat(chat), area, top);
            }
            None => self.focus = None,
        }
        Some(handled)
    }

    /// **A right-click on a row** asks for the row's menu — the one its ⋯
    /// opens — as a right-click in the Chats view does; the window drains
    /// it ([`take_conversation_menu`](Self::take_conversation_menu)).
    /// `false` when it is not on a row, or the list offers no menu.
    pub(crate) fn list_context_menu(&mut self, x: f32, y: f32) -> bool {
        let Some((area, top)) = self.list_band() else {
            return false;
        };
        if !area.contains(x, y) {
            return false;
        }
        let Some(chats) = self.conversations.surface.context_menu(area, top, x, y) else {
            return false;
        };
        match chats.as_slice() {
            [chat] => {
                self.return_focus = self.focus;
                self.conversation_menu = Some((*chat, (x, y)));
            }
            _ => self.list_notice = Some(Words::BotsTrashOneAtATime),
        }
        true
    }

    /// The pointer over the list. `true` when its hover changed.
    pub(super) fn list_move(&mut self, x: f32, y: f32) -> bool {
        let Some((area, top)) = self.list_band() else {
            return false;
        };
        if area.contains(x, y) {
            self.conversations.surface.mouse_move(area, top, x, y)
        } else {
            self.conversations.surface.clear_hover()
        }
    }

    /// The wheel over the list. `false` when it is not over it.
    pub(super) fn list_scroll(&mut self, x: f32, y: f32, dy: f32) -> bool {
        let Some((area, top)) = self.list_band() else {
            return false;
        };
        if !area.contains(x, y) {
            return false;
        }
        self.conversations.surface.scroll_by(area, top, dy);
        true
    }

    /// **A key while the keyboard is on the list**: the Chats list's own
    /// keys — Up, Down, Home, End, Enter — and Escape gives the keyboard
    /// back to the conversation. `None` when the keyboard is not on it.
    pub(super) fn list_key(&mut self, key: &WinitKey, mods: ModifiersState) -> Option<bool> {
        if self.focus != Some(CONVERSATION_LIST) {
            return None;
        }
        let (area, top) = self.list_band()?;
        let press = Press::of_with(key, mods)?;
        if press == Press::Escape {
            self.conversations.surface.clear_focus();
            self.focus = None;
            return Some(true);
        }
        self.conversations.surface.focus_first(area, top);
        Some(self.conversations.surface.key(area, top, press))
    }

    /// The keyboard landed on the list: on the open conversation's row.
    pub(super) fn focus_list(&mut self) {
        if let Some((area, top)) = self.list_band() {
            self.conversations.surface.focus_first(area, top);
        }
    }

    /// The list, for the accessibility tree: a list named for the Bot's
    /// conversations, one item per row, named as the Chats view names its
    /// rows — title, who answered and when, and the status's words.
    pub(super) fn list_controls(&self) -> Vec<Control> {
        let Some((area, top)) = self.list_band() else {
            return Vec::new();
        };
        let surface = &self.conversations.surface;
        let focused_row = surface.focused_chat();
        let items = surface
            .chat_rects(area, top)
            .into_iter()
            .map(|(chat, rect)| Control {
                key: conversation_key(chat),
                rect,
                role: ControlRole::ListItem,
                label: self.conversation_name(chat),
                value: None,
                selected: Some(chat == self.chat),
                disabled: false,
                focused: self.focus == Some(CONVERSATION_LIST) && focused_row == Some(chat),
                children: Vec::new(),
            })
            .collect();
        vec![Control {
            key: CONVERSATION_LIST,
            rect: surface.band(area, top),
            role: ControlRole::List,
            label: t(self.lang, Words::BotsConversations).to_owned(),
            value: None,
            selected: None,
            disabled: false,
            focused: self.focus == Some(CONVERSATION_LIST) && focused_row.is_none(),
            children: items,
        }]
    }

    /// A row's accessible name: the Chats view's, and *Main conversation*
    /// for the Bot's main one.
    /// A row's accessible name: the shared list's, from the title the row
    /// shows — which marks the main conversation in words a person sees,
    /// not only in what a screen reader hears.
    pub(crate) fn conversation_name(&self, chat: ChatId) -> String {
        self.conversations
            .surface
            .accessible_name(chat)
            .unwrap_or_default()
    }

    /// **A conversation of this Bot changed in the chat** — renamed by hand
    /// or from its first message: the list reads again to show it.
    pub(crate) fn conversation_list_moved(&mut self) {
        self.conversations.owe_read();
    }

    // ------------------------------------------------- renaming a row

    /// **Why `chat` cannot be renamed now**, in the sentence the owner's
    /// refusal is said in (`BOT_REFUSALS`): an archived Bot's conversations
    /// are read-only (`bot_archived`), a conversation in Trash
    /// (`bot_conversation_trashed`) or a Bot in Trash (`bot_trashed`) is
    /// hidden, and an operation of the Bot holds it (`bot_lifecycle_pending`).
    /// `None` when a rename may be sent. A conversation the list does not
    /// hold is not this tab's to rename.
    pub(crate) fn rename_refusal(&self, chat: ChatId) -> Option<Words> {
        let facts = self.facts.as_ref()?;
        if facts.trashed {
            return Some(Words::BotsErrBotTrashed);
        }
        if facts.archived {
            return Some(Words::BotsErrArchived);
        }
        if self.chat_in_trash && chat == self.chat {
            return Some(Words::BotsErrTrashed);
        }
        if self.lifecycle.busy() {
            return Some(Words::BotsErrPending);
        }
        None
    }

    /// Whether a rename of `chat` is offered: listed, and nothing refuses it.
    pub(crate) fn rename_offered(&self, chat: ChatId) -> bool {
        self.conversations
            .listed()
            .iter()
            .any(|held| held.id == chat)
            && self.rename_refusal(chat).is_none()
    }

    /// **The row `F2` renames**, as the Chats view chooses: the row under the
    /// pointer (in the tab's coordinates), else the one the keyboard is on
    /// while the list holds it. `None` when neither is a row.
    pub(crate) fn rename_target(&mut self, pointer: Option<(f32, f32)>) -> Option<ChatId> {
        let (area, top) = self.list_band()?;
        if let Some((x, y)) = pointer
            && area.contains(x, y)
            && let Some(chat) = self.conversations.surface.chat_at(area, top, x, y)
        {
            return Some(chat);
        }
        if self.focus == Some(CONVERSATION_LIST) {
            return self.conversations.surface.focused_chat();
        }
        None
    }

    /// The name a rename field starts from: the conversation's own title,
    /// or — while it still carries a placeholder — what its row shows for
    /// another conversation, and nothing for the main one, whose row says
    /// *Main conversation* rather than a name of its own.
    pub(crate) fn rename_start(&self, chat: ChatId) -> Option<String> {
        let held = self
            .conversations
            .listed()
            .iter()
            .find(|held| held.id == chat)?;
        let main = self.facts.as_ref().map(|facts| facts.main_chat) == Some(chat);
        let shown = self.conversations.title_of(chat)?;
        let placeholder = super::conversations::is_placeholder(held, &self.label());
        Some(match (placeholder, main) {
            (true, true) => String::new(),
            (true, false) => shown,
            (false, _) => held.title.trim().to_owned(),
        })
    }

    /// **Where the window's rename field stands** over `chat`'s row: the
    /// shared list's own answer (`ChatsSurface::rename_rect`), in the tab's
    /// coordinates.
    pub(crate) fn rename_rect(&self, chat: ChatId) -> Option<Rect> {
        let (area, top) = self.list_band()?;
        self.conversations
            .surface
            .rename_rect(area, top, chat, koru_ui::text_field::PAD)
    }

    /// The row the window's field is over stands aside (koru-ai D89).
    pub(crate) fn set_renaming(&mut self, chat: Option<ChatId>) -> bool {
        self.conversations.surface.set_renaming(chat)
    }

    /// **Rename `chat`** through the conversation's own chat surface — the
    /// rename command the Chats view renames an ordinary chat through, which
    /// the owner admits for a Bot conversation — and the list reads again
    /// when it lands (`conversation_list_moved`). Refused in its sentence
    /// when something holds it; an unchanged title sends nothing.
    pub(crate) fn rename_conversation(&mut self, chat: ChatId, title: &str) -> bool {
        let title = title.trim();
        if let Some(refused) = self.rename_refusal(chat) {
            self.list_notice = Some(refused);
            return true;
        }
        // Unchanged: the title it holds, or the words its row shows (the
        // field starts from those while the title is a placeholder).
        let unchanged = self
            .conversations
            .listed()
            .iter()
            .any(|held| held.id == chat && held.title.trim() == title)
            || self.conversations.title_of(chat).as_deref() == Some(title);
        if title.is_empty() || unchanged {
            return true;
        }
        let sent = self
            .conversation
            .as_mut()
            .is_some_and(|view| view.rename_chat(chat, title));
        if sent {
            self.list_notice = None;
            #[cfg(test)]
            self.conversations.renamed.push((chat, title.to_owned()));
        } else {
            self.list_notice = Some(Words::BotsErrUnreachable);
        }
        true
    }

    /// The renames this tab handed its conversation's chat surface, in
    /// order. The surface sends each as `Command::Rename`, which its reader
    /// answers through `client.ai().rename_chat` (koru-ai-ui `reader.rs`);
    /// a test's surface has no reader, so the hand-over is what it can see.
    #[cfg(test)]
    pub(crate) fn renames_sent(&self) -> &[(ChatId, String)] {
        &self.conversations.renamed
    }

    /// **The list answered with `page`**, as the owner's `ListBotChats`
    /// answers the tab's first read (`list_bot_chats`,
    /// `apps/koru-ai-server/src/worker/bots/reads.rs`).
    #[cfg(test)]
    pub(crate) fn listed_for_test(&mut self, page: koru_ai_core::bots::BotChatPage) {
        let bot = self.bot.expect("a Bot tab that knows its Bot");
        let Some(crate::bots::link::Command::ListChats { seq, .. }) = self.conversations.read()
        else {
            panic!("the list owes its first read");
        };
        assert!(self.conversations.answer(seq, bot, Ok(page)));
    }

    /// Say why a row's rename was not started, under the list.
    pub(crate) fn say_list_notice(&mut self, words: Words) {
        self.list_notice = Some(words);
    }

    /// Every listed conversation with the title its row shows, for the
    /// roster's activity rows.
    pub(crate) fn conversation_titles(&self) -> Vec<(ChatId, String)> {
        self.conversations.titles()
    }

    /// An accessibility client's press on a row: it opens, as a click does.
    pub(super) fn list_action(&mut self, key: Key, click: bool) -> Option<bool> {
        if key.part != CONVERSATION_PART {
            return None;
        }
        let koru_draw::el::Object::Model(id) = key.object else {
            return None;
        };
        let chat = self
            .conversations
            .listed()
            .iter()
            .map(|chat| chat.id)
            .find(|chat| chat.as_uuid().as_u128() == id)?;
        if !click {
            self.focus = Some(CONVERSATION_LIST);
            if let Some((area, top)) = self.list_band() {
                self.conversations
                    .surface
                    .set_focus(koru_chats_ui::Focus::Chat(chat), area, top);
            }
            return Some(true);
        }
        self.conversations.surface.ask_open(chat);
        Some(true)
    }

    /// Paint the list's rows in their element's rect, inside a record of
    /// their own, so the inspector nests them under the column.
    pub(super) fn paint_list(
        &mut self,
        scene: &mut vello::Scene,
        root: vello::kurbo::Affine,
        area: Rect,
    ) {
        if !self.conversations.shows_rows() {
            return;
        }
        let _list = koru_draw::inspect::scope(
            "Bot conversation list",
            "container",
            root,
            koru_draw::scene::rect(area.x, area.y, area.w, area.h),
            |record| {
                record.property(
                    "surface",
                    "koru_chats_ui::ChatsSurface, fed by ListBotChats, open only",
                    "bots::view::conversations::Conversations",
                );
            },
        );
        let lang = self.lang;
        let surface = &mut self.conversations.surface;
        surface.set_lang(lang);
        // The moment the rows' times are read against, handed to the list
        // on the frames that draw it, as the Chats view's host does.
        surface.set_now(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(0),
        );
        crate::input_engine::with(|engine| surface.draw(scene, root, engine, area, area.y));
    }
}
