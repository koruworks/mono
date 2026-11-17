// SPDX-License-Identifier: MPL-2.0

//! **Bots another of the person's computers runs** (Bots M8, P8.3; contract
//! 6.3 to 6.5): what the roster and a Bot's page say about them, from what
//! the Bot service last answered.
//!
//! A linked desktop keeps a **copy** of the Bots its other computers run.
//! The copy is read like a local Bot, through the same commands; a change is
//! sent to the computer that runs the Bot, which is the only one that may
//! make it. This module is the words and the states of that:
//!
//! - **the roster's groups** ([`Linked`], [`Computer`], [`Heading`]): one
//!   heading per computer, *On {computer}*, with how current its copy is,
//!   the Bots it runs, and how many it does not copy;
//! - **a remote Bot's page** ([`page`]): the saved-copy sentence, the
//!   composer that keeps its draft, and what a command came to.
//!
//! **It is the model only.** It asks nothing and sends nothing: the roster
//! and a Bot's tab hand it each answer of their link's thread and take from
//! it what to read next. It is fed plain values, so every state is reachable
//! in a test without a service.
//!
//! **Nothing here guesses.** A computer's name is the one the window's own
//! list of linked devices gives ([`Names`]); the copy carries none. A state
//! the service does not answer — whether a linked conversation is running —
//! is not drawn at all.
//!
//! **With no copy, nothing is drawn.** A Koru that keeps no copy answers the
//! linked read with no computers; the roster then holds no group and looks
//! exactly as it did before M8 (`m8_ui_linked_roster_no_linked_bots_is_todays_roster`).

pub(crate) mod page;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use koru_ai_core::bots::{
    BotCommand, BotId, BotResponse, DeviceId, LinkedBotsView, LinkedOwnerState, LinkedOwnerView,
    TeamPauseStage, TeamPauseView,
};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;

use super::link::{Failure, Said};
use super::roster::RosterBot;

/// **Whether this build says that Bot chats are copied to linked desktops**
/// (contract 6.5, D22). A mirror of the gateway's
/// `LINKED_BOT_CONTROL_AVAILABLE` (`koru-api-gateway`, `composition.rs`),
/// which this crate cannot name: while that is `false` nothing is copied
/// anywhere, and a sentence saying it is would not be true. **Whoever opens
/// that switch opens this one in the same change: the product owner opened
/// both on 2026-11-17.**
pub(crate) const COPIES_DISCLOSED: bool = true;

/// The Bot service's refusals that are about another computer
/// (`koru_ai_wire::bot_linked_codes`, which this crate does not name).
pub(crate) const EXECUTING_COMPUTER_ONLY: &str = "executing_computer_only";
pub(crate) const OWNER_UNREACHABLE: &str = "bot_owner_unreachable";
pub(crate) const OWNER_UNSUPPORTED: &str = "bot_owner_unsupported";
pub(crate) const ALREADY_ANSWERED: &str = "bot_request_already_answered";
/// The detail of [`OWNER_UNREACHABLE`] that says how far a command got, and
/// its three values.
pub(crate) const DELIVERY: &str = "delivery";
pub(crate) const NOT_SENT: &str = "not_sent";
pub(crate) const NOT_CONFIRMED: &str = "not_confirmed";
pub(crate) const UNKNOWN: &str = "unknown";

/// The most computers one read asks about: the owner's own bound on the
/// devices it serves at once (contract D18).
pub(crate) const MAX_COMPUTERS: usize = 8;

/// How often the copies' states are read again while they are shown: a
/// computer going out of reach must stop saying *Connected* without the
/// person doing anything. Only while a linked computer is listed; a Koru
/// with none reads once per showing and keeps no clock.
pub(crate) const POLL: Duration = Duration::from_secs(5);

/// **One other computer, as this one last read it**: how current its copy
/// is, and the Bots of it whose home is the roster's workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Computer {
    pub device: DeviceId,
    pub state: LinkedOwnerState,
    /// When the copy was last known to be up to date.
    pub last_confirmed_at_ms: Option<i64>,
    /// That computer's team pause as last received.
    pub team: Option<TeamPauseView>,
    /// How many of its Bots it does not copy: their workspace is kept on
    /// that computer only. No name is known here.
    pub withheld: u32,
    pub bots: Vec<RosterBot>,
}

