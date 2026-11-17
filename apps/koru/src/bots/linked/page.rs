// SPDX-License-Identifier: MPL-2.0

//! **A remote Bot's page, as a saved copy** (Bots M8, P8.3; contract 6.3):
//! what the tab of a Bot another computer runs says, from that computer's
//! copy state.
//!
//! - **Connected**: the page is an ordinary Bot's. Its commands go to the
//!   computer that runs the Bot.
//! - **Not connected**: the sentence that this is a saved copy and as of
//!   when, a composer that keeps its draft and sends nothing, and nothing
//!   that pretends a change was made.
//! - **Needs an update**: the copy is still read; nothing can be sent.
//!
//! And **what a command came to**, from the refusal's code and its
//! `details.delivery` ([`outcome`]): nothing left this computer (*Not
//! saved*, the draft kept), it left and no answer came (*Not confirmed*;
//! Koru asks again by itself, the person never has to), or the other
//! computer cannot say (*Not confirmed. Check the conversation.*).
//!
//! **A new try is a new request.** Nothing here keeps a request id to send
//! again: the settings mint one per press and the chat one per message, and
//! a refused one is never reused.
//!
//! The model only: fed plain values, it sends nothing.

use std::time::Instant;

use koru_ai_core::bots::{BotRequestId, DeviceId, LinkedOwnerState};
use koru_ai_ui::{CopyDraft, CopyNote, CopyOutcome, SavedCopy};
use koru_i18n::{Key as Words, Lang, fill, t};

use super::{
    ALREADY_ANSWERED, DELIVERY, EXECUTING_COMPUTER_ONLY, NOT_CONFIRMED, NOT_SENT,
    OWNER_UNREACHABLE, OWNER_UNSUPPORTED, POLL, UNKNOWN,
};
use crate::bots::link::Failure;

/// **What a command to the other computer came to**, as the page says it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing left this computer: *Not saved. {computer} is not reachable.*
    NotSaved,
    /// It left and no answer came: Koru asks again when that computer is
    /// reached. Never sent again from here.
    NotConfirmed,
    /// That computer cannot say whether it took effect. Final.
    NotConfirmedFinal,
    /// Only the computer that runs the Bot can make this change.
    ChangeThere,
    /// The two computers share no version.
    NeedsUpdate,
}

/// **The outcome a refusal names**, from its stable code and its
/// `details.delivery`; `None` for any other refusal, which is the owner's
/// own and reads as it does for a local Bot.
pub(crate) fn outcome(code: &str, delivery: Option<&str>) -> Option<Outcome> {
    Some(match (code, delivery) {
        (OWNER_UNREACHABLE, Some(NOT_SENT)) => Outcome::NotSaved,
        (OWNER_UNREACHABLE, Some(NOT_CONFIRMED)) => Outcome::NotConfirmed,
        // `unknown`, and an unreachable computer that named no delivery:
        // never said as not sent, which nothing here knows.
        (OWNER_UNREACHABLE, _) => Outcome::NotConfirmedFinal,
        // A settled request id used again: how it ended is in the
        // conversation, and a new try takes a new id.
        (ALREADY_ANSWERED, _) => Outcome::NotConfirmedFinal,
        (OWNER_UNSUPPORTED, _) => Outcome::NeedsUpdate,
        (EXECUTING_COMPUTER_ONLY, _) => Outcome::ChangeThere,
        _ => return None,
    })
}

/// An outcome in words; `computer` is the name its heading wears.
pub(crate) fn outcome_words(lang: Lang, outcome: Outcome, computer: &str) -> String {
    match outcome {
        Outcome::NotSaved => fill(t(lang, Words::BotsLinkedNotSaved), &[computer]),
        Outcome::NotConfirmed => fill(t(lang, Words::BotsLinkedNotConfirmed), &[computer]),
        Outcome::NotConfirmedFinal => t(lang, Words::BotsLinkedNotConfirmedFinal).to_owned(),
        Outcome::ChangeThere => fill(t(lang, Words::BotsLinkedChangeOn), &[computer]),
        Outcome::NeedsUpdate => super::unsupported_words(lang, computer),
    }
}

/// *Sending…*: a message on its way to the other computer.
pub(crate) fn sending_words(lang: Lang) -> &'static str {
    t(lang, Words::BotsLinkedSending)
}

