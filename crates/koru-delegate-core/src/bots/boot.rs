// SPDX-License-Identifier: MPL-2.0

//! Transient delegate-boot coordination. These values never authorize an owner.
use super::{BotAdmissionBarrier, BotOwner, BotPolicyBarrierKey};
use crate::{BotId, DelegateBootId, RunId};
use serde::{Deserialize, Serialize};

pub const BOT_BOOT_PAGE_LIMIT: u16 = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotBootCursor {
    pub boot: DelegateBootId,
    pub owner: BotOwner,
    pub after: u64,
    pub through: u64,
}

impl BotBootCursor {
    pub fn valid_for(&self, boot: DelegateBootId, owner: BotOwner) -> bool {
        self.boot == boot
            && self.owner == owner
            && self.after <= self.through
            && self.through <= i64::MAX as u64
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scan", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotBootScan {
    Probe {},
    Start {},
    Continue { cursor: BotBootCursor },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotBootIdentity {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub sequence: u64,
}

/// Execution-owner identity evidence, not canonical desired policy. A failed
/// read must never masquerade as permission to synthesize a missing barrier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotBootAdmission {
    Missing {},
    Present { barrier: BotAdmissionBarrier },
    Unavailable {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotBootEntry {
    pub identity: BotBootIdentity,
    pub admission: BotBootAdmission,
}

/// Column-only ledger projection; the orchestrator binds its cursor to a boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotBootPage {
    pub through: u64,
    pub scanned_through: u64,
    pub entries: Vec<BotBootIdentity>,
    pub more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotHello {
    pub boot: DelegateBootId,
    pub entries: Vec<BotBootEntry>,
    pub next: Option<BotBootCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotBootState {
    Ready,
    PendingPolicy,
    NoUnsettledWork,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotBootReceipt {
    pub boot: DelegateBootId,
    pub barrier: BotAdmissionBarrier,
    pub policy: BotPolicyBarrierKey,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub state: BotBootState,
}