/// **What a computer's heading says** beside its name (contract 6.3).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Heading {
    /// A subscription is open and the copy is up to date.
    Connected,
    /// Not reachable; the copy is as of this time.
    LastUpdated(i64),
    /// Not reachable, and no time is known.
    NotReachable,
    /// The two computers share no version.
    NeedsUpdate,
    /// Nothing was received from it yet.
    NothingYet,
}

/// The heading state for a copy's `state`, or `None` for a computer that is
/// not drawn: one that is being forgotten.
pub(crate) fn heading(
    state: LinkedOwnerState,
    last_confirmed_at_ms: Option<i64>,
) -> Option<Heading> {
    Some(match state {
        LinkedOwnerState::Current => Heading::Connected,
        LinkedOwnerState::Stale => match last_confirmed_at_ms {
            Some(at) => Heading::LastUpdated(at),
            None => Heading::NotReachable,
        },
        LinkedOwnerState::Unsupported => Heading::NeedsUpdate,
        LinkedOwnerState::Never => Heading::NothingYet,
        LinkedOwnerState::Withdrawing => return None,
    })
}

/// A heading state in words. `when` says a time as the window says times.
pub(crate) fn heading_words(lang: Lang, heading: Heading, when: impl Fn(i64) -> String) -> String {
    match heading {
        Heading::Connected => t(lang, Words::BotsLinkedConnected).to_owned(),
        Heading::LastUpdated(at) => {
            koru_i18n::fill(t(lang, Words::BotsLinkedLastUpdated), &[when(at).as_str()])
        }
        Heading::NotReachable => t(lang, Words::BotsLinkedNotReachable).to_owned(),
        Heading::NeedsUpdate => t(lang, Words::BotsLinkedNeedsUpdate).to_owned(),
        Heading::NothingYet => t(lang, Words::BotsLinkedNothingYet).to_owned(),
    }
}

/// *On {computer}*.
pub(crate) fn title(lang: Lang, computer: &str) -> String {
    koru_i18n::fill(t(lang, Words::BotsLinkedOn), &[computer])
}

/// The line under a heading for the Bots a computer does not copy, or
/// `None` when it copies them all.
pub(crate) fn withheld_words(lang: Lang, withheld: u32, computer: &str) -> Option<String> {
    match withheld {
        0 => None,
        1 => Some(koru_i18n::fill(
            t(lang, Words::BotsLinkedWithheldOne),
            &[computer],
        )),
        many => Some(koru_i18n::fill(
            t(lang, Words::BotsLinkedWithheld),
            &[
                koru_i18n::number::decimal(lang, u64::from(many)).as_str(),
                computer,
            ],
        )),
    }
}

/// *Koru on {computer} needs an update to show its Bots here.*
pub(crate) fn unsupported_words(lang: Lang, computer: &str) -> String {
    koru_i18n::fill(t(lang, Words::BotsLinkedUnsupported), &[computer])
}

/// **The disclosure where linking is explained and in the Bots view's
/// first-use text** (contract 6.5), while this build copies anything
/// ([`COPIES_DISCLOSED`]); `None` while it does not.
pub(crate) fn disclosure(lang: Lang) -> Option<&'static str> {
    disclosure_when(lang, COPIES_DISCLOSED)
}

/// The same sentence for a build that copies (`open`): what a test reads.
pub(crate) fn disclosure_when(lang: Lang, open: bool) -> Option<&'static str> {
    open.then(|| t(lang, Words::BotsLinkedDisclosure))
}

/// **The sentence where unlinking is explained** (contract 6.5), under the
/// same switch.
// Not drawn anywhere yet: the screens that explain unlinking are not
// in this crate's string table (M8.LINKED-UI notes, "Not wired").
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn unlink_disclosure(lang: Lang) -> Option<&'static str> {
    unlink_disclosure_when(lang, COPIES_DISCLOSED)
}

