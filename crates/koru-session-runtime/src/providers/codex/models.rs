// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;

use koru_session_protocol::{ErrorCode, ModelName, ProviderModel, ProviderModels};
use serde_json::json;

use super::{CodexControl, ProviderDiagnostic, ProviderFailure, ProviderStage, failure, normalize};
use crate::providers::AgentControl;

const MODEL_PAGE_SIZE: usize = 100;
const MAXIMUM_MODEL_PAGES: usize = 32;
/// The one group every Codex model belongs to: the picker shows a single
/// group as no level at all.
const MODEL_GROUP: &str = "openai";

impl CodexControl {
    /// Every text model the app-server lists, paged to the end, with
    /// exactly one of them marked default.
    pub(super) fn load_models(&mut self) -> Result<Vec<normalize::CodexModel>, ProviderFailure> {
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        let mut models: Vec<normalize::CodexModel> = Vec::new();
        for _ in 0..MAXIMUM_MODEL_PAGES {
            let params = match cursor.as_ref() {
                Some(cursor) => json!({ "cursor": cursor, "limit": MODEL_PAGE_SIZE }),
                None => json!({ "limit": MODEL_PAGE_SIZE }),
            };
            let response = self.call("model/list", params, ProviderStage::Initialize)?;
            let page = normalize::model_page(&response).ok_or_else(handshake_rejected)?;
            for model in page.models {
                if model.is_default && models.iter().any(|known| known.is_default) {
                    return Err(handshake_rejected());
                }
                models.push(model);
            }
            let Some(next) = page.next_cursor else {
                if models.iter().any(|model| model.is_default) {
                    return Ok(models);
                }
                return Err(handshake_rejected());
            };
            if !seen.insert(next.clone()) {
                return Err(handshake_rejected());
            }
            cursor = Some(next);
        }
        Err(handshake_rejected())
    }

    /// The model the thread starts on: the one this launch asked for when
    /// the list carries it, otherwise the list's default. A request the
    /// list does not carry is refused, never silently swapped.
    pub(super) fn select_model(
        &mut self,
        requested: Option<&ModelName>,
    ) -> Result<normalize::CodexModel, ProviderFailure> {
        let models = self.load_models()?;
        match requested {
            Some(requested) => models
                .into_iter()
                .find(|model| model.model == requested.as_str())
                .ok_or_else(model_unavailable),
            None => models
                .into_iter()
                .find(|model| model.is_default)
                .ok_or_else(handshake_rejected),
        }
    }

    /// The catalog, for a picker: a fresh app-server is started, asked,
    /// and shut down. No thread is opened and no prompt is sent.
    pub(super) fn discover_models(&mut self) -> Result<ProviderModels, ProviderFailure> {
        self.initialize()?;
        let listed = self.load_models();
        let _ = self.shutdown();
        let group: ModelName = MODEL_GROUP.parse().map_err(|_| handshake_rejected())?;
        let models: Vec<ProviderModel> = listed?
            .into_iter()
            .filter_map(|model| {
                let mut listed = ProviderModel::new(model.model.parse().ok()?, group.clone());
                listed.name = model.display_name;
                listed.description = model.description;
                Some(listed)
            })
            .collect();
        models.try_into().map_err(|_| handshake_rejected())
    }
}

fn model_unavailable() -> ProviderFailure {
    failure(
        ErrorCode::ProviderModelUnavailable,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}

fn handshake_rejected() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}
