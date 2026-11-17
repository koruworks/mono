// SPDX-License-Identifier: MPL-2.0

//! The one reader of "is this Bot held" (M8, P8.1).
//!
//! A Bot is held when its own admission barrier is paused, or its team is
//! paused, or this run of the delegate has not been told the team's state.
//! The Bot's barrier is read from the ledger, as before; the team half is
//! memory of this process ([`super::team`]) and is lost when it stops.
//!
//! Every point of the orchestrator that admits, dispatches, starts or
//! resumes work for a Bot asks here and nowhere else:
//!
//! | Point | Where | Held by the Bot | Held by the team | Not told |
//! | --- | --- | --- | --- | --- |
//! | Admit a submission | `bots.rs` `bot_submit` | the ledger's `bot_fenced` | `bot_team_paused` | admitted and queued |
//! | Dispatch, before reserving | `bots.rs` `bot_dispatch_reserved` | not dispatched | not dispatched | not dispatched |
//! | Dispatch, after reserving | the same | reservation kept | reservation kept | reservation kept |
//! | Attempt start | `start.rs` | `bot_boot_pending` | `bot_boot_pending` | `bot_boot_pending` |
//! | Routine preclaim | `native_results/policy.rs` | the existing refusal | `bot_team_paused` | `bot_boot_pending` |
//! | Decision successor (edit or private operation) | `decisions/driver.rs` | waits | waits | waits |
//! | Child request, each recipient | `children/request.rs` | the ledger's recipient-paused refusal | the same refusal | admitted and queued |
//! | Parent successor after a child wait | `children/reservation.rs` | the ledger's `Paused` | waits | waits |
//!
//! The ledger keeps its own in-transaction checks of the Bot's barrier; they
//! are the last word for a Bot's own pause and are not replaced by this.
//! When a Bot is held both ways a refusal names the Bot's own pause first:
//! the team codes appear only when the team alone holds the Bot.
//!
//! Not asked, on purpose: a native approval answer and a steer continue work
//! that is already running, and a private operation's dispatch inside a
//! running Run is fenced on its policy barriers, not on admission.

use super::team::TeamHalf;
use super::{Orchestrator, RunError};
use koru_delegate_core::{BotAdmissionBarrier, BotId, BotOwner};

/// Why a Bot may not be given new work now, if it may not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Neither the Bot nor its team holds it.
    No,
    /// The Bot's own barrier is paused. Also the answer when the team's
    /// state is not known: the Bot's own pause is named first.
    Bot,
    /// The team is paused and the Bot is not.
    Team,
    /// Both the Bot and its team are paused.
    Both,
    /// The Bot is not paused and this run of the delegate has not been told
    /// the team's state. Counts as held for dispatch, start, preclaim and
    /// resume; a submission is still admitted and queued.
    NotTold,
}

impl Held {
    /// Whether anything holds the Bot.
    pub fn is_held(self) -> bool {
        self != Self::No
    }

    /// The code that names what holds the Bot, for a driver that waits.
    /// `None` when nothing holds it.
    pub fn waiting_code(self) -> Option<&'static str> {
        match self {
            Self::No => None,
            Self::Bot | Self::Both => Some("bot_paused"),
            Self::Team => Some("bot_team_paused"),
            Self::NotTold => Some("bot_boot_pending"),
        }
    }

    /// The refusal of a point the team alone closes: `bot_team_paused` when
    /// only the team is paused, `bot_boot_pending` when the team's state is
    /// not known. `None` when nothing holds the Bot, and when the Bot's own
    /// pause does, which the caller's existing check then names.
    pub fn team_refusal(self) -> Option<&'static str> {
        match self {
            Self::Team => Some("bot_team_paused"),
            Self::NotTold => Some("bot_boot_pending"),
            Self::No | Self::Bot | Self::Both => None,
        }
    }

    fn of(bot_paused: bool, team: TeamHalf) -> Self {
        match (bot_paused, team) {
            (true, TeamHalf::Paused) => Self::Both,
            (true, TeamHalf::Open | TeamHalf::NotTold) => Self::Bot,
            (false, TeamHalf::Paused) => Self::Team,
            (false, TeamHalf::NotTold) => Self::NotTold,
            (false, TeamHalf::Open) => Self::No,
        }
    }
}

impl Orchestrator {
    /// Whether this Bot is held, and by what. One ledger read of the Bot's
    /// barrier and one look at the team value in memory; nothing is written.
    pub fn bot_held(&mut self, owner: BotOwner, bot: BotId) -> Result<Held, RunError> {
        Ok(self.bot_gate(owner, bot)?.1)
    }