// Not drawn anywhere yet: the screens that explain unlinking are not
// in this crate's string table (M8.LINKED-UI notes, "Not wired").
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn unlink_disclosure_when(lang: Lang, open: bool) -> Option<&'static str> {
    open.then(|| t(lang, Words::BotsLinkedUnlinkDisclosure))
}

impl Computer {
    /// One computer from the two reads that describe it: the owners' list,
    /// and that computer's Bots for the roster's workspace. The Bots' read
    /// is the later one, so its state and time are the ones kept.
    pub(crate) fn of(owner: LinkedOwnerView, bots: Option<LinkedBotsView>) -> Self {
        let (state, last, bots) = match bots {
            Some(view) => (
                view.state,
                view.last_confirmed_at_ms.or(owner.last_confirmed_at_ms),
                view.bots,
            ),
            None => (owner.state, owner.last_confirmed_at_ms, Vec::new()),
        };
        Self {
            device: owner.owner_device,
            state,
            last_confirmed_at_ms: last,
            team: owner.team,
            withheld: owner.withheld_bots,
            bots: bots
                .into_iter()
                .map(|bot| {
                    let mut row = RosterBot::from(bot);
                    row.owner = Some(owner.owner_device);
                    row
                })
                .collect(),
        }
    }

    pub(crate) fn heading(&self) -> Option<Heading> {
        heading(self.state, self.last_confirmed_at_ms)
    }

    /// Whether a command can be sent to it now.
    pub(crate) fn connected(&self) -> bool {
        self.state == LinkedOwnerState::Current
    }

    /// **Whether its Bots say *Paused with the team***: from that
    /// computer's confirmed pause until its resume is confirmed, as the
    /// roster's own rows do (`team_pause::TeamPause::holds_bots`). The
    /// pause is that computer's own; this one only repeats what it last
    /// received.
    pub(crate) fn team_holds(&self) -> bool {
        self.team.as_ref().is_some_and(|team| {
            matches!(
                (team.stage, team.paused),
                (TeamPauseStage::Acknowledged, true) | (TeamPauseStage::Pending, false)
            )
        })
    }

    /// **Whether the roster draws a group for it.** A computer with Bots in
    /// this workspace, with Bots it does not copy, whose Koru needs an
    /// update, or from which nothing came yet has something to say; one
    /// whose Bots all live in other workspaces says nothing here. A
    /// computer being forgotten is never drawn.
    pub(crate) fn listed(&self) -> bool {
        self.heading().is_some()
            && (!self.bots.is_empty()
                || self.withheld > 0
                || matches!(
                    self.state,
                    LinkedOwnerState::Unsupported | LinkedOwnerState::Never
                ))
    }
}

/// **The names the window knows its linked devices by**: the Agent's own
/// list of them, which the device and pairing screens draw from. The copy
/// of a computer's Bots carries no name (`LinkedOwnerView.display_name` is
/// always absent), and nothing else is read to find one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Names(HashMap<DeviceId, String>);

impl Names {
    pub(crate) fn new(names: impl IntoIterator<Item = (DeviceId, String)>) -> Self {
        Self(
            names
                .into_iter()
                .filter(|(_, name)| !name.trim().is_empty())
                .collect(),
        )
    }

    /// The device's name, when the window knows one.
    pub(crate) fn known(&self, device: DeviceId) -> Option<&str> {
        self.0.get(&device).map(String::as_str)
    }

    /// The device's name, or *another computer* while the window knows none.
    pub(crate) fn of(&self, lang: Lang, device: DeviceId) -> String {
        match self.known(device) {
            Some(name) => name.trim().to_owned(),
            None => t(lang, Words::BotsLinkedAnotherComputer).to_owned(),
        }
    }
}

/// **The roster's linked computers**: what was last read, and when to read
/// again.
#[derive(Default)]
pub(crate) struct Linked {
    computers: Vec<Computer>,
    names: Names,
    seq: u64,
    in_flight: bool,
    /// When the states are read again, while a computer is listed.
    next: Option<Instant>,
    /// **The host offers *Refresh this copy***. Off unless the host says a
    /// refresh can be asked for: no command of the Bot contract asks one
    /// today, and a control that leads nowhere is not drawn.
    refresh_offered: bool,
    /// A *Refresh this copy* pressed: the computer, for the host. Taken once.
    wants_refresh: Option<DeviceId>,
}

