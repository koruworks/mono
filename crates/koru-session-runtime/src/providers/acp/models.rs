// SPDX-License-Identifier: MPL-2.0

//! OpenCode owns configuration, credentials and catalog resolution. No prompt is sent.
use super::*;
use crate::AttachmentPathResolver;
use koru_session_protocol::{
    LaunchReasoning, ModelLabel, ModelName, ModelReasoning, ProviderModel, ProviderModels,
};
use std::{ffi::OsString, path::PathBuf, sync::Arc};

mod accounts;

const DISCOVERY_BUDGET: Duration = Duration::from_secs(25);

pub(super) fn discover(
    route: &AcpRoute,
    executable: OsString,
    cwd: PathBuf,
    attachments: Arc<dyn AttachmentPathResolver>,
) -> Result<ProviderModels, ProviderFailure> {
    match route.dialect {
        routes::AcpDialect::Generic => {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Discovery,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        routes::AcpDialect::OpenCode => {}
        _ => return discover_session_options(route, executable, cwd, attachments),
    }
    let started = Instant::now();
    let operation = koru_session_protocol::IdempotencyKey::mint();
    tracing::info!(target: "koru::acp", event = "acp.models.discovery.started", provider = "opencode", operation = %operation);
    let result = (|| {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(invalid_catalog());
        }
        let executable = resolve_probe_executable(&executable)?;
        let probe =
            AcpCompatibilityProbe::new(route.clone(), executable.clone(), cwd.clone(), attachments);
        let deadline = started + DISCOVERY_BUDGET;
        probe.version(ProbeLimits {
            timeout: remaining(deadline)?.min(Duration::from_secs(5)),
            ..ProbeLimits::default()
        })?;
        let connections = if let Some(content) = accounts::environment_content(route)? {
            accounts::parse(content.as_bytes())?
        } else {
            let output = catalog_command(
                route,
                &executable,
                &cwd,
                &["debug", "paths"],
                remaining(deadline)?.min(Duration::from_secs(5)),
                CATALOG_OUTPUT_BYTES,
            )?;
            accounts::read(accounts::data_path(output.stdout())?)?
        };
        // `--verbose` carries each model's own name. A listing that fails,
        // overflows or does not parse falls back to the plain one: the ids
        // are what a chat needs, the names only what the picker shows.
        let verbose = catalog_command(
            route,
            &executable,
            &cwd,
            &["models", "--verbose"],
            remaining(deadline)?.min(Duration::from_secs(20)),
            VERBOSE_CATALOG_OUTPUT_BYTES,
        )
        .and_then(|output| parse_verbose_catalog(output.stdout()));
        let models = match verbose {
            Ok(models) => models,
            Err(_) => {
                tracing::warn!(target: "koru::acp", event = "acp.models.verbose.fallback", provider = "opencode", operation = %operation);
                let output = catalog_command(
                    route,
                    &executable,
                    &cwd,
                    &["models"],
                    remaining(deadline)?.min(Duration::from_secs(20)),
                    CATALOG_OUTPUT_BYTES,
                )?;
                parse_catalog(output.stdout())?
            }
        };
        tracing::info!(target: "koru::acp", event = "acp.models.connections.filtered", provider = "opencode", operation = %operation,
            resolved_models = models.models().len(), explicit_connections = connections.len());
        remaining(deadline)?;
        accounts::intersect(models, &connections)
    })();
    tracing::info!(target: "koru::acp", event = "acp.models.discovery.finished", provider = "opencode", operation = %operation,
        success = result.is_ok(), count = result.as_ref().map_or(0, |catalog| catalog.models().len()),
        elapsed_ms = started.elapsed().as_millis() as u64);
    result
}

