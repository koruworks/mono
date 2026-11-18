// SPDX-License-Identifier: MPL-2.0

//! `GET`/`PATCH /v1/settings/{section}` — the settings authority's surface.
//!
//! **Reads ride an ordinary scope; writes take a narrow one.** A default
//! session can read what the app looks like and cannot change it, because
//! `settings.write` is not in the ordinary default set (owner, 2026-09-01): an
//! MCP tool or a script must not be able to silently change what every window
//! looks like. A first-party window asks for the scope by name at connect
//! time, which is the same mechanism `ai.manage_providers` already uses.
//!
//! Three sections, two of them files and one a facade:
//!
//! - `global` and `terminal` are `koru-settings`' own, read and written here.
//! - `ai` is a **facade over the existing AI authority**. `ai.db` and the OS
//!   vault are unchanged and this route owns neither; it maps the worker's
//!   provider settings onto the settings section shape. The AI worker is
//!   already embedded in this process, so the facade is a function call rather
//!   than a hop — and it is real, not a stub. The one thing it deliberately
//!   cannot do is *write* the OpenAI secret: no route at any layer does that
//!   today, the desktop writes the vault directly, and adding one is its own
//!   packet.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json};

use koru_settings::{
    AiPatch, AiSettings, GlobalPatch, OllamaSection, OpenAiSection, PatchError, Section,
    SectionValues, TerminalPatch,
};

use crate::{GatewayPrincipal, GatewayState, SETTINGS_READ_SCOPE, SETTINGS_WRITE_SCOPE, failure};

#[cfg(test)]
mod tests;

/// The internal acceptance seam waits longer than the public client's
/// 30-second request deadline. The link's authoritative readback reconciles
/// the committed value before an optional safe, field-scoped retry.
const SETTINGS_REPLY_HOLD: Duration = Duration::from_secs(35);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultOperation {
    GlobalDictationEnable,
    GlobalDictationDisable,
    StartOnLoginDisable,
}

