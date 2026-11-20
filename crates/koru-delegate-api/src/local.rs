// SPDX-License-Identifier: MPL-2.0

//! [`LocalDelegate`]: the port over this device's delegate worker.
//!
//! The gateway reaches the sessions worker over that worker's own IPC and
//! routes to it; this is the same shape for the delegate worker. One
//! private, root-qualified connection per call — connect, one frame out,
//! one frame in, close — so a slow call never holds a connection another
//! caller needs, and a worker restart is invisible to the next call. A
//! stream keeps its connection and reads frames until the worker ends it.
//!
//! Nothing here retries: a worker that does not answer is
//! `delegate_unavailable`, and the supervisor is the party that brings it
//! back.

use std::sync::Arc;
use std::time::Duration;

use koru_delegate_core::{RunId, WorkspaceId};
use koru_ipc::{Endpoint, Stream, frame};

use crate::ipc::{Caller, Hello, Request, Response, WIRE_VERSION};
use crate::port::{
    DelegatePort, RequestContext, Result, RunStream, ScheduleStream, ScheduleStreamCancellation,
};
use crate::wire::{
    CancelRunResponse, CreateRunRequest, CreateRunResponse, CreateScheduleRequest,
    CreateScheduleResponse, EditScheduleRequest, EditScheduleResponse, Failure, ListRunsResponse,
    ListSchedulesResponse, PauseScheduleRequest, PauseScheduleResponse, PurgeRunResponse,
    RateRunRequest, RateRunResponse, RenameScheduleRequest, RenameScheduleResponse,
    RestoreRunResponse, ResumeRunRequest, ResumeRunResponse, ResumeScheduleRequest,
    ResumeScheduleResponse, RetireScheduleRequest, RetireScheduleResponse, RunStreamItem, RunView,
    ScheduleChange, TimelineQuery, TrashRunRequest, TrashRunResponse,
};

/// How long a call waits for the worker: a creation is durable before it
/// answers, and the ledger and the store each fsync.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct LocalDelegate {
    endpoint: Endpoint,
    product: String,
}

