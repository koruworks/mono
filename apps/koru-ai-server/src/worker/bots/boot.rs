// SPDX-License-Identifier: MPL-2.0

//! One bounded, event-driven replay walk; idle polling reads boot metadata only.
use super::*;
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, BotReply};
use koru_delegate_core::{
    BotBootAdmission, BotBootEntry, BotBootScan, BotBootState, BotPolicyBarrier, DelegateBootId,
};
use std::collections::VecDeque;

type DelegateOwner = koru_delegate_core::BotOwner;
const PROBE_EVERY: Duration = Duration::from_secs(32);
const IPC_WITHIN: Duration = Duration::from_secs(5);

pub(in crate::worker) struct BootRecovery {
    observed: Option<(DelegateOwner, DelegateBootId)>,
    busy: Option<u64>,
    serial: u64,
    walking: bool,
    next: Option<BotBootScan>,
    page: VecDeque<BotBootEntry>,
    followup: bool,
    retry_needed: bool,
    due: Instant,
    probe_at: Instant,
    probe_not_before: Instant,
    probe_requested: bool,
    round: u32,
    /// What this worker owes the delegate about the team pause (M8). It is
    /// kept here because the boot `Hello` is what shows a new delegate run;
    /// its sends use this lane's job and none of the walk's state.
    pub(super) team: team_pause::TeamCoordination,
}
impl Default for BootRecovery {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            observed: None,
            busy: None,
            serial: 0,
            walking: false,
            next: None,
            page: VecDeque::new(),
            followup: true,
            retry_needed: false,
            due: now,
            probe_at: now,
            probe_not_before: now,
            probe_requested: false,
            round: 0,
            team: Default::default(),
        }
    }
}
impl BootRecovery {
    fn request(&mut self) {
        self.followup = true;
        self.probe_requested = true;
        self.probe_at = self.probe_at.min(Instant::now().max(self.probe_not_before));
    }
    fn begin(&mut self) {
        self.walking = true;
        self.next = Some(BotBootScan::Start {});
    }
    fn probed(&mut self, now: Instant) {
        // Rate limit from completion too: a slow successful Probe and a stream
        // of mutations must leave the advancing inventory a scheduling turn.
        self.probe_not_before = now + Duration::from_secs(1);
        self.probe_at = if self.probe_requested {
            self.probe_not_before
        } else {
            now + PROBE_EVERY
        };
    }
    fn defer(&mut self, now: Instant) {
        self.due = now + Duration::from_millis(250) * (1u32 << self.round.min(7));
        self.round = (self.round + 1).min(7);
    }
    fn finish(&mut self, now: Instant) {
        if std::mem::take(&mut self.retry_needed) {
            self.begin();
            self.defer(now);
            // A fresh-pass request must survive errors, even if this causes
            // one extra pass after the retry succeeds.
        } else if std::mem::take(&mut self.followup) {
            self.begin();
            self.round = 0;
        } else {
            self.walking = false;
            self.round = 0;
        }
    }
}

enum Work {
    Hello(BotBootScan),
    Replay {
        boot: DelegateBootId,
        bot: Bot,
        admission: koru_delegate_core::BotAdmissionBarrier,
        policy: BotPolicyBarrier,
        routine: ApiResult<Option<policy::dispatch::RoutineDispatch>>,
    },
    /// The team value (M8): what the delegate is told, and the delegate run
    /// it is meant for. Its answer goes to `team_pause.rs`, never to the walk.
    Team {
        boot: DelegateBootId,
        barrier: koru_delegate_core::BotTeamBarrier,
    },
}
pub(in crate::worker) struct BotBootJob {
    serial: u64,
    work: Work,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
}
pub(in crate::worker) struct BotBootResult {
    serial: u64,
    work: Work,
    result: Result<(DelegateOwner, BotReply), koru_delegate_api::Failure>,
    routine_ready: bool,
}
fn unavailable() -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(
        "bot_boot_unavailable",
        "Bot permissions are still being reconciled.",
        true,
    )
}
fn delegate_owner(bot: &Bot) -> DelegateOwner {
    DelegateOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}
