// SPDX-License-Identifier: MPL-2.0

//! **Pause all Bots**: what the roster's control says and may do, from
//! what the native engine last answered.
//!
//! This is the model only. It asks nothing and sends nothing: the roster
//! hands it each answer and takes from it the change to send. It is fed
//! plain values, so every state of the control is reachable in a test
//! without a service.
//!
//! Two rules it is built under:
//!
//! - **Never an unconfirmed state.** The control says *Resume all Bots* only
//!   once the computer's Koru confirmed the pause, and *Pause all Bots* only
//!   once it confirmed the resume. Between a press and that confirmation it
//!   says *Pausing…* or *Resuming…*, disabled, with the line that says what
//!   it waits for. No timeout turns waiting into done.
//! - **The pause is the person's.** Pausing asks first (the window's
//!   question); resuming does not.

use std::time::{Duration, Instant};

use koru_ai_core::bots::{BotRequestId, TeamPauseStage, TeamPauseView};
use koru_i18n::Key as Words;

use super::link::Failure;

/// The owner's refusal of new work while every Bot is paused.
pub(crate) const TEAM_PAUSED: &str = "team_paused";

/// The owner's refusal of a change that named another revision than the
/// one it holds: somebody else changed the team first. Read again.
pub(crate) const REVISION_CONFLICT: &str = "team_revision_conflict";

const FIRST: Duration = Duration::from_millis(250);
const MAX: Duration = Duration::from_secs(2);

/// **The question asked before every Bot is paused** (contract 6.1): what
/// the pause holds, and that running work keeps going. Pausing is
/// reversible and stops nothing, so the answer is not drawn as a
/// destructive one; Cancel is still what an Enter in flight reaches.
///
/// Two of the contract's sentences are left out because they would not be
/// true in this build: that running work can still use its connections
/// (private connections are closed here), and that a Bot waiting for an
/// answer will not continue (the only questions a Bot can ask here are its
/// agent's own approvals, whose answer takes effect at once).
pub(crate) fn question(lang: koru_i18n::Lang) -> koru_ui::Confirm {
    let body = format!(
        "{}

{}",
        koru_i18n::t(lang, Words::BotsTeamPauseBodyNoNew),
        koru_i18n::t(lang, Words::BotsTeamPauseBodyRunning),
    );
    koru_ui::Confirm::asking(
        koru_i18n::t(lang, Words::BotsTeamPauseTitle).to_owned(),
        body,
        koru_i18n::t(lang, Words::BotsTeamPauseAll).to_owned(),
        koru_i18n::t(lang, Words::BotsDialogCancel),
    )
}

/// What the control shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TeamState {
    /// Nothing was read yet: no control is drawn.
    Unknown,
    NotPaused,
    /// A pause was asked for and is not confirmed.
    Pausing,
    Paused,
    /// A resume was asked for and is not confirmed.
    Resuming,
}

/// The control as drawn: its words, and whether it can be pressed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Control {
    pub words: Words,
    pub enabled: bool,
}

/// One change to send: `pause_team` or `resume_team` at the revision read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub request: BotRequestId,
    pub paused: bool,
    pub expected: u64,
}

#[derive(Default)]
pub(crate) struct TeamPause {
    view: Option<TeamPauseView>,
    /// A change that was sent and that no answer has settled.
    sent: Option<Change>,
    /// The newest read's sequence when that change was sent: only a read
    /// asked after it can say whether the change landed.
    sent_after: u64,
    /// The last change could not be made; said under the control until the
    /// next press or the next change.
    failed: bool,
    /// The state was read since the roster was last shown.
    fresh: bool,
    in_flight: bool,
    seq: u64,
    /// When a change that is not confirmed is read again.
    next: Option<Instant>,
    backoff: Duration,
}

impl TeamPause {
    pub(crate) fn state(&self) -> TeamState {
        if let Some(change) = &self.sent {
            return if change.paused {
                TeamState::Pausing
            } else {
                TeamState::Resuming
            };
        }
        match &self.view {
            None => TeamState::Unknown,
            Some(view) => match (view.stage, view.paused) {
                (TeamPauseStage::Pending, true) => TeamState::Pausing,
                (TeamPauseStage::Pending, false) => TeamState::Resuming,
                (TeamPauseStage::Acknowledged, true) => TeamState::Paused,
                (TeamPauseStage::Acknowledged, false) => TeamState::NotPaused,
            },
        }
    }

    /// The control, or `None` while nothing is known.
    pub(crate) fn control(&self) -> Option<Control> {
        let (words, enabled) = match self.state() {
            TeamState::Unknown => return None,
            TeamState::NotPaused => (Words::BotsTeamPauseAll, true),
            TeamState::Pausing => (Words::BotsTeamPausing, false),
            TeamState::Paused => (Words::BotsTeamResumeAll, true),
            TeamState::Resuming => (Words::BotsTeamResuming, false),
        };
        Some(Control { words, enabled })
    }

