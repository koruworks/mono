// SPDX-License-Identifier: MPL-2.0

//! The team half of the gate (M8, P8.1): the team pause as this run of the
//! delegate was told it.
//!
//! The durable truth is the AI owner's row. This is a copy in memory, per
//! owner, set only by the `TeamBarrier` command and lost when the delegate
//! stops. Nothing here is written to the ledger and no Bot's revision moves.
//!
//! **Not told.** A delegate run which has not been told an owner's team
//! value is closed for that owner: it dispatches and starts nothing of a
//! Bot until the AI owner speaks (contract 3.2). That is safe because the
//! AI worker sends `TeamBarrier` each time its boot `Hello` shows a delegate
//! run it has not told, before that run's per-Bot boot pass, and sends
//! "never paused" for a team with no row
//! (`apps/koru-ai-server/src/worker/bots/team_pause.rs`). The per-Bot boot
//! proof a dispatch already needed is renewed by the same `Hello`, so a run
//! waits for the worker no longer than it did before M8.
//! [`TEAM_VALUE_REQUIRED`] is the switch for that rule.

use super::bots::{owner_device, refused};
use super::{Orchestrator, RunError};
use koru_delegate_core::{BotOwner, BotTeamBarrier, DeviceId};
use std::collections::HashMap;

/// Whether a delegate run that was not told an owner's team value holds that
/// owner's Bots (contract 3.2, "closed until told").
///
/// `true` since the AI worker sends `TeamBarrier` after each `Hello` (M8
/// piece W). It must not be `true` in a tree whose worker does not send the
/// value: no Bot task would be dispatched again. See the module comment.
pub const TEAM_VALUE_REQUIRED: bool = true;

/// The most owners one delegate run keeps a team value for. A delegate
/// serves one device and, in practice, one person; the bound only keeps a
/// misbehaving caller from growing the map.
const TEAM_OWNER_LIMIT: usize = 64;

/// What the team value says about one owner's Bots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TeamHalf {
    /// Told, and not paused.
    Open,
    /// Told, and paused.
    Paused,
    /// Not told in this run of the delegate, and that closes.
    NotTold,
}

/// The team values of this run, and whether an absent one closes.
#[derive(Debug)]
pub(super) struct TeamState {
    values: HashMap<BotOwner, BotTeamBarrier>,
    required: bool,
}

impl Default for TeamState {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
            required: TEAM_VALUE_REQUIRED,
        }
    }
}

impl Orchestrator {
    /// Apply the team value the AI owner sent, with the rule of a Bot's own
    /// barrier, in memory: an epoch below the held one is `bot_team_stale`
    /// (the caller reads the held epoch with [`Self::bot_team_value`]); the
    /// same epoch with other content is `bot_team_conflict`; otherwise the
    /// value is stored and answered. The owner must be this device's.
    pub fn bot_team_barrier(
        &mut self,
        barrier: &BotTeamBarrier,
        device: DeviceId,
    ) -> Result<BotTeamBarrier, RunError> {
        owner_device(barrier.owner, device)?;
        if !barrier.valid() {
            return Err(refused("bot_invalid"));
        }
        match self.team.values.get(&barrier.owner) {
            Some(held) if barrier.epoch < held.epoch => return Err(refused("bot_team_stale")),
            Some(held) if barrier.epoch == held.epoch => {
                return if held == barrier {
                    Ok(held.clone())
                } else {
                    Err(refused("bot_team_conflict"))
                };
            }
            Some(_) => {}
            None if self.team.values.len() >= TEAM_OWNER_LIMIT => {
                return Err(refused("bot_capacity"));
            }
            None => {}
        }
        self.team.values.insert(barrier.owner, barrier.clone());
        Ok(barrier.clone())
    }

    /// The team value this run holds for `owner`; `None` when it was not
    /// told. Memory only.
    pub fn bot_team_value(&self, owner: BotOwner) -> Option<&BotTeamBarrier> {
        self.team.values.get(&owner)
    }

    /// Whether an owner this run was not told about is held. Starts as
    /// [`TEAM_VALUE_REQUIRED`].
    pub fn team_value_required(&self) -> bool {
        self.team.required
    }

    /// Set whether an owner this run was not told about is held. The host
    /// composition does not call this: the constant is the product's switch.
    pub fn require_team_value(&mut self, required: bool) {
        self.team.required = required;
    }

    /// Drop every team value this run holds: what a restart of the delegate
    /// does to this memory. A test of the closed state uses it on a fixture
    /// whose builder already told the run "never paused".
    #[cfg(any(test, feature = "test-support"))]
    pub fn forget_team_values(&mut self) {
        self.team.values.clear();
    }

    pub(super) fn team_half(&self, owner: BotOwner) -> TeamHalf {
        match self.team.values.get(&owner) {
            Some(value) if value.paused => TeamHalf::Paused,
            Some(_) => TeamHalf::Open,
            None if self.team.required => TeamHalf::NotTold,
            None => TeamHalf::Open,
        }
    }
}
