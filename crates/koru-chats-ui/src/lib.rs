// SPDX-License-Identifier: MPL-2.0

//! **The Chats surface** — every AI chat, newest first, with who answered it.
//!
//! A chat is kept (AGENTUX §1): it has a home of its own, revealed the way a
//! document is. This crate is that home, and it is **one component with two
//! hosts** (I5):
//!
//! - the workspace window's navigator, as the *Chats* view right of Tasks
//!   (`Alt+4`), where a row opens the chat as a tab;
//! - the standalone chat window, as the floating pane its *Chats* button
//!   opens, where a row reads the chat in place.
//!
//! Both hosts get the same rows, the same keys and the same accessibility
//! names, because both draw *this* — the old popover inside the chat is gone
//! (R1), and a second list would be a second answer to drift.
//!
//! ## What a row says
//!
//! The chat's **title**, **who answered it** — the agent by the name a person
//! knows it by, the model for OpenAI and Ollama, the hosted model's label for
//! Koru — and **when**, as a relative time in the person's language. A private
//! chat wears the detective mark the chat's own header wears.
//!
//! ## Three rules it is built under
//!
//! **No work while hidden, no polling.** The host calls [`refresh`] when the
//! surface is shown and whenever a chat surface tells it a chat was created,
//! renamed or deleted; the answer is folded in by [`poll`] on the host's wake
//! path. Nothing here runs on a timer.
//!
//! **Nothing here touches the network on the window thread.** Every request is
//! the [`reader`]'s, on its own threads; the window thread only folds what came
//! back. Under `cfg(test)` and with the `no-reader` feature there is no reader
//! at all, so no test can dial a developer's own gateway.
//!
//! **Every word goes through `koru-i18n`**, and every time through its
//! formatter — see [`text`], the only module that spells a sentence.
//!
//! ## What it is not
//!
//! It does not open a chat, start one or ask whether to delete one. Those are
//! the host's: a row that is chosen leaves a request behind
//! ([`take_wants_open`](ChatsSurface::take_wants_open),
//! [`take_wants_new`](ChatsSurface::take_wants_new),
//! [`take_wants_delete`](ChatsSurface::take_wants_delete)) and the host answers
//! it in its own idiom — a tab in one, the same window in the other, and each
//! host's own confirmation. Deleting, once confirmed, is this crate's to do
//! ([`delete`](ChatsSurface::delete)) so a refusal lands on the row it is about.
//!
//! **Renaming is the host's too.** Each host draws its own field over the
//! row's title ([`title_rect`](ChatsSurface::title_rect)) and renames through
//! the chat surface's rename command, the one the chat's title row used — so
//! a chat open in a tab or in the window takes its new name with the list.
//!
//! [`refresh`]: ChatsSurface::refresh
//! [`poll`]: ChatsSurface::poll

use std::collections::HashMap;
use std::path::PathBuf;

use koru_ai_core::Chat;
use koru_ai_wire::{AgentProviderView, Failure};
use koru_i18n::Lang;

mod draw;
mod gateway;
mod input;
mod reader;
mod rows;
pub mod text;

pub use draw::MODE_ICON;
/// Re-exported so a host names the id a row carries without a
/// `koru-ai-core` dependency of its own.
pub use koru_ai_core::ChatId;
pub use rows::{Notice, Row, RowParts};

/// What the surface built, so a test can assert a count rather than a wall
/// clock. Both are per call, reset at the start of each.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct Work {
    pub rows: usize,
    pub layouts: usize,
}

/// A key press the surface answers. Its own vocabulary rather than winit's,
/// so the keyboard can be driven from a test without building an event.
///
/// **Wider than the Artifacts column's.** Home and End move to either end of a
/// list that can hold hundreds of chats, and Delete asks — the interaction
/// contract both hosts share (`docs/features/koruui/CONTRIBUTING.md`).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Press {
    Up,
    Down,
    Home,
    End,
    Enter,
    Space,
    Delete,
    Escape,
}

