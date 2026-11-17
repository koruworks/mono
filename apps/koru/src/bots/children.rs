// SPDX-License-Identifier: MPL-2.0

//! **One Bot asks another, as the Bot's tab shows it** (M6, contract 7).
//!
//! A conversation of a Bot may have asked other Bots for work (its *request
//! cards*), and a conversation may itself have been started by another Bot's
//! request (its *origin*). This module holds both for the conversation a Bot
//! tab shows, reads them from the Bot owner, and says them in the reader's
//! words. The tab draws what it says above the conversation:
//!
//! ```text
//! col  Bot requests                       the band over the conversation, S2 between
//!   row  Child origin                     *Asked by {bot}*, *Open {bot}'s conversation*
//!   Child closed                          the closing line of a request settled without a handoff
//!   Child queued                          *{bot} is waiting for another bot…*, while one is open
//!   Child stale                           a read did not answer; asked again
//!   KoruStatusCard × n                    one per request (`koru_ui::status_card`)
//! ```
//!
//! **Where it reads from.** `ListChildRequests` and `ReadChildOrigin`
//! (contract 7.1), through a [`BotsLink`] of its own — a thread, never the
//! window's — asked only while the tab is shown and knows its Bot. A link of
//! its own keeps these reads out of the tab's own request sequence, which
//! the tab's other surfaces count on.
//!
//! **When it reads.** When the conversation is shown, when it changes, when
//! its work moved (a reply started or ended: `BotView::take_status_moved`),
//! and — only while a request is still open — again every [`OPEN_POLL`].
//! The delegate's `delegate.child.*` events reach no window (contract 8:
//! they only wake a reader in the AI worker), so a bounded poll while
//! something is open is the window's only way to see a request move.
//! Nothing is read for a hidden tab.
//!
//! **A read that did not answer is not a failure.** `bot_unavailable` — the
//! Bot's delegate did not answer the AI worker — is retryable (piece W), and
//! so is a lost connection: the cards that were shown stay, a quiet line says
//! they are being checked again, and the read is asked again on a bounded
//! clock. Nothing ever says a request failed because a read did not answer.
//!
//! **What it does not show** (corrections recorded in
//! `docs/features/bots/M6.UI.EXECUTION.notes.md`): the cards stand over the
//! conversation, not under the assistant message that asked — the shared
//! chat has no seam to anchor a host's element to one of its messages; the
//! card has no *Open result*, *in Trash* or *no longer available* — the card
//! the owner serves carries a page count, not a page or a version.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use koru_ai_core::ChatId;
use koru_ai_core::bots::{
    BotChildOrigin, BotChildOutcome, BotChildProgress, BotChildRequestCard, BotChildResult, BotId,
};
use koru_delegate_core::children::{ChildClock, ChildRequestState};
use koru_draw::el::{El, Key, Role, col, row, text};
use koru_draw::theme::{Ink, Metric, Space, roles};
use koru_i18n::{Key as Words, Lang, fill, t};
use koru_ui::status_card::{StatusCard, StatusCardKeys, StatusTone};

use super::link::{Answer, BotsLink, Command, Failure};

/// **Whether a Bot can ask another in this build.** A mirror of the gateway's
/// `CHILD_REQUESTS_AVAILABLE` (`koru-api-gateway-core`, `run_mcp/native/
/// children.rs`), which this crate cannot name: while it is `false` the
/// asking tools are not listed to any Bot, and the settings say the switch is
/// kept for later rather than promising it does anything. **P6.5 opens both
/// together.**
pub(crate) const ASKING_OPEN: bool = false;

/// How often an open request is read again while its conversation is shown.
pub(crate) const OPEN_POLL: Duration = Duration::from_secs(3);
/// The bounded clock a read that did not answer is asked again on.
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);
/// While a read is with the link: the tab's usual short tick.
const TICK: Duration = Duration::from_millis(50);

/// A request card's face.
pub(crate) const CARD_FACE: Metric = Metric::new("BOT_FACE_CARD", 24.0);

