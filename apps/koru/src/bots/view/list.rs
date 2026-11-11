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

/// A conversation row's key, as the accessibility tree names it.
pub(crate) fn conversation_key(chat: ChatId) -> Key {
    Key::model(chat.as_uuid().as_u128(), CONVERSATION_PART)
}

impl BotView {
    /// Whether the conversation column is part of the tab: once the tab
    /// knows its Bot, with the settings closed.
    pub(super) fn shows_column(&self) -> bool {
        self.facts.is_some() && !self.settings_open && !self.compact()
    }

    /// **Narrower than a page's compact width**, the conversation column
    /// leaves and the conversation takes the tab — the rule the app's other
    /// two-column pages follow (`koru_ui::PageShell::compact`: Settings and
    /// Manage drop their nav column below `COMPACT_BELOW`). The column kept
    /// its 258 px at any width, and at 720 × 600 the chat was left about
    /// 120 px: a word per line, its composer clipped (the first window run's
    /// finding 9).
    pub(super) fn compact(&self) -> bool {
        self.size.0 < koru_ui::PageShell::COMPACT_BELOW
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
        let mut column = col("Bot conversations")
            .id(CONVERSATIONS)
            .w(metrics::PANEL_W)
            .pad_x(alias::NAV_PAD)
            .pad_top(Space::S2)
            .gap(Space::S1)
            .semantics(Role::Group)
            .name_text(koru_i18n::fill(
                t(self.lang, Words::BotsConversationsA11y),
                &[self.label().as_str()],
            ))
            .child(heading);
        if let Some(notice) = list.notice() {
            column = column.child(
                text(t(self.lang, notice))
                    .named("Bot conversations notice")
                    .id(CONVERSATIONS_NOTICE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        let mut area = boxed("Bot conversation list").id(CONVERSATION_LIST).fill();
        if list.shows_rows() {
            area = area.hit().focusable();
        }
        column = column.child(area);
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
        let mut changed = false;
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
        if let Some(made) = self.conversations.take_made() {
            changed |= self.show_conversation(made);
        }
        if let Some(chat) = self.conversations.surface.take_wants_open() {
            changed |= self.show_conversation(chat);
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
        self.focus = None;
        Some(self.conversations.surface.mouse_down(area, top, x, y))
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
    pub(crate) fn conversation_name(&self, chat: ChatId) -> String {
        let name = self
            .conversations
            .surface
            .accessible_name(chat)
            .unwrap_or_default();
        if self.conversations.main() == Some(chat) {
            format!("{name}, {}", t(self.lang, Words::BotsMainConversation))
        } else {
            name
        }
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