/// The models a route advertises through its session's `model` config
/// option (AGENTUX I3): a discovery session is started exactly as a chat's
/// would be — version gate, `initialize`, `session/new`, the route's own
/// authentication wait — its advertised options are kept, and it is shut
/// down. No prompt is sent. A route that advertises no `model` option
/// answers an empty catalog, which the picker shows as no level.
fn discover_session_options(
    route: &AcpRoute,
    executable: OsString,
    cwd: PathBuf,
    attachments: Arc<dyn AttachmentPathResolver>,
) -> Result<ProviderModels, ProviderFailure> {
    let started = Instant::now();
    let operation = koru_session_protocol::IdempotencyKey::mint();
    tracing::info!(target: "koru::acp", event = "acp.models.discovery.started", provider = %route.provider, operation = %operation);
    let result = (|| {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(invalid_catalog());
        }
        let executable = resolve_probe_executable(&executable)?;
        let probe = AcpCompatibilityProbe::new(route.clone(), executable, cwd, attachments);
        let models = probe.session_models(ProbeLimits {
            timeout: SESSION_DISCOVERY_TIMEOUT,
            ..ProbeLimits::default()
        })?;
        ProviderModels::try_from(models).map_err(|_| invalid_catalog())
    })();
    tracing::info!(target: "koru::acp", event = "acp.models.discovery.finished", provider = %route.provider, operation = %operation,
        success = result.is_ok(), count = result.as_ref().map_or(0, |catalog| catalog.models().len()),
        elapsed_ms = started.elapsed().as_millis() as u64);
    result
}

/// A discovery session waits for the route's own start — Claude's wrapper
/// authentication, Grok's inventory — which the readiness probe's budget
/// has been seen to cut short under load.
const SESSION_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

/// **The session's reasoning levels** (agent-models D15): the one `select`
/// config option whose `category` is `thought_level`, ACP's own *Thought /
/// reasoning level selector*, read in Koru's vocabulary. It describes the
/// session's current model.
pub(super) struct ThoughtLevel {
    /// The option's id, which `session/set_config_option` names.
    pub id: String,
    /// The levels it offers; `None` when no value is one of Koru's.
    pub reasoning: Option<ModelReasoning>,
    /// The agent's own value for each Koru level, and for *off*.
    pub values: Vec<(LaunchReasoning, String)>,
    /// The option's `currentValue`, as the agent spells it.
    pub current: Option<String>,
    /// Values outside Koru's vocabulary, dropped (D17).
    pub dropped: usize,
}

impl ThoughtLevel {
    /// The agent's own value for `level`, when the option offers it.
    pub fn value_for(&self, level: LaunchReasoning) -> Option<&str> {
        self.values
            .iter()
            .find(|(offered, _)| *offered == level)
            .map(|(_, value)| value.as_str())
    }
}

/// The `thought_level` option of a `session/new` or `set_config_option`
/// answer, or `None` when it carries none — or more than one, which is no
/// single answer.
pub(super) fn thought_level(response: &Value) -> Option<ThoughtLevel> {
    let options = response.get("configOptions")?.as_array()?;
    let mut matching = options
        .iter()
        .filter(|option| option["category"] == "thought_level" && option["type"] == "select");
    let option = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    let id = option["id"]
        .as_str()
        .filter(|id| !id.is_empty())?
        .to_owned();
    let mut tokens = Vec::new();
    for entry in option["options"].as_array()? {
        match entry["options"].as_array() {
            Some(members) => tokens.extend(members.iter().filter_map(|m| m["value"].as_str())),
            None => tokens.extend(entry["value"].as_str()),
        }
    }
    let current = option["currentValue"].as_str().map(str::to_owned);
    let (reasoning, dropped) =
        ModelReasoning::from_agent_tokens(tokens.iter().copied(), current.as_deref());
    let values = tokens
        .iter()
        .filter_map(|token| {
            let lowered = token.trim().to_ascii_lowercase();
            let level = match lowered.as_str() {
                "off" | "none" => LaunchReasoning::Off,
                other => other
                    .parse::<LaunchReasoning>()
                    .ok()
                    .filter(|level| *level != LaunchReasoning::On)?,
            };
            Some((level, (*token).to_owned()))
        })
        .collect();
    Some(ThoughtLevel {
        id,
        reasoning,
        values,
        current,
        dropped,
    })
}

/// A count of agent level values Koru has no word for, as the catalog's
/// event says it; none is ever shown.
pub(super) fn levels_dropped(provider: &str, count: usize) {
    if count > 0 {
        tracing::info!(target: "koru::provider_models", event = "catalog.levels.dropped",
            provider, count);
    }
}

