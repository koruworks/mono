// SPDX-License-Identifier: MPL-2.0
//! Durable Routine recovery shares the existing physical creation lane.
use super::*;
use koru_ai_core::ProfileId;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use std::collections::VecDeque;
mod commands;
pub(in crate::worker) use commands::{create_body, retained_create};
#[cfg(test)]
mod tests;

pub(in crate::worker) struct RoutineRecovery {
    owner: Option<BotOwner>,
    epoch: u64,
    probe: bool,
    scan: Option<Option<BotRoutineRecoveryCursor>>,
    page: VecDeque<Result<BotRoutineRecoveryIdentity, BotError>>,
    followup: bool,
    owed: bool,
    failures: u32,
    retry_at: Option<Instant>,
    active: Option<Cancellation>,
}
impl Default for RoutineRecovery {
    fn default() -> Self {
        Self {
            owner: None,
            epoch: 0,
            probe: true,
            scan: None,
            page: VecDeque::new(),
            followup: true,
            owed: false,
            failures: 0,
            retry_at: None,
            active: None,
        }
    }
}
impl RoutineRecovery {
    pub(in crate::worker) fn owner_epoch(&self) -> (Option<BotOwner>, u64) {
        (self.owner, self.epoch)
    }
    pub(in crate::worker) fn request(&mut self) {
        self.followup = true;
    }
    /// An explicit admitted create/retry may bypass old backoff. It does not
    /// cancel the current pass or release its physical creation slot.
    pub(in crate::worker) fn wake(&mut self) {
        self.followup = true;
        self.retry_at = None;
    }
    pub(in crate::worker) fn invalidate_owner(&mut self) {
        if let Some(active) = &self.active {
            active.cancel();
        }
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("Routine owner epoch exhausted");
        self.owner = None;
        self.probe = true;
        self.scan = None;
        self.page.clear();
        self.owed = false;
        self.failures = 0;
        self.retry_at = None;
        self.followup = true;
    }
    pub(in crate::worker) fn observe(&mut self, owner: BotOwner) {
        if self.owner != Some(owner) {
            if let Some(active) = &self.active {
                active.cancel();
            }
            self.epoch = self
                .epoch
                .checked_add(1)
                .expect("Routine owner epoch exhausted");
            self.owner = Some(owner);
            self.scan = None;
            self.page.clear();
            self.owed = false;
            self.retry_at = None;
            self.failures = 0;
            self.followup = true;
        }
    }
    fn defer(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(
            Instant::now()
                + Duration::from_secs((1u64 << self.failures.min(9).saturating_sub(1)).min(300)),
        );
    }
}

