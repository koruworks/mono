// SPDX-License-Identifier: MPL-2.0

//! **A Bot's quick setup** (QUESTIONS O155): what the card over a Bot's
//! empty main conversation is doing, and which Bots' cards are over.
//!
//! The card is a **local presentation**. It is not a message, it is not
//! stored with the chat and nothing of it is sent to the model. What it
//! *saves* is real and goes through what already exists:
//!
//! - **Step 1** replaces the text on the Bot's Role page, an ordinary page
//!   in the Bot's home workspace, as another writer over the Store
//!   ([`RoleWrite`], carried out by the window off its thread). No Bot
//!   command exists for it and none was added.
//! - **Step 2** adds a page to the Bot's Knowledge through the Library's
//!   own draft and `SetKnowledge` (`bots::library`).
//!
//! [`Setup`] is the card's state, a plain value with no I/O. [`SetupBook`]
//! is the set of Bots whose card the person ended, **on this install**: one
//! line of the session file (`Settings::bot_setup`), read at restore and
//! written at capture, the way the chats' seen marks are
//! (`mainview/chat_status.rs`). It is not in ai.db and not on the wire: the
//! card is this install's furniture, not a fact about the Bot.

use std::sync::Mutex;

use koru_ai_core::bots::{BotId, BotPage};
use koru_i18n::Key as Words;

/// Where the card is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Step 1: the Role.
    #[default]
    Role,
    /// Step 2: Knowledge pages.
    Context,
    /// Ended here, by Done or by a skip: the card says so and offers the
    /// way back.
    Done,
}

/// The two steps, in order.
pub(crate) const STEPS: [Stage; 2] = [Stage::Role, Stage::Context];

/// How a step is drawn and named. **A tick means saved**, never merely
/// passed: a skipped step keeps its number.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Done,
    Current,
    Waiting,
}

/// Where step 1's write stands.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum RoleSave {
    #[default]
    Idle,
    /// With the window's worker: nothing else is sent until it answers.
    Saving,
    Saved,
    /// The Store did not take it. Save reads *Retry*.
    Failed,
}

/// **What the card knows of the Bot's Role page.** The card is offered
/// only for a Role page with nothing under its title: a Role a person
/// already wrote is never the card's to replace, so until a read has said
/// the page is empty the card does not show and nothing can be saved.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum RoleKnown {
    /// Not read yet.
    #[default]
    Unknown,
    /// A read is with the window's worker.
    Reading,
    /// Nothing under the title: the card may write it.
    Empty,
    /// It holds something: no card, and no write.
    Written,
    /// The read did not answer. Not treated as empty; asked again when the
    /// tab is next shown.
    Failed,
}

/// **One read of a Bot's Role page**, for the window to carry out on a
/// worker: whether anything is under the page's title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoleRead {
    pub(crate) serial: u64,
    pub(crate) bot: BotId,
    pub(crate) page: BotPage,
}

/// An example that fills step 1's text. Each is a job a Bot can be told to
/// do; none names a schedule or a trigger, which need a Routine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Example {
    Digest,
    Tasks,
    Specs,
}

impl Example {
    pub(crate) const ALL: [Example; 3] = [Example::Digest, Example::Tasks, Example::Specs];

    /// The chip's words.
    pub(crate) fn label(self) -> Words {
        match self {
            Self::Digest => Words::BotsSetupExampleDigest,
            Self::Tasks => Words::BotsSetupExampleTasks,
            Self::Specs => Words::BotsSetupExampleSpecs,
        }
    }

    /// The words it fills in.
    pub(crate) fn text(self) -> Words {
        match self {
            Self::Digest => Words::BotsSetupExampleDigestText,
            Self::Tasks => Words::BotsSetupExampleTasksText,
            Self::Specs => Words::BotsSetupExampleSpecsText,
        }
    }
}

