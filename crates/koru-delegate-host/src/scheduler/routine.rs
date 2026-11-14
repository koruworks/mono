// SPDX-License-Identifier: MPL-2.0

//! Finite process-local rendezvous. Unknown completion retains the exact run.
use super::*;
use koru_delegate_core::{
    BotAdmissionBarrier, FreshBotPolicyProof, RoutinePrepared, WorkspaceScope,
};

const CANCEL_SLICE: Duration = Duration::from_millis(50);

pub(super) struct PendingRoutine {
    run: RunId,
    release: Option<mpsc::Receiver<PortResult<()>>>,
}

fn refused(code: &'static str) -> PortError {
    PortError::new(Port::Schedules, code, "Routine rendezvous is unresolved")
}

impl ActorEffects {
    pub(super) fn finish_pending_routine_release(&mut self, deadline: Instant) -> PortResult<()> {
        match self.routine.as_ref().map(|pending| pending.run) {
            Some(run) => self.release_routine_wait(run, deadline),
            None => Ok(()),
        }
    }
    pub(super) fn new(actor: mpsc::Sender<ActorCommand>, stopped: Arc<AtomicBool>) -> Self {
        Self {
            actor,
            stopped,
            routine: None,
        }
    }

    pub(super) fn no_pending_routine(&self) -> PortResult<()> {
        if self.routine.is_some() {
            Err(refused("routine_release_required"))
        } else {
            Ok(())
        }
    }

    pub(super) fn begin_routine_wait(
        &mut self,
        prepared: &RoutinePrepared,
        admission: &BotAdmissionBarrier,
        proof: &FreshBotPolicyProof,
        scope: ScopeIdentity,
        authored_scope: WorkspaceScope,
        deadline: Instant,
    ) -> PortResult<ScheduleAdmission> {
        self.no_pending_routine()?;
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(refused("routine_begin_expired"));
        }
        if !prepared.valid() || prepared.submission.request.scope != authored_scope {
            return Err(refused("routine_subject_mismatch"));
        }
        let run = prepared.ids.run;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, answer) = mpsc::sync_channel(1);
        self.routine = Some(PendingRoutine { run, release: None });
        if self
            .actor
            .send(ActorCommand::RoutineBegin {
                prepared: Box::new(prepared.clone()),
                admission: admission.clone(),
                proof: proof.clone(),
                scope,
                authored_scope,
                deadline,
                cancelled: cancelled.clone(),
                reply,
            })
            .is_err()
        {
            // Send failure proves this fresh message was never enqueued.
            self.routine = None;
            return Err(Self::disconnected());
        }
        self.finish_routine_begin(run, deadline, cancelled, answer)
    }

    // The receive half also covers a successful actor send immediately followed
    // by cancellation; receipt delivery alone never renews a stale deadline.
    fn finish_routine_begin(
        &mut self,
        run: RunId,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        answer: mpsc::Receiver<PortResult<ScheduleAdmission>>,
    ) -> PortResult<ScheduleAdmission> {
        loop {
            let now = Instant::now();
            if self.stopped.load(Ordering::Acquire) || now >= deadline {
                break;
            }
            match answer.recv_timeout(deadline.saturating_duration_since(now).min(CANCEL_SLICE)) {
                Ok(result) => {
                    if self.stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
                        break;
                    }
                    if !matches!(result, Ok(ScheduleAdmission::Granted)) {
                        self.routine = None;
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        cancelled.store(true, Ordering::Release);
        drop(answer);
        // Same sender preserves Begin -> Release ordering. A lost/late Granted
        // reply is not absence, and cleanup enqueue is not acknowledgement.
        let _ = self.queue_routine_release(run);
        Err(refused("routine_begin_unknown"))
    }

    fn queue_routine_release(&mut self, run: RunId) -> PortResult<()> {
        if let Some(pending) = &self.routine {
            if pending.run != run {
                return Err(refused("routine_release_mismatch"));
            }
            if pending.release.is_some() {
                return Ok(());
            }
        } else {
            self.routine = Some(PendingRoutine { run, release: None });
        }
        let (reply, answer) = mpsc::sync_channel(1);
        self.actor
            .send(ActorCommand::RoutineRelease { run, reply })
            .map_err(|_| Self::disconnected())?;
        self.routine.as_mut().expect("release retains run").release = Some(answer);
        Ok(())
    }

    pub(super) fn release_routine_wait(&mut self, run: RunId, deadline: Instant) -> PortResult<()> {
        self.queue_routine_release(run)?;
        // Cleanup remains allowed after Stop, with a separate finite budget.
        // It cannot renew readiness or authorize another begin before this ack.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(refused("routine_release_unknown"));
        }
        let result = self
            .routine
            .as_ref()
            .expect("release retains run")
            .release
            .as_ref()
            .expect("release receiver")
            .recv_timeout(remaining);
        match result {
            Ok(Ok(())) => {
                self.routine = None;
                Ok(())
            }
            Ok(Err(error)) => {
                self.routine.as_mut().expect("release retains run").release = None;
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(refused("routine_release_unknown")),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.routine.as_mut().expect("release retains run").release = None;
                Err(Self::disconnected())
            }
        }
    }
}

#[cfg(test)]
mod tests;