impl BotBootJob {
    pub(in crate::worker) fn run(self) -> BotBootResult {
        let mut routine_ready = true;
        let deadline = Instant::now() + IPC_WITHIN;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let (owner, command) = match &self.work {
                Work::Hello(scan) => {
                    let owner = self.authority.owner().map_err(|_| unavailable())?;
                    let owner = DelegateOwner {
                        user_scope: owner.user_scope,
                        device: owner.device,
                    };
                    if matches!(scan,BotBootScan::Continue {cursor} if cursor.owner != owner) {
                        return Err(koru_delegate_api::Failure::new(
                            "bot_boot_owner_changed",
                            "Bot owner changed.",
                            true,
                        ));
                    }
                    (
                        owner,
                        D::Hello {
                            owner,
                            scan: scan.clone(),
                        },
                    )
                }
                Work::Replay {
                    boot,
                    bot,
                    admission,
                    policy,
                    ..
                } => {
                    let mut pages = if policy.effects_allowed {
                        bot.selected_pages.clone()
                    } else {
                        Vec::new()
                    };
                    if policy.effects_allowed && !pages.contains(&bot.config.role) {
                        pages.push(bot.config.role.clone());
                    }
                    let checked = self
                        .authority
                        .inspect(&[bot.home_workspace], &pages)
                        .map_err(|_| unavailable())?;
                    let owner = delegate_owner(bot);
                    if checked.owner.user_scope != owner.user_scope
                        || checked.owner.device != owner.device
                    {
                        return Err(koru_delegate_api::Failure::new(
                            "bot_boot_owner_changed",
                            "Bot owner changed.",
                            true,
                        ));
                    }
                    checked
                        .workspace(bot.home_workspace)
                        .map_err(|_| unavailable())?;
                    for page in &pages {
                        checked.page(page).map_err(|_| unavailable())?;
                    }
                    (
                        owner,
                        D::SynchronizeBootBot {
                            boot: *boot,
                            barrier: admission.clone(),
                            policy: policy.clone(),
                        },
                    )
                }
                Work::Team { barrier, .. } => {
                    // The value names the owner the `Hello` showed. It is sent
                    // only while that is still this worker's owner.
                    let owner = self.authority.owner().map_err(|_| unavailable())?;
                    if owner.user_scope != barrier.owner.user_scope
                        || owner.device != barrier.owner.device
                    {
                        return Err(koru_delegate_api::Failure::new(
                            "bot_boot_owner_changed",
                            "Bot owner changed.",
                            true,
                        ));
                    }
                    (
                        barrier.owner,
                        D::TeamBarrier {
                            barrier: barrier.clone(),
                        },
                    )
                }
            };
            // A deadline is unknown execution. No completion is inferred from
            // its expiration; later passes derive the current evidence again.
            if Instant::now()>=deadline { return Err(unavailable()); }
            let reply = self
                .delegate
                .execute_within(command, deadline.saturating_duration_since(Instant::now()))
                .map(|reply| (owner, reply))?;
            // Global replay is independent. An unavailable Routine cannot
            // erase this Bot's successful receipt or fence ordinary work.
            if let Work::Replay { routine, boot, admission, policy, .. } = &self.work {
                let exact = matches!(&reply.1,BotReply::BootSynchronized {receipt}
                    if receipt.boot==*boot && receipt.barrier==*admission && receipt.policy==policy.key
                    && receipt.config_revision==policy.config_revision && receipt.policy_revision==policy.policy_revision
                    && receipt.state==BotBootState::Ready);
                routine_ready = catch_unwind(AssertUnwindSafe(|| match routine {
                    Ok(Some(routine)) if exact => routine
                        .synchronize(
                            self.authority.as_ref(),
                            self.delegate.as_ref(),
                            deadline,
                        )
                        .is_ok(),
                    Ok(None) => true,
                    _ => false,
                }))
                .unwrap_or(false);
            }
            Ok(reply)
        }))
        .unwrap_or_else(|_| Err(unavailable()));
        BotBootResult {
            serial: self.serial,
            work: self.work,
            result,
            routine_ready,
        }
    }
}

impl WorkerState {
    /// Call after canonical Bot mutation or durable Submit, including Library
    /// changes. Never rewinds an active cursor or stores a per-Bot payload queue.
    pub(in crate::worker) fn request_bot_boot_refresh(&mut self) {
        self.bot_boot.request();
    }