/// The `model` option's values as a catalog: a flat list under the route's
/// own kind, or ACP's groups under each group's id. A value that is not a
/// model name, or a repeat, is left out. Each option's `name` and
/// `description`, and each group's `name`, are kept as the agent sent them,
/// within `ModelLabel`'s bounds; one past them is dropped, not refused. The
/// session's `thought_level` option describes its current model, so that
/// model alone carries levels here (D15); the others' are read by walking
/// them (D16).
pub(super) fn config_models(session: &Value, fallback_group: &str) -> Vec<ProviderModel> {
    let mut models = config_model_list(session, fallback_group);
    if let Some(level) = thought_level(session) {
        levels_dropped(fallback_group, level.dropped);
        let current = option(session)
            .ok()
            .and_then(|option| option["currentValue"].as_str());
        if let Some(model) = models
            .iter_mut()
            .find(|model| Some(model.id.as_str()) == current)
        {
            model.reasoning = level.reasoning;
        }
    }
    models
}

fn config_model_list(session: &Value, fallback_group: &str) -> Vec<ProviderModel> {
    let Ok(option) = option(session) else {
        return Vec::new();
    };
    let Some(entries) = option["options"].as_array() else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::new();
    let mut keep = |value: &Value, group: &str, group_name: Option<ModelLabel>| {
        let Some(id) = value["value"]
            .as_str()
            .and_then(|value| value.parse::<ModelName>().ok())
        else {
            return;
        };
        let group: ModelName = match group.parse() {
            Ok(group) => group,
            Err(_) => match fallback_group.parse() {
                Ok(group) => group,
                Err(_) => return,
            },
        };
        if seen.insert(id.clone()) && models.len() < koru_session_protocol::MAX_PROVIDER_MODELS {
            let mut model = ProviderModel::new(id, group);
            model.name = value["name"].as_str().and_then(ModelLabel::name);
            model.description = value["description"]
                .as_str()
                .and_then(ModelLabel::description);
            model.group_name = group_name;
            models.push(model);
        }
    };
    for entry in entries {
        match entry["options"].as_array() {
            Some(members) => {
                let group = entry["group"]
                    .as_str()
                    .or_else(|| entry["name"].as_str())
                    .unwrap_or(fallback_group);
                let group_name = entry["name"].as_str().and_then(ModelLabel::name);
                for member in members {
                    keep(member, group, group_name.clone());
                }
            }
            None => keep(entry, fallback_group, None),
        }
    }
    models
}

fn remaining(deadline: Instant) -> Result<Duration, ProviderFailure> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| {
            failure(
                ErrorCode::ProviderUnavailable,
                ProviderStage::Discovery,
                ProviderDiagnostic::Timeout,
            )
        })
}

/// The most a plain catalog command may print.
const CATALOG_OUTPUT_BYTES: usize = 1024 * 1024;
/// The most `opencode models --verbose` may print: about 1.5 KB a model,
/// 350 KB for the 233 models seen on one machine (1.19.5).
const VERBOSE_CATALOG_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

fn catalog_command(
    route: &AcpRoute,
    executable: &std::ffi::OsStr,
    cwd: &std::path::Path,
    tokens: &[&'static str],
    timeout: Duration,
    maximum_output_bytes: usize,
) -> Result<super::super::discovery::ProbeOutput, ProviderFailure> {
    // Only fixed adapter-owned tokens reach Windows batch launchers.
    let (program, mut args) = route.launch(executable, true)?;
    args.pop().ok_or_else(invalid_catalog)?;
    args.extend(tokens.iter().map(|token| OsString::from(*token)));
    let command = args
        .iter()
        .fold(ProbeCommand::new(program), |command, arg| command.arg(arg))
        .current_dir(cwd.to_path_buf());
    let command = route
        .environment
        .iter()
        .fold(command, |command, (key, value)| command.env(key, value));
    let output = run_probe_command(
        command,
        ProbeLimits {
            timeout,
            maximum_output_bytes,
        },
    )?;
    if !output.success() {
        return Err(invalid_catalog());
    }
    Ok(output)
}

fn invalid_catalog() -> ProviderFailure {
    failure(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}

fn parse_catalog(bytes: &[u8]) -> Result<ProviderModels, ProviderFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid_catalog())?;
    let mut models = std::collections::BTreeMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let model = catalog_model(line)?;
        models.insert(model.id.clone(), model);
        if models.len() > koru_session_protocol::MAX_PROVIDER_MODELS {
            return Err(invalid_catalog());
        }
    }
    models
        .into_values()
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| invalid_catalog())
}

