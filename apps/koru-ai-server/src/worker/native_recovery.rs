// SPDX-License-Identifier: MPL-2.0
//! Process-scoped native readiness and finite jobs on the existing admin pool.
use super::*;
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotId, BotOwner, NativeDrainReason, NativeRecoveryCursor, NativeResultKey,
};
use koru_store_server::StoreWorkerClient;
use std::collections::VecDeque;

const MAX_RECOVERY_SCOPES: usize = 64;

/// Created once by gateway-process composition and reused by AI-thread restarts.
/// Ordinary Chat/read readiness does not depend on this native mutation gate.
pub struct NativeMutationGate {
    ready: AtomicBool,
    recovery: Mutex<RecoveryState>,
}
struct RecoveryState {
    owner: Option<BotOwner>,
    queue: VecDeque<Sweep>,
    // A single sticky slot, only for the vouched owner's overflow uncertainty.
    // It survives mailbox saturation and AI-thread replacement with this gate.
    unconfirmed_overflow: Option<Sweep>,
    overflow_last: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRecoveryAdmission {
    Queued,
    Coalesced,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRecoveryRefusal {
    Unavailable,
    ForeignOwner,
    Busy,
}
impl NativeMutationGate {
    pub fn closed() -> Self {
        Self {
            ready: AtomicBool::new(false),
            recovery: Mutex::new(RecoveryState {
                owner: None,
                queue: VecDeque::from([Sweep::new(
                    None,
                    None,
                    None,
                    None,
                    NativeDrainReason::GatewayStartup,
                )]),
                unconfirmed_overflow: None,
                overflow_last: false,
            }),
        }
    }
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) && !self.recovery.is_poisoned()
    }
    fn enqueue(&self, sweep: Sweep) -> Result<NativeRecoveryAdmission, NativeRecoveryRefusal> {
        use NativeRecoveryAdmission::*;
        let mut state = self
            .recovery
            .lock()
            .map_err(|_| NativeRecoveryRefusal::Unavailable)?;
        let owner = state.owner.ok_or(NativeRecoveryRefusal::Unavailable)?;
        if sweep.owner != Some(owner) {
            return Err(NativeRecoveryRefusal::ForeignOwner);
        }
        if sweep.reason == NativeDrainReason::GatewayStartup {
            return Err(NativeRecoveryRefusal::ForeignOwner);
        }
        if let Some(old) = state.queue.iter_mut().find(|old| old.same_scope(&sweep)) {
            old.dirty = true;
            return Ok(Coalesced);
        }
        if sweep.reason == NativeDrainReason::UnconfirmedOperation {
            if let Some(overflow) = &mut state.unconfirmed_overflow {
                overflow.dirty = true;
                return Ok(Pending);
            }
            if let Some(old) = state.queue.iter_mut().find(|old| {
                old.owner == sweep.owner && old.bot.is_none() && old.reason == sweep.reason
            }) {
                old.dirty = true;
                return Ok(Coalesced);
            }
            if let Some(old) = state.queue.iter_mut().find(|old| {
                old.owner == sweep.owner && old.bot == sweep.bot && old.reason == sweep.reason
            }) {
                // Widen to the same affected Bot. Restart the cursor with a new
                // ticket so an older in-flight completion cannot skip targets.
                *old = Sweep::new(Some(owner), sweep.bot, None, None, sweep.reason);
                return Ok(Coalesced);
            }
        }
        if state.queue.len() < MAX_RECOVERY_SCOPES {
            state.queue.push_back(sweep);
            return Ok(Queued);
        }
        if sweep.reason != NativeDrainReason::UnconfirmedOperation {
            // Policy and Run callers retain durable pending receipts/phases and
            // retry through their existing bounded recovery mechanisms.
            return Err(NativeRecoveryRefusal::Busy);
        }
        state.queue.retain(|old| {
            !(old.owner == Some(owner) && old.reason == NativeDrainReason::UnconfirmedOperation)
        });
        let owner_scope = Sweep::new(
            Some(owner),
            None,
            None,
            None,
            NativeDrainReason::UnconfirmedOperation,
        );
        if state.queue.len() < MAX_RECOVERY_SCOPES {
            state.queue.push_back(owner_scope);
            Ok(Coalesced)
        } else {
            state.unconfirmed_overflow = Some(owner_scope);
            Ok(Pending)
        }
    }
    fn complete(&self, result: NativeDrainResult) -> bool {
        let Ok(mut state) = self.recovery.lock() else {
            return false;
        };
        let overflow = state
            .unconfirmed_overflow
            .as_ref()
            .is_some_and(|sweep| sweep.id == result.id);
        let mut sweep = if overflow {
            state.unconfirmed_overflow.take().expect("matched overflow")
        } else if let Some(index) = state.queue.iter().position(|sweep| sweep.id == result.id) {
            state.queue.remove(index).expect("position exists")
        } else {
            return false;
        };
        let progress = result.progress.and_then(|(owner, after, pending)| {
            if state.owner.is_some_and(|known| known != owner) {
                return Err(());
            }
            state.owner = Some(owner);
            Ok((after, pending))
        });
        let keep = match progress {
            Err(()) => {
                sweep.defer();
                true
            }
            Ok((after, pending)) => {
                sweep.retry_needed |= pending;
                sweep.after = after;
                if sweep.after.is_some() {
                    true
                } else if sweep.retry_needed {
                    sweep.retry_needed = false;
                    sweep.defer();
                    true
                } else if sweep.dirty {
                    sweep.dirty = false;
                    true
                } else {
                    if sweep.reason == NativeDrainReason::GatewayStartup {
                        self.ready.store(true, Ordering::Release);
                    }
                    false
                }
            }
        };
        if keep {
            if overflow {
                state.unconfirmed_overflow = Some(sweep);
            } else {
                state.queue.push_back(sweep);
            }
        }
        true
    }
    fn next_sweep(&self, now: Instant) -> Option<Sweep> {
        let mut state = self.recovery.lock().ok()?;
        let queued = state.queue.iter().find(|sweep| sweep.due <= now).cloned();
        let overflow = state
            .unconfirmed_overflow
            .as_ref()
            .filter(|sweep| sweep.due <= now)
            .cloned();
        let choose_overflow = overflow.is_some() && (queued.is_none() || !state.overflow_last);
        state.overflow_last = choose_overflow;
        if choose_overflow { overflow } else { queued }
    }
}