    fn boot_policy(
        &self,
        snapshot: &koru_ai_sqlite::BotBootSnapshot,
    ) -> ApiResult<BotPolicyBarrier> {
        let bot = &snapshot.bot;
        let mut policy = match self.lifecycle_policy_barrier(bot)? {
            Some(policy) => policy,
            None => {
                let mut policy = policy::barrier(bot)?;
                if bot.lifecycle != BotLifecycle::Active {
                    let owner = BotOwner {
                        user_scope: bot.user_scope,
                        device: bot.owner_device,
                    };
                    let holders = self
                        .repository
                        .bot_lifecycle_holders(owner, &[(bot.id, bot.lifecycle)])
                        .map_err(bot_failure)?;
                    let holder = holders
                        .get(&bot.id)
                        .ok_or_else(|| bot_failure(BotError::Storage))?;
                    let op = self
                        .repository
                        .bot_lifecycle_operation(owner.user_scope, holder.request_id)
                        .map_err(bot_failure)?
                        .ok_or_else(|| bot_failure(BotError::Storage))?;
                    if op.owner != owner
                        || op.bot != bot.id
                        || op.kind != holder.kind
                        || op.stage != BotLifecycleStage::Completed
                        || !bot.paused
                    {
                        return Err(bot_failure(BotError::Storage));
                    }
                    policy.effects_allowed = false;
                }
                policy
            }
        };
        if policy.config_revision != bot.revision || policy.policy_revision != bot.policy_revision {
            return Err(bot_failure(BotError::RevisionConflict));
        }
        if let Some(pending) = &snapshot.pending_access {
            if *pending != policy {
                return Err(bot_failure(BotError::Conflict));
            }
            policy = pending.clone();
        }
        Ok(policy)
    }

    /// Queue one send of the team value on this lane. `Some(true)` when it
    /// was queued, `Some(false)` when the lane is full, `None` when this
    /// composition has no delegate to tell.
    pub(super) fn submit_team_barrier(
        &mut self,
        serial: u64,
        boot: DelegateBootId,
        barrier: koru_delegate_core::BotTeamBarrier,
    ) -> Option<bool> {
        let (Some(authority), Some(delegate), Some(pool)) = (
            &self.bot_authority,
            &self.bot_delegate,
            &self.provider_admin,
        ) else {
            return None;
        };
        Some(
            pool.try_submit(ProviderAdminJob::BotBoot(BotBootJob {
                serial,
                work: Work::Team { boot, barrier },
                authority: authority.clone(),
                delegate: delegate.clone(),
            }))
            .is_ok(),
        )
    }