impl FaultOperation {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "global-dictation-enable" => Some(Self::GlobalDictationEnable),
            "global-dictation-disable" => Some(Self::GlobalDictationDisable),
            "start-on-login-disable" => Some(Self::StartOnLoginDisable),
            _ => None,
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::GlobalDictationEnable => "global-dictation-enable",
            Self::GlobalDictationDisable => "global-dictation-disable",
            Self::StartOnLoginDisable => "start-on-login-disable",
        }
    }

    /// Recognize only the exact patches Agent emits for its three confirmed
    /// Voice writes. A broader Global patch must not accidentally spend the
    /// one-shot acceptance fault.
    fn from_patch(patch: &GlobalPatch) -> Option<Self> {
        let mut other = patch.clone();
        match patch.global_dictation_enabled {
            Some(true) if matches!(patch.start_on_login, None | Some(true)) => {
                other.global_dictation_enabled = None;
                other.start_on_login = None;
                (other == GlobalPatch::default()).then_some(Self::GlobalDictationEnable)
            }
            Some(false) if patch.start_on_login.is_none() => {
                other.global_dictation_enabled = None;
                (other == GlobalPatch::default()).then_some(Self::GlobalDictationDisable)
            }
            None if patch.start_on_login == Some(false) => {
                other.start_on_login = None;
                (other == GlobalPatch::default()).then_some(Self::StartOnLoginDisable)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultState {
    Off,
    Armed(FaultOperation),
    Fired(FaultOperation),
}

/// Result of reading the acceptance-only fault configuration. It is public so
/// the binary host can report a refused release-build or misspelled request
/// without exposing a product route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultConfiguration {
    Off,
    Armed,
    RefusedReleasedBuild,
    UnknownOperation,
}

/// Process-local and one-shot. There is deliberately no HTTP arm/status API:
/// this is a startup-only internal-build seam, not a capability a session can
/// discover or invoke.
pub(crate) struct SettingsWriteFault {
    state: Mutex<FaultState>,
    hold: Duration,
}

impl Default for SettingsWriteFault {
    fn default() -> Self {
        Self {
            state: Mutex::new(FaultState::Off),
            hold: SETTINGS_REPLY_HOLD,
        }
    }
}

impl SettingsWriteFault {
    pub(crate) fn configure(
        &self,
        internal_build: bool,
        operation: Option<&str>,
    ) -> FaultConfiguration {
        let Some(value) = operation else {
            return FaultConfiguration::Off;
        };
        if !internal_build {
            return FaultConfiguration::RefusedReleasedBuild;
        }
        let Some(operation) = FaultOperation::parse(value) else {
            return FaultConfiguration::UnknownOperation;
        };
        *self.state.lock().unwrap_or_else(|held| held.into_inner()) = FaultState::Armed(operation);
        tracing::warn!(
            operation = operation.token(),
            event = "settings_write_fault_armed"
        );
        FaultConfiguration::Armed
    }

    fn take(&self, operation: Option<FaultOperation>) -> Option<Duration> {
        let operation = operation?;
        let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
        if *state != FaultState::Armed(operation) {
            return None;
        }
        *state = FaultState::Fired(operation);
        tracing::warn!(
            operation = operation.token(),
            event = "settings_write_fault_fired"
        );
        Some(self.hold)
    }

    #[cfg(test)]
    fn with_hold(hold: Duration) -> Self {
        Self {
            state: Mutex::new(FaultState::Off),
            hold,
        }
    }

    #[cfg(test)]
    fn state(&self) -> FaultState {
        *self.state.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// One catalogued settings route.
#[derive(Clone, Copy, Debug)]
pub struct SettingsRoute {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub scope: &'static str,
    pub summary: &'static str,
    pub returns: &'static str,
}

/// The table. Two operations over one path, and the scope column is the whole
/// of the authorization story — there is nowhere else to look.
pub const ROUTES: &[SettingsRoute] = &[
    SettingsRoute {
        operation_id: "settings_read_section",
        method: "GET",
        path: "/v1/settings/{section}",
        scope: SETTINGS_READ_SCOPE,
        summary: "Read one settings section",
        returns: "The typed section: global, ai, or terminal. Secrets are never returned.",
    },
    SettingsRoute {
        operation_id: "settings_patch_section",
        method: "PATCH",
        path: "/v1/settings/{section}",
        scope: SETTINGS_WRITE_SCOPE,
        summary: "Change named fields of one settings section",
        returns: "The section as it stands after the change, read back from the authority.",
    },
];

pub fn router() -> Router<Arc<GatewayState>> {
    Router::new().route(
        "/v1/settings/{section}",
        get(read_section).patch(patch_section),
    )
}

pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::{Map, Value, json};

    let mut schemas = schemars::generate::SchemaSettings::openapi3().into_generator();
    let global = schemas.subschema_for::<koru_settings::GlobalSettings>();
    let terminal = schemas.subschema_for::<koru_settings::TerminalSettings>();
    let ai = schemas.subschema_for::<AiSettings>();
    let global_patch = schemas.subschema_for::<GlobalPatch>();
    let terminal_patch = schemas.subschema_for::<TerminalPatch>();
    let ai_patch = schemas.subschema_for::<AiPatch>();
    let section = json!({"oneOf": [global, ai, terminal]});
    let patch = json!({"oneOf": [global_patch, ai_patch, terminal_patch]});

    let paths = document["paths"]
        .as_object_mut()
        .expect("gateway OpenAPI paths");
    for route in ROUTES {
        let mut operation = json!({
            "operationId": route.operation_id,
            "summary": route.summary,
            "description": route.returns,
            "security": [{"bearerAuth": [route.scope]}],
            "parameters": [{
                "name": "section",
                "in": "path",
                "required": true,
                "schema": {"type": "string", "enum": ["global", "ai", "terminal"]}
            }],
            "responses": {
                "200": {
                    "description": route.returns,
                    "content": {"application/json": {"schema": section}}
                },
                "400": {"description": "The patch names a field that is not a setting, or a value the registry refuses"},
                "401": {"description": "The gateway bearer is absent or invalid"},
                "403": {"description": "The session does not hold the scope this operation needs"},
                "404": {"description": "There is no such settings section"},
                "409": {"description": "A required system side effect refused, so nothing was written"},
                "500": {"description": "The write did not land; the body carries the authoritative section"},
                "503": {"description": "The settings authority or the AI worker is unavailable"}
            }
        });
        if route.method == "PATCH" {
            operation["requestBody"] = json!({
                "required": true,
                "content": {"application/json": {"schema": patch}}
            });
        }
        let entry = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        entry
            .as_object_mut()
            .expect("settings OpenAPI path")
            .insert(route.method.to_lowercase(), operation);
    }
    let components = document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas");
    components.extend(
        schemas
            .take_definitions(true)
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).expect("settings schema"))),
    );
}

