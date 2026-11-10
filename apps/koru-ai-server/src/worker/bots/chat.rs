// SPDX-License-Identifier: MPL-2.0
//! Shared AI conversation dispatch through the existing delegate owner.
use super::*;
use koru_ai_core::bots::replay::*;
use koru_ai_provider_agent::mapping::{
    AgentEventMapper, AgentLiveAction, AgentSource, UsageDelivery,
};
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{BotAdmissionState, BotSubmissionKey};
use koru_session_protocol::{AgentEvent, PendingInput};

mod controls;
mod preparation;
mod replay;
mod runtime;
mod steering;
pub(in crate::worker) use controls::{
    ControlJob, ControlReadJob, ControlReadResult, ControlResult,
};
pub(in crate::worker) use runtime::{LiveChat, StepJob, StepResult};
pub(in crate::worker) use steering::{
    SteerBinding, SteerJob, SteerReadJob, SteerReadResult, SteerResult,
};

pub(in crate::worker) use preparation::{chat_capture, retry_anchor, retry_input};

fn bot_route_error() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The Bot task could not be reconciled. Continue from its saved conversation.",
        false,
    )
}
