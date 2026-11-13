// SPDX-License-Identifier: MPL-2.0

//! **A Bot tab's lifecycle** (Bots P1.8 S14): the header's ⋯ — Archive and
//! Move to Trash, or Unarchive — and a conversation row's Move to Trash,
//! each confirmed by the window before anything is sent; the operation's
//! progress, a parked operation's Continue and Cancel, and what the tab says
//! when it is done.
//!
//! ```text
//! Bot header (col)
//! ├─ Bot title (row)        robot · name · Paused · Archived · [⋯] [Settings]
//! ├─ …                      main conversation, destination, its limit
//! └─ Bot lifecycle (col)    LIFECYCLE: one sentence, and the actions it offers
//!    ├─ Bot lifecycle notice        LIFECYCLE_NOTICE
//!    └─ Bot lifecycle actions (row) [Continue] [Cancel] · [Unarchive] · [Restore]
//! ```
//!
//! **Everything said comes from the owner** ([`crate::bots::lifecycle`]):
//! *Finishing the bot's current work…* while the owner holds an Archive or
//! a Trash as moving, the parked sentence with its two answers once it is
//! parked, and what is true once it is done — archived, in Trash with what
//! stayed where it was, restored and paused. A Restore or an Unarchive is
//! offered when the owner's read names the operation that holds the Bot
//! (`Bot.held_by`) — after a restart too — or the tab just finished it;
//! otherwise the tab says it cannot be undone from here yet.
//!
//! **The menus and the dialogs are the window's** (the shared `koru_ui`
//! menu and confirmation): the tab hands its asks up
//! ([`BotView::take_actions_request`], [`BotView::take_conversation_menu`],
//! [`BotView::take_conversation_trash`]) and the window calls back on the
//! person's answer, then gives the keyboard back to the control that asked
//! ([`BotView::dialog_closed`]).

use koru_ai_core::ChatId;
use koru_ai_core::bots::{Bot, BotLifecycle};
use koru_draw::el::{El, Key, Look, Ring, Role, StateRecipe, col, leaf, row, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Line, Space, roles};
use koru_i18n::{Key as Words, t};
use koru_ui::button::ActionStyle;

use super::controls::IconLeaf;
use super::{BotView, SURFACE};
use crate::bots::lifecycle::{Ask, Finished, Outcome, Stage, parked_words};
use crate::bots::link::{Command, Failure};
use crate::bots::stops::WorkNow;

/// The header's ⋯.
pub(crate) const ACTIONS_BUTTON: Key = Key::surface(SURFACE, "bot-actions");
/// The lifecycle's place in the header.
pub(crate) const LIFECYCLE: Key = Key::surface(SURFACE, "lifecycle");
/// Its sentence.
pub(crate) const LIFECYCLE_NOTICE: Key = Key::surface(SURFACE, "lifecycle-notice");
/// A parked operation's *Continue* and *Cancel*.
pub(crate) const CONTINUE: Key = Key::surface(SURFACE, "lifecycle-continue");
pub(crate) const CANCEL_OPERATION: Key = Key::surface(SURFACE, "lifecycle-cancel");
/// An archived Bot's *Unarchive*, and a trashed Bot's *Restore*.
pub(crate) const UNARCHIVE: Key = Key::surface(SURFACE, "unarchive");
pub(crate) const RESTORE: Key = Key::surface(SURFACE, "restore");

/// The ⋯'s treatments: the shared icon button lights itself; the element
/// draws the keyboard's ring.
pub(super) static ICON_STATES: StateRecipe = StateRecipe {
    name: "bot actions",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::NONE,
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// What the lifecycle's place says, and how it says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    pub words: Words,
    /// A refusal, in the accent ink; everything else is a fact, in the
    /// secondary.
    pub refused: bool,
}

impl BotView {
    /// The Bot as the owner last held it, for a lifecycle command's
    /// revision: the trashed one as its Trash filter lists it.
    fn lifecycle_bot(&self) -> Option<Bot> {
        if self.facts.as_ref().is_some_and(|facts| facts.trashed) {
            return self.trashed_bot.as_deref().cloned();
        }
        self.settings.bot().cloned()
    }

