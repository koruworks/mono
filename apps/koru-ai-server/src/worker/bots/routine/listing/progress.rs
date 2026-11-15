// SPDX-License-Identifier: MPL-2.0
//! Projection progress only. Owner and Run jobs retain their own authorization.
use super::*;
use koru_ai_sqlite::{RoutineListCheckpoint, RoutineListSnapshot};

pub(in crate::worker::bots::routine) struct ListingProgress {
    pub(in crate::worker::bots::routine) snapshot: RoutineListSnapshot,
    pub(super) checkpoints: Vec<RoutineListCheckpoint>,
    pub(super) items: Vec<BotRoutineListItem>,
}
impl ListingProgress {
    pub(in crate::worker::bots::routine) fn new(
        repository: &Repository,
        owner: BotOwner,
        bot: BotId,
        after: Option<ScheduleId>,
        limit: u8,
    ) -> ApiResult<Self> {
        Ok(Self {
            snapshot: repository
                .routine_list_snapshot(owner, bot, after, limit)
                .map_err(bot_failure)?,
            checkpoints: Vec::new(),
            items: Vec::new(),
        })
    }
    /// At most one full body leaves the writer. The caller must requeue at the
    /// shared preparation lane's tail for each returned Bound identity.
    pub(in crate::worker::bots::routine) fn next(
        &mut self,
        repository: &Repository,
        live: impl Fn() -> bool,
    ) -> ApiResult<Option<BotRoutineCreation>> {
        while self.items.len() < self.snapshot.roster.items.len() {
            if !live() {
                return Err(unavailable_failure());
            }
            let (creation, checkpoint, item) = repository
                .routine_list_entry(&self.snapshot, self.items.len())
                .map_err(bot_failure)?;
            self.checkpoints.push(checkpoint);
            self.items.push(item);
            if creation.state == BotRoutineCreationState::Bound {
                return Ok(Some(creation));
            }
        }
        Ok(None)
    }
    pub(in crate::worker::bots::routine) fn observe(
        &mut self,
        observation: RoutineOverviewObservation,
    ) -> ApiResult<()> {
        self.items.last_mut().ok_or_else(unavailable_failure)?.live = observation.into();
        Ok(())
    }
    pub(in crate::worker::bots::routine) fn finish(
        &mut self,
        repository: &Repository,
    ) -> ApiResult<BotRoutineListPage> {
        repository
            .validate_routine_list(&self.snapshot, &self.checkpoints)
            .map_err(bot_failure)?;
        Ok(BotRoutineListPage {
            bot_id: self.snapshot.metadata.id,
            bot_revision: self.snapshot.metadata.revision,
            effective_model: self.snapshot.model.clone(),
            items: std::mem::take(&mut self.items),
            next_schedule: self.snapshot.roster.next,
        })
    }
}
