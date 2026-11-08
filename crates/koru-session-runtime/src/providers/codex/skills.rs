// SPDX-License-Identifier: MPL-2.0

//! **Codex's skills** (koru-ai D121): what `$` offers in a chat's composer,
//! and the `skill` input item a picked one becomes.
//!
//! The list is the app-server's own `skills/list` for the folder the agent
//! works in — never a walk of `.codex` or `.agents` by Koru. Every
//! supported version has the method, its `skills/changed` notification and
//! the `skill` input item, all in the stable (non-experimental) schema:
//! `tests/fixtures/codex-app-server/<version>/schema-evidence.json` says so
//! version by version.
//!
//! - **Discovery** (`discover_commands`) starts an app-server, initializes
//!   it, lists the folder's skills and shuts it down — no thread, no
//!   prompt. Only enabled skills are offered, in the picker's order (the
//!   folder's, the person's, then the agent's own and an administrator's
//!   together; by name within each), each name once.
//! - **A turn** names its skills by name ([`AgentInput::skills`]). The path
//!   Codex reads each from never leaves this adapter: it is resolved from
//!   the session's last listing, which is read at the first turn that names
//!   a skill and again after Codex says its skills changed. A name no longer
//!   listed is dropped from the turn and logged; the text still says
//!   `$name`, and goes as typed.
//!
//! [`AgentInput::skills`]: crate::providers::AgentInput::skills

use std::collections::BTreeMap;

use koru_session_protocol::{
    AgentCommand, AgentCommandKind, MAX_AGENT_COMMANDS, command_text, valid_command_name,
};
use serde_json::{Value, json};

use super::{CodexControl, ProviderDiagnostic, ProviderFailure, ProviderStage, failure};
use crate::providers::AgentControl;

/// The method that lists a folder's skills.
pub(super) const LIST_METHOD: &str = "skills/list";
/// The notification that says the listing is out of date.
pub(super) const CHANGED_NOTIFICATION: &str = "skills/changed";
/// The most bytes of a skill's path the adapter keeps.
const PATH_BYTES: usize = 32 * 1024;

/// **One enabled skill as the adapter keeps it**: what a picker shows, and
/// where Codex said the skill is, which only a turn's input item carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ListedSkill {
    pub(super) command: AgentCommand,
    pub(super) path: String,
}

/// A skill's scope as the kind a picker orders by; `None` for a scope this
/// adapter does not know, which is not offered.
fn kind(scope: &str) -> Option<AgentCommandKind> {
    match scope {
        "repo" => Some(AgentCommandKind::RepoSkill),
        "user" => Some(AgentCommandKind::UserSkill),
        "system" => Some(AgentCommandKind::SystemSkill),
        "admin" => Some(AgentCommandKind::AdminSkill),
        _ => None,
    }
}

fn text_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

/// One `SkillMetadata`, when it is enabled and well formed.
fn listed_skill(skill: &Value) -> Option<ListedSkill> {
    if skill.get("enabled").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let name = skill.get("name").and_then(Value::as_str)?;
    if !valid_command_name(name) {
        return None;
    }
    let kind = kind(skill.get("scope").and_then(Value::as_str)?)?;
    let path = skill.get("path").and_then(Value::as_str)?;
    if path.is_empty() || path.len() > PATH_BYTES || path.chars().any(char::is_control) {
        return None;
    }
    // The interface's short line first, then the legacy one, then the
    // description itself — one line of it either way.
    let description = text_at(skill, "/interface/shortDescription")
        .or_else(|| text_at(skill, "/shortDescription"))
        .or_else(|| text_at(skill, "/description"))
        .map(command_text)
        .unwrap_or_default();
    let display_name = text_at(skill, "/interface/displayName")
        .map(command_text)
        .filter(|display| !display.is_empty() && display != name);
    Some(ListedSkill {
        command: AgentCommand {
            name: name.to_owned(),
            description,
            display_name,
            kind,
            input_hint: None,
        },
        path: path.to_owned(),
    })
}