/// *Saving…*: a setting on its way to the other computer. The settings'
/// own word for a change on its way, which a remote Bot's settings say too.
// Read by the tests of this model only, for now.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn saving_words(lang: Lang) -> &'static str {
    t(lang, Words::BotsSaving)
}

/// *Routines are shown on {computer}.*: a remote Bot's Routines are not in
/// the copy (contract D17).
// Not drawn yet: the Routines pane's notice is a string key with no
// name in it (M8.LINKED-UI notes, "Not wired").
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn routines_words(lang: Lang, computer: &str) -> String {
    fill(t(lang, Words::BotsLinkedRoutinesOn), &[computer])
}

/// *This page is not on this computer.*
pub(crate) fn page_missing_words(lang: Lang) -> &'static str {
    t(lang, Words::BotsLinkedPageNotHere)
}

/// *This message is too large to copy. Open it on {computer}.*
pub(crate) fn too_large_words(lang: Lang, computer: &str) -> String {
    fill(t(lang, Words::BotsLinkedTooLarge), &[computer])
}

/// *{n} attachments are on {computer}.*, or `None` for none.
pub(crate) fn attachments_words(lang: Lang, count: u32, computer: &str) -> Option<String> {
    match count {
        0 => None,
        1 => Some(fill(t(lang, Words::BotsLinkedAttachmentsOne), &[computer])),
        many => Some(fill(
            t(lang, Words::BotsLinkedAttachments),
            &[
                koru_i18n::number::decimal(lang, u64::from(many)).as_str(),
                computer,
            ],
        )),
    }
}

/// **What a copied message did not carry**, as the copy's record says it:
/// its text was left out because it was too large, and how many attachments
/// stayed on the other computer. Plain values; nothing the Bot contract
/// answers today carries either, so no page is fed one yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Uncopied {
    pub message: koru_ai_core::MessageId,
    pub too_large: bool,
    pub attachments: u32,
}

/// The chat's note for one such message, or `None` when it carried all.
pub(crate) fn note(lang: Lang, uncopied: Uncopied, computer: &str) -> Option<CopyNote> {
    let instead = uncopied.too_large.then(|| too_large_words(lang, computer));
    let under = attachments_words(lang, uncopied.attachments, computer);
    (instead.is_some() || under.is_some()).then_some(CopyNote {
        message: uncopied.message,
        instead,
        under,
    })
}

/// What a read of the copy's state said of the page's Bot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Read {
    /// Not this page's read, or nothing that is drawn changed.
    Same,
    /// What the page says changed.
    Changed,
    /// **That computer's copy does not list this Bot**: the page is not a
    /// saved copy after all, and is an ordinary Bot's from here on.
    NotACopy,
}

/// **One remote Bot's page**: the computer that runs the Bot, its copy's
/// state as last read, and what the last command came to.
#[derive(Debug)]
pub(crate) struct CopyPage {
    owner: DeviceId,
    /// The copy's state and when it was last up to date. `None` until a
    /// read answers: nothing is said, and nothing is held, on a guess.
    state: Option<(LinkedOwnerState, Option<i64>)>,
    seq: u64,
    in_flight: bool,
    /// The state was read since the page was last shown.
    fresh: bool,
    next: Option<Instant>,
    /// The delivery the link noted for a refused command, by its request,
    /// just before the refusal itself.
    delivery: Option<(BotRequestId, String)>,
    /// What the last command came to, until the next one is answered.
    said: Option<Outcome>,
    /// What copied messages did not carry, as the host was told.
    uncopied: Vec<Uncopied>,
}

impl CopyPage {
    /// The page of a Bot `owner` runs. `known` is the copy's state when the
    /// roster's row already says it; otherwise the page reads it.
    pub(crate) fn new(owner: DeviceId, known: Option<(LinkedOwnerState, Option<i64>)>) -> Self {
        Self {
            owner,
            state: known,
            seq: 0,
            in_flight: false,
            fresh: false,
            next: None,
            delivery: None,
            said: None,
            uncopied: Vec::new(),
        }
    }

    pub(crate) fn owner(&self) -> DeviceId {
        self.owner
    }

