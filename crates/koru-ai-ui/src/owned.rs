// SPDX-License-Identifier: MPL-2.0

//! **A conversation a host owns** (Bots P1.8 S3; QUESTIONS O31.1,
//! **approved 2026-11-10**).
//!
//! An ordinary [`ChatSurface`] is a person's own chat window: it makes a new
//! chat when the one it was opened on is gone, it remembers the model chosen
//! as where the next chat starts, and every control it has acts on the chat
//! directly. A Bot's conversation is none of those things. Its owner — the
//! Bot service — decides where its work is saved, which agent answers it and
//! which controls exist, and a Bot conversation that cannot be read must say
//! so rather than quietly becoming somebody's new ordinary chat.
//!
//! [`ChatSurface::bind_owned`] is the seam. A surface made with
//! [`ChatSurface::new_owned_at`] connects first-party with the ordinary
//! scopes and `bots.manage`, and once bound:
//!
//! 1. every Send, Retry and question-answer turn carries
//!    `destination_workspace = Some(destination)`;
//! 2. a conversation that is gone or cannot be reached is
//!    [`Unavailable`]: the surface shows one sentence, sends nothing, and
//!    never sets out to make a new chat;
//! 3. a model picked in it is not remembered as the person's new-chat
//!    default;
//! 4. its picker offers only the owner's agent;
//! 5. rename, pin, privacy, the working folder, workspace access and path
//!    references are absent — from drawing, hit-testing and the commands
//!    the surface will queue — unless [`OwnedOffers`] admits them. Pictures
//!    and stored files are offered: the owner admits attachment staging;
//! 6. the context workspace is `destination`, whatever the host's window
//!    shows.
//!
//! **An ordinary surface never takes this path.** Every branch below is
//! behind `owned.is_some()`, and the tests in `owned/tests.rs` pin the
//! ordinary behaviour beside the owned one.
//!
//! ## A host-drawn empty transcript
//!
//! An empty chat opens on the chat's own greeting with the composer centred
//! under it. A host may take that room instead
//! ([`ChatSurface::host_empty_transcript`]): while the conversation is
//! **read and holds nothing**, the surface draws no greeting, keeps the
//! composer where it sits once a chat has begun, and hands the host the
//! room above it.
//!
//! - [`ChatSurface::hosted_empty_area`] is that room, and `None` whenever
//!   the host's content is not what shows: nothing asked, the conversation
//!   not read yet, a message in it or on its way, or the conversation
//!   unavailable.
//! - [`ChatSurface::draw_hosting`] draws the surface and calls the host's
//!   painter where the greeting would have been: over the transcript's
//!   ground and under everything of the chat's that floats (its pickers,
//!   panes and tooltips).
//! - [`ChatSurface::take_hosted_press`] is a press in that room that
//!   nothing of the chat's claimed first.
//! - [`ChatSurface::set_host_keyboard`] says the host's own content holds
//!   the keyboard, so the composer draws no caret beside the host's.
//!
//! **The surface knows nothing about what the host draws.** It takes no
//! words, no elements and no state from it: only "I draw this room" and a
//! painter. So nothing of the host's can reach the transcript, the stored
//! chat, a draft or a request, and the composer is the ordinary composer
//! throughout. The seam is not tied to an owned conversation; a surface
//! whose host never asks is unchanged, which `owned/tests.rs` pins.
//!
//! ## A saved copy
//!
//! A host may show a conversation that **another computer runs**, from the
//! copy this computer keeps of it (Bots M8, P8.3). The surface reads and
//! sends as it always does; what differs is said by the host, as plain
//! data ([`ChatSurface::set_saved_copy`], [`SavedCopy`]):
//!
//! - **Whether anything can be sent now.** While it cannot
//!   ([`SavedCopy::held`]), Send does nothing, the composer keeps what was
//!   typed and its placeholder is the host's sentence. Nothing is queued,
//!   so nothing is sent later behind the person's back.
//! - **What a refused send means** ([`CopyOutcome`]): the host names the
//!   refusals that are about the other computer, by code and by one detail,
//!   with the sentence each reads as and whether the message comes back to
//!   the composer ([`CopyDraft::Kept`]: nothing left this computer) or does
//!   not ([`CopyDraft::Sent`]: it left, and sending it again by hand would
//!   make a second message). A message that does not come back stays in the
//!   composer's history, one Up away.
//! - **What a copied message could not carry** ([`CopyNote`]): a sentence
//!   in place of a text that was too large to copy, or one under a message
//!   whose attachments stayed where they are.
//!
//! **The surface knows nothing about computers, copies or their codes.** It
//! takes sentences and codes as data and calls nothing for them. A surface
//! whose host never says any of this is unchanged, which `owned/tests.rs`
//! pins.

