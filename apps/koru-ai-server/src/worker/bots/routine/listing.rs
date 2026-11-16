// SPDX-License-Identifier: MPL-2.0
//! Owner listing continuations on the existing preparation lane.
use super::*;
use koru_delegate_core::ScheduleId;
use std::sync::atomic::AtomicU64;
mod progress;
pub(super) use progress::ListingProgress;
#[cfg(test)]
mod tests;

const CAPACITY: usize = PREPARATION_QUEUE_CAPACITY + 1;
struct Slots([AtomicU64; CAPACITY]);
struct Epoch {
    cancellation: Cancellation,
}

pub(in crate::worker) struct Listings {
    slots: Arc<Slots>,
    epoch: Arc<Epoch>,
    serial: u64,
    owner: Option<BotOwner>,
}
impl Default for Listings {
    fn default() -> Self {
        Self {
            slots: Arc::new(Slots(std::array::from_fn(|_| AtomicU64::new(0)))),
            epoch: Arc::new(Epoch {
                cancellation: Cancellation::default(),
            }),
            serial: 0,
            owner: None,
        }
    }
}
impl Listings {
    pub(in crate::worker) fn invalidate(&mut self) {
        self.epoch.cancellation.cancel();
        self.epoch = Arc::new(Epoch {
            cancellation: Cancellation::default(),
        });
        self.owner = None;
        // Deliberately retain slots. Old physical jobs still own their tickets.
    }
    pub(super) fn acquire(&mut self) -> ApiResult<Ticket> {
        let serial = self.serial.checked_add(1).ok_or_else(unavailable_failure)?;
        let index = self
            .slots
            .0
            .iter()
            .position(|slot| {
                slot.compare_exchange(0, serial, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
            .ok_or_else(|| {
                failure(
                    "ai_worker_saturated",
                    "The Routine setup lane is busy. Try again.",
                    true,
                )
            })?;
        self.serial = serial;
        Ok(Ticket {
            slots: self.slots.clone(),
            epoch: self.epoch.clone(),
            index,
            serial,
        })
    }
    pub(super) fn owns(&self, ticket: &Ticket) -> bool {
        Arc::ptr_eq(&self.epoch, &ticket.epoch) && ticket.valid()
    }
    pub(super) fn bind_owner(&mut self, owner: BotOwner) -> ApiResult<()> {
        if self.owner.is_some_and(|old| old != owner) {
            self.invalidate();
            return Err(bot_failure(BotError::Forbidden));
        }
        self.owner = Some(owner);
        Ok(())
    }
}
/// Non-Clone: there is exactly one phase/completion owner. Out-of-order
/// workflows have separate slots; there is no global completion high water.
pub(super) struct Ticket {
    slots: Arc<Slots>,
    epoch: Arc<Epoch>,
    index: usize,
    serial: u64,
}
impl Ticket {
    pub(super) fn valid(&self) -> bool {
        !self.epoch.cancellation.is_cancelled()
            && self.slots.0[self.index].load(Ordering::Acquire) == self.serial
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = self.slots.0[self.index].compare_exchange(
            self.serial,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}
/// Ends with the synchronous client wait. An HTTP future drop does not itself
/// drop a still-running spawn_blocking wait; the finite wait remains the bound.
pub(in crate::worker) struct CancelOnDrop(pub Cancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

enum Phase {
    Owner,
    Authority,
    Item(Box<BotRoutineCreation>),
    Finish,
}
enum Evidence {
    Owner(BotOwner),
    Authority,
    Item(BotRoutineLiveObservation),
    Finished,
}
pub(in crate::worker) struct ListingJob {
    ticket: Ticket,
    context: CommandContext,
    bot: BotId,
    after: Option<ScheduleId>,
    limit: u8,
    cancellation: Cancellation,
    deadline: Instant,
    authority: Arc<dyn BotAuthority>,
    reply: SyncSender<ApiResult<BotResponse>>,
    phase: Phase,
    progress: Option<ListingProgress>,
    observed_at_ms: i64,
    /// This computer's time zone, read for each row, never cached.
    zone: Option<control::ZoneSource>,
}
pub(in crate::worker) struct ListingResult {
    job: ListingJob,
    result: ApiResult<Evidence>,
}
impl ListingJob {
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
        let s = &self
            .progress
            .as_ref()
            .ok_or_else(unavailable_failure)?
            .snapshot;
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
    pub(in crate::worker) fn run(self) -> ListingResult {
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.live()?;
            let value = match &self.phase {
                Phase::Owner => Evidence::Owner(self.authority.owner().map_err(bot_failure)?),
                Phase::Authority => {
                    self.current()?;
                    Evidence::Authority
                }
                Phase::Item(creation) => {
                    self.current()?;
                    let s = &self
                        .progress
                        .as_ref()
                        .ok_or_else(unavailable_failure)?
                        .snapshot;
                    let value = self.authority.read_routine_overview(
                        s.metadata.owner,
                        s.profile,
                        &creation.identity,
                        &self.cancellation,
                    );
                    // Even an item refusal must not hide a shared human change.
                    self.current()?;
                    // The next time, computed as the detail computes it. A
                    // row reads no history.
                    let this = control::this_computer(self.zone.as_ref(), s.metadata.owner.device);
                    Evidence::Item(BotRoutineLiveObservation::observed(
                        value.map_err(bot_failure)?,
                        self.observed_at_ms,
                        this.as_ref(),
                    ))
                }
                Phase::Finish => {
                    self.current()?;
                    Evidence::Finished
                }
            };
            self.live()?;
            Ok(value)
        }))
        .unwrap_or_else(|_| Err(unavailable_failure()));
        ListingResult { job: self, result }
    }
    fn refuse(self, failure: Failure) {
        let _ = self.reply.send(Err(failure));
    }
}

impl WorkerState {
    pub(in crate::worker) fn start_routine_listing(
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
            validate_routine_list_bytes(&request).map_err(bot_failure)?;
            let BotCommand::ReadRoutines {
                bot_id,
                after_schedule,
                limit,
            } = request
            else {
                return Err(bot_failure(BotError::Invalid));
            };
            validate_routine_list_request(bot_id, after_schedule, limit).map_err(bot_failure)?;
            Ok(ListingJob {
                ticket: self.routine_listings.acquire()?,
                context,
                bot: bot_id,
                after: after_schedule,
                limit,
                cancellation,
                deadline,
                authority: self.bot_authority.clone().ok_or_else(unavailable_failure)?,
                reply: reply.clone(),
                phase: Phase::Owner,
                progress: None,
                observed_at_ms: Timestamp::now().millis(),
                zone: self.routine_controls.zone(),
            })
        })();
        match prepared {
            Ok(job) => self.submit_routine_listing(job),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn submit_routine_listing(&mut self, job: ListingJob) {
        if let Err(error) = job.live() {
            job.refuse(error);
            return;
        }
        let Some(pool) = &self.provider_admin else {
            job.refuse(unavailable_failure());
            return;
        };
        match pool.try_submit(ProviderAdminJob::BotRoutineListing(job)) {
            Ok(()) => {}
            Err(AdminSubmitError {
                job: ProviderAdminJob::BotRoutineListing(job),
                failure,
            }) => job.refuse(failure),
            Err(_) => unreachable!("listing job returned another job"),
        }
    }
    pub(in crate::worker) fn complete_routine_listing(&mut self, result: ListingResult) {
        let ListingResult { mut job, result } = result;
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
                    job.progress = Some(ListingProgress::new(
                        &self.repository,
                        owner,
                        job.bot,
                        job.after,
                        job.limit,
                    )?);
                    job.phase = Phase::Authority;
                    return Ok(None);
                }
                (Phase::Authority, Evidence::Authority) => {}
                (Phase::Item(_), Evidence::Item(observation)) => {
                    job.progress
                        .as_mut()
                        .ok_or_else(unavailable_failure)?
                        .observe(observation)?;
                }
                (Phase::Finish, Evidence::Finished) => {
                    let response = BotResponse::Routines {
                        page: job
                            .progress
                            .as_mut()
                            .ok_or_else(unavailable_failure)?
                            .finish(&self.repository)?,
                    };
                    validate_routine_list_bytes(&response).map_err(bot_failure)?;
                    job.live()?;
                    return Ok(Some(response));
                }
                _ => return Err(unavailable_failure()),
            }
            job.live()?;
            if let Some(creation) = job
                .progress
                .as_mut()
                .ok_or_else(unavailable_failure)?
                .next(&self.repository, || {
                    job.ticket.valid()
                        && !job.cancellation.is_cancelled()
                        && Instant::now() < job.deadline
                })?
            {
                job.phase = Phase::Item(Box::new(creation));
                return Ok(None);
            }
            job.phase = Phase::Finish;
            Ok(None)
        })();
        match next {
            Ok(None) => self.submit_routine_listing(job),
            Ok(Some(response)) => {
                let _ = job.reply.send(Ok(response));
            }
            Err(error) => job.refuse(error),
        }
    }
}
