// SPDX-License-Identifier: MPL-2.0

//! The team half of the gate (M8, P8.1): the team pause as this run of the
//! delegate was told it.
//!
//! The durable truth is the AI owner's row. This is a copy in memory, per
//! owner, set only by the `TeamBarrier` command and lost when the delegate
//! stops. Nothing here is written to the ledger and no Bot's revision moves.
//!
//! **Not told.** The contract's rule is that a delegate which has not been
//! told the team's state is closed: it dispatches and starts nothing until
//! the AI owner speaks. That rule is only safe once the AI owner sends
//! `TeamBarrier` after every `Hello` (piece W). Until then no worker sends
//! it at all, and a closed delegate would never dispatch a Bot task again.
//! [`TEAM_VALUE_REQUIRED`] is that one switch: while it is `false`, an owner
//! this run was not told about counts as "told, not paused", exactly as
//! before M8; a value that *was* sent is enforced either way.

use super::bots::{owner_device, refused};
use super::{Orchestrator, RunError};
use koru_delegate_core::{BotOwner, BotTeamBarrier, DeviceId};
use std::collections::HashMap;

/// Whether a delegate run that was not told an owner's team value holds that
/// owner's Bots (contract 3.2, "closed until told").
///
/// `false` until the AI worker sends `TeamBarrier` after each `Hello`: the
/// change that makes the worker send it (piece W) sets this to `true` in the
/// same commit, and not before. See the module comment.
pub const TEAM_VALUE_REQUIRED: bool = false;

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
    /// It exists so the closed behavior is tested before it is switched on.
    pub fn require_team_value(&mut self, required: bool) {
        self.team.required = required;
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