enum Work {
    Probe,
    Candidate {
        creation: BotRoutineCreation,
        profile: ProfileId,
    },
}
pub(in crate::worker) struct RoutineCreationJob {
    epoch: u64,
    work: Work,
    authority: Arc<dyn BotAuthority>,
    delegate: Arc<dyn BotDelegatePort>,
    cancellation: Cancellation,
}
pub(in crate::worker) struct RoutineCreationResult {
    epoch: u64,
    work: Work,
    cancellation: Cancellation,
    result: Result<Answer, BotError>,
}
enum Answer {
    Owner(BotOwner),
    Page(PreparedRoutineDocument),
}
impl RoutineCreationJob {
    pub(in crate::worker) fn run(self) -> RoutineCreationResult {
        let result = catch_unwind(AssertUnwindSafe(|| {
            if self.cancellation.is_cancelled() {
                return Err(BotError::Unavailable.into());
            }
            match &self.work {
                Work::Probe => self
                    .authority
                    .owner()
                    .map(Answer::Owner)
                    .map_err(Into::into),
                Work::Candidate { creation, profile } => {
                    let identity = &creation.identity;
                    let owner = BotOwner {
                        user_scope: identity.owner.user_scope,
                        device: identity.owner.device,
                    };
                    let proof = self.authority.inspect(&[identity.home], &[])?;
                    if proof.owner != owner {
                        return Err(BotError::Forbidden.into());
                    }
                    let reader = proof.workspace(identity.home)?;
                    if reader != *profile {
                        return Err(BotError::Forbidden);
                    }
                    // No Store handle or AI writer crosses this finite private
                    // process wait. Only an exact durable marker ack permits creation.
                    let route = identity.route();
                    match self.delegate.execute_within(
                        DelegateCommand::RegisterRoutineRoute {
                            identity: route.clone(),
                        },
                        Duration::from_secs(10),
                    ) {
                        Ok(BotReply::RoutineRoute { identity: echoed }) if echoed == route => {}
                        _ => return Err(BotError::Unavailable),
                    }
                    if self.cancellation.is_cancelled() {
                        return Err(BotError::Unavailable);
                    }
                    let page = self.authority.prepare_routine_document(
                        owner,
                        reader,
                        identity,
                        &self.cancellation,
                    )?;
                    let proof = self.authority.inspect(&[identity.home], &[])?;
                    if proof.owner != owner || proof.workspace(identity.home)? != reader {
                        return Err(BotError::Forbidden.into());
                    }
                    if self.cancellation.is_cancelled() {
                        return Err(BotError::Unavailable.into());
                    }
                    Ok(Answer::Page(page))
                }
            }
        }))
        .unwrap_or(Err(BotError::Unavailable.into()));
        RoutineCreationResult {
            epoch: self.epoch,
            work: self.work,
            cancellation: self.cancellation,
            result,
        }
    }
}

