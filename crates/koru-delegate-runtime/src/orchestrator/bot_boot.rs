// SPDX-License-Identifier: MPL-2.0

//! Current-owner replay gates only new Bot launches, never existing sessions.
use super::{
    Orchestrator, RunError,
    bots::{owner_device, refused},
};
use koru_delegate_core::*;
use koru_delegate_sqlite::{BotSettlement, NativePolicyRecord};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct Proof {
    barrier: BotAdmissionBarrier,
    policy: NativePolicyRecord,
}

pub(super) struct BootCoordination {
    boot: DelegateBootId,
    // One small proof per real unsettled Bot, not a bounded payload cache whose
    // evictions could starve more than 64 valid queued Bots.
    acknowledged: HashMap<(BotOwner, BotId), Proof>,
}
impl BootCoordination {
    pub(super) fn new() -> Self {
        Self {
            boot: DelegateBootId::mint(),
            acknowledged: HashMap::new(),
        }
    }
}

impl Orchestrator {
    pub fn bot_hello(
        &mut self,
        owner: BotOwner,
        scan: BotBootScan,
        device: DeviceId,
    ) -> Result<BotHello, RunError> {
        owner_device(owner, device)?;
        let boot = self.bot_boot.boot;
        let (after, through) = match scan {
            BotBootScan::Probe {} => {
                return Ok(BotHello {
                    boot,
                    entries: Vec::new(),
                    next: None,
                });
            }
            BotBootScan::Start {} => (0, None),
            BotBootScan::Continue { cursor } => {
                if !cursor.valid_for(boot, owner) {
                    return Err(refused("bot_boot_cursor_stale"));
                }
                (cursor.after, Some(cursor.through))
            }
        };
        let page = self
            .bot_ledger()?
            .bot_boot_inventory(owner, after, through)?;
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for entry in page.entries {
            if seen.insert(entry.bot)
                && !self
                    .bot_boot_ack_current(owner, entry.bot, 0, 0)
                    .unwrap_or(false)
            {
                entries.push(entry);
            }
        }
        Ok(BotHello {
            boot,
            entries,
            next: page.more.then_some(BotBootCursor {
                boot,
                owner,
                after: page.scanned_through,
                through: page.through,
            }),
        })
    }

    pub fn synchronize_boot_bot(
        &mut self,
        boot: DelegateBootId,
        barrier: BotAdmissionBarrier,
        policy: BotPolicyBarrier,
        device: DeviceId,
    ) -> Result<BotBootReceipt, RunError> {
        owner_device(barrier.owner, device)?;
        if boot != self.bot_boot.boot {
            return Err(refused("bot_boot_stale"));
        }
        if barrier.owner != policy.key.owner
            || barrier.bot != policy.key.bot
            || barrier.epoch != policy.config_revision
            || barrier.epoch == 0
            || barrier.epoch > i64::MAX as u64
            || policy.policy_revision == 0
            || policy.policy_revision > i64::MAX as u64
        {
            return Err(refused("bot_boot_pair_invalid"));
        }
        let receipt = |state| BotBootReceipt {
            boot,
            barrier: barrier.clone(),
            policy: policy.key.clone(),
            config_revision: policy.config_revision,
            policy_revision: policy.policy_revision,
            state,
        };
        if !self
            .bot_ledger()?
            .bot_has_unsettled(barrier.owner, barrier.bot)?
        {
            self.bot_boot
                .acknowledged
                .remove(&(barrier.owner, barrier.bot));
            return Ok(receipt(BotBootState::NoUnsettledWork));
        }
        self.bot_barrier(&barrier, device)?;
        self.synchronize_native_policy(policy.clone(), device)?;
        let installed = self
            .native_ledger()?
            .native_policy(&policy.key)?
            .ok_or_else(|| refused("bot_boot_policy_missing"))?;
        let current = self
            .native_ledger()?
            .current_native_policy(barrier.owner, barrier.bot)?;
        if self
            .bot_ledger()?
            .bot_barrier(barrier.owner, barrier.bot)?
            .as_ref()
            != Some(&barrier)
            || current.as_ref() != Some(&installed)
            || installed.config_revision != policy.config_revision
            || installed.policy_revision != policy.policy_revision
            || installed.effects_allowed != policy.effects_allowed
        {
            self.bot_boot
                .acknowledged
                .remove(&(barrier.owner, barrier.bot));
            return Err(refused("bot_boot_pair_stale"));
        }
        if installed.state != BotPolicyBarrierState::Effective {
            self.bot_boot
                .acknowledged
                .remove(&(barrier.owner, barrier.bot));
            return Ok(receipt(BotBootState::PendingPolicy));
        }
        self.bot_boot.acknowledged.insert(
            (barrier.owner, barrier.bot),
            Proof {
                barrier: barrier.clone(),
                policy: installed,
            },
        );
        Ok(receipt(BotBootState::Ready))
    }