/// The surface every key below belongs to: the Bot tab's.
const SURFACE: &str = super::view::SURFACE;
/// The band over the conversation.
pub(crate) const BAND: Key = Key::surface(SURFACE, "child-band");
/// *Asked by {bot}*.
pub(crate) const ORIGIN: Key = Key::surface(SURFACE, "child-origin");
/// *Open {bot}'s conversation*.
pub(crate) const OPEN_PARENT: Key = Key::surface(SURFACE, "child-open-parent");
/// The closing line of a request settled without a handoff.
pub(crate) const CLOSED: Key = Key::surface(SURFACE, "child-closed");
/// *{bot} is waiting for another bot…*.
pub(crate) const QUEUED: Key = Key::surface(SURFACE, "child-queued");
/// *Couldn't check these requests just now…*.
pub(crate) const STALE: Key = Key::surface(SURFACE, "child-stale");
/// A card's parts, under its request's own id.
pub(crate) const CARD_PART: &str = "child-card";
pub(crate) const TITLE_PART: &str = "child-title";
pub(crate) const STATUS_PART: &str = "child-status";
pub(crate) const NOTE_PART: &str = "child-note";
pub(crate) const OPEN_PART: &str = "child-open";

const PARTS: [&str; 5] = [CARD_PART, TITLE_PART, STATUS_PART, NOTE_PART, OPEN_PART];

fn card_key(card: &BotChildRequestCard, part: &'static str) -> Key {
    Key::model(card.child_request_id.as_uuid().as_u128(), part)
}

/// A card's *Open conversation*.
pub(crate) fn open_key(card: &BotChildRequestCard) -> Key {
    card_key(card, OPEN_PART)
}

/// Whether `key` is one of the band's: what the tab publishes and routes.
pub(crate) fn is_band_key(key: Key) -> bool {
    [BAND, ORIGIN, OPEN_PARENT, CLOSED, QUEUED, STALE].contains(&key) || PARTS.contains(&key.part)
}

/// **A conversation the person asked to open** from the band: whose it is,
/// that Bot's name for its tab until it is read, and the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenRequest {
    pub bot: BotId,
    pub name: String,
    pub chat: ChatId,
}

/// **What one request's status line says**, and how it reads (contract 7.2).
/// The Needs-you line is the one with the attention treatment; a settled
/// request is quiet. A settled request whose ending is not read yet says it
/// is being checked — never a guess, never *failed*.
pub(crate) fn status_words(card: &BotChildRequestCard, lang: Lang) -> (String, StatusTone) {
    let name = card.recipient_name.trim();
    let named = |key: Words| fill(t(lang, key), &[name]);
    match card.state {
        ChildRequestState::Open => {
            if card.needs_you || card.child == BotChildProgress::NeedsYou {
                return (named(Words::BotsChildNeedsYou), StatusTone::Attention);
            }
            let key = match card.child {
                BotChildProgress::Queued => Words::BotsChildQueued,
                // A child that finished while its request is still being
                // settled is still working on it, as far as the asker goes.
                BotChildProgress::Running
                | BotChildProgress::Finished
                | BotChildProgress::NeedsYou => Words::BotsChildWorking,
            };
            (named(key), StatusTone::Plain)
        }
        ChildRequestState::Settled => match card.outcome {
            Some(BotChildOutcome::Completed) => {
                let saved = matches!(card.result, BotChildResult::Saved { pages } if pages > 0);
                let key = if saved {
                    Words::BotsChildSaved
                } else {
                    Words::BotsChildNoPage
                };
                (t(lang, key).to_owned(), StatusTone::Settled)
            }
            Some(BotChildOutcome::Failed) => (named(Words::BotsChildFailed), StatusTone::Settled),
            Some(BotChildOutcome::NeededYou) => {
                (named(Words::BotsChildNeededYou), StatusTone::Settled)
            }
            Some(BotChildOutcome::Canceled) => {
                (named(Words::BotsChildStoppedByYou), StatusTone::Settled)
            }
            Some(BotChildOutcome::TimedOut) => {
                (named(Words::BotsChildTimedOut), StatusTone::Settled)
            }
            Some(BotChildOutcome::ParentStopped) => (
                t(lang, Words::BotsChildParentEnded).to_owned(),
                StatusTone::Settled,
            ),
            None => (
                t(lang, Words::BotsChildChecking).to_owned(),
                StatusTone::Plain,
            ),
        },
    }
}