/// The settings authority's change facts, on their way to the journal.
///
/// **The port's implementation, and the only place the two vocabularies
/// meet.** `koru-settings` knows what a section is and deliberately does not
/// link `koru-events`; this crate knows both, which is exactly the split the
/// Store's catalog sink already uses.
///
/// A journal that will not accept an observation is a window that refreshes
/// late, not a setting that was lost, so a failed publish is logged and
/// dropped. It can never fail the write it reports — the write has already
/// happened by the time this runs.
pub(crate) struct SettingsFacts {
    events: koru_events_server::EventsHandle,
}

impl koru_settings::SettingsEventSink for SettingsFacts {
    fn publish(&self, section: Section) {
        let draft: koru_events::Draft = match section {
            Section::Global => koru_events::SettingsGlobalChanged.into(),
            Section::Ai => koru_events::SettingsAiChanged.into(),
            Section::Terminal => koru_events::SettingsTerminalChanged.into(),
        };
        let envelope = draft.into_envelope(&self.events.state.ctx);
        if let Err(why) = self.events.state.append(std::slice::from_ref(&envelope)) {
            tracing::warn!(
                name = %envelope.name,
                "a committed settings change was not recorded: {why}"
            );
        }
    }
}

impl SettingsFacts {
    pub(crate) fn new(events: koru_events_server::EventsHandle) -> Self {
        Self { events }
    }
}

// ------------------------------------------------------------- handlers

async fn read_section(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(section): Path<String>,
) -> Response {
    let Some(section) = Section::parse(&section) else {
        return unknown_section();
    };
    if let Err(response) = require(&principal, SETTINGS_READ_SCOPE) {
        return response;
    }
    match section {
        Section::Global => match state.settings() {
            Some(authority) => Json(SectionValues::Global(authority.global())).into_response(),
            None => unavailable(),
        },
        Section::Terminal => match state.settings() {
            Some(authority) => Json(SectionValues::Terminal(authority.terminal())).into_response(),
            None => unavailable(),
        },
        Section::Ai => match read_ai(&state, &principal).await {
            Ok(settings) => Json(SectionValues::Ai(settings)).into_response(),
            Err(response) => response,
        },
    }
}

async fn patch_section(
    State(state): State<Arc<GatewayState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(section): Path<String>,
    payload: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let Some(section) = Section::parse(&section) else {
        return unknown_section();
    };
    // **The scope is checked before the body is looked at.** A caller that may
    // not write learns that it may not write, rather than learning which
    // field names the authority accepts.
    if let Err(response) = require(&principal, SETTINGS_WRITE_SCOPE) {
        return response;
    }
    let Ok(Json(body)) = payload else {
        return invalid_request("the settings patch body is not JSON");
    };

    match section {
        Section::Global => {
            let Some(authority) = state.settings() else {
                return unavailable();
            };
            let patch: GlobalPatch = match serde_json::from_value(body) {
                Ok(patch) => patch,
                Err(why) => return invalid_request(&why.to_string()),
            };
            let fault_operation = FaultOperation::from_patch(&patch);
            let capacity_changed = !patch.bots_session_capacity.is_unchanged();
            let response = match authority.patch_global(patch) {
                Ok(settings) => {
                    if capacity_changed && let Some(engine) = state.bots_engine.get() {
                        engine.capacity_changed();
                    }
                    if let Some(hold) = state.settings_write_fault.take(fault_operation) {
                        tokio::time::sleep(hold).await;
                    }
                    Json(SectionValues::Global(settings)).into_response()
                }
                Err(error) => refused(
                    error.message(),
                    error
                        .authoritative()
                        .map(|section| SectionValues::Global(section.clone())),
                    status_for(&error),
                ),
            };
            // Receipt, successful authority result, then injected hold, then
            // reply. This
            // exact placement creates an unknown client outcome without
            // changing whether the authoritative write landed. Only a
            // startup-armed internal build can make `take` return a duration.
            response
        }
        Section::Terminal => {
            let Some(authority) = state.settings() else {
                return unavailable();
            };
            let patch: TerminalPatch = match serde_json::from_value(body) {
                Ok(patch) => patch,
                Err(why) => return invalid_request(&why.to_string()),
            };
            match authority.patch_terminal(patch) {
                Ok(settings) => Json(SectionValues::Terminal(settings)).into_response(),
                Err(error) => refused(
                    error.message(),
                    error
                        .authoritative()
                        .map(|section| SectionValues::Terminal(section.clone())),
                    status_for(&error),
                ),
            }
        }
        Section::Ai => {
            let patch: AiPatch = match serde_json::from_value(body) {
                Ok(patch) => patch,
                Err(why) => return invalid_request(&why.to_string()),
            };
            if let Err(invalid) = patch.validate() {
                return invalid_request(&invalid.to_string());
            }
            match patch_ai(&state, &principal, patch).await {
                Ok(settings) => Json(SectionValues::Ai(settings)).into_response(),
                Err(response) => response,
            }
        }
    }
}

