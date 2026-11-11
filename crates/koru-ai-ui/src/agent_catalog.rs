// SPDX-License-Identifier: MPL-2.0

//! **The agents-and-models catalog for a host that is not a chat** (Bots
//! P1.8 S6): a Bot's create form and its settings choose an agent and a
//! model with the agent-configuration control, and need what the chat's
//! picker reads — which agent runtimes the AI service registers, how ready
//! each is, what each lists — without a chat surface.
//!
//! **The conversion, written once here; never the call.** This module holds
//! no client and makes no service call — the standalone-component rule: a
//! host lifts it with the control into another project and feeds it. The
//! host asks the AI service itself, on its own thread with its own client,
//! and hands each answer here, which turns it into plain owned values
//! ([`agents_from`] → [`CatalogAgent`], [`listing_from`] →
//! [`CatalogListing`], [`readiness_from`] → a [`Readiness`];
//! [`is_still_listing`] says when to ask again). Those go to the control
//! borrowed ([`CatalogListing::records`], [`CatalogListing::offers`],
//! [`CatalogListing::footer`]). The chat's own reader shares the one retry
//! of a listing through a crate-private path.
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

/// How many times one listing is asked while the host says it is still
/// building it (the chat's reader asks the same way; a catalog host may
/// follow it).
pub const LISTING_ASKS: usize = 3;
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

/// **An agent's list, asked again while the host is still building it** —
/// the chat's reader's own retry, crate-private: nothing that takes a client
/// is public here. `refresh` rebuilds it first; the asks after the first
/// only wait for that rebuild.
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

/// Whether `failure` says the host is still building the list: ask again,
/// up to [`LISTING_ASKS`] times.
pub fn is_still_listing(failure: &koru_ai_wire::Failure) -> bool {
    failure.code == AGENT_MODELS_LISTING
}

/// **A listing's failure as the control reads it**, by the service's code:
/// still being built, a refusal, or nothing answered.
pub fn listing_failure(failure: &koru_ai_wire::Failure) -> ListingFailure {
    if is_still_listing(failure) {
        ListingFailure::StillListing
    } else if failure.code.is_empty() {
        ListingFailure::Unreachable
    } else {
        ListingFailure::Refused(failure.code.clone())
    }
}

/// **The agent runtimes of a settings answer**, in its order: the agent
/// providers, never an API-access one.
pub fn agents_from(settings: &ProviderSettingsResponse) -> Vec<CatalogAgent> {
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

/// **A validation answer as the control's readiness**, as the chat's picker
/// says it.
pub fn readiness_from(validation: &ProviderValidationResponse) -> Readiness {
    crate::agents::readiness(AgentStatus::from_validation(validation))
}

/// **What an agent lists**, owned: its models, modes and starting mode.
pub fn listing_from(response: &AgentModelsResponse) -> CatalogListing {
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