impl Linked {
    /// The computers the roster draws a group for, in the service's order.
    pub(crate) fn computers(&self) -> impl Iterator<Item = &Computer> + '_ {
        self.computers.iter().filter(|computer| computer.listed())
    }

    /// Every remote Bot the roster lists, in the order it draws them.
    pub(crate) fn bots(&self) -> impl Iterator<Item = &RosterBot> + '_ {
        self.computers().flat_map(|computer| computer.bots.iter())
    }

    pub(crate) fn bot(&self, bot: BotId) -> Option<&RosterBot> {
        self.bots().find(|row| row.id == bot)
    }

    /// The computer that runs `bot`, when it is a remote Bot listed here.
    pub(crate) fn computer_of(&self, bot: BotId) -> Option<&Computer> {
        self.computers()
            .find(|computer| computer.bots.iter().any(|row| row.id == bot))
    }

    pub(crate) fn computer(&self, device: DeviceId) -> Option<&Computer> {
        self.computers().find(|computer| computer.device == device)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.computers().next().is_none()
    }

    pub(crate) fn names(&self) -> &Names {
        &self.names
    }

    /// The window's names for its linked devices. `true` when they moved.
    pub(crate) fn set_names(&mut self, names: Names) -> bool {
        if self.names == names {
            return false;
        }
        self.names = names;
        true
    }

    /// The name a computer's heading wears.
    pub(crate) fn name_of(&self, lang: Lang, device: DeviceId) -> String {
        self.names.of(lang, device)
    }

    // ------------------------------------------------------------ reads

    /// A read was sent beside a listing or on the clock: its sequence.
    pub(crate) fn reading(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.in_flight = true;
        self.next = None;
        self.seq
    }

    /// The read could not be queued: asked again on the clock.
    pub(crate) fn unsent(&mut self, now: Instant) {
        self.in_flight = false;
        self.schedule(now);
    }

    /// Whether the clock came due and no read is on its way.
    pub(crate) fn read_due(&self, now: Instant) -> bool {
        !self.in_flight && self.next.is_some_and(|at| at <= now)
    }

    /// **Whether an answer is owed**: the window keeps waking until it is
    /// folded. The clock is not a reason to keep waking — it is one wake,
    /// at [`Self::deadline`].
    pub(crate) fn awaiting(&self) -> bool {
        self.in_flight
    }

    /// When the states are read again: one wake for the window, and `None`
    /// while no computer is listed or a read is on its way.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next.filter(|_| !self.in_flight)
    }

    /// The roster was hidden: nothing is read while hidden.
    pub(crate) fn hidden(&mut self) {
        self.next = None;
        self.in_flight = false;
        // An answer still on its way was asked for a roster that is gone.
        self.seq = self.seq.wrapping_add(1);
    }

    /// Another workspace: its computers' Bots are not this one's.
    pub(crate) fn clear(&mut self) {
        self.computers.clear();
        self.wants_refresh = None;
        self.hidden();
    }

    /// **What a read answered.** Only the latest read's answer is taken. A
    /// read that failed keeps what was shown: a copy does not vanish because
    /// Koru AI did not answer once. `true` when what is drawn changed.
    pub(crate) fn take(
        &mut self,
        seq: u64,
        result: Result<Vec<Computer>, Failure>,
        now: Instant,
    ) -> bool {
        if seq != self.seq {
            return false;
        }
        self.in_flight = false;
        let changed = match result {
            Ok(computers) if computers != self.computers => {
                self.computers = computers;
                true
            }
            _ => false,
        };
        self.schedule(now);
        changed
    }

    /// The clock runs only while a computer is listed.
    fn schedule(&mut self, now: Instant) {
        self.next = (!self.is_empty()).then(|| now.checked_add(POLL).unwrap_or(now));
    }

    // ---------------------------------------------- refresh this copy

    /// Whether the host offers *Refresh this copy*.
    pub(crate) fn refresh_offered(&self) -> bool {
        self.refresh_offered
    }

    /// The host can ask a computer to send its Bots again, or cannot.
    // No product path reaches this yet: no command of the Bot contract
    // asks a computer for a reset (M8.LINKED-UI notes, "Not wired").
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn offer_refresh(&mut self, offered: bool) -> bool {
        if self.refresh_offered == offered {
            return false;
        }
        self.refresh_offered = offered;
        true
    }

    /// **Whether a computer's *Refresh this copy* can be pressed**: offered,
    /// and the computer is connected (contract 6.3).
    pub(crate) fn refresh_enabled(&self, device: DeviceId) -> bool {
        self.refresh_offered
            && self
                .computer(device)
                .is_some_and(|computer| computer.connected())
    }

    /// *Refresh this copy* was pressed. `false` while it cannot be.
    pub(crate) fn press_refresh(&mut self, device: DeviceId) -> bool {
        if !self.refresh_enabled(device) {
            return false;
        }
        self.wants_refresh = Some(device);
        true
    }

    /// The computer whose copy the person asked to refresh. Taken once.
    // No product path reaches this yet: no command of the Bot contract
    // asks a computer for a reset (M8.LINKED-UI notes, "Not wired").
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn take_refresh(&mut self) -> Option<DeviceId> {
        self.wants_refresh.take()
    }

    #[cfg(test)]
    pub(crate) fn read_for_test(&mut self, computers: Vec<Computer>) {
        self.computers = computers;
    }

    #[cfg(test)]
    pub(crate) fn due_for_test(&mut self) -> bool {
        match &mut self.next {
            Some(at) => {
                *at = Instant::now();
                true
            }
            None => false,
        }
    }
}

