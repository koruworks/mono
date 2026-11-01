// SPDX-License-Identifier: MPL-2.0

//! Provider identity, model selection, capabilities, and non-secret settings.

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::inference::{AiError, AiErrorClass};

/// Extensible provider token. Product metadata remains code-owned.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AiProviderId(String);

impl AiProviderId {
    pub const OPENAI: &'static str = "openai";
    pub const OLLAMA: &'static str = "ollama";
    pub const KORU: &'static str = "koru";
    /// Wire id of the hosted Koru AI model. Product-owned, not user-configurable.
    pub const KORU_HOSTED_MODEL: &'static str = "Qwen/Qwen3-8B";
    pub const KORU_HOSTED_MODEL_LABEL: &'static str = "Qwen3 8B";

    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err("provider id must be a non-empty lowercase token");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AiProviderId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<AiProviderId> for String {
    fn from(value: AiProviderId) -> Self {
        value.0
    }
}

impl fmt::Debug for AiProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AiProviderId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for AiProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for AiProviderId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AiProviderId")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9-]{0,63}$",
            "description": "Extensible provider token such as openai, ollama, or koru."
        })
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ReasoningChoice {
    #[default]
    Default,
    Off,
    On,
    Level(ReasoningLevel),
}

impl ReasoningChoice {
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::On => "on",
            Self::Level(level) => level.as_token(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "off" => Some(Self::Off),
            "on" => Some(Self::On),
            value => ReasoningLevel::parse(value).map(Self::Level),
        }
    }
}

impl Serialize for ReasoningChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_token())
    }
}