use std::path::PathBuf;

use koru_ai_core::{AiProviderId, ChatId, MessageId, WorkspaceId};
use koru_draw::geometry::Rect;

use crate::ChatSurface;
use crate::reader::{Command, Scopes};

/// **A host's painter for the empty transcript**: the scene, the root
/// transform and the room it may draw in, in the surface's coordinates.
pub type HostPaint<'a> = &'a mut dyn FnMut(&mut vello::Scene, vello::kurbo::Affine, Rect);

/// What a host that draws the empty transcript has said, and what the
/// surface holds for it. All off by default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct HostedEmpty {
    /// The host draws the empty transcript.
    asked: bool,
    /// A press in the hosted room that nothing of the chat's claimed.
    press: Option<(f32, f32)>,
    /// The host's own content holds the keyboard.
    keyboard: bool,
}

/// The conversation a host binds a surface to, and what its owner admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedConversation {
    /// The conversation: for a Bot, its main chat or another of its chats.
    pub chat: ChatId,
    /// Where its work is saved. Sent with every turn; the owner refuses a
    /// turn without one.
    pub destination: WorkspaceId,
    /// The one agent that answers it. The picker offers no other.
    pub provider: AiProviderId,
    pub offers: OwnedOffers,
    /// **The words for refusals this chat does not itself know**, by the
    /// owner's stable code: a refusal whose code is here reads as that
    /// sentence and nothing more — a Send, a Retry, a recovery, a steer.
    /// Plain data, handed in at bind time: the surface knows no product's
    /// codes and calls nothing for them. Empty for an owner with none.
    pub refusals: Refusals,
}

/// **A host's refusal words**: each owner's stable code and the sentence it
/// reads as, in every language the host's strings carry.
pub type Refusals = &'static [(&'static str, koru_i18n::Key)];

/// **The controls an owner admits beyond the conversation itself.** Each is
/// absent while `false`, which is the default: the Bot owner refuses all of
/// them today. Sending, steering, Stop, approvals and answers, the model and
/// effort within the agent, the agent's own mode and Fast, Branch, storing a
/// reply as a page, and attaching pictures and stored files are the
/// conversation and are always offered.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnedOffers {
    /// Renaming the chat, and the automatic title from its first message.
    pub rename: bool,
    /// Pinning it in the Chats list.
    pub pin: bool,
    /// The private mark.
    pub privacy: bool,
    /// *Work in a project folder…*: the agent's working folder.
    pub working_folder: bool,
    /// The chat's own workspace access and the grants a send asks for.
    pub tool_access: bool,
    /// Files and folders on this computer attached by their path.
    pub path_references: bool,
}

impl OwnedOffers {
    /// Everything an ordinary surface offers: what [`ChatSurface::offers`]
    /// answers for a surface no host owns.
    pub(crate) const ORDINARY: Self = Self {
        rename: true,
        pin: true,
        privacy: true,
        working_folder: true,
        tool_access: true,
        path_references: true,
    };
}

/// **Why an owned conversation is not shown.** Each reads as the same one
/// sentence today; the reason is kept so a host can say more once it can.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The service holds no such conversation for this caller — trashed,
    /// deleted, or not this person's.
    Missing,
    /// The service did not answer.
    Unreachable,
    /// It belongs to another profile than this window's; the host was handed
    /// it ([`ChatSurface::take_wants_open_elsewhere`]).
    Elsewhere,
}