/// **The linked computers and their Bots for one workspace**, read through
/// `ask` on the link's thread: the owners, then each owner's Bots whose
/// home is `workspace`. Any read that fails fails the whole answer, so what
/// the roster shows is kept rather than replaced by a part of it; an answer
/// to some other question is never read as a list.
pub(crate) fn read(
    workspace: WorkspaceId,
    mut ask: impl FnMut(BotCommand) -> Result<BotResponse, Said>,
) -> Result<Vec<Computer>, Failure> {
    let owners = match ask(BotCommand::ListLinkedOwners {}) {
        Ok(BotResponse::LinkedOwners { owners }) => owners,
        Ok(_) => return Err(Failure::Unanswered),
        Err(said) => return Err(Failure::of(said)),
    };
    let mut computers = Vec::new();
    for owner in owners
        .into_iter()
        .filter(|owner| owner.state != LinkedOwnerState::Withdrawing)
        .take(MAX_COMPUTERS)
    {
        let device = owner.owner_device;
        let bots = match ask(BotCommand::ListLinkedBots {
            owner_device: device,
            workspace: Some(workspace),
        }) {
            Ok(BotResponse::LinkedBots { linked }) if linked.owner_device == device => linked,
            Ok(_) => return Err(Failure::Unanswered),
            Err(said) => return Err(Failure::of(said)),
        };
        computers.push(Computer::of(owner, Some(bots)));
    }
    Ok(computers)
}

/// **One computer's copy, for a remote Bot's page**: its state and time,
/// and whether it still lists `bot`. `None` in the answer means that
/// computer does not run the Bot as far as this one knows: the page is then
/// an ordinary Bot's.
pub(crate) fn read_owner(
    owner: DeviceId,
    workspace: WorkspaceId,
    bot: BotId,
    mut ask: impl FnMut(BotCommand) -> Result<BotResponse, Said>,
) -> Result<Option<(LinkedOwnerState, Option<i64>)>, Failure> {
    match ask(BotCommand::ListLinkedBots {
        owner_device: owner,
        workspace: Some(workspace),
    }) {
        Ok(BotResponse::LinkedBots { linked }) if linked.owner_device == owner => Ok(linked
            .bots
            .iter()
            .any(|row| row.id == bot)
            .then_some((linked.state, linked.last_confirmed_at_ms))),
        Ok(_) => Err(Failure::Unanswered),
        Err(said) => Err(Failure::of(said)),
    }
}

#[cfg(test)]
pub(crate) mod tests;