impl Press {
    /// The keys this surface claims, and **only** those: everything else
    /// falls through, so a focused list never makes the window feel stuck.
    pub fn of(key: &winit::keyboard::Key) -> Option<Self> {
        use winit::keyboard::{Key, NamedKey};
        match key {
            Key::Named(NamedKey::ArrowUp) => Some(Self::Up),
            Key::Named(NamedKey::ArrowDown) => Some(Self::Down),
            Key::Named(NamedKey::Home) => Some(Self::Home),
            Key::Named(NamedKey::End) => Some(Self::End),
            Key::Named(NamedKey::Enter) => Some(Self::Enter),
            Key::Named(NamedKey::Space) => Some(Self::Space),
            Key::Named(NamedKey::Delete) => Some(Self::Delete),
            Key::Named(NamedKey::Escape) => Some(Self::Escape),
            Key::Character(text) if text == " " => Some(Self::Space),
            _ => None,
        }
    }
}

/// Where the list stands.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub enum Load {
    /// Never asked: the surface has not been shown.
    #[default]
    Cold,
    /// Asked, nothing back yet.
    Loading,
    Ready,
    /// The AI service is not answering. **Not an empty list**: what was on
    /// screen stays.
    Unavailable,
}

/// What one refresh answers: the chats, and the names of the local agents
/// this device can drive — so an agent chat says *OpenCode* rather than
/// `agent-opencode`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    pub chats: Vec<Chat>,
    pub agents: Vec<AgentProviderView>,
}

/// Where the keyboard is. An id rather than a row index, so a refresh that
/// reorders the list does not move the keyboard to another chat. One
/// variant since *New chat* left the list for the heading's button (the
/// CTO's note); kept as an enum so a host's match reads the same.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Focus {
    Chat(ChatId),
}

/// How far a confirmed delete has got.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Deleting {
    Sending,
    /// Refused, with the service's stable code. The row stays and says why
    /// (UC13) — the code is never drawn, its sentence is.
    Refused(String),
}

/// The Chats surface.
pub struct ChatsSurface {
    pub(crate) lang: Lang,
    /// The clock relative times are read against, pushed by the host — this
    /// crate has none, the discipline `koru_i18n::time` keeps.
    pub(crate) now_ms: i64,
    pub(crate) load: Load,
    /// Newest first, by `updated_at`.
    pub(crate) chats: Vec<Chat>,
    pub(crate) agents: Vec<AgentProviderView>,
    /// The chat the host has open. It wears the card, the way the document
    /// in front of you does in the library.
    pub(crate) current: Option<ChatId>,
    /// `Some` doubles as *this surface holds the keyboard*.
    pub(crate) focus: Option<Focus>,
    pub(crate) hover: Option<Row>,
    pub(crate) deleting: HashMap<ChatId, Deleting>,
    /// A chat the host asked to show before the list had arrived with it.
    /// Applied when a listing holds it — `Ctrl+Shift+E` on a chat made a
    /// moment ago must still land on it.
    pub(crate) pending_reveal: Option<ChatId>,
    /// The keyboard moved without a band to scroll against; the next draw,
    /// which has one, brings the row into view.
    pub(crate) reveal_owed: bool,
    pub(crate) wants_open: Option<ChatId>,
    pub(crate) wants_new: bool,
    /// The chats a delete was asked for — one, or the selection.
    pub(crate) wants_delete: Vec<ChatId>,
    /// **The picked chats**, the library's multi-selection one column over
    /// (the CTO's note): a plain press picks one, Ctrl toggles, Shift
    /// extends from the anchor, and a right-click on a picked row raises
    /// the menu about all of them. Kept as ids; read in list order.
    pub(crate) selection: Vec<ChatId>,
    pub(crate) anchor: Option<ChatId>,
    pub(crate) deleted: Vec<ChatId>,
    pub(crate) scroll: f32,
    pub(crate) work: Work,
    /// A refresh has been sent and not answered — the one reason a host keeps
    /// its wake clock running for this surface.
    pub(crate) in_flight: bool,
    reader_link: Option<reader::Reader>,
    seq: u64,
}

impl Default for ChatsSurface {
    fn default() -> Self {
        Self::without_reader()
    }
}

impl ChatsSurface {
    /// A surface with its reader running, dialling `root`'s gateway when the
    /// host was given `--root`.
    ///
    /// **Never under test.** `cfg(test)` covers this crate's own suite and the
    /// `no-reader` feature covers a host's.
    pub fn new(root: Option<PathBuf>) -> Self {
        let mut surface = Self::without_reader();
        surface.start(root);
        surface
    }

