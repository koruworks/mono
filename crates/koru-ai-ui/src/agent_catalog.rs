// SPDX-License-Identifier: MPL-2.0

//! **The agents-and-models catalog for a host that is not a chat** (Bots
//! P1.8 S6): a Bot's create form and its settings choose an agent and a
//! model with the agent-configuration control, and need what the chat's
//! picker reads — which agent runtimes the AI service registers, how ready
//! each is, what each lists — without a chat surface.
//!
//! **The reads, and the conversion, written once here.** A host's own thread
//! asks with an ordinary client holding [`SCOPES`] — the chat's reader holds
//! the same scope ordinarily, and a Bot link's first-party scopes would be
//! refused — and gets plain owned values back ([`CatalogAgent`],
//! [`CatalogListing`], a [`Readiness`]), which it hands to the control
//! borrowed ([`CatalogListing::records`], [`CatalogListing::offers`],
//! [`CatalogListing::footer`]). The service's types stay in this crate: the
//! app cannot name them, and a control never sees them.
//!
//! **Agent runtimes only.** The catalog lists the service's agent providers,
//! never the API-access ones: a Bot runs on an installed agent runtime.

use koru_agent_config_ui::effort::{Effort, Levels};
use koru_agent_config_ui::modes::ModeOffer;
use koru_agent_config_ui::picker::{Context, FooterOffer, ModelRecord, Readiness};
use koru_ai_core::{AiProviderId, ReasoningChoice};
use koru_ai_wire::{AgentModelsResponse, ProviderSettingsResponse, ProviderValidationResponse};
use koru_draw::brand::ProfileMark;

use crate::agents::{AgentStatus, agent_key, is_agent_provider};

/// **The control itself**, for a host that names it through this crate.
pub use koru_agent_config_ui as config;

/// The scopes a catalog client asks for — and, because naming scopes
/// filters rather than adds, the only ones it holds. Every catalog read is
/// `ai.manage_providers` (`provider_settings`, `validate_provider`,
/// `discover_agent_models`, `refresh_agent_models`).
pub const SCOPES: &[&str] = &["ai.manage_providers"];

/// How many times one listing is asked while the host says it is still
/// building it (the chat's reader asks the same way).
const LISTING_ASKS: usize = 3;
/// The AI service's answer for a list the host is still building.
const AGENT_MODELS_LISTING: &str = "agent_models_listing";

/// **One agent runtime the AI service registers.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogAgent {
    pub id: AiProviderId,
    /// The key favorites and the hidden list name it by (`claude`).
    pub key: Option<String>,
    pub label: String,
    /// The mark the control draws for it.
    pub mark: ProfileMark,
}

/// One model an agent lists, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    pub group: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub group_name: Option<String>,
    pub legacy: bool,
    /// The levels it offers, in the control's words; `None` where it takes
    /// none.
    pub levels: Option<Levels>,
    /// The id and the size of its longer context window.
    pub context: Option<(String, u32)>,
    /// It offers Fast mode, and whether Fast is on by default.
    pub fast: Option<bool>,
}

/// One mode an agent lists, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogMode {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

/// **What an agent lists**: its models, its modes and the mode it says it
/// starts in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogListing {
    pub models: Vec<CatalogModel>,
    pub modes: Vec<CatalogMode>,
    pub default_mode: Option<String>,
}

/// Why a listing did not come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListingFailure {
    /// The host is still building it after every ask: asked again later.
    StillListing,
    /// The service refused, by its stable code.
    Refused(String),
    /// Nothing answered.
    Unreachable,
}

impl CatalogListing {
    /// The models as the control takes them, borrowed.
    pub fn records(&self) -> Vec<ModelRecord<'_>> {
        self.models
            .iter()
            .map(|model| ModelRecord {
                id: &model.id,
                group: &model.group,
                name: model.name.as_deref(),
                description: model.description.as_deref(),
                group_name: model.group_name.as_deref(),
                legacy: model.legacy,
                context: model.context.as_ref().map(|(id, _)| id.as_str()),
            })
            .collect()
    }

    /// The modes as the control takes them, borrowed.
    pub fn offers(&self) -> Vec<ModeOffer<'_>> {
        self.modes
            .iter()
            .map(|mode| ModeOffer {
                id: &mode.id,
                name: mode.name.as_deref(),
                description: mode.description.as_deref(),
            })
            .collect()
    }

    /// The listed model `id` names, and whether `id` is its longer window.
    pub fn find(&self, id: &str) -> Option<(&CatalogModel, bool)> {
        self.models.iter().find_map(|model| {
            if model.id == id {
                Some((model, false))
            } else if model.context.as_ref().is_some_and(|(long, _)| long == id) {
                Some((model, true))
            } else {
                None
            }
        })
    }

    /// **What the control's footer offers for `model`**: its levels, its
    /// Fast mode — on as `fast` says, else as the model's own default — and
    /// its longer window, on while `model` is that window's id. Nothing for
    /// a model the listing does not hold.
    pub fn footer(&self, model: &str, fast: Option<bool>) -> FooterOffer {
        let Some((listed, long)) = self.find(model) else {
            return FooterOffer::default();
        };
        FooterOffer {
            levels: listed.levels.clone(),
            fast: listed
                .fast
                .map(|on_by_default| fast.unwrap_or(on_by_default)),
            context: listed.context.as_ref().map(|(_, tokens)| Context {
                tokens: *tokens,
                long,
            }),
        }
    }
}

