// SPDX-License-Identifier: MPL-2.0
//! One physical creation slot, shared fairly by Bot, Memory and Routine effects.
use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum Family {
    Bot,
    Memory,
    Routine,
}

pub(in crate::worker) enum CreationWork {
    Bot(CreationJob),
    Memory(memory::recovery::MemoryCreationJob),
    Routine(routine::RoutineCreationJob),
}
impl CreationWork {
    fn family(&self) -> Family {
        match self {
            Self::Bot(_) => Family::Bot,
            Self::Memory(_) => Family::Memory,
            Self::Routine(_) => Family::Routine,
        }
    }
}

pub(in crate::worker) struct CreationLaneJob {
    serial: u64,
    work: CreationWork,
}
pub(in crate::worker) struct CreationLaneResult {
    serial: u64,
    result: CreationResult,
}
enum CreationResult {
    Bot(CreationStepResult),
    Memory(memory::recovery::MemoryCreationResult),
    Routine(routine::RoutineCreationResult),
}
impl CreationLaneJob {
    pub(in crate::worker) fn run(self) -> CreationLaneResult {
        let result = match self.work {
            CreationWork::Bot(job) => CreationResult::Bot(job.run()),
            CreationWork::Memory(job) => CreationResult::Memory(job.run()),
            CreationWork::Routine(job) => CreationResult::Routine(job.run()),
        };
        CreationLaneResult {
            serial: self.serial,
            result,
        }
    }
}

pub(in crate::worker) struct CreationLane {
    serial: u64,
    active: Option<(u64, Family)>,
    last: Family,
}
impl Default for CreationLane {
    fn default() -> Self {
        Self {
            serial: 0,
            active: None,
            last: Family::Routine,
        }
    }
}

impl CreationLane {
    fn complete(&mut self, serial: u64, family: Family) -> bool {
        if self.active != Some((serial, family)) {
            return false;
        }
        self.active = None;
        true
    }
}

impl WorkerState {
    pub(in crate::worker) fn submit_creation_work(&mut self, work: CreationWork) -> bool {
        if self.creation_lane.active.is_some() {
            return false;
        }
        let Some(serial) = self.creation_lane.serial.checked_add(1) else {
            return false;
        };
        let family = work.family();
        let Some(pool) = &self.provider_admin else {
            return false;
        };
        if pool
            .try_submit(ProviderAdminJob::BotCreationStep(CreationLaneJob {
                serial,
                work,
            }))
            .is_err()
        {
            return false;
        }
        // Only successful enqueue owns the physical slot. No owner-change or
        // cancellation path clears this; the matching completion does so.
        self.creation_lane.serial = serial;
        self.creation_lane.active = Some((serial, family));
        self.creation_lane.last = family;
        true
    }

    pub(in crate::worker) fn schedule_creation_lanes(&mut self) {
        if self.bot_authority.is_none() || self.provider_admin.is_none() {
            return;
        }
        let bot = self.ready_bot_creation();
        if self.creation_lane.active.is_some() {
            return;
        }
        let order = match self.creation_lane.last {
            Family::Bot => [Family::Memory, Family::Routine, Family::Bot],
            Family::Memory => [Family::Routine, Family::Bot, Family::Memory],
            Family::Routine => [Family::Bot, Family::Memory, Family::Routine],
        };
        for family in order {
            match family {
                Family::Bot => {
                    if let Some(key) = bot {
                        self.step_bot_creation(key);
                    }
                }
                Family::Memory => self.schedule_memory_creation(),
                Family::Routine => self.schedule_routine_creation(),
            }
            if self.creation_lane.active.is_some() {
                break;
            }
        }
    }

    pub(in crate::worker) fn complete_creation_lane(&mut self, result: CreationLaneResult) {
        let family = match &result.result {
            CreationResult::Bot(_) => Family::Bot,
            CreationResult::Memory(_) => Family::Memory,
            CreationResult::Routine(_) => Family::Routine,
        };
        if !self.creation_lane.complete(result.serial, family) {
            return;
        }
        match result.result {
            CreationResult::Bot(result) => self.complete_bot_creation_step(result),
            CreationResult::Memory(result) => self.complete_memory_creation(result),
            CreationResult::Routine(result) => self.complete_routine_creation(result),
        }
    }
}