impl ChatSurface {
    /// **A surface for a conversation a host owns**: [`Self::new_at`], whose
    /// threads connect first-party with the ordinary scopes and
    /// `bots.manage`. Bind it with [`Self::bind_owned`] before activating it,
    /// so its first boot reads that conversation and nothing else.
    pub fn new_owned_at(root: Option<PathBuf>) -> Self {
        let mut surface = Self::new_at(root.clone());
        surface.scopes = Scopes::Owned;
        surface.attachment_work = crate::attachments::Work::new(root).with_scopes(Scopes::Owned);
        surface
    }

    /// **Bind this surface to `conversation`** and show it. Binding the same
    /// conversation again changes nothing; binding another chat, or the same
    /// one after it was unavailable, reads it afresh. `true` when anything
    /// changed.
    pub fn bind_owned(&mut self, conversation: OwnedConversation) -> bool {
        if self.owned.as_ref() == Some(&conversation) && self.unavailable.is_none() {
            return false;
        }
        let previous = self.owned.as_ref().map(|owned| owned.chat);
        let reread = self.unavailable.is_some() || previous != Some(conversation.chat);
        let chat = conversation.chat;
        if previous.is_some() && previous != Some(chat) {
            self.move_composer(previous, chat);
        }
        self.context_workspace = Some(conversation.destination);
        self.prioritize_context_workspace();
        self.fresh_chat_wanted = false;
        self.unmade_chat = None;
        self.unavailable = None;
        self.owned = Some(conversation);
        if !reread {
            return true;
        }
        // A boot that never answered is asked again: the conversation needs
        // the settings and the agent's listing a boot brings.
        if self.reader.is_some() && self.settings.is_none() && !self.waiting {
            self.pending_link_chat = Some(chat);
            return self.send_command(Command::Boot(Some(chat), self.profile));
        }
        self.open_link(chat, None)
    }

    /// **The unsent message stays with the chat it was written in** when
    /// the surface moves from `from` to `to` (the fourth window run of Bots:
    /// one draft followed a Bot's tab across its conversations). What was
    /// being typed — chips, quotes, caret and undo — is set aside for `from`,
    /// and `to`'s own unsent message, if it has one, comes back. A walk back
    /// through earlier messages is left first, so what is set aside is what
    /// was typed, not a recalled message.
    fn move_composer(&mut self, from: Option<ChatId>, to: ChatId) {
        if self.history_cursor.is_some() {
            self.leave_input_walk();
        }
        let typed = self.composer.stash();
        if let Some(from) = from {
            self.composer_drafts.insert(from, typed);
        }
        if let Some(draft) = self.composer_drafts.remove(&to) {
            self.composer.unstash(draft);
        }
    }

    /// The conversation this surface is bound to, if a host owns it.
    pub fn owned(&self) -> Option<&OwnedConversation> {
        self.owned.as_ref()
    }

    // ------------------------------------ a host-drawn empty transcript

    /// **The host draws the empty transcript**, or no longer does. `true`
    /// when that changed. Nothing is drawn differently until the
    /// conversation is read and holds nothing ([`Self::hosted_empty_area`]).
    pub fn host_empty_transcript(&mut self, hosted: bool) -> bool {
        if self.hosted_empty.asked == hosted {
            return false;
        }
        self.hosted_empty = HostedEmpty {
            asked: hosted,
            ..HostedEmpty::default()
        };
        true
    }

    /// **Whether the conversation shown is read and empty**: `None` until
    /// it is read — for an owned surface, until the conversation it is
    /// bound to is the one read — `Some(true)` while it holds no message
    /// and none is on its way, `Some(false)` from the first one on.
    pub fn transcript_is_empty(&self) -> Option<bool> {
        let transcript = self.transcript.as_ref()?;
        if self
            .owned
            .as_ref()
            .is_some_and(|owned| owned.chat != transcript.chat.id)
        {
            return None;
        }
        Some(self.chat_is_empty())
    }

    /// Whether the host's content is what shows in the transcript's place.
    pub(crate) fn hosts_empty(&self) -> bool {
        self.hosted_empty.asked
            && self.unavailable.is_none()
            && !self.showing_setup()
            && self.transcript_is_empty() == Some(true)
    }