impl WorkerState {
    pub(in crate::worker) fn schedule_routine_creation(&mut self) {
        if self.bot_delegate.is_none() {
            return;
        }
        if self
            .routine_recovery
            .retry_at
            .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        if self.routine_recovery.probe {
            // A database without unsettled Routine performs no extra Store hop
            // at startup. This boolean is only a scheduling hint, not authority.
            match self.repository.bot_routine_recovery_needed() {
                Ok(false) => {
                    self.routine_recovery.probe = false;
                    self.routine_recovery.followup = false;
                    return;
                }
                Err(_) => {
                    self.routine_recovery.defer();
                    return;
                }
                Ok(true) => {}
            }
            self.submit_routine_creation(Work::Probe);
            return;
        }
        let Some(owner) = self.routine_recovery.owner else {
            return;
        };
        if self.routine_recovery.scan.is_none() && self.routine_recovery.page.is_empty() {
            if self.routine_recovery.followup {
                self.routine_recovery.followup = false;
                self.routine_recovery.scan = Some(None);
            } else if self.routine_recovery.owed {
                self.routine_recovery.owed = false;
                self.routine_recovery.followup = true;
                self.routine_recovery.defer();
                return;
            } else {
                return;
            }
        }
        if self.routine_recovery.page.is_empty()
            && let Some(cursor) = self.routine_recovery.scan
        {
            match self.repository.bot_routine_recovery_page(owner, cursor) {
                Ok(page) => {
                    self.routine_recovery.page = page.entries.into();
                    self.routine_recovery.scan = page.next.map(Some);
                }
                Err(_) => {
                    self.routine_recovery.defer();
                    return;
                }
            }
        }
        // At most one fixed page is inspected per loop iteration. Errors never
        // hold its front entry; the next pass retries what remains owed.
        while let Some(entry) = self.routine_recovery.page.pop_front() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.routine_recovery.owed = true;
                    continue;
                }
            };
            match self
                .repository
                .bot_routine_recovery_candidate(owner, &entry)
            {
                Ok((creation, profile)) => {
                    if !self.submit_routine_creation(Work::Candidate { creation, profile }) {
                        self.routine_recovery.page.push_front(Ok(entry));
                    }
                    return;
                }
                Err(BotError::RevisionConflict) => {
                    if self
                        .repository
                        .reject_changed_bot_routine_creation(
                            owner,
                            &entry,
                            Timestamp::now().millis(),
                        )
                        .is_err()
                    {
                        self.routine_recovery.owed = true;
                    }
                }
                Err(_) => self.routine_recovery.owed = true,
            }
        }
    }

    fn submit_routine_creation(&mut self, work: Work) -> bool {
        let Some(authority) = self.bot_authority.clone() else {
            return false;
        };
        let Some(delegate) = self.bot_delegate.clone() else {
            return false;
        };
        let cancellation = Cancellation::default();
        let job = RoutineCreationJob {
            epoch: self.routine_recovery.epoch,
            work,
            authority,
            delegate,
            cancellation: cancellation.clone(),
        };
        if self.submit_creation_work(CreationWork::Routine(job)) {
            self.routine_recovery.active = Some(cancellation);
            true
        } else {
            self.routine_recovery.defer();
            false
        }
    }

    pub(in crate::worker) fn complete_routine_creation(&mut self, result: RoutineCreationResult) {
        self.routine_recovery.active = None;
        if result.epoch != self.routine_recovery.epoch || result.cancellation.is_cancelled() {
            self.routine_recovery.followup = true;
            return;
        }
        match result.work {
            Work::Probe => match result.result {
                Ok(Answer::Owner(owner)) => {
                    self.routine_recovery.observe(owner);
                    self.routine_recovery.probe = false;
                    self.routine_recovery.retry_at = None;
                }
                _ => self.routine_recovery.defer(),
            },
            Work::Candidate { creation, profile } => {
                let identity = &creation.identity;
                let owner = BotOwner {
                    user_scope: identity.owner.user_scope,
                    device: identity.owner.device,
                };
                let entry = BotRoutineRecoveryIdentity {
                    bot: identity.bot,
                    schedule: identity.schedule,
                    observed: creation.observed(),
                };
                let current = self
                    .repository
                    .bot_routine_recovery_candidate(owner, &entry);
                if matches!(&current, Err(BotError::RevisionConflict))
                    && self.routine_recovery.owner == Some(owner)
                {
                    // This rejection uses fresh SQL evidence, never the late
                    // Store result or a failed authority lookup as proof.
                    if self
                        .repository
                        .reject_changed_bot_routine_creation(
                            owner,
                            &entry,
                            Timestamp::now().millis(),
                        )
                        .is_err()
                    {
                        self.routine_recovery.owed = true;
                    }
                    return;
                }
                if !matches!(current, Ok((ref current, current_profile)) if current == &creation && current_profile == profile)
                    || self.routine_recovery.owner != Some(owner)
                {
                    self.routine_recovery.owed = true;
                    return;
                }
                let now = Timestamp::now().millis();
                let applied = match result.result {
                    Ok(Answer::Page(PreparedRoutineDocument::Confirmed(proof)))
                        if proof.identity == *identity =>
                    {
                        self.repository
                            .transition_bot_routine_creation(
                                identity,
                                creation.observed(),
                                BotRoutineCreationTransition::Confirmed(proof),
                                now,
                            )
                            .map(|_| ())
                    }
                    Ok(Answer::Page(PreparedRoutineDocument::Disagreement(reason))) => self
                        .repository
                        .transition_bot_routine_creation(
                            identity,
                            creation.observed(),
                            BotRoutineCreationTransition::Conflict(reason),
                            now,
                        )
                        .map(|_| ()),
                    _ => {
                        self.routine_recovery.owed = true;
                        self.repository
                            .transition_bot_routine_creation(
                                identity,
                                creation.observed(),
                                BotRoutineCreationTransition::Unconfirmed,
                                now,
                            )
                            .map(|_| ())
                    }
                };
                if applied.is_err() {
                    self.routine_recovery.owed = true;
                } else {
                    self.request_bot_boot_refresh();
                }
            }
        }
    }
}