#[derive(Clone)]
pub(super) struct NativeRecoveryConfig {
    pub gate: Arc<NativeMutationGate>,
    pub store: StoreWorkerClient,
}
#[derive(Clone)]
pub(super) struct Sweep {
    id: Uuid,
    owner: Option<BotOwner>,
    bot: Option<BotId>,
    run: Option<koru_delegate_core::RunId>,
    key: Option<NativeResultKey>,
    reason: NativeDrainReason,
    after: Option<NativeRecoveryCursor>,
    retry_needed: bool,
    dirty: bool,
    due: Instant,
    round: u32,
}
impl Sweep {
    fn new(
        owner: Option<BotOwner>,
        bot: Option<BotId>,
        run: Option<koru_delegate_core::RunId>,
        key: Option<NativeResultKey>,
        reason: NativeDrainReason,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            owner,
            bot,
            run,
            key,
            reason,
            after: None,
            retry_needed: false,
            dirty: false,
            due: Instant::now(),
            round: 0,
        }
    }
    fn same_scope(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.bot == other.bot
            && self.run == other.run
            && self.key == other.key
            && self.reason == other.reason
    }
    fn defer(&mut self) {
        self.due = Instant::now() + Duration::from_secs(1u64 << self.round.min(5));
        self.round = (self.round + 1).min(5);
    }
}