    /// **The rows the header's ⋯ offers**, by the Bot's state: Archive and
    /// Move to Trash for an active Bot; Unarchive — when the owner names the
    /// Archive — and Move to Trash for an archived one; nothing while an
    /// operation is on its way, or for a trashed Bot.
    pub(crate) fn lifecycle_rows(&self) -> Vec<Ask> {
        let Some(facts) = &self.facts else {
            return Vec::new();
        };
        if facts.trashed
            || self.lifecycle.busy()
            || self.lifecycle_awaits_read
            || self.settings.bot().is_none()
        {
            return Vec::new();
        }
        if facts.archived {
            let mut rows = Vec::new();
            if self.lifecycle.archive_source().is_some() {
                rows.push(Ask::Unarchive);
            }
            rows.push(Ask::Trash);
            return rows;
        }
        vec![Ask::Archive, Ask::Trash]
    }

    /// Whether the header shows its ⋯: once there is a row to offer.
    fn shows_actions(&self) -> bool {
        !self.lifecycle_rows().is_empty()
    }

    /// The ⋯ as an element.
    pub(super) fn actions_button(&self) -> El {
        leaf(
            "Bot actions",
            IconLeaf {
                glyph: koru_draw::icons::DOTS_THREE,
                disabled: false,
            },
        )
        .semantic()
        .id(ACTIONS_BUTTON)
        .hit()
        .focusable()
        .states(&ICON_STATES)
        .semantics(Role::Button)
        .name(Words::BotsActions)
    }

    /// **The confirmation a whole-Bot ask needs**, in the person's words:
    /// its title, what will happen, and the answering button. `None` for an
    /// ask that needs none (Unarchive and Restore put things back).
    ///
    /// **What it says of running work is what the window knows** (the
    /// fourth window run, `bots::stops`): that it is stopped when work runs
    /// or waits — the question then leads with it ([`Self::stops_words`]);
    /// nothing of it when the window knows nothing runs; and, when it does
    /// not know, a sentence true either way: anything still running is
    /// stopped.
    pub(crate) fn confirm_words(
        &self,
        ask: Ask,
        work: WorkNow,
    ) -> Option<(String, String, String)> {
        let name = self.label();
        let (title, body, confirm) = match (ask, work) {
            (Ask::Archive, WorkNow::Running | WorkNow::NeedsYou) => (
                Words::BotsArchiveTitle,
                Words::BotsArchiveBody,
                Words::BotsArchiveConfirm,
            ),
            (Ask::Archive, WorkNow::Idle) => (
                Words::BotsArchiveTitle,
                Words::BotsArchiveBodyIdle,
                Words::BotsArchiveConfirm,
            ),
            (Ask::Archive, WorkNow::Unknown) => (
                Words::BotsArchiveTitle,
                Words::BotsArchiveBodyUnknown,
                Words::BotsArchiveConfirm,
            ),
            (Ask::Trash, WorkNow::Running | WorkNow::NeedsYou) => (
                Words::BotsTrashTitle,
                Words::BotsTrashBody,
                Words::BotsTrashConfirm,
            ),
            (Ask::Trash, WorkNow::Idle) => (
                Words::BotsTrashTitle,
                Words::BotsTrashBodyIdle,
                Words::BotsTrashConfirm,
            ),
            (Ask::Trash, WorkNow::Unknown) => (
                Words::BotsTrashTitle,
                Words::BotsTrashBodyUnknown,
                Words::BotsTrashConfirm,
            ),
            (Ask::Unarchive | Ask::Restore, _) => return None,
        };
        Some((
            koru_i18n::fill(t(self.lang, title), &[name.as_str()]),
            t(self.lang, body).to_owned(),
            t(self.lang, confirm).to_owned(),
        ))
    }

    /// **What this tab knows itself of work running now**: the
    /// conversation it shows, that conversation's status as the window's
    /// pass last set it from the chat surface — evidence that a reply runs
    /// or waits on the person — and whether the surface has read that
    /// conversation and knows nothing runs in it (`bots::stops`). `None`
    /// while the conversation is let go.
    pub(crate) fn own_work(&self) -> Option<crate::bots::stops::Own> {
        self.conversation().map(|view| crate::bots::stops::Own {
            chat: self.chat,
            status: view.status(),
            known_idle: view.chat_id() == Some(self.chat) && view.knows_nothing_runs(),
        })
    }