    /// Recheck the durable *current* tuple: an Effective historical receipt is
    /// idempotency evidence, never permission to reopen a newer fence.
    pub(super) fn bot_boot_ack_current(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        config: u64,
        policy: u64,
    ) -> Result<bool, RunError> {
        let Some(proof) = self.bot_boot.acknowledged.get(&(owner, bot)).cloned() else {
            return Ok(false);
        };
        if !self.bot_ledger()?.bot_has_unsettled(owner, bot)? {
            self.bot_boot.acknowledged.remove(&(owner, bot));
            return Ok(false);
        }
        if proof.policy.config_revision < config || proof.policy.policy_revision < policy {
            self.bot_boot.acknowledged.remove(&(owner, bot));
            return Ok(false);
        }
        let current_barrier = self.bot_ledger()?.bot_barrier(owner, bot)?;
        let current_policy = self.native_ledger()?.current_native_policy(owner, bot)?;
        let valid = current_barrier.as_ref() == Some(&proof.barrier)
            && current_policy.as_ref() == Some(&proof.policy)
            && proof.policy.state == BotPolicyBarrierState::Effective;
        if !valid {
            self.bot_boot.acknowledged.remove(&(owner, bot));
        }
        Ok(valid)
    }

    pub(super) fn invalidate_boot_for_submission(&mut self, submission: &BotSubmission) {
        let p = &submission.provenance;
        if self
            .bot_boot
            .acknowledged
            .get(&(p.owner, p.bot))
            .is_some_and(|proof| {
                proof.policy.config_revision < p.config_revision
                    || proof.policy.policy_revision < p.policy_revision
            })
        {
            self.bot_boot.acknowledged.remove(&(p.owner, p.bot));
        }
    }

    pub(super) fn invalidate_boot_for_barrier(&mut self, barrier: &BotAdmissionBarrier) {
        if self
            .bot_boot
            .acknowledged
            .get(&(barrier.owner, barrier.bot))
            .is_some_and(|proof| proof.barrier != *barrier)
        {
            self.bot_boot
                .acknowledged
                .remove(&(barrier.owner, barrier.bot));
        }
    }

    pub(super) fn prune_bot_boot(&mut self, owner: BotOwner, bot: BotId) -> Result<(), RunError> {
        if !self.bot_ledger()?.bot_has_unsettled(owner, bot)? {
            self.bot_boot.acknowledged.remove(&(owner, bot));
        }
        Ok(())
    }

    pub(super) fn settle_bot_admission(
        &mut self,
        run: RunId,
        outcome: BotSettlement,
    ) -> Result<(), RunError> {
        let record = self.bot_ledger()?.bot_execution(run)?;
        self.bot_ledger()?.settle_bot(run, outcome)?;
        // Cleanup must not turn a durable settlement into failure or retain its
        // folder claim. A failed read keeps only the small proof; dispatch still
        // checks current durable authority before using it.
        if let Some(record) = record {
            if let Err(error) = self.prune_bot_boot(record.owner, record.bot) {
                self.trace(run, "bot_boot_prune_pending", format_args!("{error}"));
            }
        }
        Ok(())
    }

    /// Before queue draining, release only the at-most-two unused reservations
    /// proven safe by the writer. Any attempt/unknown evidence retains capacity.
    pub fn recover_unused_bot_reservations(&mut self, device: DeviceId) -> Result<(), RunError> {
        if device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        let records = self.bot_ledger()?.active_bot_executions(device)?;
        for record in records {
            if record.state == BotAdmissionState::Reserved {
                self.bot_ledger()?.recover_bot_reservation(&BotRunKey {
                    owner: record.owner,
                    bot: record.bot,
                    run: record.run,
                })?;
            }
        }
        Ok(())
    }
}