    /// **The room the host draws in**: the conversation's, above the
    /// composer, in the surface's coordinates. `None` whenever the host's
    /// content is not what shows.
    pub fn hosted_empty_area(&mut self) -> Option<Rect> {
        if !self.hosts_empty() {
            return None;
        }
        Some(self.conversation_rect())
    }

    /// **Draw the surface, and the host's empty transcript where it
    /// shows**: `paint` is called at most once, in the greeting's place,
    /// with the room [`Self::hosted_empty_area`] answers. It is not called
    /// while the host's content is not what shows, and the surface then
    /// draws exactly what [`Self::draw`] draws.
    pub fn draw_hosting(
        &mut self,
        canvas: &mut vello::Scene,
        root: vello::kurbo::Affine,
        paint: HostPaint<'_>,
    ) {
        self.draw_with(canvas, root, Some(paint));
    }

    /// A press at `(x, y)` that is the host's: in its room, with the
    /// host's content showing. Kept for [`Self::take_hosted_press`].
    pub(crate) fn hosted_press_at(&mut self, x: f32, y: f32) -> bool {
        let Some(area) = self.hosted_empty_area() else {
            return false;
        };
        if !area.contains(x, y) {
            return false;
        }
        self.hosted_empty.press = Some((x, y));
        true
    }

    /// **A press in the host's room that nothing of the chat's claimed** —
    /// no open picker, pane or menu over it. Taken once, by the host, after
    /// it hands the surface a press.
    pub fn take_hosted_press(&mut self) -> Option<(f32, f32)> {
        self.hosted_empty.press.take()
    }

    /// **The host's own content holds the keyboard**, or gave it back: the
    /// composer draws no caret while it does. The host routes the keys;
    /// the surface only stops showing a second caret. `true` when that
    /// changed.
    pub fn set_host_keyboard(&mut self, held: bool) -> bool {
        let held = held && self.hosted_empty.asked;
        if self.hosted_empty.keyboard == held {
            return false;
        }
        self.hosted_empty.keyboard = held;
        true
    }

    /// Whether the host's content holds the keyboard, while it shows.
    pub(crate) fn host_holds_keyboard(&self) -> bool {
        self.hosted_empty.keyboard && self.hosts_empty()
    }

    /// Why the owned conversation is not shown, while it is not. Always
    /// `None` for an ordinary surface.
    pub fn unavailable(&self) -> Option<Unavailable> {
        self.unavailable
    }

    /// The refusal words the host handed in: none for an ordinary surface.
    pub(crate) fn refusals(&self) -> Refusals {
        self.owned.as_ref().map_or(&[], |owned| owned.refusals)
    }

    /// **A refusal as this surface says it**: what a saved copy's host
    /// says of it ([`CopyOutcome`]), else the host's sentence for a code it
    /// names, else the chat's own reading ([`crate::failure_text`]).
    pub(crate) fn failure_words(&self, failure: &koru_ai_wire::Failure) -> String {
        if let Some(outcome) = self.copy_outcome(failure) {
            return outcome.words.clone();
        }
        crate::failure_text(failure, self.lang, self.refusals())
    }

    /// What this surface offers: everything, unless a host owns it.
    pub(crate) fn offers(&self) -> OwnedOffers {
        self.owned
            .as_ref()
            .map_or(OwnedOffers::ORDINARY, |owned| owned.offers)
    }

    /// The workspace a turn's results go to: the owner's destination, and
    /// `None` — exactly as before — for an ordinary surface.
    pub(crate) fn owned_destination(&self) -> Option<WorkspaceId> {
        self.owned.as_ref().map(|owned| owned.destination)
    }

    /// **The owned conversation cannot be shown**, for `reason`. Nothing of
    /// another chat stays on screen and nothing sets out to make one; the
    /// host still knows which conversation it is ([`Self::owned`]).
    pub(crate) fn owned_unavailable(&mut self, reason: Unavailable) {
        self.unavailable = Some(reason);
        self.fresh_chat_wanted = false;
        self.unmade_chat = None;
        self.pending_link_chat = None;
        if self.transcript.take().is_some() {
            self.forget_input_walk();
            self.messages_dirty = true;
        }
        self.waiting = false;
        self.status = None;
    }