/// *Paused* under a card while its clock is held: the asking Bot is paused,
/// and its wait has no time limit until it is resumed (decision 14).
pub(crate) fn note_words(card: &BotChildRequestCard, lang: Lang) -> Option<String> {
    (card.state == ChildRequestState::Open && card.clock == ChildClock::Paused)
        .then(|| t(lang, Words::BotsPaused).to_owned())
}

/// **The closing line of a child conversation** whose request settled
/// without a handoff (contract 7.3): after the 15 minutes, or because the
/// asking task ended. Nothing for any other ending.
pub(crate) fn closing_words(origin: &BotChildOrigin, lang: Lang) -> Option<String> {
    if origin.state != ChildRequestState::Settled {
        return None;
    }
    let key = match origin.outcome? {
        BotChildOutcome::TimedOut => Words::BotsChildClosedTimedOut,
        BotChildOutcome::ParentStopped => Words::BotsChildClosedParentEnded,
        _ => return None,
    };
    Some(fill(t(lang, key), &[origin.parent_name.trim()]))
}

/// Where a read stands.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct Asking {
    cards: Option<u64>,
    origin: Option<u64>,
}

/// **The requests and the origin of the conversation a Bot tab shows.**
#[derive(Default)]
pub(crate) struct Children {
    link: Option<BotsLink>,
    started: bool,
    /// Whose conversation, which, and whether it is the Bot's main one —
    /// which no request starts, so its origin is never read.
    bot: Option<BotId>,
    chat: Option<ChatId>,
    main: bool,
    shown: bool,
    lang: Lang,
    /// The tab's own Bot's name, for the line said while it waits.
    bot_name: String,
    /// The faces the window knows, by Bot, for the cards' leads.
    faces: HashMap<BotId, u8>,
    seq: u64,
    asking: Asking,
    /// A read is owed on the next pass: shown, the conversation changed, or
    /// its work moved.
    owed: bool,
    /// The bounded clock: an open request's next read, or a retry.
    next: Option<Instant>,
    backoff: Duration,
    /// The last read of the cards did not answer; what was shown stays.
    stale: bool,
    cards: Vec<BotChildRequestCard>,
    origin: Option<BotChildOrigin>,
    /// An origin the owner just answered, for the tab's conversation list:
    /// the conversation and the asking Bot's name. Taken once.
    origin_news: Option<(ChatId, String)>,
    /// What was handed to the link, for a test to count.
    #[cfg(test)]
    pub(crate) sent: usize,
}

impl Children {
    /// The link, once, the first time the tab shows a known Bot. `None`
    /// under test: [`BotsLink::start`] answers `None` there.
    pub(crate) fn start(&mut self, root: Option<PathBuf>) {
        if self.started {
            return;
        }
        self.started = true;
        self.link = BotsLink::start(root);
    }

    /// Over a link a test holds the other end of.
    #[cfg(test)]
    pub(crate) fn with_link(link: BotsLink) -> Self {
        Self {
            started: true,
            link: Some(link),
            ..Self::default()
        }
    }

    /// **Which conversation, of which Bot.** Another conversation's cards are
    /// never this one's: a change empties them, drops any answer still on its
    /// way, and reads again. `true` when it changed.
    pub(crate) fn set_subject(&mut self, bot: BotId, chat: ChatId, main: ChatId) -> bool {
        if self.bot == Some(bot) && self.chat == Some(chat) {
            self.main = chat == main;
            return false;
        }
        self.bot = Some(bot);
        self.chat = Some(chat);
        self.main = chat == main;
        self.seq = self.seq.wrapping_add(1);
        self.asking = Asking::default();
        self.cards.clear();
        self.origin = None;
        self.stale = false;
        self.next = None;
        self.backoff = RETRY_FIRST;
        self.owed = true;
        true
    }

    /// The words the band speaks, and its Bot's name. `true` when either
    /// moved.
    pub(crate) fn set_words(&mut self, lang: Lang, bot_name: &str) -> bool {
        if self.lang == lang && self.bot_name == bot_name {
            return false;
        }
        self.lang = lang;
        self.bot_name = bot_name.to_owned();
        true
    }

    /// The faces the window's roster lists. `true` when they changed.
    pub(crate) fn set_faces(&mut self, faces: impl IntoIterator<Item = (BotId, u8)>) -> bool {
        let faces: HashMap<BotId, u8> = faces.into_iter().collect();
        if faces == self.faces {
            return false;
        }
        self.faces = faces;
        true
    }