    pub(in crate::worker) fn schedule_bot_boot(&mut self) {
        // The team value is owed to a new delegate run before any of its
        // Bots is synchronized (M8), and again whenever the row moved.
        self.schedule_team_barrier();
        let now = Instant::now();
        if self.bot_boot.busy.is_some() || now < self.bot_boot.due {
            return;
        }
        let (Some(authority), Some(delegate), Some(pool)) = (
            &self.bot_authority,
            &self.bot_delegate,
            &self.provider_admin,
        ) else {
            return;
        };
        let authority = authority.clone();
        let delegate = delegate.clone();
        let work = if self.bot_boot.observed.is_none() || now >= self.bot_boot.probe_at {
            Work::Hello(BotBootScan::Probe {})
        } else {
            if !self.bot_boot.walking {
                if !std::mem::take(&mut self.bot_boot.followup) {
                    return;
                }
                self.bot_boot.begin();
            }
            // The per-Bot pass of a delegate run starts only after the first
            // send of the team value to that run came back (M8). A send that
            // failed is repeated beside the pass: until it is echoed the
            // delegate holds every Bot of this owner anyway.
            if self.bot_boot.page.front().is_some()
                && !self.bot_boot.team.sent_to(self.bot_boot.observed)
            {
                return;
            }
            if let Some(entry) = self.bot_boot.page.front() {
                let owner = BotOwner {
                    user_scope: entry.identity.owner.user_scope,
                    device: entry.identity.owner.device,
                };
                let snapshot =
                    self.repository
                        .bot_boot_snapshot(owner, entry.identity.bot, &entry.admission);
                let prepared = snapshot.map_err(bot_failure).and_then(|snapshot| {
                    let policy = self.boot_policy(&snapshot)?;
                    Ok((snapshot, policy))
                });
                let (snapshot, policy) = match prepared {
                    Ok(pair) => pair,
                    Err(_) => {
                        self.bot_boot.page.pop_front();
                        self.bot_boot.retry_needed = true;
                        return;
                    }
                };
                Work::Replay {
                    boot: self.bot_boot.observed.expect("observed").1,
                    bot: snapshot.bot,
                    admission: snapshot.admission,
                    policy,
                    routine: self
                        .repository
                        .bot_submission_for_run(owner, entry.identity.bot, entry.identity.run)
                        .map_err(bot_failure)
                        .and_then(|receipt| {
                            let receipt = receipt.ok_or_else(not_found)?;
                            if receipt.intent.submission.provenance.routine.is_none() {
                                return Ok(None);
                            }
                            policy::dispatch::RoutineDispatch::snapshot(
                                &self.repository,
                                &receipt.intent.submission,
                            )
                            .map(Some)
                        }),
                }
            } else if let Some(scan) = &self.bot_boot.next {
                Work::Hello(scan.clone())
            } else {
                self.bot_boot.finish(now);
                return;
            }
        };
        let replay = matches!(work, Work::Replay { .. });
        let serial = self
            .bot_boot
            .serial
            .checked_add(1)
            .expect("boot job sequence exhausted");
        if pool
            .try_submit(ProviderAdminJob::BotBoot(BotBootJob {
                serial,
                work,
                authority,
                delegate,
            }))
            .is_ok()
        {
            self.bot_boot.serial = serial;
            self.bot_boot.busy = Some(serial);
            if !replay && (self.bot_boot.observed.is_none() || now >= self.bot_boot.probe_at) {
                self.bot_boot.probe_not_before = now + Duration::from_secs(1);
                self.bot_boot.probe_requested = false;
            }
            if replay {
                self.bot_boot.page.pop_front();
            }
        }
    }