    /// [`Self::bot_held`] with the Bot's barrier as the ledger holds it, for
    /// the one point that also refuses a Bot with no barrier at all (the
    /// attempt start).
    pub(super) fn bot_gate(
        &mut self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<(Option<BotAdmissionBarrier>, Held), RunError> {
        let barrier = self.bot_ledger()?.bot_barrier(owner, bot)?;
        let paused = barrier.as_ref().is_some_and(|barrier| barrier.paused);
        let held = Held::of(paused, self.team_half(owner));
        Ok((barrier, held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `m8_gate_is_the_only_reader`: the truth table of the two halves.
    #[test]
    fn m8_gate_is_the_only_reader_and_names_the_bots_own_pause_first() {
        assert_eq!(Held::of(false, TeamHalf::Open), Held::No);
        assert_eq!(Held::of(true, TeamHalf::Open), Held::Bot);
        assert_eq!(Held::of(false, TeamHalf::Paused), Held::Team);
        assert_eq!(Held::of(true, TeamHalf::Paused), Held::Both);
        assert_eq!(Held::of(false, TeamHalf::NotTold), Held::NotTold);
        assert_eq!(Held::of(true, TeamHalf::NotTold), Held::Bot);
        for held in [Held::Bot, Held::Team, Held::Both, Held::NotTold] {
            assert!(held.is_held());
            assert!(held.waiting_code().is_some());
        }
        assert!(!Held::No.is_held());
        assert_eq!(Held::No.waiting_code(), None);
        // The team codes appear only when the team alone holds the Bot.
        assert_eq!(Held::Team.team_refusal(), Some("bot_team_paused"));
        assert_eq!(Held::NotTold.team_refusal(), Some("bot_boot_pending"));
        for held in [Held::No, Held::Bot, Held::Both] {
            assert_eq!(held.team_refusal(), None);
        }
    }

    /// `m8_gate_is_the_only_reader`: no admission point of the orchestrator
    /// reads a Bot's `paused` for itself. A new one fails here until it asks
    /// the gate, and is then added to the table at the top of this file.
    #[test]
    fn m8_gate_is_the_only_reader_of_a_bots_pause_in_the_orchestrator() {
        // (file, source, how many times it asks the gate)
        let sites: [(&str, &str, usize); 6] = [
            ("bots.rs", include_str!("bots.rs"), 3),
            ("start.rs", include_str!("start.rs"), 1),
            (
                "native_results/policy.rs",
                include_str!("native_results/policy.rs"),
                1,
            ),
            (
                "decisions/driver.rs",
                include_str!("decisions/driver.rs"),
                1,
            ),
            (
                "children/request.rs",
                include_str!("children/request.rs"),
                1,
            ),
            (
                "children/reservation.rs",
                include_str!("children/reservation.rs"),
                1,
            ),
        ];
        for (file, source, asks) in sites {
            let found = source.matches(".bot_held(").count() + source.matches(".bot_gate(").count();
            assert_eq!(found, asks, "{file} asks the gate {found} times");
            assert!(
                !source.contains("barrier.paused"),
                "{file} reads a Bot's pause without the gate"
            );
        }
        // Every other file that could start or resume work. Two files may
        // read `paused` from a barrier and are not listed: this one, and
        // `children/clock.rs`, whose tick asks the ledger to restart a paused
        // child clock once the ledger's own rule (the parent Bot's barrier)
        // allows it.
        let mut all = String::new();
        for file in [
            include_str!("bot_boot.rs"),
            include_str!("bot_controls.rs"),
            include_str!("bot_delivery.rs"),
            include_str!("children.rs"),
            include_str!("children/driver.rs"),
            include_str!("children/launch.rs"),
            include_str!("children/request.rs"),
            include_str!("children/reservation.rs"),
            include_str!("decisions.rs"),
            include_str!("decisions/driver.rs"),
            include_str!("decisions/launch.rs"),
            include_str!("decisions/private.rs"),
            include_str!("decisions/resume.rs"),
            include_str!("decisions/successor.rs"),
            include_str!("native_results.rs"),
            include_str!("native_results/policy.rs"),
            include_str!("restart.rs"),
            include_str!("resume.rs"),
            include_str!("start.rs"),
        ] {
            all.push_str(file);
        }
        // `admission.paused` in the preclaim is the offered value's shape
        // check, not a read of the Bot's state; the ledger comparison that
        // follows it is the equality check, which the gate precedes.
        assert!(!all.contains("barrier.paused"));
        assert!(!all.contains("|barrier| barrier.paused"));
    }
}