    /// Give a surface that is already showing its reader, keeping everything
    /// it holds — a reveal asked for before the reader existed still lands.
    /// Nothing when it has one, and nothing under test.
    pub fn start(&mut self, root: Option<PathBuf>) {
        if self.reader_link.is_some() || cfg!(test) || cfg!(feature = "no-reader") {
            return;
        }
        self.reader_link = reader::Reader::start(root).ok();
    }

    /// A surface that will never read anything — for a test, and for a host
    /// that feeds it itself through [`take_listing`](Self::take_listing).
    pub fn without_reader() -> Self {
        Self {
            lang: Lang::default(),
            now_ms: 0,
            load: Load::Cold,
            chats: Vec::new(),
            agents: Vec::new(),
            current: None,
            focus: None,
            hover: None,
            deleting: HashMap::new(),
            pending_reveal: None,
            reveal_owed: false,
            wants_open: None,
            wants_new: false,
            wants_delete: Vec::new(),
            selection: Vec::new(),
            anchor: None,
            deleted: Vec::new(),
            scroll: 0.0,
            work: Work::default(),
            in_flight: false,
            reader_link: None,
            seq: 0,
        }
    }

    // ------------------------------------------------------ what it knows

    pub fn lang(&self) -> Lang {
        self.lang
    }

    pub fn set_lang(&mut self, lang: Lang) -> bool {
        let changed = self.lang != lang;
        self.lang = lang;
        changed
    }

