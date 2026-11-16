// SPDX-License-Identifier: MPL-2.0
//! Selected reads share listing's physical tickets and the preparation lane.
use super::*;
use koru_ai_sqlite::{RoutineDetailSnapshot, RoutineReadSubject};
use koru_delegate_core::ScheduleId;

#[derive(Clone, Copy)]
enum Phase {
    Owner,
    Authority,
    Detail,
    Finish,
}
enum Evidence {
    Owner(BotOwner),
    Authority,
    Detail(RoutineDetailLive, RoutineFiringsObservation),
    Finished,
}
pub(in crate::worker) struct DetailJob {
    ticket: listing::Ticket,
    context: CommandContext,
    bot: BotId,
    schedule: ScheduleId,
    before: Option<i64>,
    limit: u8,
    cancellation: Cancellation,
    deadline: Instant,
    observed_at_ms: i64,
    authority: Arc<dyn BotAuthority>,
    reply: SyncSender<ApiResult<BotResponse>>,
    phase: Phase,
    subject: Option<RoutineReadSubject>,
    snapshot: Option<RoutineDetailSnapshot>,
    live: RoutineDetailLive,
    /// One page of the Schedule's own firings, read with the observation.
    firings: RoutineFiringsObservation,
    /// This computer's time zone, read on every detail read, never cached.
    zone: Option<control::ZoneSource>,
    missing: bool,
}
pub(in crate::worker) struct DetailResult {
    job: DetailJob,
    result: ApiResult<Evidence>,
}
impl DetailJob {
    fn live(&self) -> ApiResult<()> {
        authorize(&self.context, AiScope::ManageBots)?;
        if !self.ticket.valid()
            || self.cancellation.is_cancelled()
            || Instant::now() >= self.deadline
        {
            return Err(unavailable_failure());
        }
        Ok(())
    }
    fn current(&self) -> ApiResult<()> {
        self.live()?;
        let s = self.subject.as_ref().ok_or_else(unavailable_failure)?;
        let proof = self
            .authority
            .inspect(&[s.metadata.home_workspace], &[])
            .map_err(bot_failure)?;
        if proof.owner != s.metadata.owner
            || proof
                .workspace(s.metadata.home_workspace)
                .map_err(bot_failure)?
                != s.profile
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        self.live()
    }
    pub(in crate::worker) fn run(self) -> DetailResult {
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.live()?;
            let evidence = match self.phase {
                Phase::Owner => Evidence::Owner(self.authority.owner().map_err(bot_failure)?),
                Phase::Authority => {
                    self.current()?;
                    Evidence::Authority
                }
                Phase::Detail => {
                    // The document reader owns its before/after human proof.
                    // Finish adds the final common proof without duplicating
                    // another pair around that same Store operation.
                    self.live()?;
                    let snapshot = self.snapshot.as_ref().ok_or_else(unavailable_failure)?;
                    let value = self.authority.read_routine_detail_page(
                        snapshot.subject.metadata.owner,
                        snapshot.subject.profile,
                        &snapshot.creation.identity,
                        self.observed_at_ms,
                        self.before,
                        self.limit,
                        &self.cancellation,
                    );
                    self.live()?;
                    let (observation, firings) = value.map_err(bot_failure)?;
                    // A floating rule this computer owns is shown in this
                    // computer's zone; any other stays "owner local".
                    let this = control::this_computer(
                        self.zone.as_ref(),
                        snapshot.subject.metadata.owner.device,
                    );
                    Evidence::Detail(
                        observation.live_on(self.observed_at_ms, this.as_ref()),
                        firings,
                    )
                }
                Phase::Finish => {
                    self.current()?;
                    Evidence::Finished
                }
            };
            self.live()?;
            Ok(evidence)
        }))
        .unwrap_or_else(|_| Err(unavailable_failure()));
        DetailResult { job: self, result }
    }
    fn refuse(self, error: Failure) {
        let _ = self.reply.send(Err(error));
    }
}
impl WorkerState {
    pub(in crate::worker) fn start_routine_detail(
        &mut self,
        command: WorkerCommand,
        cancellation: Cancellation,
        deadline: Instant,
    ) {
        let WorkerCommand::Bots {
            context,
            request,
            reply,
        } = command
        else {
            command.refuse(bot_failure(BotError::Invalid));
            return;
        };
        let prepared = (|| {
            authorize(&context, AiScope::ManageBots)?;
            validate_routine_detail_bytes(&request).map_err(bot_failure)?;
            let BotCommand::ReadRoutineDetail {
                bot_id,
                schedule_id,
                before_occurrence_ms,
                history_limit,
            } = request
            else {
                return Err(bot_failure(BotError::Invalid));
            };
            validate_routine_detail_request(bot_id, schedule_id, history_limit)
                .map_err(bot_failure)?;
            Ok(DetailJob {
                ticket: self.routine_listings.acquire()?,
                context,
                bot: bot_id,
                schedule: schedule_id,
                before: before_occurrence_ms,
                limit: history_limit,
                cancellation,
                deadline,
                observed_at_ms: Timestamp::now().millis(),
                authority: self.bot_authority.clone().ok_or_else(unavailable_failure)?,
                reply: reply.clone(),
                phase: Phase::Owner,
                subject: None,
                snapshot: None,
                live: RoutineDetailLive::Unavailable {},
                firings: RoutineFiringsObservation::Unavailable,
                zone: self.routine_controls.zone(),
                missing: false,
            })
        })();
        match prepared {
            Ok(job) => self.submit_routine_detail(job),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn submit_routine_detail(&mut self, job: DetailJob) {
        if let Err(error) = job.live() {
            job.refuse(error);
            return;
        }
        let Some(pool) = &self.provider_admin else {
            job.refuse(unavailable_failure());
            return;
        };
        match pool.try_submit(ProviderAdminJob::BotRoutineDetail(job)) {
            Ok(()) => {}
            Err(AdminSubmitError {
                job: ProviderAdminJob::BotRoutineDetail(job),
                failure,
            }) => job.refuse(failure),
            Err(_) => unreachable!("detail job returned another job"),
        }
    }
    pub(in crate::worker) fn complete_routine_detail(&mut self, result: DetailResult) {
        let DetailResult { mut job, result } = result;
        let next = (|| -> ApiResult<Option<BotResponse>> {
            job.live()?;
            if !self.routine_listings.owns(&job.ticket) {
                return Err(unavailable_failure());
            }
            let evidence = result?;
            let phase = std::mem::replace(&mut job.phase, Phase::Finish);
            match (phase, evidence) {
                (Phase::Owner, Evidence::Owner(owner)) => {
                    self.routine_listings.bind_owner(owner)?;
                    job.subject = Some(
                        self.repository
                            .routine_read_subject(owner, job.bot)
                            .map_err(bot_failure)?,
                    );
                    job.phase = Phase::Authority;
                }
                (Phase::Authority, Evidence::Authority) => {
                    let subject = job.subject.as_ref().ok_or_else(unavailable_failure)?;
                    // Exact absence is answered only after common human proof.
                    let snapshot = match self.repository.routine_detail_snapshot(
                        subject,
                        job.schedule,
                        job.before,
                        job.limit,
                    ) {
                        Ok(snapshot) => snapshot,
                        Err(BotError::NotFound) => {
                            job.missing = true;
                            job.phase = Phase::Finish;
                            return Ok(None);
                        }
                        Err(error) => return Err(bot_failure(error)),
                    };
                    job.phase = if snapshot.creation.state == BotRoutineCreationState::Bound {
                        Phase::Detail
                    } else {
                        Phase::Finish
                    };
                    job.snapshot = Some(snapshot);
                }
                (Phase::Detail, Evidence::Detail(live, firings)) => {
                    job.live = live;
                    job.firings = firings;
                    job.phase = Phase::Finish;
                }
                (Phase::Finish, Evidence::Finished) => {
                    if job.missing {
                        let subject = job.subject.as_ref().ok_or_else(unavailable_failure)?;
                        if self
                            .repository
                            .routine_read_subject(subject.metadata.owner, subject.metadata.id)
                            .map_err(bot_failure)?
                            != *subject
                        {
                            return Err(bot_failure(BotError::Conflict));
                        }
                        return match self.repository.routine_detail_snapshot(
                            subject,
                            job.schedule,
                            job.before,
                            job.limit,
                        ) {
                            Err(BotError::NotFound) => Err(bot_failure(BotError::NotFound)),
                            Err(error) => Err(bot_failure(error)),
                            Ok(_) => Err(bot_failure(BotError::Conflict)),
                        };
                    }
                    let snapshot = job.snapshot.as_ref().ok_or_else(unavailable_failure)?;
                    self.repository
                        .validate_routine_detail(snapshot)
                        .map_err(bot_failure)?;
                    let response = BotResponse::RoutineDetail {
                        reply: BotRoutineDetailEnvelope {
                            detail: snapshot
                                .response_with(job.observed_at_ms, job.live.clone(), &job.firings)
                                .map_err(bot_failure)?,
                        },
                    };
                    validate_routine_detail_bytes(&response).map_err(bot_failure)?;
                    job.live()?;
                    return Ok(Some(response));
                }
                _ => return Err(unavailable_failure()),
            }
            job.live()?;
            Ok(None)
        })();
        match next {
            Ok(None) => self.submit_routine_detail(job),
            Ok(Some(response)) => {
                let _ = job.reply.send(Ok(response));
            }
            Err(error) => job.refuse(error),
        }
    }
}