/// **One write of a Bot's Role page**, for the window to carry out on a
/// worker: what is under the page's title is replaced by `markdown`,
/// between two History markers so the earlier text is kept.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RoleWrite {
    /// Which save this is: only the latest one's answer is taken.
    pub(crate) serial: u64,
    pub(crate) bot: BotId,
    /// The Role page, in the Bot's home workspace.
    pub(crate) page: BotPage,
    /// The page's title, unchanged by the write: the builder needs one.
    pub(crate) title: String,
    pub(crate) markdown: String,
    /// **The card has written nothing to this page yet**: the worker writes
    /// only if the page is still empty under its title, and answers
    /// `role_not_empty` otherwise. `false` once the card's own text is what
    /// the page holds.
    pub(crate) only_if_blank: bool,
    /// The History labels either side of the write, localized.
    pub(crate) before: String,
    pub(crate) after: String,
}

impl std::fmt::Debug for RoleWrite {
    /// The title and the text are content: neither is printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleWrite")
            .field("serial", &self.serial)
            .field("bot", &self.bot)
            .field("markdown_bytes", &self.markdown.len())
            .finish_non_exhaustive()
    }
}

/// What the person just did to the card's standing, for the window's book.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum News {
    /// Ended: Done, a skip, or a first message.
    Dismissed,
    /// Opened again from the finished card.
    Reopened,
}

/// **The card's state.** No I/O: a save leaves a [`RoleWrite`] for the
/// window to take, and a change of standing leaves [`News`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Setup {
    /// Whether the card was ended on this install, as the window's book
    /// says. `None` until the window has said: nothing shows before then,
    /// so a card never flashes over a Bot whose setup is over.
    told: Option<bool>,
    /// Ended in this showing of the tab: the finished card stays until the
    /// tab is made again or a message is sent.
    here: bool,
    stage: Stage,
    /// Step 1's text, as typed.
    text: String,
    save: RoleSave,
    /// The text the write on its way carries.
    sending: Option<String>,
    /// The text the Store last took.
    saved: Option<String>,
    serial: u64,
    write: Option<RoleWrite>,
    news: Option<News>,
    /// What a read said of the Role page.
    role: RoleKnown,
    read_serial: u64,
    read: Option<RoleRead>,
}

impl Setup {
    /// **The window's word on whether this Bot's card was ended.** `true`
    /// when what shows may have changed.
    pub(crate) fn tell(&mut self, dismissed: bool) -> bool {
        if self.told == Some(dismissed) {
            return false;
        }
        self.told = Some(dismissed);
        true
    }

    /// Whether the card has anything to show: not ended on this install
    /// **and the Role page read as empty**, or ended in this showing and
    /// saying so.
    pub(crate) fn active(&self) -> bool {
        (self.told == Some(false) && self.role == RoleKnown::Empty) || self.here
    }

    /// What a read said of the Role page. Test only.
    #[cfg(test)]
    pub(crate) fn role_known(&self) -> RoleKnown {
        self.role
    }

    /// **Ask whether the Role page is empty**, once: when the card is not
    /// ended on this install and nothing is known of the page yet. `true`
    /// when a read was left for the window.
    pub(crate) fn read_role(&mut self, bot: BotId, page: BotPage) -> bool {
        if self.told != Some(false) || self.role != RoleKnown::Unknown {
            return false;
        }
        self.read_serial = self.read_serial.wrapping_add(1);
        self.read = Some(RoleRead {
            serial: self.read_serial,
            bot,
            page,
        });
        self.role = RoleKnown::Reading;
        true
    }

    /// The read the card left, for the window. Taken once.
    pub(crate) fn take_read(&mut self) -> Option<RoleRead> {
        self.read.take()
    }

    /// **The window's worker answered read `serial`**: `Some(true)` for a
    /// page with nothing under its title, `Some(false)` for one that holds
    /// something, `None` when the read failed. Only the read on its way is
    /// answered.
    pub(crate) fn role_read(&mut self, serial: u64, blank: Option<bool>) -> bool {
        if self.role != RoleKnown::Reading || serial != self.read_serial {
            return false;
        }
        self.role = match blank {
            Some(true) => RoleKnown::Empty,
            Some(false) => RoleKnown::Written,
            None => RoleKnown::Failed,
        };
        true
    }

    /// A read that failed is asked again: the tab was shown again.
    pub(crate) fn retry_role_read(&mut self) -> bool {
        if self.role != RoleKnown::Failed {
            return false;
        }
        self.role = RoleKnown::Unknown;
        true
    }