    /// `Update::Unavailable` from an owned reader: about the bound chat, or
    /// about a boot that named none. A failed refresh of the conversation
    /// already on screen keeps it there.
    pub(crate) fn owned_read_failed(&mut self, chat: Option<ChatId>, reason: Unavailable) {
        let Some(bound) = self.owned.as_ref().map(|owned| owned.chat) else {
            self.waiting = false;
            return;
        };
        if chat.is_some_and(|chat| chat != bound) {
            self.waiting = false;
            return;
        }
        let shown = self.transcript.as_ref().map(|t| t.chat.id) == Some(bound);
        if shown && reason == Unavailable::Unreachable {
            self.waiting = false;
            return;
        }
        self.owned_unavailable(reason);
    }

    /// The unavailable conversation, drawn: the one sentence, centred in the
    /// conversation's column, in the dim ink of a statement — no composer,
    /// no control, nothing to press.
    pub(crate) fn draw_unavailable(
        &mut self,
        canvas: &mut vello::Scene,
        root: vello::kurbo::Affine,
    ) {
        let content = self.content_rect();
        let words = koru_i18n::t(self.lang, koru_i18n::Key::BotsConversationUnavailable);
        let layout = self.engine.simple_layout_wrapped(
            words,
            koru_draw::theme::role::meta(),
            content.w.max(1.0),
        );
        let x = content.x + (content.w - layout.width()).max(0.0) * 0.5;
        let y = content.y + (content.h - layout.height()).max(0.0) * 0.5;
        koru_draw::scene::draw_layout(
            canvas,
            root,
            &layout,
            x,
            y,
            Some(koru_draw::theme::p().text_dim),
        );
    }

    /// **A failed reply a branch copied from another conversation** (QUESTIONS
    /// O34): its `origin_message_id` names a message this chat does not hold.
    /// An ordinary Retry also sets the origin — to the failed reply it
    /// replaces, a message **of the same chat** (`koru_ai_core::reading_order`)
    /// — so the origin alone says nothing; where it points does. The
    /// transcript a surface holds is the whole chat (`read_chat` reads every
    /// message, in order, with no window), so an origin it does not hold is
    /// one in another chat, not one outside a loaded page.
    ///
    /// A message the transcript does not hold yet — a reply still arriving
    /// live — is not a copy.
    pub(crate) fn copied_from_elsewhere(&self, message: koru_ai_core::MessageId) -> bool {
        let Some(transcript) = &self.transcript else {
            return false;
        };
        let Some(origin) = transcript
            .messages
            .iter()
            .find(|held| held.id == message)
            .and_then(|held| held.origin_message_id)
        else {
            return false;
        };
        !transcript.messages.iter().any(|held| held.id == origin)
    }

    /// **Whether a failed reply offers Retry.** Always on an ordinary
    /// surface. On an owned one, not on a reply a branch copied: the Bot
    /// owner retries a reply from the attempt behind it, which a branch does
    /// not copy, so that Retry could only answer not-found (O34). The person
    /// sends the request again instead.
    pub(crate) fn offers_retry(&self, message: koru_ai_core::MessageId) -> bool {
        self.owned.is_none() || !self.copied_from_elsewhere(message)
    }