    /// **The first paragraph of an Archive or Trash question when it will
    /// stop a reply**: running, or waiting on the person, in the Bot
    /// (`conversation` `None`) or in one conversation. `None` when the
    /// window knows nothing runs, or does not know (the question then
    /// defaults to Cancel and says what it always says).
    pub(crate) fn stops_words(&self, work: WorkNow, conversation: bool) -> Option<String> {
        let words = match (work, conversation) {
            (WorkNow::Running, false) => Words::BotsStopsRunning,
            (WorkNow::NeedsYou, false) => Words::BotsStopsWaiting,
            (WorkNow::Running, true) => Words::BotsConversationStopsRunning,
            (WorkNow::NeedsYou, true) => Words::BotsConversationStopsWaiting,
            (WorkNow::Idle | WorkNow::Unknown, _) => return None,
        };
        Some(t(self.lang, words).to_owned())
    }

    /// **Begin `ask`** — the person confirmed it, or it needs no question.
    /// Sent at the revision the tab read; shown as moving until the owner
    /// answers. `false` when nothing was sent.
    pub(crate) fn begin_lifecycle(&mut self, ask: Ask) -> bool {
        let Some(bot) = self.lifecycle_bot() else {
            return false;
        };
        match self.lifecycle.begin(ask, &bot) {
            Some(command) => {
                self.send_write(command);
                true
            }
            None => false,
        }
    }

    /// *Continue* on a parked operation.
    pub(super) fn continue_lifecycle(&mut self) -> bool {
        let Some(bot) = self.lifecycle_bot() else {
            return false;
        };
        match self.lifecycle.continue_parked(&bot) {
            Some(command) => {
                self.send_write(command);
                true
            }
            None => false,
        }
    }

    /// *Cancel* on a parked operation: the Restore naming it.
    pub(super) fn cancel_lifecycle(&mut self) -> bool {
        let Some(bot) = self.lifecycle_bot() else {
            return false;
        };
        match self.lifecycle.cancel_parked(&bot) {
            Some(command) => {
                self.send_write(command);
                true
            }
            None => false,
        }
    }

    /// Whether an operation of the Bot is moving or parked: a second one
    /// waits until it is done (the owner holds one at a time).
    pub(crate) fn lifecycle_busy(&self) -> bool {
        self.lifecycle.busy()
    }

    // ------------------------------------------------- one conversation

    /// **One conversation's Trash confirmation**, in the person's words:
    /// the main conversation's says the Bot keeps working without it.
    ///
    /// **What it says of running work is what the window knows**, from
    /// the facts the question's default answer is chosen by (`bots::stops`):
    /// that it is stopped when work runs or waits; nothing of it when the
    /// window knows nothing runs (the third window run); and a sentence true
    /// either way when it does not know (the fourth).
    pub(crate) fn conversation_trash_words(
        &self,
        chat: ChatId,
        work: WorkNow,
    ) -> Option<(String, String, String)> {
        let facts = self.facts.as_ref()?;
        let title = self.conversation_title(chat);
        let body = match (chat == facts.main_chat, work) {
            (true, WorkNow::Running | WorkNow::NeedsYou) => Words::BotsTrashMainBody,
            (true, WorkNow::Idle) => Words::BotsTrashMainBodyIdle,
            (true, WorkNow::Unknown) => Words::BotsTrashMainBodyUnknown,
            (false, WorkNow::Running | WorkNow::NeedsYou) => Words::BotsTrashConversationBody,
            (false, WorkNow::Idle) => Words::BotsTrashConversationBodyIdle,
            (false, WorkNow::Unknown) => Words::BotsTrashConversationBodyUnknown,
        };
        Some((
            // **Named as a conversation of its Bot**, so it cannot be read
            // as trashing the Bot (the second window run: both questions
            // read *Move “Scout Smoke” to Trash?*).
            koru_i18n::fill(
                t(self.lang, Words::BotsTrashConversationTitle),
                &[&title, &self.label()],
            ),
            t(self.lang, body).to_owned(),
            t(self.lang, Words::BotsTrashConfirm).to_owned(),
        ))
    }

    /// A listed conversation's title as its row shows it
    /// (`conversations::shown_title`): the same words in the list, the
    /// roster and the Trash question.
    fn conversation_title(&self, chat: ChatId) -> String {
        self.conversations
            .title_of(chat)
            .unwrap_or_else(|| t(self.lang, Words::BotsMainConversation).to_owned())
    }