// ------------------------------------------------------------ the facade

/// Read the AI section through the worker that owns `ai.db` and the vault.
async fn read_ai(
    state: &GatewayState,
    principal: &GatewayPrincipal,
) -> Result<AiSettings, Response> {
    let (client, context) = ai_access(state, principal)?;
    let response = tokio::task::spawn_blocking(move || client.provider_settings(context))
        .await
        .map_err(|_| unavailable())?
        .map_err(|failure| ai_refused(&failure.message))?;
    Ok(from_provider_settings(response))
}

/// Apply an AI patch through the worker.
///
/// The worker's surface is three operations rather than one patch, so a patch
/// that spans providers becomes more than one call. Each is a whole-record
/// write of *that provider's* settings, which is why the current values are
/// read first and folded: sending only the changed field would clear the rest.
async fn patch_ai(
    state: &GatewayState,
    principal: &GatewayPrincipal,
    patch: AiPatch,
) -> Result<AiSettings, Response> {
    let mut wanted = read_ai(state, principal).await?;
    let touches_openai = !patch.openai_model.is_unchanged() || patch.openai_reasoning.is_some();
    let touches_ollama = !patch.ollama_model.is_unchanged()
        || patch.ollama_reasoning.is_some()
        || patch.ollama_endpoint.is_some();
    let touches_selection = !patch.selected_provider.is_unchanged();
    patch.apply(&mut wanted);

    if touches_openai {
        let (client, context) = ai_access(state, principal)?;
        let request = koru_ai_wire::UpdateOpenAiSettingsRequest {
            model: wanted.openai.model.clone(),
            reasoning: reasoning_out(wanted.openai.reasoning),
        };
        tokio::task::spawn_blocking(move || client.update_openai_settings(context, request))
            .await
            .map_err(|_| unavailable())?
            .map_err(|failure| ai_refused(&failure.message))?;
    }
    if touches_ollama {
        let (client, context) = ai_access(state, principal)?;
        let request = koru_ai_wire::UpdateOllamaSettingsRequest {
            endpoint: wanted.ollama.endpoint.clone(),
            model: wanted.ollama.model.clone(),
            reasoning: reasoning_out(wanted.ollama.reasoning),
        };
        tokio::task::spawn_blocking(move || client.update_ollama_settings(context, request))
            .await
            .map_err(|_| unavailable())?
            .map_err(|failure| ai_refused(&failure.message))?;
    }
    if touches_selection && let Some(provider) = wanted.selected_provider {
        let (client, context) = ai_access(state, principal)?;
        let Ok(id) = koru_ai_core::AiProviderId::parse(provider.token()) else {
            return Err(invalid_request("that is not a provider"));
        };
        let request = koru_ai_wire::SelectProviderRequest { provider: id };
        tokio::task::spawn_blocking(move || client.select_provider(context, request))
            .await
            .map_err(|_| unavailable())?
            .map_err(|failure| ai_refused(&failure.message))?;
    }

    // Read back rather than return what was sent: the credential flag and any
    // normalization the worker applied are its answer, not ours.
    read_ai(state, principal).await
}

