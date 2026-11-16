// SPDX-License-Identifier: MPL-2.0

//! An owner Test has its own consuming type. It claims one occurrence of a
//! Routine that is paused, so it shares no function with the due path, the
//! expiry path or an ordinary schedule: [`ScheduleDocument::may_claim`] and
//! `ScheduleClaimAuthority::validate_current` are neither called nor relaxed
//! here. A paused Routine therefore stays unclaimable by every path that
//! runs unattended.
use super::*;
use koru_delegate_schedule::SchedulePauseReason;

/// Single-use proof that one prepared occurrence may be claimed as the owner
/// Test of a Routine that is exactly Paused(Manual). It holds the live Loro
/// document and revalidates it when consumed, so an Enable, an edit, a
/// retirement or a handoff after minting leaves a token that claims nothing.
///
/// It cannot be passed where the ordinary or the expiry authority is expected:
///
/// ```compile_fail
/// use koru_delegate_document::{RoutineTestAuthority, ScheduleClaimAuthority};
/// fn ordinary(_: ScheduleClaimAuthority) {}
/// fn wrong_path(test: RoutineTestAuthority) { ordinary(test); }
/// ```
///
/// ```compile_fail
/// use koru_delegate_document::{RoutineExpiryAuthority, RoutineTestAuthority};
/// fn expiry(_: RoutineExpiryAuthority) {}
/// fn wrong_path(test: RoutineTestAuthority) { expiry(test); }
/// ```
///
/// and neither of those can be passed where it is expected:
///
/// ```compile_fail
/// use koru_delegate_document::{RoutineTestAuthority, ScheduleClaimAuthority};
/// fn test(_: RoutineTestAuthority) {}
/// fn wrong_path(ordinary: ScheduleClaimAuthority) { test(ordinary); }
/// ```
///
/// ```compile_fail
/// use koru_delegate_document::{RoutineExpiryAuthority, RoutineTestAuthority};
/// fn test(_: RoutineTestAuthority) {}
/// fn wrong_path(expiry: RoutineExpiryAuthority) { test(expiry); }
/// ```
///
/// The names above exist, so each example fails for its types alone:
///
/// ```
/// use koru_delegate_document::{
///     RoutineExpiryAuthority, RoutineTestAuthority, ScheduleClaimAuthority,
/// };
/// fn _named(_: Option<(RoutineTestAuthority, ScheduleClaimAuthority, RoutineExpiryAuthority)>) {}
/// ```
pub struct RoutineTestAuthority {
    doc: LoroDoc,
    compatibility: Compatibility,
    schedule: ScheduleId,
    owner: DeviceId,
    epoch: Option<HandoffId>,
    definition: ScheduleDefinition,
    workspace: WorkspaceId,
    prepared: RoutinePrepared,
}

impl fmt::Debug for RoutineTestAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutineTestAuthority([REDACTED])")
    }
}

fn paused_manual(lifecycle: &ScheduleLifecycle) -> bool {
    matches!(
        lifecycle,
        ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual
        }
    )
}

impl RoutineTestAuthority {
    /// The Test's own revalidation. It requires the Routine to still be the
    /// same paused definition under the same owner; it never asks whether the
    /// Routine may be claimed by the ordinary path, because it may not.
    fn validate_paused(&self) -> Result<()> {
        let current = ScheduleDocument {
            doc: self.doc.clone(),
            compatibility: self.compatibility,
        };
        current.writable()?;
        current.firings()?;
        let header = current.header()?;
        let handoff = current.handoff_state()?;
        if current.root().get(K_RETIRED).is_some()
            || header.schedule != self.schedule
            || header.definition != self.definition
            || !paused_manual(&header.definition.lifecycle)
            || handoff.owner != self.owner
            || handoff.epoch != self.epoch
            || !handoff.pending.is_empty()
        {
            return Err(ScheduleDocumentError::StaleClaimAuthority);
        }
        Ok(())
    }

    /// Consume the live authority into the Routine claim facts the ledger's
    /// existing claim transaction takes. There is no representative miss: a
    /// Test catches nothing up.
    pub fn into_bot_routine_test(self) -> Result<RoutineClaimFacts> {
        self.validate_paused()?;
        let association = self
            .definition
            .routine
            .as_ref()
            .ok_or(ScheduleDocumentError::ClaimBindingMismatch)?;
        let slot = RoutineSlot {
            owner: association.owner,
            bot: association.bot,
            schedule: self.schedule,
            occurrence_ms: self.prepared.slot.occurrence_ms,
        };
        let definition = routine_identity(&self.definition, self.workspace, self.epoch)?;
        RoutineClaimFacts::from_consumed_authority(
            self.prepared,
            slot,
            definition,
            self.workspace,
            association.creation_request,
            None,
        )
        .ok_or(ScheduleDocumentError::ClaimBindingMismatch)
    }
}

impl ScheduleDocument {
    /// Mint the owner Test authority for one prepared occurrence.
    ///
    /// Requires a Routine association whose home is `workspace` and whose
    /// owner is `device`; a definition that is exactly Paused(Manual); `device`
    /// as the current owner with no pending handoff; a document that is not
    /// retired; and the prepared occurrence bound exactly as the due path
    /// binds one, with no representative miss. An Active Routine is refused:
    /// the ordinary path owns it.
    pub fn test_claim_authority(
        &self,
        workspace: WorkspaceId,
        device: DeviceId,
        prepared: RoutinePrepared,
    ) -> Result<RoutineTestAuthority> {
        self.writable()?;
        self.firings()?;
        if self.root().get(K_RETIRED).is_some() {
            return Err(ScheduleDocumentError::Retired);
        }
        let header = self.header()?;
        if header.definition.routine.is_none() {
            return Err(ScheduleDocumentError::ClaimBindingMismatch);
        }
        let handoff = self.handoff_state()?;
        if !handoff.pending.is_empty() {
            return Err(ScheduleDocumentError::HandoffPending);
        }
        if handoff.owner != device {
            return Err(ScheduleDocumentError::NotOwner);
        }
        if !paused_manual(&header.definition.lifecycle) {
            return Err(ScheduleDocumentError::LifecycleConflict);
        }
        let BoundScheduleClaim::BotRoutine {
            prepared,
            representative_miss: None,
        } = bind_routine(&header, workspace, device, handoff.epoch, prepared, None)?
        else {
            return Err(ScheduleDocumentError::ClaimBindingMismatch);
        };
        Ok(RoutineTestAuthority {
            doc: self.doc.clone(),
            compatibility: self.compatibility,
            schedule: header.schedule,
            owner: device,
            epoch: handoff.epoch,
            definition: header.definition,
            workspace,
            prepared,
        })
    }
}