    /// The line under the control.
    pub(crate) fn line(&self) -> Option<Words> {
        match self.state() {
            TeamState::Pausing | TeamState::Resuming => Some(Words::BotsTeamWaitingConfirm),
            _ if self.failed => Some(Words::BotsTeamChangeFailed),
            TeamState::Paused => Some(Words::BotsTeamPausedLine),
            TeamState::Unknown | TeamState::NotPaused => None,
        }
    }

    /// Whether each Bot row says *Paused with the team*: from the confirmed
    /// pause until the resume is confirmed. A Bot paused on its own says
    /// *Paused* instead; that is the row's to decide.
    pub(crate) fn holds_bots(&self) -> bool {
        matches!(self.state(), TeamState::Paused | TeamState::Resuming)
    }

    /// Whether a press of the control asks the person first.
    pub(crate) fn press_asks(&self) -> bool {
        self.state() == TeamState::NotPaused
    }

    /// The change a press (or the question's yes) sends, when the control
    /// is in the state that offers it. The caller sends it and says so with
    /// [`Self::sent`] or [`Self::not_sent`].
    pub(crate) fn begin(&mut self, paused: bool) -> Option<Change> {
        let offered = match self.state() {
            TeamState::NotPaused => paused,
            TeamState::Paused => !paused,
            _ => false,
        };
        let view = self.view.as_ref().filter(|_| offered)?;
        self.failed = false;
        Some(Change {
            request: BotRequestId::mint(),
            paused,
            expected: view.revision,
        })
    }

    pub(crate) fn sent(&mut self, change: Change) {
        self.sent = Some(change);
        self.sent_after = self.seq;
        self.backoff = FIRST;
        self.schedule(Instant::now());
    }

    /// The change could not even be queued.
    pub(crate) fn not_sent(&mut self) {
        self.failed = true;
    }

    /// A read is owed: nothing was read since shown, or an unconfirmed
    /// change's clock came due.
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

    /// The roster was hidden: shown again, it reads once.
    pub(crate) fn hidden(&mut self) {
        self.fresh = false;
        self.next = None;
    }

    /// Whether an answer or a clock is owed: the window keeps waking.
    pub(crate) fn awaiting(&self) -> bool {
        self.in_flight || self.sent.is_some() || self.next.is_some()
    }

    /// What a read answered. An older sequence is not this read's.
    pub(crate) fn read(&mut self, seq: u64, result: Result<TeamPauseView, Failure>) -> bool {
        if seq != self.seq {
            return false;
        }
        self.in_flight = false;
        match result {
            Ok(view) => {
                // A change whose own answer was lost: the read says whether
                // it landed. A revision past the one it named means the team
                // moved; the same revision means it did not.
                if let Some(change) = &self.sent
                    && seq > self.sent_after
                {
                    if view.revision == change.expected {
                        self.failed = true;
                    }
                    self.sent = None;
                }
                self.take(view);
            }
            // What was shown stays; asked again on the clock.
            Err(_) => {
                self.backoff = self.backoff.max(FIRST);
                let now = Instant::now();
                self.next = Some(now.checked_add(self.backoff).unwrap_or(now));
                self.backoff = (self.backoff * 2).min(MAX);
                return true;
            }
        }
        self.schedule(Instant::now());
        true
    }

    /// What a change answered, by its request.
    pub(crate) fn changed(
        &mut self,
        request: BotRequestId,
        result: Result<TeamPauseView, Failure>,
    ) -> bool {
        if self.sent.as_ref().map(|change| change.request) != Some(request) {
            return false;
        }
        match result {
            Ok(view) => {
                self.sent = None;
                self.take(view);
            }
            // Another window changed the team first: nothing failed, the
            // state is simply read again.
            Err(Failure::Refused(code)) if code == REVISION_CONFLICT => {
                self.sent = None;
                self.fresh = false;
            }
            Err(Failure::Refused(_)) => {
                self.sent = None;
                self.failed = true;
            }
            // No answer says what happened: the change may have been made.
            // It stays *Pausing…* or *Resuming…* until a read says.
            Err(Failure::Unanswered) => self.fresh = false,
        }
        self.schedule(Instant::now());
        true
    }

    fn take(&mut self, view: TeamPauseView) {
        // An answer older than what is shown never replaces it.
        if self
            .view
            .as_ref()
            .is_none_or(|shown| view.revision >= shown.revision)
        {
            self.view = Some(view);
        }
    }

    /// While a change is not confirmed, read again: 250 ms doubling to two
    /// seconds, for as long as it takes.
    fn schedule(&mut self, now: Instant) {
        if matches!(self.state(), TeamState::Pausing | TeamState::Resuming) {
            let wait = self.backoff.max(FIRST);
            self.next = Some(now.checked_add(wait).unwrap_or(now));
            self.backoff = (wait * 2).min(MAX);
        } else {
            self.next = None;
            self.backoff = FIRST;
        }
    }

    #[cfg(test)]
    pub(crate) fn next_read_for_test(&self) -> Option<Instant> {
        self.next
    }
}

#[cfg(test)]
mod tests;