/// The AI client plus the context the worker will authorize against.
///
/// **The settings scope stands in for the AI scope, deliberately.** The caller
/// has already been authorized for this operation by the narrower, first-party
/// `settings.write`; the AI section *is* the provider settings, and requiring
/// a second scope for the same surface would mean a window could open the
/// Settings panel and find one section inexplicably read-only.
fn ai_access(
    state: &GatewayState,
    principal: &GatewayPrincipal,
) -> Result<(koru_ai_client::Client, koru_ai_api::RequestContext), Response> {
    let Some(client) = state.ai_client() else {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "capability_unavailable",
            "the AI worker is unavailable, so the AI settings section cannot be read",
        ));
    };
    Ok((
        client,
        koru_ai_api::RequestContext {
            principal: koru_ai_api::AiPrincipal {
                label: principal.label.clone(),
                scopes: vec![koru_ai_api::AiScope::ManageProviders],
            },
            trace_id: uuid::Uuid::new_v4().to_string(),
            deadline_millis: None,
        },
    ))
}

fn from_provider_settings(response: koru_ai_wire::ProviderSettingsResponse) -> AiSettings {
    AiSettings {
        selected_provider: response.selected_provider.and_then(|id| {
            koru_settings::AiProvider::ALL
                .iter()
                .copied()
                .find(|provider| provider.token() == id.as_str())
        }),
        openai: OpenAiSection {
            model: response.openai.model,
            reasoning: reasoning_in(response.openai.reasoning),
            credential_configured: response.openai.credential_configured,
        },
        ollama: OllamaSection {
            endpoint: response.ollama.endpoint,
            model: response.ollama.model,
            reasoning: reasoning_in(response.ollama.reasoning),
        },
    }
}

fn reasoning_in(choice: koru_ai_core::ReasoningChoice) -> koru_settings::ReasoningChoice {
    koru_settings::ReasoningChoice::parse(choice.as_token())
}

fn reasoning_out(choice: koru_settings::ReasoningChoice) -> koru_ai_core::ReasoningChoice {
    koru_ai_core::ReasoningChoice::parse(choice.token()).unwrap_or_default()
}

// ------------------------------------------------------------- responses

fn require(principal: &GatewayPrincipal, scope: &str) -> Result<(), Response> {
    if principal.scopes.iter().any(|held| held == scope) {
        return Ok(());
    }
    Err(failure(
        StatusCode::FORBIDDEN,
        "permission_denied",
        &format!("this session does not hold `{scope}`"),
    ))
}

fn status_for<T>(error: &PatchError<T>) -> StatusCode {
    match error {
        PatchError::Invalid(_) => StatusCode::BAD_REQUEST,
        // The value was refused by something outside this process, and nothing
        // was written. Not a 500: the request was well formed and the
        // authority behaved correctly.
        PatchError::SideEffect { .. } => StatusCode::CONFLICT,
        PatchError::Write { .. } | PatchError::Inconsistent { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// A refusal that carries what is actually true.
///
/// **The reconciliation shape.** A control that could not save must not go on
/// showing the value somebody typed, so the failure body carries the section
/// as it stands — re-read from the authority, not assumed.
fn refused(message: String, authoritative: Option<SectionValues>, status: StatusCode) -> Response {
    let mut body = serde_json::json!({
        "code": if status == StatusCode::BAD_REQUEST { "invalid_request" } else { "settings_write_failed" },
        "message": message,
        "waiting_on": [],
        "retryable": status == StatusCode::INTERNAL_SERVER_ERROR,
    });
    if let Some(section) = authoritative {
        body["authoritative"] = serde_json::to_value(section).unwrap_or(serde_json::Value::Null);
    }
    let mut response = Json(body).into_response();
    *response.status_mut() = status;
    response
}

fn unknown_section() -> Response {
    failure(
        StatusCode::NOT_FOUND,
        "not_found",
        "the settings sections are `global`, `ai` and `terminal`",
    )
}

fn unavailable() -> Response {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "capability_unavailable",
        "this host opened no settings authority",
    )
}

fn invalid_request(message: &str) -> Response {
    failure(StatusCode::BAD_REQUEST, "invalid_request", message)
}

fn ai_refused(message: &str) -> Response {
    failure(StatusCode::BAD_GATEWAY, "settings_write_failed", message)
}