    pub(crate) fn state(&self) -> Option<LinkedOwnerState> {
        self.state.map(|(state, _)| state)
    }

    /// Whether a command can be sent now: the copy is up to date and its
    /// computer connected.
    // Read by the tests of this model only, for now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn connected(&self) -> bool {
        self.state() == Some(LinkedOwnerState::Current)
    }

    /// **Whether the page holds every send**: the state is known and is not
    /// connected. A state not read yet holds nothing: the send would be
    /// refused and say so, which is truer than a hold on a guess.
    pub(crate) fn held(&self) -> bool {
        self.state()
            .is_some_and(|state| state != LinkedOwnerState::Current)
    }

    /// **What the roster's row says now**, when it lists this Bot: taken
    /// without a read of the page's own. `true` when what is drawn changed.
    pub(crate) fn know(
        &mut self,
        state: LinkedOwnerState,
        last_confirmed_at_ms: Option<i64>,
    ) -> bool {
        let known = Some((state, last_confirmed_at_ms));
        if self.state == known {
            return false;
        }
        self.state = known;
        true
    }

    // ------------------------------------------------------------ words

    /// **The sentence over the page** (contract 6.3): that this is a saved
    /// copy and as of when, or that the other computer's Koru needs an
    /// update. `None` while connected, and until the state is read.
    pub(crate) fn banner(
        &self,
        lang: Lang,
        computer: &str,
        when: impl Fn(i64) -> String,
    ) -> Option<String> {
        let (state, last) = self.state?;
        match state {
            LinkedOwnerState::Current => None,
            LinkedOwnerState::Unsupported => Some(super::unsupported_words(lang, computer)),
            LinkedOwnerState::Stale | LinkedOwnerState::Never | LinkedOwnerState::Withdrawing => {
                Some(match last {
                    Some(at) => fill(
                        t(lang, Words::BotsLinkedCopyBanner),
                        &[computer, when(at).as_str(), computer],
                    ),
                    None => fill(
                        t(lang, Words::BotsLinkedCopyBannerNoTime),
                        &[computer, computer],
                    ),
                })
            }
        }
    }

    /// **The composer's placeholder while nothing can be sent**: that the
    /// computer is not reachable and the draft is kept, or that its Koru
    /// needs an update. `None` while sending is open.
    pub(crate) fn placeholder(&self, lang: Lang, computer: &str) -> Option<String> {
        if !self.held() {
            return None;
        }
        Some(match self.state() {
            Some(LinkedOwnerState::Unsupported) => super::unsupported_words(lang, computer),
            _ => fill(t(lang, Words::BotsLinkedComposerHeld), &[computer]),
        })
    }

    /// What the last command came to, in words, until the next is answered.
    pub(crate) fn line(&self, lang: Lang, computer: &str) -> Option<String> {
        self.said
            .map(|outcome| outcome_words(lang, outcome, computer))
    }

    // Read by the tests of this model only, for now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn said(&self) -> Option<Outcome> {
        self.said
    }

    /// **What the conversation is told** (`ChatSurface::set_saved_copy`):
    /// whether it may send, what it says meanwhile, and the sentence each
    /// refusal about the other computer reads as, with whether the message
    /// comes back to the composer.
    pub(crate) fn surface(&self, lang: Lang, computer: &str) -> SavedCopy {
        let unreachable = |delivery: Option<&str>, draft, outcome| CopyOutcome {
            code: OWNER_UNREACHABLE.to_owned(),
            detail: delivery.map(|value| (DELIVERY.to_owned(), value.to_owned())),
            draft,
            words: outcome_words(lang, outcome, computer),
        };
        let coded = |code: &str, draft, outcome| CopyOutcome {
            code: code.to_owned(),
            detail: None,
            draft,
            words: outcome_words(lang, outcome, computer),
        };
        SavedCopy {
            held: self.placeholder(lang, computer),
            sending: sending_words(lang).to_owned(),
            outcomes: vec![
                // Nothing left this computer: the message is the person's
                // draft again, and the composer says where it is kept.
                CopyOutcome {
                    code: OWNER_UNREACHABLE.to_owned(),
                    detail: Some((DELIVERY.to_owned(), NOT_SENT.to_owned())),
                    draft: CopyDraft::Kept,
                    words: fill(t(lang, Words::BotsLinkedComposerHeld), &[computer]),
                },
                unreachable(Some(NOT_CONFIRMED), CopyDraft::Sent, Outcome::NotConfirmed),
                unreachable(Some(UNKNOWN), CopyDraft::Sent, Outcome::NotConfirmedFinal),
                // No delivery named: never read as not sent.
                unreachable(None, CopyDraft::Sent, Outcome::NotConfirmedFinal),
                coded(
                    ALREADY_ANSWERED,
                    CopyDraft::Sent,
                    Outcome::NotConfirmedFinal,
                ),
                coded(OWNER_UNSUPPORTED, CopyDraft::Kept, Outcome::NeedsUpdate),
                coded(
                    EXECUTING_COMPUTER_ONLY,
                    CopyDraft::Kept,
                    Outcome::ChangeThere,
                ),
            ],
            notes: self
                .uncopied
                .iter()
                .filter_map(|uncopied| note(lang, *uncopied, computer))
                .collect(),
            page_missing: page_missing_words(lang).to_owned(),
        }
    }

    /// What copied messages did not carry, as the host was told. `true`
    /// when it changed.
    // No product path reaches this yet: the copy's transcript carries
    // neither fact (M8.LINKED-UI notes, "Not wired").
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_uncopied(&mut self, uncopied: Vec<Uncopied>) -> bool {
        if self.uncopied == uncopied {
            return false;
        }
        self.uncopied = uncopied;
        true
    }

    // --------------------------------------------------------- commands

    /// The link noted how far a refused command got, just before its
    /// refusal.
    pub(crate) fn note_delivery(&mut self, request: BotRequestId, delivery: String) {
        self.delivery = Some((request, delivery));
    }

    /// **A command of this page was answered.** A refusal about the other
    /// computer is said ([`Self::line`]); any other answer clears what the
    /// last one said. `true` when what is said changed.
    pub(crate) fn wrote<T>(&mut self, request: BotRequestId, result: &Result<T, Failure>) -> bool {
        let delivery = self
            .delivery
            .take()
            .filter(|(noted, _)| *noted == request)
            .map(|(_, delivery)| delivery);
        let said = match result {
            Err(Failure::Refused(code)) => outcome(code, delivery.as_deref()),
            _ => None,
        };
        // A copy that was behind what was sent is worth reading again.
        if said.is_some() {
            self.fresh = false;
        }
        std::mem::replace(&mut self.said, said) != said
    }

    // ------------------------------------------------------------ reads

    /// Whether a read of the copy's state is owed: nothing read since the
    /// page was shown, or its clock came due.
    pub(crate) fn read_due(&self, now: Instant) -> bool {
        !self.in_flight && (!self.fresh || self.next.is_some_and(|at| at <= now))
    }

    /// A read was sent: its sequence number.
    pub(crate) fn reading(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.in_flight = true;
        self.fresh = true;
        self.next = None;
        self.seq
    }

    /// The read could not be queued: owed again on the next pass.
    pub(crate) fn unsent(&mut self) {
        self.in_flight = false;
        self.fresh = false;
    }

    /// The page was hidden: shown again, it reads once.
    pub(crate) fn hidden(&mut self) {
        self.fresh = false;
        self.next = None;
    }

    /// Whether an answer or a clock is owed.
    // Read by the tests of this model only, for now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn awaiting(&self) -> bool {
        self.in_flight || self.next.is_some()
    }

    /// When the state is read again.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next
    }

    /// Whether a read is on its way.
    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// **What a read answered**: the copy's state when that computer lists
    /// the Bot, `None` when it does not. A read that failed keeps what the
    /// page says and is asked again on the clock.
    pub(crate) fn take(
        &mut self,
        seq: u64,
        result: Result<Option<(LinkedOwnerState, Option<i64>)>, Failure>,
        now: Instant,
    ) -> Read {
        if seq != self.seq {
            return Read::Same;
        }
        self.in_flight = false;
        self.next = Some(now.checked_add(POLL).unwrap_or(now));
        match result {
            Ok(Some((state, last))) => {
                if self.know(state, last) {
                    Read::Changed
                } else {
                    Read::Same
                }
            }
            Ok(None) => Read::NotACopy,
            Err(_) => Read::Same,
        }
    }
}
