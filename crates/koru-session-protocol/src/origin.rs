// SPDX-License-Identifier: MPL-2.0

//! Who a session belongs to, and how a creator proves it may create.
//!
//! Protocol v16. A session's **origin** is what created it: a person's
//! window (`Manual`), a delegated run (`Delegated`), or a Koru AI chat
//! (`Chat`). The host owns the answer from its live registry — never from
//! evictable history — and enforces two rules on `Chat` origins that the
//! other two do not need:
//!
//! - **one conversation per chat**: a create for a chat that already has a
//!   live session or an in-flight reservation is refused `origin_owned`,
//!   under the same lock that reserved the first;
//! - **creation authority is a generation**: only a connection that said
//!   `AiControl` in its hello may create chat sessions, and only while its
//!   generation is current. A successor's hello bumps the generation, so
//!   every earlier worker's create — including one already inside a host
//!   handler — is refused `stale_authority` whenever it gets there. The
//!   host never needs to notice a dead pipe; it notices the successor.
//!
//! [`LaunchSelection`] is the explicit model and effort a creator asks for,
//! separate from delegation's own launch. It rides defaulted fields behind
//! the `launch-selection` capability: a host that did not negotiate it must
//! never be sent one, because ignoring an explicit choice and running the
//! default is the one thing the field exists to prevent.

use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ChatOriginId, ModelName, SessionSummary, TokenParseError};

/// What created a session. Defaulted to `Manual` on every peer that
/// predates it, which is the truth: before v16 only windows created
/// sessions without a delegation launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionOrigin {
    #[default]
    Manual,
    Delegated,
    Chat {
        chat_id: ChatOriginId,
    },
}

impl SessionOrigin {
    /// The one origin the host enforces uniqueness and authority on.
    pub fn chat(self) -> Option<ChatOriginId> {
        match self {
            Self::Chat { chat_id } => Some(chat_id),
            Self::Manual | Self::Delegated => None,
        }
    }
}

/// What a connection says it is in its hello. Absent is an ordinary
/// client; `AiControl` is the AI worker's one control connection, and is
/// what a chat-origin create needs to be admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    AiControl,
}

/// The creation authority a connection holds: the generation the host
/// handed it when it registered as `AiControl`. Compared for equality at
/// admission with the host's current generation, under the admission lock.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct AiControlGeneration(u64);

impl AiControlGeneration {
    pub const fn first() -> Self {
        Self(1)
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// How hard a model should think, as a closed token a provider adapter maps
/// onto its own vocabulary. Not the AI core's reasoning choice: this crate
/// is a leaf, and a protocol token must be spelled here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LaunchReasoning {
    Off,
    On,
    Minimal,
    Low,
    Medium,
    High,
    /// v18: an older peer cannot read it, so it is sent only to a v18 host.
    /// Spelled `xhigh`, as the agents and the AI core spell it.
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl LaunchReasoning {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl FromStr for LaunchReasoning {
    type Err = TokenParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "off" => Self::Off,
            "on" => Self::On,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            _ => return Err(TokenParseError("invalid reasoning token")),
        })
    }
}

/// An explicit model and effort a creator asks a managed session for.
///
/// `None` in either field is "the provider's own default", which is never
/// gated. A `Some` is acknowledged by the adapter before the first prompt
/// or refused visibly; it is never silently replaced by the default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LaunchSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<LaunchReasoning>,
}

impl LaunchSelection {
    pub fn is_explicit(&self) -> bool {
        self.model.is_some() || self.reasoning.is_some()
    }
}

/// The answer to `lookup_origin`: what the registry holds under an origin
/// right now. Ended sessions are not in it and are not needed — a creator
/// that finds nothing live and nothing reserved may create, and admission
/// is what makes that race-free.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OriginLookup {
    pub live: Vec<SessionSummary>,
    pub creating: u32,
}

impl OriginLookup {
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.creating == 0
    }
}

impl fmt::Display for SessionOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => f.write_str("manual"),
            Self::Delegated => f.write_str("delegated"),
            Self::Chat { .. } => f.write_str("chat"),
        }
    }
}