impl LocalDelegate {
    pub fn new(endpoint: Endpoint, product: impl Into<String>) -> Self {
        Self {
            endpoint,
            product: product.into(),
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The worker's own account of itself, or the reason it gave none.
    pub fn hello(&self) -> Result<Hello> {
        match self.call(&Request::Hello)? {
            Response::Hello(hello) => Ok(hello),
            other => Err(unexpected(&other)),
        }
    }

    /// Ask the worker to stop, with the supervisor's authority.
    pub fn stop(&self, authority: String) -> Result<()> {
        match self.call(&Request::Stop { authority })? {
            Response::Stopping => Ok(()),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    pub fn wake_schedules(&self, authority: String) -> Result<()> {
        match self.call(&Request::WakeSchedules { authority })? {
            Response::SchedulesWoken => Ok(()),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn caller(&self, context: &RequestContext) -> Caller {
        Caller {
            product: format!("{}:{}", self.product, context.principal.label),
            trace_id: context.trace_id.clone(),
        }
    }

    fn connect(&self, within: Duration) -> Result<Stream> {
        let stream = koru_ipc::connect_timed(&self.endpoint, CONNECT_TIMEOUT.min(within))
            .map_err(|_| Failure::unavailable())?;
        koru_ipc::set_timeouts(&stream, Some(within));
        Ok(stream)
    }

    pub(crate) fn call(&self, request: &Request) -> Result<Response> {
        self.call_within(request, CALL_TIMEOUT)
    }

    /// One call that waits at most `within` (at least a millisecond) to
    /// connect, write and read; past it the call is `delegate_unavailable`.
    pub(crate) fn call_within(&self, request: &Request, within: Duration) -> Result<Response> {
        let stream = self.connect(within.max(Duration::from_millis(1)))?;
        let mut writer = &stream;
        frame::write_frame(&mut writer, WIRE_VERSION, request)
            .map_err(|_| Failure::unavailable())?;
        let mut reader = &stream;
        read_current_response(&mut reader)
    }
}

/// Check the frame version before assigning the body its current shape. A v4
/// hello does not have v5's schedule counters, but it is still an incompatible
/// worker rather than a malformed response.
pub(crate) fn read_current_response(reader: &mut impl std::io::Read) -> Result<Response> {
    let (version, body) = frame::read_raw(reader).map_err(|_| Failure::unavailable())?;
    if version != WIRE_VERSION {
        return Err(Failure::new(
            "delegate_incompatible",
            "The delegate worker speaks another frame version.",
            false,
        ));
    }
    frame::decode(&body).map_err(|_| Failure::unavailable())
}

fn unexpected(response: &Response) -> Failure {
    Failure::new(
        "delegate_unexpected_response",
        format!(
            "The delegate worker answered with {}.",
            match response {
                Response::RunAuthorityConfirmed {} => "run_authority_confirmed",
                Response::Hello(_) => "hello",
                Response::Stopping => "stopping",
                Response::SchedulesWoken => "schedules_woken",
                Response::ScheduleCreated(_) => "schedule_created",
                Response::ScheduleRenamed(_) => "schedule_renamed",
                Response::ScheduleEdited(_) => "schedule_edited",
                Response::SchedulePaused(_) => "schedule_paused",
                Response::ScheduleResumed(_) => "schedule_resumed",
                Response::ScheduleRetired(_) => "schedule_retired",
                Response::Schedules(_) => "schedules",
                Response::ScheduleWatchReady => "schedule_watch_ready",
                Response::ScheduleChanged(_) => "schedule_changed",
                Response::ScheduleEnd => "schedule_end",
                Response::Created(_) => "created",
                Response::Runs(_) => "runs",
                Response::Run(_) => "run",
                Response::Cancelled(_) => "cancelled",
                Response::Resumed(_) => "resumed",
                Response::Rated(_) => "rated",
                Response::Trashed(_) => "trashed",
                Response::Restored(_) => "restored",
                Response::Purged(_) => "purged",
                Response::Item(_) => "item",
                Response::End => "end",
                Response::Failure(_) => "failure",
            }
        ),
        false,
    )
}

impl DelegatePort for LocalDelegate {
    fn serving(&self) -> bool {
        koru_ipc::connect_timed(&self.endpoint, CONNECT_TIMEOUT).is_ok()
    }

    fn create(
        &self,
        context: RequestContext,
        request: CreateRunRequest,
    ) -> Result<CreateRunResponse> {
        match self.call(&Request::Create {
            caller: self.caller(&context),
            request,
        })? {
            Response::Created(created) => Ok(created),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn list(&self, context: RequestContext, workspace: WorkspaceId) -> Result<ListRunsResponse> {
        match self.call(&Request::List {
            caller: self.caller(&context),
            workspace,
        })? {
            Response::Runs(runs) => Ok(runs),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn run(&self, context: RequestContext, run: RunId, window: TimelineQuery) -> Result<RunView> {
        match self.call(&Request::Run {
            caller: self.caller(&context),
            run,
            window,
        })? {
            Response::Run(view) => Ok(view),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn cancel(&self, context: RequestContext, run: RunId) -> Result<CancelRunResponse> {
        match self.call(&Request::Cancel {
            caller: self.caller(&context),
            run,
        })? {
            Response::Cancelled(cancelled) => Ok(cancelled),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn resume(
        &self,
        context: RequestContext,
        run: RunId,
        request: ResumeRunRequest,
    ) -> Result<ResumeRunResponse> {
        match self.call(&Request::Resume {
            caller: self.caller(&context),
            run,
            confirm: request.confirm,
        })? {
            Response::Resumed(resumed) => Ok(resumed),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn rate(
        &self,
        context: RequestContext,
        run: RunId,
        request: RateRunRequest,
    ) -> Result<RateRunResponse> {
        match self.call(&Request::Rate {
            caller: self.caller(&context),
            run,
            request,
        })? {
            Response::Rated(rated) => Ok(rated),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn trash(
        &self,
        context: RequestContext,
        run: RunId,
        request: TrashRunRequest,
    ) -> Result<TrashRunResponse> {
        match self.call(&Request::Trash {
            caller: self.caller(&context),
            run,
            artifacts: request.artifacts,
        })? {
            Response::Trashed(trashed) => Ok(trashed),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn restore(&self, context: RequestContext, run: RunId) -> Result<RestoreRunResponse> {
        match self.call(&Request::Restore {
            caller: self.caller(&context),
            run,
        })? {
            Response::Restored(restored) => Ok(restored),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn purge(&self, context: RequestContext, run: RunId) -> Result<PurgeRunResponse> {
        match self.call(&Request::Purge {
            caller: self.caller(&context),
            run,
        })? {
            Response::Purged(purged) => Ok(purged),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn stream(
        &self,
        context: RequestContext,
        run: RunId,
        workspace: Option<WorkspaceId>,
        after: Option<u64>,
    ) -> Result<Box<dyn RunStream>> {
        let stream = self.connect(CALL_TIMEOUT)?;
        // A stream waits as long as the run does; the worker ends it.
        koru_ipc::set_timeouts(&stream, None);
        let mut writer = &stream;
        frame::write_frame(
            &mut writer,
            WIRE_VERSION,
            &Request::Stream {
                caller: self.caller(&context),
                run,
                workspace,
                after,
            },
        )
        .map_err(|_| Failure::unavailable())?;
        Ok(Box::new(LocalStream {
            stream,
            ended: false,
        }))
    }

    fn create_schedule(
        &self,
        context: RequestContext,
        request: CreateScheduleRequest,
    ) -> Result<CreateScheduleResponse> {
        match self.call(&Request::CreateSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::ScheduleCreated(created) => Ok(created),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn rename_schedule(
        &self,
        context: RequestContext,
        request: RenameScheduleRequest,
    ) -> Result<RenameScheduleResponse> {
        match self.call(&Request::RenameSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::ScheduleRenamed(response) => Ok(response),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn edit_schedule(
        &self,
        context: RequestContext,
        request: EditScheduleRequest,
    ) -> Result<EditScheduleResponse> {
        match self.call(&Request::EditSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::ScheduleEdited(response) => Ok(response),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn pause_schedule(
        &self,
        context: RequestContext,
        request: PauseScheduleRequest,
    ) -> Result<PauseScheduleResponse> {
        match self.call(&Request::PauseSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::SchedulePaused(response) => Ok(response),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn resume_schedule(
        &self,
        context: RequestContext,
        request: ResumeScheduleRequest,
    ) -> Result<ResumeScheduleResponse> {
        match self.call(&Request::ResumeSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::ScheduleResumed(response) => Ok(response),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn retire_schedule(
        &self,
        context: RequestContext,
        request: RetireScheduleRequest,
    ) -> Result<RetireScheduleResponse> {
        match self.call(&Request::RetireSchedule {
            caller: self.caller(&context),
            request,
        })? {
            Response::ScheduleRetired(response) => Ok(response),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn list_schedules(
        &self,
        context: RequestContext,
        workspace: WorkspaceId,
    ) -> Result<ListSchedulesResponse> {
        match self.call(&Request::ListSchedules {
            caller: self.caller(&context),
            workspace,
        })? {
            Response::Schedules(schedules) => Ok(schedules),
            Response::Failure(failure) => Err(failure),
            other => Err(unexpected(&other)),
        }
    }

    fn watch_schedules(
        &self,
        context: RequestContext,
        workspace: WorkspaceId,
        after: Option<ScheduleChange>,
    ) -> Result<Box<dyn ScheduleStream>> {
        let stream = self.connect(CALL_TIMEOUT)?;
        let mut writer = &stream;
        frame::write_frame(
            &mut writer,
            WIRE_VERSION,
            &Request::WatchSchedules {
                caller: self.caller(&context),
                workspace,
                after,
            },
        )
        .map_err(|_| Failure::unavailable())?;
        let mut reader = &stream;
        match frame::read_frame::<_, Response>(&mut reader) {
            Ok((WIRE_VERSION, Response::ScheduleWatchReady)) => {
                // Admission is bounded like an ordinary call. Only the live
                // stream may wait as long as the worker does.
                koru_ipc::set_timeouts(&stream, None);
                let (reader, writer) = koru_ipc::split(stream);
                let cancellation = Arc::new(LocalScheduleCancellation {
                    writer: std::sync::Mutex::new(Some(Box::new(writer))),
                });
                Ok(Box::new(LocalScheduleStream {
                    reader: Box::new(reader),
                    cancellation,
                    ended: false,
                }))
            }
            Ok((WIRE_VERSION, Response::Failure(failure))) => Err(failure),
            Ok((WIRE_VERSION, other)) => Err(unexpected(&other)),
            _ => Err(Failure::unavailable()),
        }
    }
}

struct LocalStream {
    stream: Stream,
    ended: bool,
}

impl RunStream for LocalStream {
    fn next(&mut self) -> Option<RunStreamItem> {
        if self.ended {
            return None;
        }
        let mut reader = &self.stream;
        match frame::read_frame::<_, Response>(&mut reader) {
            Ok((WIRE_VERSION, Response::Item(item))) => Some(item),
            _ => {
                self.ended = true;
                None
            }
        }
    }
}

struct LocalScheduleStream {
    reader: Box<dyn std::io::Read + Send>,
    cancellation: Arc<LocalScheduleCancellation>,
    ended: bool,
}

struct LocalScheduleCancellation {
    writer: std::sync::Mutex<Option<Box<dyn std::io::Write + Send>>>,
}

impl ScheduleStreamCancellation for LocalScheduleCancellation {
    fn cancel(&self) {
        let Some(mut writer) = self.writer.lock().expect("schedule writer").take() else {
            return;
        };
        let _ = frame::write_frame(&mut writer, WIRE_VERSION, &Request::CancelScheduleWatch);
    }
}

impl ScheduleStream for LocalScheduleStream {
    fn next(&mut self) -> Option<ScheduleChange> {
        if self.ended {
            return None;
        }
        match frame::read_frame::<_, Response>(&mut self.reader) {
            Ok((WIRE_VERSION, Response::ScheduleChanged(change))) => Some(change),
            _ => {
                self.ended = true;
                None
            }
        }
    }

    fn cancellation(&self) -> Arc<dyn ScheduleStreamCancellation> {
        self.cancellation.clone()
    }
}

impl Drop for LocalScheduleStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
