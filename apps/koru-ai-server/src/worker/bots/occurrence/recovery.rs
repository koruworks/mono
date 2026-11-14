// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) struct Recovery {
    cursor: Option<Option<BotRoutineOccurrenceCursor>>,
    page: VecDeque<Result<BotRoutineOccurrenceRecoveryEntry, BotError>>,
    active: bool,
    owed: bool,
    failures: u32,
    next: Option<Instant>,
    followup: bool,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            cursor: Some(None),
            page: VecDeque::new(),
            active: false,
            owed: false,
            failures: 0,
            next: None,
            followup: false,
        }
    }
}
impl Recovery {
    pub(super) fn running(&self) -> bool {
        self.active
    }
    pub(super) fn started(&mut self) {
        self.active = true;
    }
    pub(super) fn finished(&mut self, owed: bool) {
        self.active = false;
        self.owed |= owed;
    }
    pub(super) fn wake(&mut self) {
        self.followup = true;
        self.next = None;
    }
    fn end_pass(&mut self) {
        self.failures = if self.owed {
            self.failures.saturating_add(1)
        } else {
            0
        };
        let delay = if self.followup {
            1
        } else if self.owed {
            (1u64 << self.failures.min(8)).min(300)
        } else {
            60
        };
        self.next = Some(Instant::now() + Duration::from_secs(delay));
        self.cursor = Some(None);
        self.owed = false;
        self.followup = false;
    }
}

impl WorkerState {
    pub(in crate::worker) fn schedule_occurrence_recovery(&mut self) {
        let (owner, epoch) = self.routine_recovery.owner_epoch();
        self.occurrences.synchronize(owner, epoch);
        let Some(owner) = owner else {
            return;
        };
        if self.bot_authority.is_none()
            || self.bot_delegate.is_none()
            || self.provider_admin.is_none()
            || self.occurrences.recovery.running()
            || self
                .occurrences
                .recovery
                .next
                .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        // At most one bounded page is fetched per maintenance turn. Corrupt
        // entries retain their highwater progress and never starve later rows.
        if self.occurrences.recovery.page.is_empty() {
            let Some(cursor) = self.occurrences.recovery.cursor else {
                self.occurrences.recovery.end_pass();
                return;
            };
            match self
                .repository
                .routine_occurrence_recovery_page(owner, cursor)
            {
                Ok(page) => {
                    self.occurrences.recovery.page = page.entries.into();
                    self.occurrences.recovery.cursor = page.next.map(Some);
                }
                Err(_) => {
                    self.occurrences.recovery.owed = true;
                    self.occurrences.recovery.end_pass();
                    return;
                }
            }
        }
        // Reserve before popping a valid candidate; active foreground work
        // retains both slots even after its external waiter has disappeared.
        let Ok(permit) = self.occurrences.gates.acquire(false) else {
            return;
        };
        while let Some(entry) = self.occurrences.recovery.page.pop_front() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.occurrences.recovery.owed = true;
                    continue;
                }
            };
            self.occurrences.recovery.started();
            self.begin_routine_occurrence(Incoming {
                request: Request::Recover(ReadPreparation {
                    slot: entry.slot,
                    semantic_hash: entry.semantic_hash,
                }),
                reply: Reply::Recovery,
                context: RoutineCallContext::new(
                    Uuid::new_v4(),
                    Instant::now() + ROUTINE_BRIDGE_DEADLINE,
                    Cancellation::default(),
                ),
                permit,
            });
            return;
        }
        if self.occurrences.recovery.cursor.is_none() {
            self.occurrences.recovery.end_pass();
        }
    }
}