/// `opencode models --verbose`: each `provider/model` id on a line of its
/// own, then a JSON object about that model. The id line is held to the
/// plain listing's rules, and the object's `name` is kept, and its
/// `variants`' names as the model's levels (D15's vocabulary; OpenCode
/// states no default). OpenCode applies them over ACP through the same
/// `thought_level` option as any agent, seen on 1.19.5. Anything else in
/// the output refuses the whole listing, and the plain one is used instead.
fn parse_verbose_catalog(bytes: &[u8]) -> Result<ProviderModels, ProviderFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid_catalog())?;
    let mut models = std::collections::BTreeMap::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let (line, after) = rest.split_once('\n').ok_or_else(invalid_catalog)?;
        let mut model = catalog_model(line.trim_end_matches('\r'))?;
        let mut facts = serde_json::Deserializer::from_str(after).into_iter::<Value>();
        let Some(Ok(facts_value)) = facts.next() else {
            return Err(invalid_catalog());
        };
        if !facts_value.is_object() {
            return Err(invalid_catalog());
        }
        rest = after[facts.byte_offset()..].trim_start();
        model.name = facts_value["name"].as_str().and_then(ModelLabel::name);
        if let Some(variants) = facts_value["variants"].as_object() {
            let (reasoning, dropped) =
                ModelReasoning::from_agent_tokens(variants.keys().map(String::as_str), None);
            model.reasoning = reasoning;
            levels_dropped("opencode", dropped);
        }
        models.insert(model.id.clone(), model);
        if models.len() > koru_session_protocol::MAX_PROVIDER_MODELS {
            return Err(invalid_catalog());
        }
    }
    models
        .into_values()
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| invalid_catalog())
}

/// One `provider/model` line: the provider is the group, and a line with no
/// model after the slash is not a model.
fn catalog_model(line: &str) -> Result<ProviderModel, ProviderFailure> {
    let id: ModelName = line.parse().map_err(|_| invalid_catalog())?;
    let (group, name) = line.split_once('/').ok_or_else(invalid_catalog)?;
    if name.is_empty() {
        return Err(invalid_catalog());
    }
    let group = group.parse().map_err(|_| invalid_catalog())?;
    Ok(ProviderModel::new(id, group))
}

pub(super) fn selection_unavailable() -> ProviderFailure {
    failure(
        ErrorCode::ProviderModelUnavailable,
        ProviderStage::Initialize,
        ProviderDiagnostic::HandshakeRejected,
    )
}

fn option(response: &Value) -> Result<&Value, ProviderFailure> {
    let options = response
        .get("configOptions")
        .and_then(Value::as_array)
        .ok_or_else(selection_unavailable)?;
    let mut matching = options.iter().filter(|option| option["id"] == "model");
    let option = matching.next().ok_or_else(selection_unavailable)?;
    if matching.next().is_some() || option["type"] != "select" {
        return Err(selection_unavailable());
    }
    Ok(option)
}

pub(super) fn validate_selection(
    response: &Value,
    selected: &ModelName,
) -> Result<(), ProviderFailure> {
    let options = option(response)?["options"]
        .as_array()
        .ok_or_else(selection_unavailable)?;
    // ACP accepts either ungrouped options or groups containing options.
    let found = options.iter().any(|option| {
        option["value"].as_str() == Some(selected.as_str())
            || option["options"].as_array().is_some_and(|group| {
                group
                    .iter()
                    .any(|item| item["value"].as_str() == Some(selected.as_str()))
            })
    });
    if found {
        Ok(())
    } else {
        Err(selection_unavailable())
    }
}

pub(super) fn validate_applied(
    response: &Value,
    selected: &ModelName,
) -> Result<(), ProviderFailure> {
    if option(response)?["currentValue"].as_str() == Some(selected.as_str()) {
        Ok(())
    } else {
        Err(selection_unavailable())
    }
}

#[cfg(test)]
pub(super) mod tests;