    /// **Move `chat` to Trash** — the person confirmed it. `false` when
    /// nothing was sent: another operation is on its way, which the list
    /// says.
    pub(crate) fn trash_conversation(&mut self, chat: ChatId) -> bool {
        let Some(bot) = self.settings.bot().cloned() else {
            return false;
        };
        let title = self.conversation_title(chat);
        match self.lifecycle.begin_chat_trash(&bot, chat, &title) {
            Some(command) => {
                self.list_notice = None;
                self.send_write(command);
                true
            }
            None => {
                self.list_notice = Some(Words::BotsErrPending);
                true
            }
        }
    }

    /// The list's asks, from the shared list: a row's ⋯ (or right-click,
    /// or menu key) and its Delete. One conversation at a time, and none
    /// while another operation is on its way — said, never queued.
    pub(super) fn take_list_asks(&mut self) -> bool {
        let mut changed = false;
        if let Some((chats, at)) = self.conversations.surface.take_wants_menu() {
            changed = true;
            match chats.as_slice() {
                [chat] => {
                    self.return_focus = self.focus;
                    self.conversation_menu = Some((*chat, at));
                }
                _ => self.list_notice = Some(Words::BotsTrashOneAtATime),
            }
        }
        let wanted = self.conversations.surface.take_wants_delete();
        if !wanted.is_empty() {
            changed = true;
            match wanted.as_slice() {
                _ if self.lifecycle.busy() => self.list_notice = Some(Words::BotsErrPending),
                [chat] => {
                    self.return_focus = self.focus;
                    self.conversation_trash = Some(*chat);
                }
                _ => self.list_notice = Some(Words::BotsTrashOneAtATime),
            }
        }
        changed
    }

    /// Why the list did not act on a row's Trash, when it said so.
    pub(crate) fn list_notice(&self) -> Option<Words> {
        self.list_notice
    }

    // ------------------------------------------------ the window's asks

    /// The ⋯ was pressed: where its menu opens, in the tab's coordinates.
    /// Taken once.
    pub(crate) fn take_actions_request(&mut self) -> Option<(f32, f32)> {
        self.actions_request.take()
    }

    /// A conversation row's menu was asked for: the conversation and where
    /// it opens, in the tab's coordinates. Taken once.
    pub(crate) fn take_conversation_menu(&mut self) -> Option<(ChatId, (f32, f32))> {
        self.conversation_menu.take()
    }

    /// A conversation row's Delete: the conversation to ask about. Taken
    /// once.
    pub(crate) fn take_conversation_trash(&mut self) -> Option<ChatId> {
        self.conversation_trash.take()
    }

    /// **The window's menu or dialog that a control of this tab opened has
    /// closed**: the keyboard goes back to that control when the keyboard
    /// opened it, and stays where it was otherwise.
    pub(crate) fn dialog_closed(&mut self) {
        if let Some(key) = self.return_focus.take() {
            let present = self.with_frame(|_, frame| frame.contains(key));
            self.focus = if present {
                Some(key)
            } else {
                self.focus_order().first().copied()
            };
        }
    }

    /// The operations finished since the window last asked: the Bot's
    /// other tabs, the roster and the Trash read again.
    pub(crate) fn take_lifecycle_finished(&mut self) -> Vec<Finished> {
        self.lifecycle.take_finished()
    }

    /// **Another surface of the window restored or unarchived this Bot, or
    /// one of its conversations**: what the tab shows is old, and it reads
    /// the Bot and its list again when it is next in front.
    pub(crate) fn lifecycle_moved_elsewhere(&mut self) {
        self.lifecycle.moved_elsewhere();
        self.conversations.owe_read();
        if self.facts.as_ref().is_some_and(|facts| facts.trashed) {
            self.trash_read_owed = true;
        }
    }

    // ----------------------------------------------------- the answers