pub(super) struct NativeDrainJob {
    sweep: Sweep,
    config: NativeRecoveryConfig,
    authority: Arc<dyn koru_ai_core::bots::BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
}
pub(super) struct NativeDrainResult {
    id: Uuid,
    progress: Result<(BotOwner, Option<NativeRecoveryCursor>, bool), ()>,
}
impl NativeDrainJob {
    pub(super) fn run(self) -> NativeDrainResult {
        let id = self.sweep.id;
        let progress = catch_unwind(AssertUnwindSafe(|| self.execute())).unwrap_or(Err(()));
        NativeDrainResult { id, progress }
    }
    fn execute(&self) -> Result<(BotOwner, Option<NativeRecoveryCursor>, bool), ()> {
        let current = self.authority.inspect(&[], &[]).map_err(|_| ())?.owner;
        let owner = BotOwner {
            user_scope: current.user_scope,
            device: current.device,
        };
        if self.sweep.owner.is_some_and(|expected| expected != owner) {
            return Err(());
        }
        let BotReply::NativePendingEffects { page } = self
            .delegate
            .execute(BotCommand::PendingNativeEffects {
                owner,
                bot: self.sweep.bot,
                after: self.sweep.after.clone(),
                limit: 1,
            })
            .map_err(|_| ())?
        else {
            return Err(());
        };
        // One target per finite normal-admin job, including Store timeout waits.
        if page.targets.len() > 1 {
            return Err(());
        }
        let Some(discovered) = page.targets.first() else {
            return if page.next.is_none() {
                Ok((owner, None, false))
            } else {
                Err(())
            };
        };
        if self.sweep.run.is_some_and(|run| run != discovered.key.run)
            || self
                .sweep
                .key
                .as_ref()
                .is_some_and(|key| key != &discovered.key)
        {
            return Ok((owner, page.next, false));
        }
        let settled = (|| -> Result<(), ()> {
            let BotReply::NativeDrainEligibility { target } = self
                .delegate
                .execute(BotCommand::CheckNativeEffectDrain {
                    key: discovered.key.clone(),
                    reason: self.sweep.reason.clone(),
                })
                .map_err(|_| ())?
            else {
                return Err(());
            };
            let Some(target) = target else { return Ok(()) };
            if target.owner != owner
                || target.key != discovered.key
                || self.sweep.bot.is_some_and(|bot| target.bot != bot)
            {
                return Err(());
            }
            // The actor's fresh exact target, not the discovery copy. No actor
            // callback occurs until this Store owner closure has returned.
            let expected = target.binding;
            let binding = expected.clone();
            let receipt = self
                .config
                .store
                .with_store(move |store| store.drain_native_effect(&binding, Timestamp::now()))
                .map_err(|_| ())?
                .map_err(|_| ())?;
            if receipt.binding != expected {
                return Err(());
            }
            let reply = self
                .delegate
                .execute(BotCommand::ConfirmNativeResult {
                    request: koru_delegate_core::ConfirmNativeResult {
                        key: target.key.clone(),
                        request_fingerprint: receipt.binding.request_fingerprint,
                        observation: koru_delegate_core::NativeResultObservation::DrainedUnknown {
                            receipt,
                        },
                    },
                })
                .map_err(|_| ())?;
            match reply {
                BotReply::NativeResult { intent }
                    if intent.key == target.key
                        && intent.request_fingerprint == expected.request_fingerprint
                        && intent.target == expected.document
                        && intent.destination == expected.workspace
                        && intent.store_request == expected.request_key
                        && matches!(
                            intent.stage,
                            koru_delegate_core::NativeResultStage::SettledUnknown
                                | koru_delegate_core::NativeResultStage::Verified
                                | koru_delegate_core::NativeResultStage::Registered
                        ) =>
                {
                    Ok(())
                }
                _ => Err(()),
            }
        })()
        .is_ok();
        Ok((owner, page.next, !settled))
    }
}

impl AiWorkerClient {
    /// Trusted gateway error reconciliation, absent from public AI transports.
    /// A healthy Held operation is ineligible at the actor.
    pub fn reconcile_native_operation(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<NativeRecoveryAdmission, NativeRecoveryRefusal> {
        let gate = self
            .native_mutations
            .as_ref()
            .ok_or(NativeRecoveryRefusal::Unavailable)?;
        let admitted = gate.enqueue(Sweep::new(
            Some(owner),
            Some(bot),
            Some(key.run),
            Some(key),
            NativeDrainReason::UnconfirmedOperation,
        ))?;
        // Queue admission happened first. This wake is advisory; the worker's
        // existing bounded loop resumes the retained scope even if it is full.
        let _ = self.sender.try_send(Envelope::NativeDrainWake);
        Ok(admitted)
    }
}
impl WorkerState {
    pub(super) fn queue_native_recovery(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        run: Option<koru_delegate_core::RunId>,
        key: Option<NativeResultKey>,
        reason: NativeDrainReason,
    ) -> bool {
        self.enqueue_native_recovery(Sweep::new(Some(owner), Some(bot), run, key, reason))
    }
    pub(super) fn enqueue_native_recovery(&mut self, sweep: Sweep) -> bool {
        let admitted = self
            .native_recovery
            .as_ref()
            .is_some_and(|config| config.gate.enqueue(sweep).is_ok());
        self.schedule_native_recovery();
        admitted
    }
    pub(super) fn schedule_native_recovery(&mut self) {
        if self.native_drain_busy {
            return;
        }
        let (Some(config), Some(authority), Some(delegate), Some(admin)) = (
            &self.native_recovery,
            &self.bot_authority,
            &self.bot_delegate,
            &self.provider_admin,
        ) else {
            return;
        };
        let sweep = config.gate.next_sweep(Instant::now());
        let Some(sweep) = sweep else { return };
        let job = NativeDrainJob {
            sweep,
            config: config.clone(),
            authority: authority.clone(),
            delegate: delegate.clone(),
        };
        if admin.try_submit(ProviderAdminJob::NativeDrain(job)).is_ok() {
            self.native_drain_busy = true;
        }
    }
    pub(super) fn complete_native_recovery(&mut self, result: NativeDrainResult) {
        self.native_drain_busy = false;
        let Some(config) = &self.native_recovery else {
            return;
        };
        if !config.gate.complete(result) {
            self.schedule_native_recovery();
            return;
        }
        // Exact settlement can unblock durable access receipts and Run delivery.
        self.recover_bot_policy(true);
        self.recover_bot_chats();
        self.schedule_native_recovery();
    }
}

#[cfg(test)]
pub(super) mod tests;
