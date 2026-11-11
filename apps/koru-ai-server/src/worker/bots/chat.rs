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

mod attachments;
mod controls;
mod preparation;
mod questions;
mod replay;
mod runtime;
mod settings;
mod steering;
pub(in crate::worker) use attachments::MAX_LISTINGS as MAX_PICTURE_LISTINGS;
pub(in crate::worker) use controls::{
    ControlJob, ControlReadJob, ControlReadResult, ControlResult,
};
pub(in crate::worker) use runtime::{LiveChat, StepJob, StepResult};
pub(in crate::worker) use steering::{
    SteerBinding, SteerJob, SteerReadJob, SteerReadResult, SteerResult,
};

pub(in crate::worker) use preparation::{chat_capture, retry_anchor, retry_input};
pub(in crate::worker) use questions::TerminalQuestionProof;
pub(in crate::worker) use settings::{SettingsProof, settings_capture};

impl LiveChat {
    /// What a lifecycle operation settling this task reads (P1.7): its Run,
    /// the Run's workspace, whether it has a session a person could hold, and
    /// its submission's owner key.
    pub(in crate::worker) fn lifecycle_view(
        &self,
    ) -> (
        koru_delegate_core::BotRunKey,
        WorkspaceId,
        bool,
        BotSubmissionKey,
    ) {
        let p = &self.submission.provenance;
        (
            koru_delegate_core::BotRunKey {
                owner: p.owner,
                bot: p.bot,
                run: self.submission.request.run,
            },
            self.submission.workspace,
            self.fence.is_some(),
            BotSubmissionKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission,
            },
        )
    }
}

fn bot_route_error() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The Bot task could not be reconciled. Continue from its saved conversation.",
        false,
    )
}

/// What a person reads when the owner refused the task's first input, in the
/// existing failure vocabulary only: the class picks the translated sentence
/// and the message is one the ordinary agent route already shows. A refusal
/// of the pictures has no sentence of its own yet, so it reads as the
/// provider's error with no added words; its reason stays on the attempt.
fn refused_input_error(refusal: koru_delegate_core::BotInputRefusal) -> AiError {
    use koru_delegate_core::BotInputRefusal as R;
    match refusal {
        R::TooLarge => context_too_large(),
        R::Pictures => AiError::new(AiErrorClass::Provider, "", false),
        R::Capability => AiError::new(
            AiErrorClass::Configuration,
            koru_ai_provider_agent::AGENT_OLDER_THAN_APP,
            false,
        ),
        R::Model => AiError::new(
            AiErrorClass::ModelUnavailable,
            "The agent refused that model.",
            false,
        ),
        R::Invalid => bot_route_error(),
    }
}