    /// The moment relative times are read against. The host pushes it on the
    /// frames it already redraws for; an idle surface asks for none.
    pub fn set_now(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    pub fn load(&self) -> Load {
        self.load
    }

    /// Every chat, newest first.
    pub fn chats(&self) -> &[Chat] {
        &self.chats
    }

    pub fn agents(&self) -> &[AgentProviderView] {
        &self.agents
    }

    pub fn work(&self) -> Work {
        self.work
    }

    /// Whether an answer is owed — the only time a host needs a wake for this
    /// surface. `false` whenever it is idle, so an idle window is never woken
    /// on its behalf.
    pub fn awaiting(&self) -> bool {
        self.in_flight
            || self
                .deleting
                .values()
                .any(|state| *state == Deleting::Sending)
    }

    /// Tell the surface which chat the host has open. `true` when that
    /// changed and a repaint is owed.
    pub fn set_current(&mut self, current: Option<ChatId>) -> bool {
        let changed = self.current != current;
        self.current = current;
        changed
    }

    pub fn current(&self) -> Option<ChatId> {
        self.current
    }

    /// How far a delete of `chat` has got, if one was confirmed.
    pub fn deleting(&self, chat: ChatId) -> Option<&Deleting> {
        self.deleting.get(&chat)
    }

    // ---------------------------------------------------------- freshness

    /// Ask again. **The host calls this when the surface is shown and when a
    /// chat surface says a chat was created, renamed or deleted** — never on
    /// a timer. Harmless without a reader.
    pub fn refresh(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        if let Some(reader) = &self.reader_link
            && reader.list(self.seq)
        {
            self.in_flight = true;
            // A refresh over a list that is already showing does not blank
            // it: what is on screen stays until the newer answer replaces it.
            if self.load != Load::Ready {
                self.load = Load::Loading;
            }
        }
    }

    /// Take in whatever the reader has answered. `true` when the surface
    /// changed and a repaint is owed.
    ///
    /// **The only place an answer is accepted**, called from the host's wake
    /// path.
    pub fn poll(&mut self) -> bool {
        let Some(reader) = &self.reader_link else {
            return false;
        };
        let answers = reader.drain();
        if answers.is_empty() {
            return false;
        }
        for answer in answers {
            match answer {
                reader::Answer::Listed { seq, result } => {
                    // An older in-flight answer never replaces newer state.
                    if seq != self.seq {
                        continue;
                    }
                    self.in_flight = false;
                    self.take_listing(result);
                }
                reader::Answer::Deleted { chat, result } => self.take_deleted(chat, result),
            }
        }
        true
    }

    /// Accept a listing — the reader's seam, and the one every test drives.
    pub fn take_listing(&mut self, result: Result<Listing, Failure>) {
        match result {
            Ok(listing) => {
                self.agents = listing.agents;
                self.replace_chats(listing.chats);
                self.load = Load::Ready;
                // A delete whose chat the service no longer lists is done,
                // whoever finished it; a refusal about a chat that is gone
                // would be a sentence about nothing.
                let live: std::collections::HashSet<ChatId> =
                    self.chats.iter().map(|chat| chat.id).collect();
                self.deleting.retain(|id, _| live.contains(id));
                if let Some(chat) = self.pending_reveal
                    && live.contains(&chat)
                {
                    self.pending_reveal = None;
                    self.focus = Some(Focus::Chat(chat));
                    self.reveal_owed = true;
                }
            }
            // **Not an empty list.** What is on screen stays; only the state
            // changes, so a person reads *Koru AI is not answering* rather
            // than *you have no chats*.
            Err(_) => self.load = Load::Unavailable,
        }
    }

    /// Accept a delete's answer — the reader's seam, and the tests'.
    ///
    /// A chat the service no longer has (`not_found`) is as deleted as one it
    /// just removed. Anything else keeps the row, which says why (UC13).
    pub fn take_deleted(&mut self, chat: ChatId, result: Result<(), Failure>) {
        match result {
            Err(failure) if failure.code != "not_found" => {
                self.deleting.insert(chat, Deleting::Refused(failure.code));
            }
            _ => {
                self.deleting.remove(&chat);
                let rest = self
                    .chats
                    .iter()
                    .filter(|candidate| candidate.id != chat)
                    .cloned()
                    .collect();
                self.replace_chats(rest);
                self.deleted.push(chat);
                // The service is the authority on what is left; the row is
                // gone now, and the list is asked again behind it.
                self.refresh();
            }
        }
    }

    /// Replace the rows, newest first, keeping the keyboard on the same chat
    /// — or, when that chat has gone, on **its neighbour**: the chat that took
    /// its place, else the one above it, else nowhere. A list that dropped
    /// the keyboard on a delete would send a keyboard-only person back to the
    /// top of it. The selection keeps what is still listed.
    fn replace_chats(&mut self, mut chats: Vec<Chat>) {
        chats.sort_by(|a, b| b.updated_at.millis().cmp(&a.updated_at.millis()));
        if let Some(Focus::Chat(focused)) = self.focus
            && !chats.iter().any(|chat| chat.id == focused)
        {
            let was = self
                .chats
                .iter()
                .position(|chat| chat.id == focused)
                .unwrap_or(0);
            self.focus = chats
                .get(was.min(chats.len().saturating_sub(1)))
                .map(|neighbour| Focus::Chat(neighbour.id));
            self.reveal_owed = true;
        }
        self.selection
            .retain(|picked| chats.iter().any(|chat| chat.id == *picked));
        if self
            .anchor
            .is_some_and(|anchor| !chats.iter().any(|chat| chat.id == anchor))
        {
            self.anchor = None;
        }
        self.chats = chats;
        // The rows moved under the pointer; an index into them means
        // something else now.
        self.hover = None;
    }

    // ------------------------------------------------------- what it does

    /// **Delete a chat the host has already confirmed.** The service stops the
    /// chat's agent first; if it cannot, the chat stays and the row says so
    /// (UC13). A second press while one is being sent does nothing.
    pub fn delete(&mut self, chat: ChatId) {
        if matches!(self.deleting.get(&chat), Some(Deleting::Sending)) {
            return;
        }
        self.deleting.insert(chat, Deleting::Sending);
        if let Some(reader) = &self.reader_link {
            reader.delete(chat);
        }
    }

    /// A chat the person chose to read. `None` most of the time.
    pub fn take_wants_open(&mut self) -> Option<ChatId> {
        self.wants_open.take()
    }

    /// *New chat* was chosen.
    pub fn take_wants_new(&mut self) -> bool {
        std::mem::take(&mut self.wants_new)
    }

    /// The chats the person asked to delete — one, or the selection; empty
    /// most of the time. **Nothing is deleted yet**: the host asks in its own
    /// confirmation and calls [`delete`](Self::delete) for each on yes.
    pub fn take_wants_delete(&mut self) -> Vec<ChatId> {
        std::mem::take(&mut self.wants_delete)
    }

    /// The picked chats, in list order.
    pub fn selected(&self) -> Vec<ChatId> {
        self.chats
            .iter()
            .map(|chat| chat.id)
            .filter(|chat| self.selection.contains(chat))
            .collect()
    }

    pub fn is_selected(&self, chat: ChatId) -> bool {
        self.selection.contains(&chat)
    }

    /// Pick `chat` alone, and anchor a later Shift there.
    pub fn select_only(&mut self, chat: ChatId) {
        if self.chats.iter().any(|candidate| candidate.id == chat) {
            self.selection = vec![chat];
            self.anchor = Some(chat);
            self.focus = Some(Focus::Chat(chat));
        }
    }

    /// Ctrl+click: in or out of the pick, and the anchor moves here.
    pub(crate) fn toggle_selected(&mut self, chat: ChatId) {
        if let Some(at) = self.selection.iter().position(|picked| *picked == chat) {
            self.selection.remove(at);
        } else {
            self.selection.push(chat);
        }
        self.anchor = Some(chat);
        self.focus = Some(Focus::Chat(chat));
    }

    /// Shift+click: every chat between the anchor and `chat`, in list order.
    pub(crate) fn select_through(&mut self, chat: ChatId) {
        let index = |id: ChatId| self.chats.iter().position(|candidate| candidate.id == id);
        let (Some(to), Some(from)) = (index(chat), self.anchor.and_then(index)) else {
            self.select_only(chat);
            return;
        };
        let (first, last) = if from <= to { (from, to) } else { (to, from) };
        self.selection = self.chats[first..=last].iter().map(|c| c.id).collect();
        self.focus = Some(Focus::Chat(chat));
    }

    /// Chats this surface deleted since the last call, so a host can close
    /// what it had open on them.
    pub fn take_deleted_chats(&mut self) -> Vec<ChatId> {
        std::mem::take(&mut self.deleted)
    }

    /// Ask to open `chat` — what a click, Enter and an accessibility client's
    /// *Open* all mean.
    pub fn ask_open(&mut self, chat: ChatId) {
        if self.chats.iter().any(|candidate| candidate.id == chat) {
            self.focus = Some(Focus::Chat(chat));
            self.wants_open = Some(chat);
        }
    }

    /// Ask to delete `chat` alone — an accessibility client's *Delete chat*.
    pub fn ask_delete(&mut self, chat: ChatId) {
        if self.chats.iter().any(|candidate| candidate.id == chat) {
            self.focus = Some(Focus::Chat(chat));
            self.wants_delete = vec![chat];
        }
    }

    /// Ask to delete what is picked — the Delete key and the context menu:
    /// the selection when the chat under the keyboard is in it, else that
    /// chat alone. `false` with nothing to delete.
    pub fn ask_delete_selected(&mut self) -> bool {
        let Some(Focus::Chat(focused)) = self.focus else {
            return false;
        };
        let chats = if self.selection.contains(&focused) {
            self.selected()
        } else {
            vec![focused]
        };
        if chats.is_empty() {
            return false;
        }
        self.wants_delete = chats;
        true
    }

    /// *New chat* was asked for — by the host's heading button, or an
    /// accessibility client. The keyboard stays where it is.
    pub fn ask_new(&mut self) {
        self.wants_new = true;
    }

    /// **Show one chat and put the keyboard on it** — what `Ctrl+Shift+E` in
    /// a chat means. Not a toggle: asking twice lands on the same row.
    ///
    /// `false` when the list does not hold it yet; the surface then remembers
    /// it and lands on it when the list that holds it arrives.
    pub fn reveal(&mut self, chat: ChatId, area: koru_draw::geometry::Rect, top: f32) -> bool {
        if !self.chats.iter().any(|candidate| candidate.id == chat) {
            self.pending_reveal = Some(chat);
            return false;
        }
        self.pending_reveal = None;
        self.focus = Some(Focus::Chat(chat));
        if let Some(row) = self.focus_row() {
            self.scroll_into_view(row, area, top);
        }
        true
    }

    /// Bring a chat's row into the band **without moving the keyboard** — an
    /// accessibility client's *scroll into view*. `false` when the list does
    /// not hold it.
    pub fn scroll_to_chat(
        &mut self,
        chat: ChatId,
        area: koru_draw::geometry::Rect,
        top: f32,
    ) -> bool {
        let Some(index) = self.chats.iter().position(|candidate| candidate.id == chat) else {
            return false;
        };
        self.scroll_into_view(Row::Chat(index), area, top);
        true
    }

    /// A row's accessible name: title, who answered, and when — and that it
    /// is private, when it is.
    pub fn accessible_name(&self, chat: ChatId) -> Option<String> {
        let found = self.chats.iter().find(|candidate| candidate.id == chat)?;
        Some(text::accessible_name(
            self.lang,
            found,
            &self.agents,
            self.now_ms,
        ))
    }
}

#[cfg(test)]
mod tests;
