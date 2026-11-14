// SPDX-License-Identifier: MPL-2.0

//! Process-local Routine leases share ordinary scope arbitration, not execution.
use super::*;
use koru_delegate_core::{
    BotAdmissionBarrier, FreshBotPolicyProof, RoutinePrepared, WorkspaceScope,
};

pub(super) fn refused(code: &'static str) -> PortError {
    PortError::new(
        koru_delegate_runtime::Port::Schedules,
        code,
        "Routine admission is unavailable",
    )
}

impl Actor {
    pub(super) fn begin_routine(
        &mut self,
        prepared: RoutinePrepared,
        admission: BotAdmissionBarrier,
        proof: FreshBotPolicyProof,
        scope: ScopeIdentity,
        authored_scope: WorkspaceScope,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        reply: mpsc::SyncSender<PortResult<ScheduleAdmission>>,
    ) {
        let run = prepared.ids.run;
        let result = (|| {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(refused("routine_begin_expired"));
            }
            if self.device.as_uuid().is_nil()
                || prepared.slot.owner.device != self.device
                || authored_scope != prepared.submission.request.scope
            {
                return Err(refused("routine_subject_mismatch"));
            }
            if self.schedule_admission.is_some() {
                return Err(refused("admission_live"));
            }
            self.orchestrator
                .routine_policy_for_prepared(&prepared, &admission, &proof)
                .map_err(|_| refused("routine_policy_stale"))?;
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(refused("routine_begin_expired"));
            }
            if let Some(holder) = self.reservation_holder_for(run, &authored_scope, Some(&scope)) {
                return Ok(ScheduleAdmission::Busy(holder));
            }
            if let Some(holder) = self.orchestrator.claims().holders_of(&scope).first() {
                return Ok(ScheduleAdmission::Busy(holder.run));
            }
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(refused("routine_begin_expired"));
            }
            self.schedule_admission = Some(ScheduleLease::Routine(run));
            Ok(ScheduleAdmission::Granted)
        })();
        let granted = matches!(result, Ok(ScheduleAdmission::Granted));
        // A receiver may cancel after the last check but before delivery. A
        // successful late send is handled by its retained exact-run release.
        if reply.send(result).is_err() && granted {
            let _ = self.release_routine(run);
        }
    }

    pub(super) fn release_routine(&mut self, run: RunId) -> PortResult<()> {
        self.release_routine_with(run, Self::attempt_deferred_restart_start)
    }

    // Reuse the restart owner's existing injected attempt seam. Tests can
    // observe wake ordering without launching a provider or opening a Store.
    pub(super) fn release_routine_with(
        &mut self,
        run: RunId,
        attempt: impl FnMut(&mut Self, RunId) -> DeferredStartDisposition,
    ) -> PortResult<()> {
        match self.schedule_admission {
            None => Ok(()),
            Some(ScheduleLease::Routine(held)) if held == run => {
                self.schedule_admission = None;
                self.drain_deferred_starts();
                self.drain_deferred_restart_starts_with(attempt);
                self.wake_bots();
                Ok(())
            }
            _ => Err(refused("admission_stale")),
        }
    }
}