    /// **Whether the tab is shown.** Hidden, nothing is read and no clock
    /// runs; shown again, it reads once.
    pub(crate) fn show(&mut self, shown: bool) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if shown {
            self.owed = true;
        } else {
            self.next = None;
            self.backoff = RETRY_FIRST;
        }
    }

    /// The conversation's work moved: read again on the next pass.
    pub(crate) fn owe_read(&mut self) {
        self.owed = true;
    }

    /// The cards the band draws: the requests of the conversation's latest
    /// ask, and any request still open from an earlier one. The owner lists
    /// the newest 60, oldest first; a conversation that asked many times
    /// would otherwise stack every card it ever made over its transcript.
    pub(crate) fn shown_cards(&self) -> Vec<&BotChildRequestCard> {
        let Some(latest) = self
            .cards
            .last()
            .map(|card| card.parent_assistant_message_id)
        else {
            return Vec::new();
        };
        self.cards
            .iter()
            .filter(|card| {
                card.parent_assistant_message_id == latest || card.state == ChildRequestState::Open
            })
            .collect()
    }

    /// Every card the owner answered for this conversation. Test only.
    #[cfg(test)]
    pub(crate) fn cards(&self) -> &[BotChildRequestCard] {
        &self.cards
    }

    /// The origin, when another Bot's request started this conversation.
    /// Test only: the band draws it.
    #[cfg(test)]
    pub(crate) fn origin(&self) -> Option<&BotChildOrigin> {
        self.origin.as_ref()
    }

    /// **The conversation waits on another Bot**: one of its requests is
    /// still open, so a message the person sends now waits in the Bot's
    /// queue (contract 7.4).
    pub(crate) fn waiting(&self) -> bool {
        self.cards
            .iter()
            .any(|card| card.state == ChildRequestState::Open)
    }

    /// Whether the band has anything to say.
    pub(crate) fn shows(&self) -> bool {
        !self.cards.is_empty() || self.origin.is_some()
    }

    /// An origin the owner just answered, taken once.
    pub(crate) fn take_origin_news(&mut self) -> Option<(ChatId, String)> {
        self.origin_news.take()
    }

    /// **The pass a shown tab runs**: fold what the link answered, then ask
    /// what is owed or due. `true` when what the band says changed.
    pub(crate) fn pass(&mut self, now: Instant) -> bool {
        if !self.shown || self.link.is_none() {
            return false;
        }
        let changed = self.fold(now);
        let due = self.next.is_some_and(|at| at <= now);
        let idle = self.asking == Asking::default();
        if idle && (self.owed || due) {
            self.ask();
        }
        changed
    }

    /// When the next pass is owed: a short tick while a read is with the
    /// link or one is owed, else the clock. Nothing while hidden or with no
    /// link to ask.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        if !self.shown || self.link.is_none() || self.bot.is_none() {
            return None;
        }
        if self.asking != Asking::default() || self.owed {
            return now.checked_add(TICK);
        }
        self.next
    }

    fn ask(&mut self) {
        let (Some(link), Some(bot), Some(chat)) = (&self.link, self.bot, self.chat) else {
            return;
        };
        let seq = self.seq;
        if !link.send(Command::ChildRequests { seq, bot, chat }) {
            // The queue is full: asked again on a later pass.
            return;
        }
        self.asking.cards = Some(seq);
        #[cfg(test)]
        {
            self.sent += 1;
        }
        // The main conversation is never started by a request.
        if !self.main && link.send(Command::ChildOrigin { seq, bot, chat }) {
            self.asking.origin = Some(seq);
            #[cfg(test)]
            {
                self.sent += 1;
            }
        }
        // An origin the queue would not take is asked with the next read.
        self.owed = !self.main && self.asking.origin.is_none();
        self.next = None;
    }

    fn fold(&mut self, now: Instant) -> bool {
        let Some(link) = &self.link else {
            return false;
        };
        let mut changed = false;
        for answer in link.drain() {
            match answer {
                Answer::ChildRequests {
                    seq,
                    bot,
                    chat,
                    result,
                } if self.asking.cards == Some(seq)
                    && Some(bot) == self.bot
                    && Some(chat) == self.chat =>
                {
                    self.asking.cards = None;
                    changed |= self.take_cards(result, now);
                }
                Answer::ChildOrigin {
                    seq,
                    bot,
                    chat,
                    result,
                } if self.asking.origin == Some(seq)
                    && Some(bot) == self.bot
                    && Some(chat) == self.chat =>
                {
                    self.asking.origin = None;
                    changed |= self.take_origin(chat, result, now);
                }
                // An answer about another conversation, or an older ask.
                _ => {}
            }
        }
        changed
    }

    fn take_cards(
        &mut self,
        result: Result<Vec<BotChildRequestCard>, Failure>,
        now: Instant,
    ) -> bool {
        match result {
            Ok(cards) => {
                let changed = self.cards != cards || self.stale;
                self.cards = cards;
                self.stale = false;
                self.backoff = RETRY_FIRST;
                // Only an open request is read again.
                self.next = (self.waiting() || self.origin_open())
                    .then(|| now.checked_add(OPEN_POLL))
                    .flatten();
                changed
            }
            Err(failure) => {
                let retry = failure.again() || failure.unvouched();
                let stale = retry && !self.cards.is_empty();
                let changed = self.stale != stale;
                self.stale = stale;
                if retry {
                    self.retry(now);
                }
                changed
            }
        }
    }

    fn take_origin(
        &mut self,
        chat: ChatId,
        result: Result<Option<BotChildOrigin>, Failure>,
        now: Instant,
    ) -> bool {
        match result {
            Ok(origin) => {
                if let Some(origin) = &origin {
                    self.origin_news = Some((chat, origin.parent_name.clone()));
                }
                let changed = self.origin != origin;
                self.origin = origin;
                if self.origin_open() && self.next.is_none() {
                    self.next = now.checked_add(OPEN_POLL);
                }
                changed
            }
            Err(failure) => {
                if failure.again() || failure.unvouched() {
                    self.retry(now);
                }
                false
            }
        }
    }

    fn origin_open(&self) -> bool {
        self.origin
            .as_ref()
            .is_some_and(|origin| origin.state == ChildRequestState::Open)
    }

    fn retry(&mut self, now: Instant) {
        let wait = self.backoff.max(RETRY_FIRST);
        let at = now.checked_add(wait).unwrap_or(now);
        self.next = Some(self.next.map_or(at, |next| next.min(at)));
        self.backoff = (wait * 2).min(RETRY_MAX);
    }

    /// Whether the last read did not answer while cards were shown. Test
    /// only: the band draws it.
    #[cfg(test)]
    pub(crate) fn stale(&self) -> bool {
        self.stale
    }

    // ---------------------------------------------------------- words

    /// **The words of one of the band's lines**, by its key: what the
    /// accessibility tree publishes for it. `None` for a key not the band's.
    pub(crate) fn line_words(&self, key: Key) -> Option<String> {
        let lang = self.lang;
        match key {
            k if k == ORIGIN => {
                let origin = self.origin.as_ref()?;
                return Some(fill(
                    t(lang, Words::BotsChildAskedBy),
                    &[origin.parent_name.trim()],
                ));
            }
            k if k == CLOSED => return closing_words(self.origin.as_ref()?, lang),
            k if k == QUEUED => {
                return self.waiting().then(|| self.queued_words());
            }
            k if k == STALE => {
                return self
                    .stale
                    .then(|| t(lang, Words::BotsChildStale).to_owned());
            }
            _ => {}
        }
        let card = self
            .cards
            .iter()
            .find(|card| PARTS.iter().any(|part| card_key(card, part) == key))?;
        match key.part {
            TITLE_PART => Some(fill(
                t(lang, Words::BotsChildAsked),
                &[card.recipient_name.trim()],
            )),
            STATUS_PART => Some(status_words(card, lang).0),
            NOTE_PART => note_words(card, lang),
            _ => None,
        }
    }

    fn queued_words(&self) -> String {
        fill(
            t(self.lang, Words::BotsWaitingQueued),
            &[self.bot_name.trim()],
        )
    }

    /// **What activating `key` asks for**: the conversation a card or the
    /// origin line leads to. `None` for anything else.
    pub(crate) fn activate(&self, key: Key) -> Option<OpenRequest> {
        if key == OPEN_PARENT {
            let origin = self.origin.as_ref()?;
            return Some(OpenRequest {
                bot: origin.parent_bot_id,
                name: origin.parent_name.clone(),
                chat: origin.parent_chat_id,
            });
        }
        if key.part != OPEN_PART {
            return None;
        }
        let card = self.cards.iter().find(|card| open_key(card) == key)?;
        Some(OpenRequest {
            bot: card.recipient_bot_id,
            name: card.recipient_name.clone(),
            chat: card.child_chat_id,
        })
    }

    /// Whether `key` is one of the band's links.
    pub(crate) fn is_link(key: Key) -> bool {
        key == OPEN_PARENT || key.part == OPEN_PART
    }

    // -------------------------------------------------------- elements

    /// **The band as elements**, or `None` when it has nothing to say.
    /// `button` builds one of the tab's own buttons — the tab's focus and
    /// pointer state, its leaf — from its name, key and label.
    pub(crate) fn band(
        &self,
        button: &mut dyn FnMut(&'static str, Key, String) -> El,
    ) -> Option<El> {
        if !self.shows() {
            return None;
        }
        let lang = self.lang;
        let mut band = col("Bot requests")
            .id(BAND)
            .pad_x(koru_draw::theme::alias::NAV_PAD)
            .pad_y(Space::S2)
            .gap(Space::S2)
            .semantics(Role::Group)
            .name(Words::BotsChildRequests);
        if let Some(origin) = &self.origin {
            let name = origin.parent_name.trim();
            band = band.child(
                row("Child origin")
                    .gap(Space::S2)
                    .items_center()
                    .child(
                        text(fill(t(lang, Words::BotsChildAskedBy), &[name]))
                            .named("Child asked by")
                            .id(ORIGIN)
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .fill()
                            .wrap(),
                    )
                    .child(button(
                        "Child open parent",
                        OPEN_PARENT,
                        fill(t(lang, Words::BotsChildOpenParent), &[name]),
                    )),
            );
            if let Some(closing) = closing_words(origin, lang) {
                band = band.child(
                    text(closing)
                        .named("Child closed")
                        .id(CLOSED)
                        .role(roles::META)
                        .ink(Ink::Hint)
                        .wrap(),
                );
            }
        }
        if self.waiting() {
            band = band.child(
                text(self.queued_words())
                    .named("Child queued")
                    .id(QUEUED)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap(),
            );
        }
        if self.stale {
            band = band.child(
                text(t(lang, Words::BotsChildStale))
                    .named("Child stale")
                    .id(STALE)
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        for card in self.shown_cards() {
            band = band.child(self.card(card, button));
        }
        Some(band)
    }

    fn card(
        &self,
        card: &BotChildRequestCard,
        button: &mut dyn FnMut(&'static str, Key, String) -> El,
    ) -> El {
        let lang = self.lang;
        let name = card.recipient_name.trim();
        let (status, tone) = status_words(card, lang);
        let face = self
            .faces
            .get(&card.recipient_bot_id)
            .copied()
            .unwrap_or_else(|| koru_ai_core::bots::bot_avatar(None));
        let lead = koru_draw::el::leaf(
            "Child face",
            super::faces::FaceLeaf::still(face, name, CARD_FACE),
        )
        .decorative();
        let open = button(
            "Child open conversation",
            open_key(card),
            t(lang, Words::BotsChildOpenConversation).to_owned(),
        );
        StatusCard {
            name: fill(t(lang, Words::BotsChildCardName), &[name]),
            title: fill(t(lang, Words::BotsChildAsked), &[name]),
            status,
            tone,
            note: note_words(card, lang),
        }
        .el(
            StatusCardKeys {
                card: card_key(card, CARD_PART),
                title: card_key(card, TITLE_PART),
                status: card_key(card, STATUS_PART),
                note: card_key(card, NOTE_PART),
            },
            Some(lead),
            vec![open],
        )
    }

    /// A test standing in for an answer the owner gave.
    #[cfg(test)]
    pub(crate) fn know_for_test(
        &mut self,
        cards: Vec<BotChildRequestCard>,
        origin: Option<BotChildOrigin>,
    ) {
        self.cards = cards;
        self.origin = origin;
    }
}

#[cfg(test)]
mod tests;