impl<'de> Deserialize<'de> for ReasoningChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("unknown reasoning choice"))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ReasoningChoice {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ReasoningChoice")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": ["default", "off", "on", "minimal", "low", "medium", "high", "xhigh", "max"]
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReasoningLevel {
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ReasoningLevel {
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReasoningControl {
    Unsupported,
    Toggle {
        default_on: bool,
    },
    Levels {
        values: Vec<ReasoningLevel>,
        /// The level the model runs at when the chat chooses none. OpenAI,
        /// Koru AI and Ollama always state it; a local agent that does not
        /// leaves it `None`, and *Default* then sends nothing — the agent
        /// decides (agent-models §6.2).
        #[serde(default)]
        default: Option<ReasoningLevel>,
        supports_off: bool,
    },
}

/// What an agent chat's failed turn says when the agent did not accept the
/// chat's reasoning level (agent-models D19). A surface that knows it says
/// the same in the person's language.
pub const AGENT_REASONING_REJECTED: &str = "The agent did not accept that reasoning level.";

/// Resolves a requested reasoning choice against a model's declared control.
pub fn validate_reasoning(
    control: &ReasoningControl,
    requested: ReasoningChoice,
) -> Result<Option<ReasoningChoice>, AiError> {
    match (control, requested) {
        (ReasoningControl::Unsupported, ReasoningChoice::Default) => Ok(None),
        (ReasoningControl::Unsupported, _) => Err(unsupported_reasoning()),
        (ReasoningControl::Toggle { default_on }, ReasoningChoice::Default) => {
            Ok(Some(if *default_on {
                ReasoningChoice::On
            } else {
                ReasoningChoice::Off
            }))
        }
        (
            ReasoningControl::Toggle { .. },
            choice @ (ReasoningChoice::Off | ReasoningChoice::On),
        ) => Ok(Some(choice)),
        (ReasoningControl::Levels { default, .. }, ReasoningChoice::Default) => {
            Ok(default.map(ReasoningChoice::Level))
        }
        (ReasoningControl::Levels { supports_off, .. }, ReasoningChoice::Off) if *supports_off => {
            Ok(Some(ReasoningChoice::Off))
        }
        (ReasoningControl::Levels { values, .. }, ReasoningChoice::Level(level))
            if values.contains(&level) =>
        {
            Ok(Some(ReasoningChoice::Level(level)))
        }
        _ => Err(unsupported_reasoning()),
    }
}

fn unsupported_reasoning() -> AiError {
    AiError::new(
        AiErrorClass::Configuration,
        "The selected reasoning effort is not supported by this model.",
        false,
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelSelection {
    pub provider: AiProviderId,
    pub requested_model: String,
    pub default_reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub structured_output: bool,
    pub vision: bool,
    #[serde(default)]
    pub web_search: bool,
    pub context_window: Option<u32>,
    pub reasoning: ReasoningControl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelDescriptor {
    pub selection: ModelSelection,
    pub resolved_model: Option<String>,
    pub capabilities: ModelCapabilities,
}

/// Runtime/provider setup input. Secrets are resolved through a separate port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi {
        model: String,
        key_account: Option<String>,
        reasoning: ReasoningChoice,
    },
    Ollama {
        endpoint: Url,
        model: Option<String>,
        reasoning: ReasoningChoice,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    pub selected_provider: Option<AiProviderId>,
    pub openai: OpenAiSettings,
    pub ollama: OllamaSettings,
    pub created_at: crate::Timestamp,
    pub updated_at: crate::Timestamp,
}

impl AiSettings {
    pub fn unconfigured(now: crate::Timestamp) -> Self {
        Self {
            selected_provider: None,
            openai: OpenAiSettings::default(),
            ollama: OllamaSettings {
                endpoint: Url::parse("http://localhost:11434")
                    .expect("the default Ollama endpoint is a URL"),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiSettings {
    pub model: Option<String>,
    pub key_account: Option<String>,
    pub reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OllamaSettings {
    pub endpoint: Url,
    pub model: Option<String>,
    pub reasoning: ReasoningChoice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderStatus {
    Unconfigured,
    Validating,
    Unavailable { reason: UnavailableReason },
    NoModels,
    Pulling(ModelPullProgress),
    Ready { model: ModelDescriptor },
    Failed { public_message: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    NotReachable,
    Authentication,
    Network,
    ModelUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPullProgress {
    pub model: String,
    pub status: String,
    pub completed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_tokens_are_extensible_but_canonical() {
        assert_eq!(AiProviderId::parse("openai").unwrap().as_str(), "openai");
        assert!(AiProviderId::parse("OpenAI").is_err());
        assert!(AiProviderId::parse("").is_err());
        assert!(AiProviderId::parse("-openai").is_err());
        assert!(AiProviderId::parse("open_ai").is_err());
    }

    /// A local agent that states no default: *Default* sends nothing, a
    /// level it offers is that level, one it does not is refused. An older
    /// control without the field reads as stating none.
    #[test]
    fn a_control_with_no_stated_default_sends_nothing_on_default() {
        let control = ReasoningControl::Levels {
            values: vec![ReasoningLevel::Low, ReasoningLevel::High],
            default: None,
            supports_off: false,
        };
        assert_eq!(
            validate_reasoning(&control, ReasoningChoice::Default).unwrap(),
            None
        );
        assert_eq!(
            validate_reasoning(&control, ReasoningChoice::Level(ReasoningLevel::High)).unwrap(),
            Some(ReasoningChoice::Level(ReasoningLevel::High))
        );
        assert!(validate_reasoning(&control, ReasoningChoice::Level(ReasoningLevel::Max)).is_err());
        let read: ReasoningControl = serde_json::from_value(serde_json::json!({
            "kind": "levels", "values": ["low"], "supports_off": false
        }))
        .unwrap();
        assert!(matches!(
            read,
            ReasoningControl::Levels { default: None, .. }
        ));
    }

    #[test]
    fn reasoning_tokens_round_trip_in_storage_spelling() {
        let choice = ReasoningChoice::Level(ReasoningLevel::XHigh);
        let value = serde_json::to_string(&choice).unwrap();
        assert_eq!(value, "\"xhigh\"");
        assert_eq!(
            serde_json::from_str::<ReasoningChoice>(&value).unwrap(),
            choice
        );
    }
}
