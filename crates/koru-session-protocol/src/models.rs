// SPDX-License-Identifier: MPL-2.0

//! Private display metadata from a provider-owned model catalog.
use crate::{LaunchReasoning, ModelName, TokenParseError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_PROVIDER_MODELS: usize = 4096;
/// The longest name or group name a model keeps. A longer one is dropped,
/// so the picker formats the id instead of showing half a sentence.
pub const MAX_MODEL_NAME_BYTES: usize = 128;
/// The longest description a model keeps; a longer one is cut short.
pub const MAX_MODEL_DESCRIPTION_BYTES: usize = 512;

/// A complete bounded catalog. Empty means the provider exposes no choices.
/// IDs remain opaque; grouping is presentation supplied by the adapter.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "Vec<ProviderModel>", into = "Vec<ProviderModel>")]
pub struct ProviderModels(Vec<ProviderModel>);

/// One model an agent offers, with what the agent itself says about it.
///
/// Every field after `group` is v18 (`PROVIDER_MODEL_FACTS_PROTOCOL_VERSION`)
/// and optional: an older peer leaves it unread, and an older agent that
/// sends none leaves the picker to format the id.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderModel {
    pub id: ModelName,
    pub group: ModelName,
    /// The agent's own name for the model: ACP's option `name`, Codex's
    /// `displayName`, OpenCode's `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ModelLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<ModelLabel>,
    /// The agent's own name for the group: ACP's group `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<ModelLabel>,
    /// The agent marks the model as older. Never inferred by Koru.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
    /// The reasoning levels the agent offers for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

impl ProviderModel {
    /// A model known only by its id and group: everything the agent could
    /// say about it is left for the adapter to fill.
    pub fn new(id: ModelName, group: ModelName) -> Self {
        Self {
            id,
            group,
            name: None,
            description: None,
            group_name: None,
            legacy: false,
            reasoning: None,
        }
    }

    fn valid(&self) -> bool {
        let short = |label: &Option<ModelLabel>| {
            label
                .as_ref()
                .is_none_or(|label| label.as_str().len() <= MAX_MODEL_NAME_BYTES)
        };
        short(&self.name)
            && short(&self.group_name)
            && self.reasoning.as_ref().is_none_or(ModelReasoning::valid)
    }
}

/// The reasoning levels one model offers, in Koru's own vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelReasoning {
    /// Koru's order: minimal < low < medium < high < xhigh < max. Never Off/On.
    pub levels: Vec<LaunchReasoning>,
    /// The agent's stated default level; `None` when the agent decides unseen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<LaunchReasoning>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

impl ModelReasoning {
    /// At least one level, none repeated, none of them `off` or `on`, and a
    /// default that is one of them.
    pub fn valid(&self) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        !self.levels.is_empty()
            && self.levels.iter().all(|level| {
                !matches!(level, LaunchReasoning::Off | LaunchReasoning::On)
                    && seen.insert(level.as_str())
            })
            && self
                .default
                .is_none_or(|default| self.levels.contains(&default))
    }
}

/// Text an agent sends about a model — its name, its group's name, its
/// description — for a person to read.
///
/// Untrusted: trimmed, never empty, free of control characters and bounded.
/// Its `Debug` is redacted; the text never reaches a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct ModelLabel(String);

impl ModelLabel {
    /// A name or a group name: `None` when it is empty, carries a control
    /// character or is longer than [`MAX_MODEL_NAME_BYTES`].
    pub fn name(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()
            && value.len() <= MAX_MODEL_NAME_BYTES
            && !value.chars().any(char::is_control))
        .then(|| Self(value.to_owned()))
    }

    /// A description: control characters become spaces, and past
    /// [`MAX_MODEL_DESCRIPTION_BYTES`] it is cut at a character boundary
    /// and ends with `…`.
    pub fn description(value: &str) -> Option<Self> {
        let cleaned: String = value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            return None;
        }
        if cleaned.len() <= MAX_MODEL_DESCRIPTION_BYTES {
            return Some(Self(cleaned.to_owned()));
        }
        let mut end = MAX_MODEL_DESCRIPTION_BYTES - '…'.len_utf8();
        while !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        Some(Self(format!("{}…", cleaned[..end].trim_end())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelLabel {
    type Error = TokenParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        (!value.is_empty()
            && value.trim() == value
            && value.len() <= MAX_MODEL_DESCRIPTION_BYTES
            && !value.chars().any(char::is_control))
        .then_some(Self(value))
        .ok_or(TokenParseError("invalid model label"))
    }
}

impl From<ModelLabel> for String {
    fn from(value: ModelLabel) -> Self {
        value.0
    }
}

impl std::fmt::Debug for ModelLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModelLabel(<redacted>)")
    }
}

impl ProviderModels {
    pub fn models(&self) -> &[ProviderModel] {
        &self.0
    }
}
impl TryFrom<Vec<ProviderModel>> for ProviderModels {
    type Error = TokenParseError;
    fn try_from(models: Vec<ProviderModel>) -> Result<Self, Self::Error> {
        let ids: std::collections::BTreeSet<_> = models.iter().map(|model| &model.id).collect();
        if models.len() > MAX_PROVIDER_MODELS
            || ids.len() != models.len()
            || !models.iter().all(ProviderModel::valid)
        {
            return Err(TokenParseError("invalid model catalog"));
        }
        Ok(Self(models))
    }
}
impl From<ProviderModels> for Vec<ProviderModel> {
    fn from(value: ProviderModels) -> Self {
        value.0
    }
}
impl std::fmt::Debug for ProviderModels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderModels")
            .field("count", &self.0.len())
            .finish()
    }
}
impl std::fmt::Debug for ProviderModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderModel(<redacted>)")
    }
}

#[cfg(test)]
mod tests;
