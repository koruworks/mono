// SPDX-License-Identifier: MPL-2.0
//! Durable Memory creation is driven on the existing physical creation lane.
use super::super::*;
use std::collections::VecDeque;
#[cfg(test)]
mod tests;

pub(in crate::worker) struct MemoryRecovery {
    owner: Option<BotOwner>,
    epoch: u64,
    probe: bool,
    scan: Option<Option<BotMemoryRecoveryCursor>>,
    page: VecDeque<Result<BotMemoryRecoveryIdentity, BotError>>,
    followup: bool,
    owed: bool,
    failures: u32,
    retry_at: Option<Instant>,
    active: Option<Cancellation>,
}
impl Default for MemoryRecovery {
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
impl MemoryRecovery {
    pub(in crate::worker) fn request(&mut self) {
        self.followup = true;
    }
    /// An explicit admitted Ensure/Retry need not wait out an earlier failure's
    /// backoff. Preserve the advancing pass and coalesce one following pass.
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
            .expect("Memory owner epoch exhausted");
        self.owner = None;
        self.probe = true;
        self.scan = None;
        self.page.clear();
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
                .expect("Memory owner epoch exhausted");
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
        creation: BotMemoryCreation,
        revision: u64,
    },
}
pub(in crate::worker) struct MemoryCreationJob {
    epoch: u64,
    work: Work,
    authority: Arc<dyn BotAuthority>,
    cancellation: Cancellation,
}
pub(in crate::worker) struct MemoryCreationResult {
    epoch: u64,
    work: Work,
    cancellation: Cancellation,
    result: Result<Answer, MemoryAuthorityError>,
}
enum Answer {
    Owner(BotOwner),
    Page(PreparedMemoryPage),
}
impl MemoryCreationJob {
    pub(in crate::worker) fn run(self) -> MemoryCreationResult {
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
                Work::Candidate { creation, .. } => {
                    let identity = &creation.identity;
                    let proof = self.authority.inspect(&[identity.workspace], &[])?;
                    if proof.owner != identity.owner {
                        return Err(BotError::Forbidden.into());
                    }
                    let reader = proof.workspace(identity.workspace)?;
                    let page = self.authority.prepare_memory_page(
                        identity.owner,
                        reader,
                        identity,
                        &self.cancellation,
                    )?;
                    let proof = self.authority.inspect(&[identity.workspace], &[])?;
                    if proof.owner != identity.owner
                        || proof.workspace(identity.workspace)? != reader
                    {
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
        MemoryCreationResult {
            epoch: self.epoch,
            work: self.work,
            cancellation: self.cancellation,
            result,
        }
    }
}

impl WorkerState {
    pub(in crate::worker) fn schedule_memory_creation(&mut self) {
        if self
            .memory_recovery
            .retry_at
            .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        if self.memory_recovery.probe {
            // A database without unsettled Memory performs no extra Store hop
            // at startup. This boolean is only a scheduling hint, not authority.
            match self.repository.bot_memory_recovery_needed() {
                Ok(false) => {
                    self.memory_recovery.probe = false;
                    self.memory_recovery.followup = false;
                    return;
                }
                Err(_) => {
                    self.memory_recovery.defer();
                    return;
                }
                Ok(true) => {}
            }
            self.submit_memory_creation(Work::Probe);
            return;
        }
        let Some(owner) = self.memory_recovery.owner else {
            return;
        };
        if self.memory_recovery.scan.is_none() && self.memory_recovery.page.is_empty() {
            if self.memory_recovery.followup {
                self.memory_recovery.followup = false;
                self.memory_recovery.scan = Some(None);
            } else if self.memory_recovery.owed {
                self.memory_recovery.owed = false;
                self.memory_recovery.followup = true;
                self.memory_recovery.defer();
                return;
            } else {
                return;
            }
        }
        if self.memory_recovery.page.is_empty()
            && let Some(cursor) = self.memory_recovery.scan
        {
            match self.repository.bot_memory_recovery_page(owner, cursor) {
                Ok(page) => {
                    self.memory_recovery.page = page.entries.into();
                    self.memory_recovery.scan = page.next.map(Some);
                }
                Err(_) => {
                    self.memory_recovery.defer();
                    return;
                }
            }
        }
        // At most one fixed page is inspected per loop iteration. Errors never
        // hold its front entry; the next pass retries what remains owed.
        while let Some(entry) = self.memory_recovery.page.pop_front() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.memory_recovery.owed = true;
                    continue;
                }
            };
            match self.repository.bot_memory_recovery_candidate(owner, &entry) {
                Ok((creation, revision)) => {
                    if !self.submit_memory_creation(Work::Candidate { creation, revision }) {
                        self.memory_recovery.page.push_front(Ok(entry));
                    }
                    return;
                }
                Err(_) => self.memory_recovery.owed = true,
            }
        }
    }

    fn submit_memory_creation(&mut self, work: Work) -> bool {
        let Some(authority) = self.bot_authority.clone() else {
            return false;
        };
        let cancellation = Cancellation::default();
        let job = MemoryCreationJob {
            epoch: self.memory_recovery.epoch,
            work,
            authority,
            cancellation: cancellation.clone(),
        };
        if self.submit_creation_work(CreationWork::Memory(job)) {
            self.memory_recovery.active = Some(cancellation);
            true
        } else {
            self.memory_recovery.defer();
            false
        }
    }

    pub(in crate::worker) fn complete_memory_creation(&mut self, result: MemoryCreationResult) {
        self.memory_recovery.active = None;
        if result.epoch != self.memory_recovery.epoch || result.cancellation.is_cancelled() {
            self.memory_recovery.followup = true;
            return;
        }
        match result.work {
            Work::Probe => match result.result {
                Ok(Answer::Owner(owner)) => {
                    self.memory_recovery.observe(owner);
                    self.memory_recovery.probe = false;
                    self.memory_recovery.retry_at = None;
                }
                _ => self.memory_recovery.defer(),
            },
            Work::Candidate { creation, revision } => {
                let identity = &creation.identity;
                let entry = BotMemoryRecoveryIdentity {
                    bot: identity.bot,
                    request: identity.creation_request,
                    observed: creation.observed(),
                };
                let current = self
                    .repository
                    .bot_memory_recovery_candidate(identity.owner, &entry);
                if !matches!(current, Ok((ref current, current_revision)) if current == &creation && current_revision == revision)
                    || self.memory_recovery.owner != Some(identity.owner)
                {
                    self.memory_recovery.owed = true;
                    return;
                }
                let now = Timestamp::now().millis();
                let applied = match result.result {
                    Ok(Answer::Page(PreparedMemoryPage::Confirmed(proof)))
                        if proof.identity == *identity =>
                    {
                        self.repository
                            .bind_bot_memory(&proof, creation.observed(), now)
                            .map(|_| ())
                    }
                    Ok(Answer::Page(PreparedMemoryPage::Disagreement(reason))) => self
                        .repository
                        .transition_bot_memory_creation(
                            identity,
                            creation.observed(),
                            BotMemoryTransition::Disagreement(reason),
                            now,
                        )
                        .map(|_| ()),
                    _ => {
                        self.memory_recovery.owed = true;
                        self.repository
                            .transition_bot_memory_creation(
                                identity,
                                creation.observed(),
                                BotMemoryTransition::Unknown,
                                now,
                            )
                            .map(|_| ())
                    }
                };
                if applied.is_err() {
                    self.memory_recovery.owed = true;
                } else {
                    self.request_bot_boot_refresh();
                }
            }
        }
    }
}