    /// Whether a Role read is with the window's worker.
    pub(crate) fn reading_role(&self) -> bool {
        self.role == RoleKnown::Reading
    }

    /// **The worker found the page no longer empty** when it went to write
    /// `serial`: someone wrote a Role meanwhile. Nothing was written, and
    /// the card is over.
    pub(crate) fn found_written(&mut self, serial: u64) -> bool {
        if self.save != RoleSave::Saving || serial != self.serial {
            return false;
        }
        self.sending = None;
        self.save = RoleSave::Idle;
        self.role = RoleKnown::Written;
        self.here = false;
        true
    }

    pub(crate) fn stage(&self) -> Stage {
        self.stage
    }

    /// How `step` is drawn. `context_added` is whether the Bot has any
    /// Knowledge page.
    pub(crate) fn mark(&self, step: Stage, context_added: bool) -> Mark {
        if self.stage == step {
            return Mark::Current;
        }
        let saved = match step {
            Stage::Role => self.saved.is_some(),
            Stage::Context => context_added,
            Stage::Done => false,
        };
        if saved { Mark::Done } else { Mark::Waiting }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn role_save(&self) -> RoleSave {
        self.save
    }

    /// The person typed in step 1, or an example filled it.
    pub(crate) fn edit(&mut self, text: &str) -> bool {
        if self.text == text {
            return false;
        }
        self.text = text.to_owned();
        // What was saved is no longer what is typed.
        if self.save == RoleSave::Saved && self.saved.as_deref() != Some(text) {
            self.save = RoleSave::Idle;
        }
        true
    }

    /// Whether Save would do anything: the Role page read as empty, there
    /// are words, and no write is on its way.
    pub(crate) fn can_save(&self) -> bool {
        self.role == RoleKnown::Empty
            && !self.text.trim().is_empty()
            && self.save != RoleSave::Saving
    }

    /// **Save step 1.** The typed text becomes one [`RoleWrite`] for the
    /// window; text the Store already took is not written again, and the
    /// card moves on. `false` when there is nothing to save or a write is
    /// already on its way.
    pub(crate) fn save(
        &mut self,
        bot: BotId,
        page: BotPage,
        title: String,
        before: String,
        after: String,
    ) -> bool {
        if !self.can_save() {
            return false;
        }
        if self.saved.as_deref() == Some(self.text.as_str()) {
            self.save = RoleSave::Saved;
            self.stage = Stage::Context;
            return true;
        }
        self.serial = self.serial.wrapping_add(1);
        self.sending = Some(self.text.clone());
        self.write = Some(RoleWrite {
            serial: self.serial,
            bot,
            page,
            title,
            markdown: self.text.clone(),
            only_if_blank: self.saved.is_none(),
            before,
            after,
        });
        self.save = RoleSave::Saving;
        true
    }

    /// The write the last Save left, for the window. Taken once.
    pub(crate) fn take_write(&mut self) -> Option<RoleWrite> {
        self.write.take()
    }

    /// **The window's worker answered write `serial`.** Only the write on
    /// its way is answered: saved, the card moves to step 2 when it is
    /// still on step 1; failed, it says so and Save reads *Retry*.
    pub(crate) fn written(&mut self, serial: u64, ok: bool) -> bool {
        if self.save != RoleSave::Saving || serial != self.serial {
            return false;
        }
        let sent = self.sending.take();
        if ok {
            self.saved = sent;
            self.save = if self.saved.as_deref() == Some(self.text.as_str()) {
                RoleSave::Saved
            } else {
                RoleSave::Idle
            };
            if self.stage == Stage::Role {
                self.stage = Stage::Context;
            }
        } else {
            self.save = RoleSave::Failed;
        }
        true
    }

    /// Step 1's *Skip*: on to step 2, nothing written.
    pub(crate) fn skip_role(&mut self) -> bool {
        if self.stage != Stage::Role {
            return false;
        }
        self.stage = Stage::Context;
        true
    }

    /// **Go to `step`**: the stepper's press, and the finished card's way
    /// back in, which opens the card again on this install.
    pub(crate) fn go(&mut self, step: Stage) -> bool {
        if step == Stage::Done || (self.stage == step && !self.here) {
            return false;
        }
        self.stage = step;
        if self.here || self.told == Some(true) {
            self.here = false;
            self.told = Some(false);
            self.news = Some(News::Reopened);
        }
        true
    }

    /// **End the setup**: step 2's *Done* or *Skip*, or *Skip setup*. The
    /// card says the Bot is ready, and is over on this install.
    pub(crate) fn finish(&mut self) -> bool {
        if self.stage == Stage::Done && self.here {
            return false;
        }
        self.stage = Stage::Done;
        self.here = true;
        if self.told != Some(true) {
            self.news = Some(News::Dismissed);
        }
        self.told = Some(true);
        true
    }

    /// **A first message was sent**: the card gives way to the
    /// conversation, for good.
    pub(crate) fn sent(&mut self) -> bool {
        if !self.active() {
            return false;
        }
        if self.told != Some(true) {
            self.news = Some(News::Dismissed);
        }
        self.told = Some(true);
        self.here = false;
        true
    }

    /// What the window's book owes, taken once.
    pub(crate) fn take_news(&mut self) -> Option<News> {
        self.news.take()
    }
}

// ------------------------------------------------------------ the book

/// How many Bots the book remembers. Past it the oldest entry goes: a Bot
/// that old with a conversation still empty offers its setup once more.
pub(crate) const BOOK_LIMIT: usize = 512;

/// **The Bots whose card was ended on this install**, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SetupBook {
    done: Vec<uuid::Uuid>,
}