    /// **Whether an owned surface will queue `command`.** The controls the
    /// owner refuses are absent already; this is the floor under them, so a
    /// host's own call or a path nobody drew cannot reach the service either.
    /// An ordinary surface queues everything, as it always did.
    pub(crate) fn owned_admits(&self, command: &Command) -> bool {
        // **A held saved copy sends no turn**: the floor under the composer
        // that keeps its draft and the controls that say why.
        if self.copy_holds_send()
            && matches!(
                command,
                Command::Send(_)
                    | Command::Retry(..)
                    | Command::Steer(..)
                    | Command::AnswerQuestions(..)
            )
        {
            return false;
        }
        let Some(owned) = &self.owned else {
            return true;
        };
        let offers = owned.offers;
        match command {
            // A new chat, and the person's own provider settings — where a
            // new chat starts — are never an owned surface's to change.
            Command::Create(_)
            | Command::SelectProvider(_)
            | Command::UpdateOpenAi(_)
            | Command::UpdateOllama(_)
            | Command::Pull(_)
            | Command::CancelPull
            | Command::ClearHistory(_)
            | Command::ClearUsage(_) => false,
            Command::Rename(..) => offers.rename,
            Command::Pin(..) => offers.pin,
            Command::UpdatePrivacy(..) => offers.privacy,
            Command::UpdateFolder(..) => offers.working_folder,
            Command::UpdateToolPolicy(..) | Command::AddGrants(..) => offers.tool_access,
            // Another model of the owner's agent, never another agent.
            Command::UpdateModel(_, request) => request.model.provider == owned.provider,
            // Never a Retry of a copied reply: it has no attempt to retry.
            Command::Retry(_, request) => !self.copied_from_elsewhere(request.message_id),
            _ => true,
        }
    }
}

// ---------------------------------------------------- a saved copy (M8)

/// **What a host says about a conversation it shows from a saved copy**:
/// one another computer runs. Plain data; every sentence is already in the
/// person's language and already names what it is about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedCopy {
    /// **Nothing can be sent now**, and this is why, in a sentence: the
    /// composer's placeholder while it holds. `None` while sending is open.
    pub held: Option<String>,
    /// Said over the composer while a message is on its way and no reply
    /// has begun. Empty says nothing.
    pub sending: String,
    /// The refusals that are about the other computer.
    pub outcomes: Vec<CopyOutcome>,
    /// What copied messages could not carry.
    pub notes: Vec<CopyNote>,
    /// Said when a page link of this conversation leads to a page this
    /// computer does not hold ([`ChatSurface::say_page_missing`]).
    pub page_missing: String,
}

/// **One refusal a saved copy's host names**: which failure it is, the
/// sentence it reads as, and what becomes of the message that was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyOutcome {
    /// The failure's stable code.
    pub code: String,
    /// One of the failure's details that must also hold, as its key and its
    /// text value. `None` matches the code alone, and is tried last.
    pub detail: Option<(String, String)>,
    pub draft: CopyDraft,
    pub words: String,
}

/// What becomes of a message whose send was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CopyDraft {
    /// **Nothing left this computer**: the message is back in the composer.
    Kept,
    /// **It left this computer**, and whether it arrived is not known:
    /// it does not come back to the composer, where a second Send would
    /// make a second message. It stays in the composer's history.
    Sent,
}

/// **What one copied message could not carry.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyNote {
    pub message: MessageId,
    /// Shown in the message's place: its text was not copied.
    pub instead: Option<String>,
    /// Shown under the message's own text.
    pub under: Option<String>,
}

/// What the surface holds of a saved copy: the host's words, and what the
/// last refused send came to, for the host to take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CopyState {
    copy: SavedCopy,
    settled: Option<CopyDraft>,
}

impl CopyState {
    /// What is drawn for `message` instead of its own text, when its host
    /// said something of it.
    pub(crate) fn shown_instead(&self, message: MessageId, markdown: &str) -> Option<String> {
        let note = self
            .copy
            .notes
            .iter()
            .find(|note| note.message == message)?;
        match (&note.instead, &note.under) {
            (Some(instead), _) => Some(instead.clone()),
            (None, Some(under)) if markdown.trim().is_empty() => Some(under.clone()),
            // Its own paragraph under the message's text.
            (None, Some(under)) => Some([markdown, under.as_str()].join("\n\n")),
            (None, None) => None,
        }
    }
}

impl ChatSurface {
    /// **Say that this conversation is shown from a saved copy**, or that it
    /// no longer is (`None`). `true` when anything the surface draws
    /// changed. What was typed is never touched: a hold keeps the draft,
    /// and lifting it leaves the draft where it is.
    pub fn set_saved_copy(&mut self, copy: Option<SavedCopy>) -> bool {
        let before = self.saved_copy.as_ref().map(|state| &state.copy);
        if before == copy.as_ref() {
            return false;
        }
        let notes = before.map(|copy| &copy.notes) != copy.as_ref().map(|copy| &copy.notes);
        let settled = self.saved_copy.as_ref().and_then(|state| state.settled);
        self.saved_copy = copy.map(|copy| CopyState { copy, settled });
        if notes {
            // A message's text is laid out once and kept.
            self.messages_dirty = true;
        }
        true
    }

