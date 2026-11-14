// SPDX-License-Identifier: MPL-2.0
//! Trusted Rust composition only. No serialized principal or owner facade.
use super::*;
use koru_delegate_core::{
    AttemptGeneration, BotProvenance, CapturedBotPolicy, Objective, RunId, ScheduleId,
};

#[derive(Clone)]
pub struct RunBotSubject {
    pub run: RunId,
    pub generation: AttemptGeneration,
    pub provenance: BotProvenance,
    pub captured: CapturedBotPolicy,
}
#[derive(Clone)]
pub struct RunRoutineCreate {
    pub request_id: BotRequestId,
    pub expected_bot_revision: u64,
    pub name: RoutineScheduleName,
    pub objective: Objective,
    pub documents: Vec<koru_ai_core::DocumentId>,
    pub recurrence: RoutineRecurrenceRule,
}
#[derive(Clone)]
pub enum RunRoutineOperation {
    Create(RunRoutineCreate),
    ReadCreation {
        request_id: BotRequestId,
    },
    RetryCreation {
        original_request_id: BotRequestId,
        observed_token: BotRoutineCreationObserved,
    },
    List {
        after_schedule: Option<ScheduleId>,
        limit: u8,
    },
}
impl RunRoutineOperation {
    pub(super) fn mutation(&self) -> bool {
        matches!(self, Self::Create(_) | Self::RetryCreation { .. })
    }
    pub(super) fn create_command(&self, bot_id: BotId) -> Option<BotCommand> {
        let Self::Create(args) = self else {
            return None;
        };
        Some(BotCommand::CreateRoutine {
            request_id: args.request_id,
            bot_id,
            expected_bot_revision: args.expected_bot_revision,
            name: args.name.clone(),
            objective: args.objective.clone(),
            documents: args.documents.clone(),
            recurrence: args.recurrence.clone(),
        })
    }
    pub(super) fn validate(&self, bot: BotId) -> ApiResult<()> {
        match self {
            Self::Create(args) => {
                if args.documents.len() > 16 {
                    return Err(invalid());
                }
                create_body(&self.create_command(bot).ok_or_else(invalid)?)?;
            }
            Self::ReadCreation { request_id }
            | Self::RetryCreation {
                original_request_id: request_id,
                ..
            } => {
                if request_id.to_storage().parse::<BotRequestId>().ok() != Some(*request_id) {
                    return Err(invalid());
                }
            }
            Self::List {
                after_schedule,
                limit,
            } => {
                validate_routine_list_request(bot, *after_schedule, *limit).map_err(tool_failure)?
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct RunRoutineRequest {
    pub subject: RunBotSubject,
    pub operation: RunRoutineOperation,
    pub deadline: Instant,
    pub cancellation: Cancellation,
}
pub enum RunRoutineResult {
    Created(BotRoutineCreateResult),
    Creation(BotRoutineCreationStatus),
    Routines(BotRoutineListPage),
}

impl AiWorkerClient {
    pub fn routine_for_run(&self, mut request: RunRoutineRequest) -> ApiResult<RunRoutineResult> {
        request.deadline = request.deadline.min(Instant::now() + RESPONSE_TIMEOUT);
        let _cancel = listing::CancelOnDrop(request.cancellation.clone());
        if request.cancellation.is_cancelled() || Instant::now() >= request.deadline {
            return Err(timed_out());
        }
        let deadline = request.deadline;
        let mutation = request.operation.mutation();
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::RunRoutine { request, reply })
            .map_err(queue_failure)?;
        match response.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(value) if Instant::now() < deadline => value,
            _ if mutation => Err(unknown()),
            _ => Err(timed_out()),
        }
    }
}
