// SPDX-License-Identifier: MPL-2.0

//! Expiry has a separate consuming type: no AI preparation or execution path.
use super::*;
use koru_delegate_core::RoutineExpiredClaimFacts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutineExpiryPlan {
    pub occurrence_ms: i64,
    /// Opaque skip-row identity, never an AI receipt or executable Run intent.
    pub planned_run: RunId,
    pub representative_miss: Option<RoutineRepresentativeMiss>,
}

/// The private inner token reuses the existing live authority validator. This
/// phase-only wrapper cannot be passed to an executable claim conversion.
///
/// ```compile_fail
/// use koru_delegate_document::{RoutineExpiryAuthority, ScheduleClaimAuthority};
/// fn executable(_: ScheduleClaimAuthority) {}
/// fn wrong_phase(expiry: RoutineExpiryAuthority) { executable(expiry); }
/// ```
pub struct RoutineExpiryAuthority {
    inner: ScheduleClaimAuthority,
}
impl fmt::Debug for RoutineExpiryAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutineExpiryAuthority([REDACTED])")
    }
}
impl RoutineExpiryAuthority {
    pub fn into_expired_bot_routine(self) -> Result<RoutineExpiredClaimFacts> {
        self.inner.validate_current()?;
        let association = self
            .inner
            .definition
            .routine
            .as_ref()
            .ok_or(ScheduleDocumentError::ClaimBindingMismatch)?;
        let BoundScheduleClaim::ExpiredRoutine(plan) = self.inner.binding else {
            return Err(ScheduleDocumentError::ClaimBindingMismatch);
        };
        RoutineExpiredClaimFacts::from_consumed_authority(
            RoutineSlot {
                owner: association.owner,
                bot: association.bot,
                schedule: self.inner.schedule,
                occurrence_ms: plan.occurrence_ms,
            },
            routine_identity(
                &self.inner.definition,
                self.inner.workspace,
                self.inner.epoch,
            )?,
            self.inner.workspace,
            association.creation_request,
            plan.planned_run,
            plan.representative_miss,
        )
        .ok_or(ScheduleDocumentError::ClaimBindingMismatch)
    }
}

impl ScheduleDocument {
    /// Bind a nonexecuting expiry proposal without model/capture/preparation.
    /// The Store handle separately pins the physical DocumentId to ScheduleId.
    pub fn expired_routine_authority(
        &self,
        workspace: WorkspaceId,
        device: DeviceId,
        plan: RoutineExpiryPlan,
    ) -> Result<Option<RoutineExpiryAuthority>> {
        self.writable()?;
        self.firings()?;
        let header = self.header()?;
        let definition = &header.definition;
        let association = definition
            .routine
            .as_ref()
            .ok_or(ScheduleDocumentError::ClaimBindingMismatch)?;
        if association.home != workspace
            || association.owner.device != device
            || plan.occurrence_ms < definition.effective_at_ms
            || plan.planned_run.to_storage().parse::<RunId>().ok() != Some(plan.planned_run)
            || plan.representative_miss.is_some_and(|miss| {
                miss.occurrence_ms < definition.effective_at_ms
                    || miss.occurrence_ms >= plan.occurrence_ms
                    || miss.planned_run == plan.planned_run
                    || miss.planned_run.to_storage().parse::<RunId>().ok() != Some(miss.planned_run)
            })
        {
            return Err(ScheduleDocumentError::ClaimBindingMismatch);
        }
        if !self.may_claim(device)? {
            return Ok(None);
        }
        let handoff = self.handoff_state()?;
        Ok(Some(RoutineExpiryAuthority {
            inner: ScheduleClaimAuthority {
                doc: self.doc.clone(),
                compatibility: self.compatibility,
                schedule: header.schedule,
                revision: definition.revision,
                owner: device,
                epoch: handoff.epoch,
                definition: header.definition,
                workspace,
                binding: BoundScheduleClaim::ExpiredRoutine(plan),
            },
        }))
    }
}
