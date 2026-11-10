// SPDX-License-Identifier: MPL-2.0
//! Why a Bot task's strict initial input was definitely refused.
//!
//! The session host pins the outcome of a `BeginPendingInput` to the input's
//! idempotency key: asking again with the same input returns the same answer,
//! never a new submit. When that answer is a refusal of the input itself,
//! retrying can only repeat it, so the delegate owner reports it as one of
//! these non-retryable `bot_input_*` codes and the AI ends the task. The code
//! string is the whole contract: no field, wire shape or version changes.
//!
//! No answer is never one of these. A lost reply, a timeout, a host or
//! provider fault, a lease or backpressure answer all stay retryable and
//! uncertain, because the turn may have begun.
use koru_session_protocol::ErrorCode;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BotInputRefusal {
    /// The input did not fit the session's prompt frame.
    TooLarge,
    /// The session or its agent refused a picture the input carried.
    Pictures,
    /// The connection did not negotiate something the input needs, such as
    /// local path references, or the session cannot take managed input.
    Capability,
    /// The agent refused the input's model.
    Model,
    /// The input is malformed, or its key was already used differently.
    Invalid,
}

impl BotInputRefusal {
    pub const ALL: [Self; 5] = [
        Self::TooLarge,
        Self::Pictures,
        Self::Capability,
        Self::Model,
        Self::Invalid,
    ];

    /// The stable code the owner reports, which names the reason.
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "bot_input_too_large",
            Self::Pictures => "bot_input_pictures_refused",
            Self::Capability => "bot_input_capability_refused",
            Self::Model => "bot_input_model_refused",
            Self::Invalid => "bot_input_invalid",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|refusal| refusal.code() == code)
    }

    /// The session's own answer that is a definite refusal of the input.
    /// Every other code, including an unknown one, is not.
    pub fn of_session(code: &ErrorCode) -> Option<Self> {
        Some(match code {
            ErrorCode::InputTooLarge => Self::TooLarge,
            ErrorCode::UnsupportedMedia
            | ErrorCode::InvalidImage
            | ErrorCode::AttachmentLimit
            | ErrorCode::AttachmentNotFound => Self::Pictures,
            ErrorCode::CapabilityNotNegotiated | ErrorCode::DeliveryUnsupported => Self::Capability,
            ErrorCode::ProviderModelUnavailable => Self::Model,
            ErrorCode::InvalidId
            | ErrorCode::InvalidRequest
            | ErrorCode::InvalidSize
            | ErrorCode::InvalidTransition
            | ErrorCode::IdempotencyConflict => Self::Invalid,
            _ => return None,
        })
    }

    /// The same, from the code string a session port carries.
    pub fn of_session_code(code: &str) -> Option<Self> {
        ErrorCode::ALL
            .iter()
            .find(|known| known.as_str() == code)
            .and_then(Self::of_session)
    }

    /// Whether sending the task's pictures as file paths instead could get
    /// past this refusal: the pictures themselves, or a frame they filled.
    pub fn concerns_pictures(self) -> bool {
        matches!(self, Self::TooLarge | Self::Pictures)
    }
}
