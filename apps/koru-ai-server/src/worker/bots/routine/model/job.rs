// SPDX-License-Identifier: MPL-2.0
//! One owned continuation across finite physical work and the writer callback.
use super::*;
use koru_delegate_core::{ActorRef, RunToolAuthority};

pub(super) struct FreshCreate {
    pub bot: Bot,
    pub routine: Option<Box<policy::routine::RoutineLookup>>,
}
pub(super) enum Phase {
    Caller,
    Item(Box<BotRoutineCreation>),
    Reply,
}
pub(super) enum Evidence {
    Caller,
    Item(RoutineOverviewObservation),
    Reply,
}
pub(in crate::worker) struct RunRoutineJob {
    pub(super) request: RunRoutineRequest,
    pub(super) subject: RoutineToolSubject,
    pub(super) ticket: listing::Ticket,
    pub(super) authority: Arc<dyn BotAuthority>,
    pub(super) delegate: Arc<dyn BotDelegatePort>,
    pub(super) phase: Phase,
    pub(super) progress: Option<listing::ListingProgress>,
    pub(super) fresh: Option<FreshCreate>,
    pub(super) value: Option<RunRoutineResult>,
    pub(super) committed: bool,
    pub(super) reply: SyncSender<ApiResult<RunRoutineResult>>,
}
pub(in crate::worker) struct RunRoutineCompletion {
    pub(super) job: Box<RunRoutineJob>,
    pub(super) result: ApiResult<Evidence>,
}
impl RunRoutineJob {
    pub(super) fn live(&self) -> ApiResult<()> {
        if self.request.cancellation.is_cancelled() || Instant::now() >= self.request.deadline {
            return Err(timed_out());
        }
        if !self.ticket.valid() {
            return Err(denied());
        }
        Ok(())
    }
    fn generation(&self) -> ApiResult<()> {
        self.live()?;
        let reply = self
            .delegate
            .execute_within(
                DelegateCommand::ResolveRunAuthority {
                    run: self.request.subject.run,
                    generation: self.request.subject.generation,
                },
                self.request
                    .deadline
                    .saturating_duration_since(Instant::now()),
            )
            .map_err(|_| unavailable_failure())?;
        self.live()?;
        let BotReply::RunAuthority { authority } = reply else {
            return Err(denied());
        };
        match authority.bot_subject() {
            Some((provenance, policy))
                if *provenance == self.request.subject.provenance
                    && *policy == self.request.subject.captured => {}
            _ => return Err(denied()),
        }
        // O147: while the Run's decision wait is open, a new Create or Retry
        // is refused; reads, and the truthful reply of work this job already
        // committed (O93's journal finishes on its own), stay available.
        if matches!(authority, RunToolAuthority::BotDecisionWait { .. })
            && self.request.operation.mutation()
            && !self.committed
        {
            return Err(decision_in_progress());
        }
        Ok(())
    }
    fn human(&self, pages: &[BotPage]) -> ApiResult<()> {
        self.live()?;
        let m = &self.subject.metadata;
        let proof = self
            .authority
            .inspect(&[m.home_workspace], pages)
            .map_err(tool_failure)?;
        self.live()?;
        if proof.owner != m.owner
            || proof.workspace(m.home_workspace).map_err(tool_failure)? != self.subject.profile
        {
            return Err(denied());
        }
        match self.subject.receipt.intent.submission.request.requested_by {
            ActorRef::Person { profile } if profile == self.subject.profile => {}
            _ => return Err(denied()),
        }
        for page in pages {
            proof.page(page).map_err(tool_failure)?;
        }
        Ok(())
    }
    pub(in crate::worker) fn run(self: Box<Self>) -> Box<RunRoutineCompletion> {
        let result = catch_unwind(AssertUnwindSafe(|| self.execute()))
            .unwrap_or_else(|_| Err(unavailable_failure()));
        Box::new(RunRoutineCompletion { job: self, result })
    }
    fn execute(&self) -> ApiResult<Evidence> {
        self.generation()?;
        let pages: Vec<_> = match (&self.phase, &self.fresh, &self.request.operation) {
            (Phase::Caller, Some(_), RunRoutineOperation::Create(args)) => args
                .documents
                .iter()
                .map(|id| BotPage {
                    workspace: self.subject.metadata.home_workspace,
                    document: *id,
                })
                .collect(),
            _ => Vec::new(),
        };
        self.human(&pages)?;
        let evidence = (|| match &self.phase {
            Phase::Caller => {
                if let Some(fresh) = &self.fresh {
                    // Workspace reads do not use selected-page write predicates
                    // or fresh-effect synchronization (O94).
                    let current = policy::barrier(&fresh.bot)?;
                    let p = &self.request.subject.provenance;
                    let captured = &self.request.subject.captured;
                    if !current.policy.valid()
                        || current.policy.workspace != captured.workspace
                        || current.key.owner != p.owner
                        || current.key.bot != p.bot
                        || current.config_revision < p.config_revision
                        || current.policy_revision < p.policy_revision
                    {
                        return Err(denied());
                    }
                    match (p.routine, fresh.routine.as_deref()) {
                        (None, None) => {}
                        (Some(subject), Some(routine)) => {
                            let grant = routine.barrier();
                            if !grant.valid()
                                || grant.owner != p.owner
                                || grant.bot != p.bot
                                || grant.schedule != subject.schedule
                                || grant.policy.workspace != captured.workspace
                                || grant.grant_revision < subject.grant_revision
                            {
                                return Err(denied());
                            }
                            self.live()?;
                            routine
                                .verify_delegate(self.delegate.as_ref(), self.request.deadline)?;
                            self.live()?;
                        }
                        _ => return Err(denied()),
                    }
                }
                Ok(Evidence::Caller)
            }
            Phase::Item(creation) => self
                .authority
                .read_routine_overview(
                    self.subject.metadata.owner,
                    self.subject.profile,
                    &creation.identity,
                    &self.request.cancellation,
                )
                .map(Evidence::Item)
                .map_err(tool_failure),
            Phase::Reply => Ok(Evidence::Reply),
        })();
        // A per-item error never hides a later shared human/generation change.
        self.human(&pages)?;
        self.generation()?;
        self.live()?;
        evidence
    }
    pub(super) fn refuse(self: Box<Self>, error: Failure) {
        let error = if self.committed && self.request.operation.mutation() {
            unknown()
        } else {
            caller_failure(error)
        };
        let _ = self.reply.send(Err(error));
    }
}
