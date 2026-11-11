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

use std::path::PathBuf;

use koru_ai_core::{AiProviderId, ChatId, WorkspaceId};

use crate::ChatSurface;
use crate::reader::{Command, Scopes};

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
        let reread = self.unavailable.is_some()
            || self.owned.as_ref().map(|owned| owned.chat) != Some(conversation.chat);
        let chat = conversation.chat;
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

    /// The conversation this surface is bound to, if a host owns it.
    pub fn owned(&self) -> Option<&OwnedConversation> {
        self.owned.as_ref()
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

    /// **A refusal as this surface says it**: the host's sentence for a
    /// code it names, else the chat's own reading ([`crate::failure_text`]).
    pub(crate) fn failure_words(&self, failure: &koru_ai_wire::Failure) -> String {
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

#[cfg(test)]
mod tests;