/// **A `skills/list` result as the picker's list**: every entry's enabled
/// skills, ordered by their kind's group and then by name
/// ([`AgentCommandKind::group`]), each name once — the first in
/// that order — and at most [`MAX_AGENT_COMMANDS`]. `None` for a result
/// that is not a listing.
pub(super) fn listing(result: &Value) -> Option<Vec<ListedSkill>> {
    let entries = result.get("data")?.as_array()?;
    let mut skills: Vec<ListedSkill> = Vec::new();
    for entry in entries {
        let Some(listed) = entry.get("skills").and_then(Value::as_array) else {
            continue;
        };
        skills.extend(listed.iter().filter_map(listed_skill));
    }
    skills.sort_by(|left, right| {
        left.command
            .kind
            .group()
            .cmp(&right.command.kind.group())
            .then_with(|| left.command.name.cmp(&right.command.name))
            .then_with(|| left.command.kind.cmp(&right.command.kind))
    });
    let mut kept: Vec<ListedSkill> = Vec::with_capacity(skills.len());
    for skill in skills {
        if kept
            .iter()
            .all(|known| known.command.name != skill.command.name)
        {
            kept.push(skill);
        }
    }
    kept.truncate(MAX_AGENT_COMMANDS);
    Some(kept)
}

impl CodexControl {
    /// The skills Codex lists for the adapter's folder.
    fn list_skills(&mut self, stage: ProviderStage) -> Result<Vec<ListedSkill>, ProviderFailure> {
        let cwd = self.config.working_directory.to_str().ok_or_else(|| {
            failure(
                koru_session_protocol::ErrorCode::InvalidRequest,
                stage,
                ProviderDiagnostic::InvalidInput,
            )
        })?;
        let cwd = cwd.to_owned();
        let response = self.call(LIST_METHOD, json!({ "cwds": [cwd] }), stage)?;
        listing(&response).ok_or_else(|| {
            failure(
                koru_session_protocol::ErrorCode::ProviderIncompatible,
                stage,
                ProviderDiagnostic::MalformedFrame,
            )
        })
    }

    /// **The folder's skills, for a picker**: a fresh app-server is started,
    /// asked, and shut down. No thread is opened and no prompt is sent.
    pub(super) fn discover_commands(&mut self) -> Result<Vec<AgentCommand>, ProviderFailure> {
        self.initialize()?;
        let listed = self.list_skills(ProviderStage::Discovery);
        let _ = self.shutdown();
        Ok(listed?.into_iter().map(|skill| skill.command).collect())
    }

    /// Codex said its skills changed: the next turn that names one lists
    /// them again first.
    pub(super) fn forget_skills(&mut self) {
        self.skills = None;
    }

    /// **The `skill` input items for a turn's picked skills**, in the order
    /// they were named, each resolved by name from the session's last
    /// listing — read now when there is none. A name no longer listed is
    /// left out and logged without its words; a listing that fails leaves
    /// them all out. Either way the text goes as typed.
    pub(super) fn skill_items(&mut self, names: &[String]) -> Vec<Value> {
        if names.is_empty() {
            return Vec::new();
        }
        if self.skills.is_none() {
            match self.list_skills(ProviderStage::Submit) {
                Ok(listed) => {
                    self.skills = Some(
                        listed
                            .into_iter()
                            .map(|skill| (skill.command.name, skill.path))
                            .collect::<BTreeMap<_, _>>(),
                    );
                }
                Err(failure) => {
                    tracing::warn!(target: "koru::codex", event = "codex.skills.list_failed",
                        code = failure.code.as_str(), skills = names.len(),
                        "Codex did not list its skills; the message goes as typed");
                    return Vec::new();
                }
            }
        }
        let listed = self.skills.as_ref().expect("listed above");
        let items: Vec<Value> = names
            .iter()
            .filter_map(|name| {
                listed
                    .get(name)
                    .map(|path| json!({ "type": "skill", "name": name, "path": path }))
            })
            .collect();
        if items.len() < names.len() {
            tracing::warn!(target: "koru::codex", event = "codex.skills.missing",
                named = names.len(), missing = names.len() - items.len(),
                "a picked skill is no longer listed; the message goes as typed");
        }
        items
    }
}