    /// What the host said of the saved copy, while it shows one.
    pub fn saved_copy(&self) -> Option<&SavedCopy> {
        self.saved_copy.as_ref().map(|state| &state.copy)
    }

    /// **What the last refused send came to**, for the host: taken once.
    /// [`CopyDraft::Sent`] is the host's cue that the copy is behind what
    /// was sent, and that it is worth reading again when it can be.
    pub fn take_copy_outcome(&mut self) -> Option<CopyDraft> {
        self.saved_copy.as_mut()?.settled.take()
    }

    /// **A page link led to a page this computer does not hold**: the host
    /// found that out and says so here, in the saved copy's own sentence.
    /// `false` when the surface shows no saved copy or the host gave no
    /// sentence, and nothing is said.
    pub fn say_page_missing(&mut self) -> bool {
        let Some(words) = self
            .saved_copy()
            .map(|copy| copy.page_missing.clone())
            .filter(|words| !words.is_empty())
        else {
            return false;
        };
        self.status = Some(words);
        true
    }

    /// Whether a saved copy holds every send: its computer is not reachable.
    pub(crate) fn copy_holds_send(&self) -> bool {
        self.saved_copy().is_some_and(|copy| copy.held.is_some())
    }

    /// The composer's placeholder while a saved copy holds every send.
    pub(crate) fn copy_placeholder(&self) -> Option<&str> {
        self.saved_copy()?.held.as_deref()
    }

    /// The line over the composer while a message is on its way to the
    /// computer the copy came from: sent, and no reply begun.
    pub(crate) fn copy_line(&self) -> Option<&str> {
        let copy = self.saved_copy()?;
        (self.pending_user.is_some() && self.attempt.is_none() && !copy.sending.is_empty())
            .then_some(copy.sending.as_str())
    }

    /// **The outcome a saved copy's host names for `failure`**: the one
    /// whose code and detail both match, else the one for the code alone.
    pub(crate) fn copy_outcome(&self, failure: &koru_ai_wire::Failure) -> Option<&CopyOutcome> {
        let outcomes = &self.saved_copy()?.outcomes;
        let detailed = outcomes.iter().find(|outcome| {
            outcome.code == failure.code
                && outcome.detail.as_ref().is_some_and(|(key, value)| {
                    failure.details.get(key).and_then(|held| held.as_str()) == Some(value.as_str())
                })
        });
        detailed.or_else(|| {
            outcomes
                .iter()
                .find(|outcome| outcome.code == failure.code && outcome.detail.is_none())
        })
    }

    /// **A send was refused, and the saved copy's host names the refusal.**
    /// The sentence is the host's. [`CopyDraft::Kept`] gives the message
    /// back to the composer, as any refused send does; [`CopyDraft::Sent`]
    /// does not, and leaves it in the composer's history. `false` when the
    /// host names no outcome for it: the surface's own reading stands.
    pub(crate) fn copy_send_failed(&mut self, failure: &koru_ai_wire::Failure) -> bool {
        let Some((draft, words)) = self
            .copy_outcome(failure)
            .map(|outcome| (outcome.draft, outcome.words.clone()))
        else {
            return false;
        };
        match draft {
            CopyDraft::Kept => self.give_back_sent(None),
            CopyDraft::Sent => {
                // What `give_back_sent` clears, without the composer: the
                // message is not typed again for the person.
                self.sent_message = None;
                self.waiting = false;
                self.attempt = None;
                self.pending_assistant_message_id = None;
                self.pending_user = None;
                self.answering = false;
                self.pending_user_tiles.clear();
                self.pulse = None;
                self.messages_dirty = true;
            }
        }
        self.status = Some(words);
        if let Some(state) = &mut self.saved_copy {
            state.settled = Some(draft);
        }
        true
    }
}

#[cfg(test)]
mod tests;