/// **The agent runtimes the service registers**, in its order: the agent
/// providers of its settings, never an API-access provider.
pub fn agents(client: &koru_api_client::Client) -> Result<Vec<CatalogAgent>, String> {
    client
        .ai()
        .provider_settings()
        .map(|settings| agents_from(&settings))
        .map_err(|failure| failure.code)
}

/// **How ready `provider` is**, as the chat's picker says it.
pub fn readiness(
    client: &koru_api_client::Client,
    provider: &AiProviderId,
) -> Result<Readiness, String> {
    client
        .ai()
        .validate_provider(provider)
        .map(|validation| readiness_from(&validation))
        .map_err(|failure| failure.code)
}

/// **What `provider` lists**, asked again while the host is still building
/// it; `refresh` rebuilds it first (↻).
pub fn listing(
    client: &koru_api_client::Client,
    provider: &AiProviderId,
    refresh: bool,
) -> Result<CatalogListing, ListingFailure> {
    match discover(client, provider, refresh) {
        Ok(response) => Ok(listing_from(&response)),
        Err(failure) if failure.code == AGENT_MODELS_LISTING => Err(ListingFailure::StillListing),
        Err(failure) if failure.code.is_empty() => Err(ListingFailure::Unreachable),
        Err(failure) => Err(ListingFailure::Refused(failure.code)),
    }
}

/// **An agent's list, asked again while the host is still building it** —
/// the one retry the chat's reader and a catalog host share. `refresh`
/// rebuilds it first; the asks after the first only wait for that rebuild.
pub(crate) fn discover(
    client: &koru_api_client::Client,
    provider: &AiProviderId,
    refresh: bool,
) -> Result<AgentModelsResponse, koru_ai_wire::Failure> {
    let mut asks = 0;
    loop {
        asks += 1;
        let answer = if refresh && asks == 1 {
            client.ai().refresh_agent_models(provider)
        } else {
            client.ai().discover_agent_models(provider)
        };
        match answer {
            Err(failure) if is_still_listing(&failure) && asks < LISTING_ASKS => {}
            answer => return answer,
        }
    }
}

/// Whether `failure` says the host is still building the list.
pub(crate) fn is_still_listing(failure: &koru_ai_wire::Failure) -> bool {
    failure.code == AGENT_MODELS_LISTING
}

/// The agent runtimes of a settings answer, in its order.
pub(crate) fn agents_from(settings: &ProviderSettingsResponse) -> Vec<CatalogAgent> {
    settings
        .agents
        .iter()
        .filter(|agent| is_agent_provider(&agent.id))
        .map(|agent| CatalogAgent {
            id: agent.id.clone(),
            key: agent_key(&agent.id).map(str::to_owned),
            label: agent.label.clone(),
            mark: mark(&agent.id),
        })
        .collect()
}

/// A validation answer as the control's readiness.
pub(crate) fn readiness_from(validation: &ProviderValidationResponse) -> Readiness {
    crate::agents::readiness(AgentStatus::from_validation(validation))
}

/// A listing answer, owned.
pub(crate) fn listing_from(response: &AgentModelsResponse) -> CatalogListing {
    CatalogListing {
        models: response
            .models
            .iter()
            .map(|model| CatalogModel {
                id: model.id.clone(),
                group: model.group.clone(),
                name: model.name.clone(),
                description: model.description.clone(),
                group_name: model.group_name.clone(),
                legacy: model.legacy,
                levels: model.reasoning.as_ref().and_then(crate::effort::levels),
                context: model
                    .context
                    .as_ref()
                    .map(|window| (window.id.clone(), window.tokens)),
                fast: model.fast.map(|fast| fast.on_by_default),
            })
            .collect(),
        modes: response
            .modes
            .iter()
            .map(|mode| CatalogMode {
                id: mode.id.clone(),
                name: mode.name.clone(),
                description: mode.description.clone(),
            })
            .collect(),
        default_mode: response.default_mode.clone(),
    }
}

/// The mark the control draws for an agent: the chat's own.
pub fn mark(provider: &AiProviderId) -> ProfileMark {
    crate::model_picker::provider_profile_mark(provider, true)
}

/// A stored level in the control's words.
pub fn effort(choice: ReasoningChoice) -> Effort {
    crate::effort::effort(choice)
}

/// A level the control chose, as it is stored.
pub fn reasoning(effort: Effort) -> ReasoningChoice {
    crate::effort::reasoning(effort)
}

#[cfg(test)]
mod tests;