    /// **Act on what an answer just settled**, once: a Bot that went to
    /// Trash is shown there; one that came back is read; a conversation
    /// that went leaves the list, and the tab moves off it.
    pub(super) fn after_lifecycle(&mut self) {
        let Some(outcome) = self.lifecycle.take_just() else {
            return;
        };
        match outcome {
            Outcome::Trashed => {
                if let Some(facts) = &mut self.facts {
                    facts.trashed = true;
                }
                if self.settings_open {
                    self.set_settings_open(false);
                }
                // Its revision is the Trash filter's to say.
                self.trash_read_owed = true;
                self.conversations.offer_trash(false);
                // Its conversation is refused now: let go of it.
                self.release_conversation();
                self.keep_keyboard_on(RESTORE);
            }
            // **Archived**: offered as the owner holds it from now —
            // Unarchive and Move to Trash, and no conversation's Trash,
            // which the owner refuses for an archived Bot.
            Outcome::Archived => {
                if let Some(facts) = &mut self.facts {
                    facts.archived = true;
                }
                self.conversations.offer_trash(false);
                self.conversations.owe_read();
                self.keep_keyboard_on(ACTIONS_BUTTON);
            }
            // **Unarchived**: active again, and paused.
            Outcome::Unarchived => {
                if let Some(facts) = &mut self.facts {
                    facts.trashed = false;
                    facts.archived = false;
                }
                self.trashed_bot = None;
                self.conversations.offer_trash(true);
                self.conversations.owe_read();
                self.keep_keyboard_on(ACTIONS_BUTTON);
            }
            // **Restored**: back as it was before its Trash, active or
            // archived — archived, trashed, restored comes back archived —
            // which only the Bot's read says. Out of the Trash now; nothing
            // else is offered until that read answers.
            Outcome::Restored => {
                if let Some(facts) = &mut self.facts {
                    facts.trashed = false;
                }
                self.trashed_bot = None;
                self.lifecycle_awaits_read = true;
                self.conversations.offer_trash(false);
                self.conversations.owe_read();
                self.keep_keyboard_on(ACTIONS_BUTTON);
            }
            Outcome::ChatTrashed { chat } => {
                self.conversations.remove(chat);
                if chat == self.chat {
                    self.move_off(chat);
                }
            }
            Outcome::ChatKept { .. } => self.conversations.owe_read(),
            Outcome::Refused(_) => {}
        }
    }

    /// The keyboard was on a control the outcome removed: it goes to `key`
    /// when the tab draws it, else to the first control there is.
    fn keep_keyboard_on(&mut self, key: Key) {
        if self.focus.is_none() {
            return;
        }
        let present = self.with_frame(|_, frame| frame.contains(key));
        self.focus = if present {
            Some(key)
        } else {
            self.focus_order().first().copied()
        };
    }

    /// **The conversation the tab showed is in Trash**: the main one, when
    /// that is another and listed; else the first other one listed; else the
    /// tab says it is in Trash and offers *New conversation*.
    pub(super) fn move_off(&mut self, gone: ChatId) {
        let main = self.facts.as_ref().map(|facts| facts.main_chat);
        let listed = self.conversations.listed();
        let next = main
            .filter(|main| *main != gone && listed.iter().any(|chat| chat.id == *main))
            .or_else(|| listed.iter().map(|chat| chat.id).find(|id| *id != gone));
        match next {
            Some(next) => {
                self.show_conversation(next);
            }
            None => self.chat_in_trash = true,
        }
    }

    /// The owner's Trash filter answered: the tab's Bot is in it — with the
    /// revision a Restore carries — or it is not, and its read's *not
    /// found* means it is gone.
    pub(super) fn take_trash_listing(&mut self, result: Result<Vec<Bot>, Failure>) {
        let Ok(bots) = result else {
            // Not answered: the tab keeps what it said, and asks when shown.
            return;
        };
        match bots.into_iter().find(|bot| Some(bot.id) == self.bot) {
            Some(bot) => {
                self.lifecycle.know(&bot);
                self.after_lifecycle();
                if let Some(facts) = &mut self.facts {
                    facts.trashed = true;
                    facts.name = bot.config.name.clone();
                    facts.paused = bot.paused;
                }
                self.conversations.offer_trash(false);
                // Trashed, perhaps elsewhere: its conversation is refused.
                self.release_conversation();
                self.trashed_bot = Some(Box::new(bot));
                self.load = super::Load::Ready;
            }
            None if self.facts.as_ref().is_some_and(|facts| facts.trashed) => {
                // Restored elsewhere meanwhile: read it.
                self.trashed_bot = None;
                if let Some(facts) = &mut self.facts {
                    facts.trashed = false;
                }
                self.asked = false;
            }
            None => self.load = super::Load::Unavailable,
        }
    }

    /// The Trash filter read the tab owes, if any: once, while none is on
    /// its way, for the Bot's home.
    pub(super) fn trash_read(&mut self) -> Option<Command> {
        if !self.trash_read_owed || self.trash_in_flight {
            return None;
        }
        let home = self.facts.as_ref()?.home;
        self.trash_read_owed = false;
        self.trash_in_flight = true;
        self.trash_seq = self.trash_seq.wrapping_add(1);
        Some(Command::Lifecycle {
            seq: self.trash_seq,
            workspace: home,
            lifecycle: BotLifecycle::Trashed,
        })
    }