    pub(in crate::worker) fn complete_bot_boot(&mut self, result: BotBootResult) {
        // A team value's answer has its own serial and never touches the walk.
        if let Work::Team { boot, barrier } = result.work {
            let answer = result.result.map(|(_, reply)| reply);
            return self.complete_team_barrier(result.serial, boot, barrier, answer);
        }
        if self.bot_boot.busy != Some(result.serial) {
            return;
        }
        self.bot_boot.busy = None;
        let now = Instant::now();
        let (owner, reply) = match result.result {
            Ok(value) => value,
            Err(error) => {
                if matches!(
                    error.code.as_str(),
                    "bot_boot_stale" | "bot_boot_cursor_stale" | "bot_boot_owner_changed"
                ) {
                    self.bot_boot.observed = None;
                    self.memory_recovery.invalidate_owner();
                    self.routine_recovery.invalidate_owner();
                    self.routine_listings.invalidate();
                    self.memory_reads.invalidate();
                    self.memory_writes.invalidate();
                    self.bot_boot.page.clear();
                    self.bot_boot.begin();
                }
                if matches!(result.work, Work::Replay { .. }) {
                    self.bot_boot.retry_needed = true;
                }
                self.bot_boot.defer(now);
                return;
            }
        };
        match (result.work, reply) {
            (Work::Hello(scan), BotReply::Hello { hello }) => {
                let observed = (owner, hello.boot);
                let changed = self.bot_boot.observed != Some(observed);
                let valid_cursor = hello.next.as_ref().is_none_or(|cursor| {
                    cursor.valid_for(hello.boot, owner)
                        && match &scan {
                            BotBootScan::Continue { cursor: old } => {
                                cursor.through == old.through && cursor.after > old.after
                            }
                            _ => cursor.after > 0,
                        }
                });
                let continuation = match &scan {
                    BotBootScan::Continue { cursor } => Some(cursor),
                    _ => None,
                };
                if hello.entries.len() > usize::from(koru_delegate_core::BOT_BOOT_PAGE_LIMIT)
                    || !valid_cursor
                    || continuation.is_some_and(|cursor| !cursor.valid_for(hello.boot, owner))
                    || (matches!(scan, BotBootScan::Probe {})
                        && (!hello.entries.is_empty() || hello.next.is_some()))
                {
                    self.bot_boot.defer(now);
                    self.bot_boot.retry_needed = true;
                    return;
                }
                self.bot_boot.observed = Some(observed);
                if changed {
                    // A delegate run this worker has not told holds every Bot
                    // of this owner until it is sent the team value (M8).
                    self.bot_boot.team.observe(owner, hello.boot);
                    // An adopted owner (or a new delegate boot) is owed this
                    // process's dispatcher announce (M5): the delegate then
                    // closes what a previous process left open.
                    if let Some(hosted) = &self.connections {
                        hosted.announce.adopted(owner);
                    }
                    self.routine_listings.invalidate();
                    self.memory_reads.invalidate();
                    self.memory_writes.invalidate();
                    self.memory_recovery.observe(BotOwner {
                        user_scope: owner.user_scope,
                        device: owner.device,
                    });
                    self.memory_recovery.request();
                    self.routine_recovery.observe(BotOwner {
                        user_scope: owner.user_scope,
                        device: owner.device,
                    });
                    self.routine_recovery.request();
                    self.bot_boot.page.clear();
                    self.bot_boot.begin();
                    self.bot_boot.retry_needed = false;
                    self.bot_boot.round = 0;
                    self.bot_recovery_after = Some(0);
                    self.recover_bot_policy(true);
                    self.recover_bot_operations();
                }
                if matches!(scan, BotBootScan::Probe {}) {
                    self.bot_boot.probed(now);
                    return;
                }
                let lower = continuation.map_or(0, |cursor| cursor.after);
                let upper = hello.next.as_ref().map_or_else(
                    || continuation.map_or(i64::MAX as u64, |cursor| cursor.through),
                    |cursor| cursor.after,
                );
                self.bot_boot.next = hello.next.map(|cursor| BotBootScan::Continue { cursor });
                let mut previous = lower;
                for entry in hello.entries {
                    let identity = &entry.identity;
                    let valid_hint = match &entry.admission {
                        BotBootAdmission::Present { barrier } => {
                            barrier.owner == owner && barrier.bot == identity.bot
                        }
                        _ => true,
                    };
                    if identity.owner == owner
                        && identity.sequence > previous
                        && identity.sequence <= upper
                        && identity.sequence <= i64::MAX as u64
                        && valid_hint
                    {
                        previous = identity.sequence;
                        self.bot_boot.page.push_back(entry);
                    } else {
                        self.bot_boot.retry_needed = true;
                    }
                }
            }
            (
                Work::Replay {
                    boot,
                    bot,
                    admission,
                    policy,
                    routine,
                },
                BotReply::BootSynchronized { receipt },
            ) => {
                let current = self.repository.bot_any_lifecycle(bot.id).ok().flatten();
                let same = current.as_ref().is_some_and(|(current, _)| {
                    delegate_owner(current) == owner
                        && current.revision == bot.revision
                        && current.policy_revision == bot.policy_revision
                        && current.paused == bot.paused
                        && current.lifecycle == bot.lifecycle
                });
                if !same
                    || self.bot_boot.observed != Some((owner, boot))
                    || receipt.boot != boot
                    || receipt.barrier != admission
                    || receipt.policy != policy.key
                    || receipt.config_revision != policy.config_revision
                    || receipt.policy_revision != policy.policy_revision
                {
                    self.bot_boot.retry_needed = true;
                    return;
                }
                if receipt.state == BotBootState::PendingPolicy {
                    self.bot_boot.retry_needed = true;
                    self.queue_native_recovery(
                        owner,
                        bot.id,
                        None,
                        None,
                        koru_delegate_core::NativeDrainReason::PolicyPending { key: policy.key },
                    );
                }
                if !result.routine_ready
                    || match routine {
                        Ok(Some(routine)) => routine.recheck(&self.repository).is_err(),
                        Ok(None) => false,
                        Err(_) => true,
                    }
                {
                    self.bot_boot.retry_needed = true;
                }
                // Ready/NoUnsettledWork settle only transient replay work. No
                // canonical operation receipt is acknowledged by this callback.
            }
            _ => {
                self.bot_boot.retry_needed = true;
                self.bot_boot.defer(now);
            }
        }
    }
}

#[cfg(test)]
mod tests;