impl SetupBook {
    /// Read the session file's line: Bot ids separated by commas. An entry
    /// that is not an id is passed over; a damaged line costs only itself.
    pub(crate) fn parse(line: &str) -> Self {
        let mut book = Self::default();
        for entry in line.split(',') {
            if let Ok(id) = uuid::Uuid::parse_str(entry.trim()) {
                book.insert(id);
            }
        }
        book
    }

    /// The line for the session file; empty when the book is.
    pub(crate) fn to_storage(&self) -> String {
        self.done
            .iter()
            .map(|id| id.simple().to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub(crate) fn contains(&self, bot: BotId) -> bool {
        self.done.contains(&bot.as_uuid())
    }

    fn insert(&mut self, id: uuid::Uuid) -> bool {
        if self.done.contains(&id) {
            return false;
        }
        self.done.push(id);
        if self.done.len() > BOOK_LIMIT {
            let over = self.done.len() - BOOK_LIMIT;
            self.done.drain(..over);
        }
        true
    }

    /// Record that `bot`'s card was ended, or opened again. `true` when the
    /// book changed.
    pub(crate) fn note(&mut self, bot: BotId, dismissed: bool) -> bool {
        let id = bot.as_uuid();
        if dismissed {
            return self.insert(id);
        }
        let before = self.done.len();
        self.done.retain(|held| *held != id);
        self.done.len() != before
    }

    /// Fold in another book: what either holds as ended stays ended.
    pub(crate) fn merge(&mut self, other: Self) -> bool {
        let mut changed = false;
        for id in other.done {
            changed |= self.insert(id);
        }
        changed
    }
}

/// **The process's book.** One, because every window of the process shows
/// the same Bots to the same person.
static BOOK: Mutex<Option<SetupBook>> = Mutex::new(None);

fn with_book<R>(f: impl FnOnce(&mut SetupBook) -> R) -> R {
    let mut guard = BOOK.lock().unwrap_or_else(|error| error.into_inner());
    f(guard.get_or_insert_with(SetupBook::default))
}

/// Fold in the session file's line. **A merge, never a replace**: a second
/// window restoring the line the first wrote changes nothing.
pub(crate) fn restore(line: &str) {
    if line.is_empty() {
        return;
    }
    let read = SetupBook::parse(line);
    with_book(|book| book.merge(read));
}

/// The line for the session file.
pub(crate) fn capture() -> String {
    with_book(|book| book.to_storage())
}

/// Whether `bot`'s card was ended on this install.
pub(crate) fn dismissed(bot: BotId) -> bool {
    with_book(|book| book.contains(bot))
}

/// Record that `bot`'s card was ended, or opened again.
pub(crate) fn note(bot: BotId, dismissed: bool) -> bool {
    with_book(|book| book.note(bot, dismissed))
}

#[cfg(test)]
mod tests;