    /// The link would not take the Trash filter read: owed again.
    pub(super) fn trash_read_unsent(&mut self) {
        self.trash_in_flight = false;
        self.trash_read_owed = true;
    }

    /// A Trash filter read on its way: the one reason the tab keeps a short
    /// wake for it.
    pub(super) fn trash_awaiting(&self) -> bool {
        self.trash_in_flight
    }

    /// When the lifecycle next needs a pass.
    pub(super) fn lifecycle_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.lifecycle.deadline(now)
    }

    /// **The lifecycle's own pass** — from a shown tab only: a read of the
    /// operation when one is owed, the Trash filter when it is, and a read
    /// of the Bot after an operation finished or was refused.
    pub(super) fn lifecycle_pass(&mut self, now: std::time::Instant) -> bool {
        let mut changed = false;
        if self.lifecycle.take_read_bot_owed() {
            self.asked = false;
            changed = true;
        }
        if let Some(command) = self.lifecycle.read(now) {
            self.send_write(command);
            changed = true;
        }
        if let Some(command) = self.trash_read() {
            if self.send_list(command) {
                changed = true;
            } else {
                self.trash_read_unsent();
            }
        }
        changed
    }

    /// The tab was shown, or its conversation's work moved: read the
    /// operation now.
    pub(super) fn lifecycle_read_now(&mut self) {
        self.lifecycle.read_now(std::time::Instant::now());
    }

    // ------------------------------------------------------- the frame

    /// **What the lifecycle's place says now**, if anything — the operation
    /// on its way, what holds the Bot, or what an operation just did.
    pub(crate) fn lifecycle_said(&self) -> Option<Said> {
        let facts = self.facts.as_ref()?;
        let fact = |words| {
            Some(Said {
                words,
                refused: false,
            })
        };
        if let Some(op) = self.lifecycle.op() {
            return match op.stage {
                Stage::Parked => fact(parked_words(op.kind, op.refusal)),
                // Koru AI did not answer the last ask: said, rather than
                // that the Bot's work is finishing. Still asked on the clock.
                Stage::Moving if op.unanswered => fact(Words::BotsLifecycleUnanswered),
                Stage::Moving => fact(match op.kind {
                    koru_ai_core::bots::BotLifecycleKind::Restore
                    | koru_ai_core::bots::BotLifecycleKind::RestoreChat => Words::BotsRestoring,
                    koru_ai_core::bots::BotLifecycleKind::TrashChat => {
                        Words::BotsTrashingConversation
                    }
                    _ => Words::BotsSettling,
                }),
            };
        }
        if let Some(Outcome::Refused(words)) = self.lifecycle.outcome() {
            // A change still being delivered held it (`bot_operation_pending`):
            // which change, when the Bot's read names it.
            let words = match *words {
                Words::BotsErrOperationPending => self
                    .settings
                    .delivering_words()
                    .unwrap_or(Words::BotsErrOperationPending),
                words => words,
            };
            return Some(Said {
                words,
                refused: true,
            });
        }
        if facts.trashed {
            return fact(Words::BotsInTrash);
        }
        match self.lifecycle.outcome() {
            Some(Outcome::Restored) => return fact(Words::BotsRestoredPaused),
            Some(Outcome::Unarchived) => return fact(Words::BotsUnarchivedPaused),
            _ => {}
        }
        if self.chat_in_trash {
            return fact(Words::BotsConversationInTrash);
        }
        if self.conversations.main_missing() {
            return fact(Words::BotsMainInTrash);
        }
        if facts.archived && self.lifecycle.archive_source().is_none() {
            return fact(Words::BotsUndoUnavailable);
        }
        None
    }

    /// The buttons the lifecycle's place offers now.
    fn lifecycle_buttons(&self) -> Vec<(Key, Words)> {
        let Some(facts) = &self.facts else {
            return Vec::new();
        };
        if let Some(op) = self.lifecycle.op() {
            return match op.stage {
                Stage::Parked => vec![
                    (CONTINUE, Words::BotsContinue),
                    (CANCEL_OPERATION, Words::BotsCancelOperation),
                ],
                Stage::Moving => Vec::new(),
            };
        }
        if facts.trashed {
            if self.lifecycle.trash_source().is_some() && self.trashed_bot.is_some() {
                return vec![(RESTORE, Words::BotsRestore)];
            }
            return Vec::new();
        }
        if facts.archived && self.lifecycle.archive_source().is_some() {
            return vec![(UNARCHIVE, Words::BotsUnarchive)];
        }
        Vec::new()
    }

    /// **The lifecycle's place as elements**: its sentence, then its
    /// buttons. Absent when it has nothing to say or offer.
    pub(super) fn lifecycle_block(&self, engine: &mut TextEngine) -> Option<El> {
        let said = self.lifecycle_said();
        let buttons = self.lifecycle_buttons();
        // A trashed Bot whose Trash the window does not know says that too.
        let unknown = self.facts.as_ref().is_some_and(|facts| facts.trashed)
            && self.lifecycle.op().is_none()
            && buttons.is_empty();
        if said.is_none() && buttons.is_empty() {
            return None;
        }
        let mut block = col("Bot lifecycle").id(LIFECYCLE).gap(Space::S2);
        if let Some(said) = &said {
            block = block.child(
                text(t(self.lang, said.words))
                    .named("Bot lifecycle notice")
                    .id(LIFECYCLE_NOTICE)
                    .role(roles::META)
                    .ink(if said.refused {
                        Ink::Accent
                    } else {
                        Ink::Secondary
                    })
                    .wrap(),
            );
        }
        if unknown {
            block = block.child(
                text(t(self.lang, Words::BotsUndoUnavailable))
                    .named("Bot lifecycle unavailable")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        if !buttons.is_empty() {
            let mut actions = row("Bot lifecycle actions").gap(Space::S2).items_center();
            for (key, words) in buttons {
                let style = if key == CANCEL_OPERATION {
                    ActionStyle::Secondary
                } else {
                    ActionStyle::Primary
                };
                actions = actions.child(self.lifecycle_button(engine, key, words, style));
            }
            block = block.child(actions);
        }
        Some(block)
    }

    fn lifecycle_button(
        &self,
        engine: &mut TextEngine,
        key: Key,
        words: Words,
        style: ActionStyle,
    ) -> El {
        let label = t(self.lang, words).to_owned();
        let focused = self.focus == Some(key);
        leaf(
            "Bot lifecycle action",
            super::controls::ButtonLeaf::new(engine, label.clone(), style, false, focused),
        )
        .semantic()
        .id(key)
        .hit()
        .focusable()
        .semantics(Role::Button)
        .name_text(label)
    }

    /// The header's lifecycle keys an accessibility client may act on.
    pub(super) fn is_lifecycle_control(key: Key) -> bool {
        [
            ACTIONS_BUTTON,
            CONTINUE,
            CANCEL_OPERATION,
            UNARCHIVE,
            RESTORE,
        ]
        .contains(&key)
    }

    /// The ⋯'s anchor, in the tab's coordinates: under its right edge.
    pub(super) fn actions_anchor(&self) -> Option<(f32, f32)> {
        self.with_frame(|_, frame| {
            frame
                .node(ACTIONS_BUTTON)
                .map(|node| node.rect)
                .map(|rect: Rect| (rect.x + rect.w, rect.y + rect.h))
        })
    }

    /// Whether the conversation is shown: never for a trashed Bot, and not
    /// once the one the tab showed went to Trash with nothing to take its
    /// place.
    pub(crate) fn shows_conversation(&self) -> bool {
        !self.chat_in_trash && !self.facts.as_ref().is_some_and(|facts| facts.trashed)
    }

    /// Whether the header shows its ⋯, for the frame.
    pub(super) fn header_shows_actions(&self) -> bool {
        self.shows_actions()
    }

    /// Fold what the lifecycle shows into the accessibility fingerprint.
    pub(super) fn revise_lifecycle(&self, rev: &mut koru_workspace_host::accessibility::Revision) {
        rev.bool(self.shows_actions())
            .bool(self.chat_in_trash)
            .bool(self.trashed_bot.is_some())
            .bool(self.lifecycle.archive_source().is_some())
            .bool(self.lifecycle.trash_source().is_some());
        match self.lifecycle_said() {
            Some(said) => {
                rev.byte(1).u64(said.words as u64);
            }
            None => {
                rev.byte(0);
            }
        }
        rev.u64(self.list_notice.map_or(u64::MAX, |words| words as u64));
    }
}
