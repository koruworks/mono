// SPDX-License-Identifier: MPL-2.0
//! Exact selected observation snapshots, with no roster scan or execution guard.
use super::*;
use koru_delegate_core::BotWorkingFolder;
use listing::RoutineReadSubject;
use occurrence::recent::RecentSnapshot;

#[derive(Clone, PartialEq, Eq)]
pub struct RoutineDetailSnapshot {
    pub subject: RoutineReadSubject,
    pub creation: BotRoutineCreation,
    /// The grant the Routine runs under now, from the one reader. Held in
    /// the snapshot so an access edit after the read is a conflict.
    pub current: BotRoutineCurrentGrant,
    pub destination: RoutineDestinationObservation,
    before: Option<i64>,
    limit: u8,
    folder_field: Option<String>,
    receipt_stage: String,
    receipt: BotRoutineCreationReceipt,
    history: Result<RecentSnapshot, BotError>,
}
impl std::fmt::Debug for RoutineDetailSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineDetailSnapshot([REDACTED])")
    }
}
impl RoutineDetailSnapshot {
    pub fn response(
        &self,
        observed_at_ms: i64,
        live: RoutineDetailLive,
    ) -> Result<BotRoutineDetail, BotError> {
        let c = &self.creation;
        Ok(BotRoutineDetail {
            bot_id: self.subject.metadata.id,
            schedule_id: c.identity.schedule,
            bot_revision: self.subject.metadata.revision,
            observed_at_ms,
            creation: BotRoutineCreationObservation {
                request_id: c.identity.creation_request,
                state: c.state,
                refusal: c.refusal,
                updated_at_ms: c.updated_at_ms,
            },
            access: self.current.grant.summary().map_err(|_| BotError::Storage)?,
            access_at_setup: c.identity.grant.summary().map_err(|_| BotError::Storage)?,
            configured_model: self.subject.model.clone(),
            destination: self.destination.clone(),
            readiness: routine_readiness(
                self.subject.metadata.paused,
                self.subject.metadata.lifecycle == BotLifecycle::Archived,
                c.state == BotRoutineCreationState::Bound,
                &live,
            ),
            live,
            local_history: self
                .history
                .as_ref()
                .ok()
                .and_then(|h| h.history(self.limit).ok())
                .unwrap_or(RoutineLocalHistoryObservation::Unavailable {}),
        })
    }
}
impl Repository {
    /// Call only after the shared current-human proof. No caller page/model/
    /// folder selection can replace this exact retained creation identity.
    pub fn routine_detail_snapshot(
        &self,
        subject: &RoutineReadSubject,
        schedule: ScheduleId,
        before: Option<i64>,
        limit: u8,
    ) -> Result<RoutineDetailSnapshot, BotError> {
        let metadata = &subject.metadata;
        validate_routine_detail_request(metadata.id, schedule, limit)?;
        if self.routine_read_subject(metadata.owner, metadata.id)? != *subject {
            return Err(BotError::Conflict);
        }
        let creation =
            bounded::by_schedule(&self.connection, metadata.owner, metadata.id, schedule)?
                .ok_or(BotError::NotFound)?;
        if creation.identity.home != metadata.home_workspace {
            return Err(BotError::Forbidden);
        }
        let current = access::current_on(&self.connection, &creation)?;
        let receipt = bounded::receipt(
            &self.connection,
            metadata.owner,
            metadata.id,
            creation.identity.creation_request,
        )?
        .ok_or(BotError::Storage)?;
        let folder_field:Option<String>=self.connection.query_row("SELECT CASE WHEN typeof(working_folder)='text' AND length(CAST(working_folder AS BLOB)) BETWEEN 1 AND 32768 THEN working_folder END FROM bots WHERE bot_id=?1",[metadata.id.to_storage()],|r|r.get(0)).map_err(bad)?;
        let destination = folder_field
            .clone()
            .and_then(|v| BotWorkingFolder::new(v).ok())
            .map(|working_folder| RoutineDestinationObservation::Current {
                home: metadata.home_workspace,
                working_folder,
            })
            .unwrap_or(RoutineDestinationObservation::Unavailable {});
        let history = if creation.state == BotRoutineCreationState::Bound {
            occurrence::recent::read_page(
                self,
                metadata.owner,
                metadata.id,
                schedule,
                creation.identity.home,
                before,
                limit,
            )
        } else {
            Err(BotError::Unavailable)
        };
        Ok(RoutineDetailSnapshot {
            subject: subject.clone(),
            creation,
            current,
            destination,
            before,
            limit,
            folder_field,
            receipt_stage: receipt.stage,
            receipt: receipt.outcome,
            history,
        })
    }
    pub fn validate_routine_detail(
        &self,
        snapshot: &RoutineDetailSnapshot,
    ) -> Result<(), BotError> {
        let subject = self.routine_read_subject(
            snapshot.subject.metadata.owner,
            snapshot.subject.metadata.id,
        )?;
        let current = self.routine_detail_snapshot(
            &subject,
            snapshot.creation.identity.schedule,
            snapshot.before,
            snapshot.limit,
        )?;
        if current != *snapshot {
            return Err(BotError::Conflict);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
