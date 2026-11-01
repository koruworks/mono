// SPDX-License-Identifier: MPL-2.0

//! What the chat surface knows about local agents: how a picker row, a
//! Settings row, the composer chip and an activity row read when the
//! provider is a coding agent on this computer.
//!
//! Nothing here speaks to the session host or knows a protocol. The agent
//! list comes from the AI service's settings; readiness comes from the same
//! validation answer every provider gives; the words come from `koru-i18n`.

use std::collections::HashMap;

use koru_ai_core::{
    AGENT_DEFAULT_MODEL, AgentToolKind, AiProviderId, ToolActivity, ToolActivityStatus,
    ToolApproval,
};
use koru_ai_wire::{
    AgentModelView, AgentProviderView, ChatTranscript, ProviderValidationResponse,
    ProviderValidationStatus,
};
use koru_i18n::{Key, Lang, t};

/// How ready one agent is for a chat, as the picker and the card say it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Checking,
    Ready,
    NotInstalled,
    SignedOut,
    UnsupportedVersion,
    CheckFailed,
    Unavailable,
}

impl AgentStatus {
    pub(crate) fn from_validation(validation: &ProviderValidationResponse) -> Self {
        match validation.status {
            ProviderValidationStatus::Ready => Self::Ready,
            ProviderValidationStatus::Failed => Self::CheckFailed,
            ProviderValidationStatus::Unconfigured
            | ProviderValidationStatus::NoModels
            | ProviderValidationStatus::Unavailable => {
                match validation
                    .failure
                    .as_ref()
                    .map(|failure| failure.code.as_str())
                {
                    Some("agent_checking") => Self::Checking,
                    Some("agent_not_installed") => Self::NotInstalled,
                    Some("agent_signed_out") => Self::SignedOut,
                    Some("agent_unsupported_version") => Self::UnsupportedVersion,
                    Some("agent_check_failed") => Self::CheckFailed,
                    _ => Self::Unavailable,
                }
            }
        }
    }

    /// The word beside an agent that is not simply ready; `None` when it is.
    pub(crate) fn word(self, lang: Lang) -> Option<String> {
        let key = match self {
            Self::Ready => return None,
            Self::Checking => Key::TerminalChecking,
            Self::NotInstalled => Key::TerminalNotInstalled,
            Self::SignedOut => Key::TerminalManagedSignedOut,
            Self::UnsupportedVersion => Key::AiAgentUnsupportedVersion,
            Self::CheckFailed => Key::TerminalManagedCheckFailed,
            Self::Unavailable => Key::AiAgentUnavailable,
        };
        Some(t(lang, key).to_owned())
    }

    pub(crate) fn selectable(self) -> bool {
        self == Self::Ready
    }
}

/// One row of the composer's picker: a model of the chat's provider, the
/// *Local agents* heading, or an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerRow {
    Model(String),
    Header,
    Agent(AiProviderId),
    /// One of an agent's model groups, when it has more than one.
    Group {
        agent: AiProviderId,
        group: String,
    },
    /// One model of an agent, by the agent's own id.
    AgentModel {
        agent: AiProviderId,
        id: String,
    },
    /// A word under an expanded agent: listing, failed, or nothing listed.
    Note(Key),
}

impl PickerRow {
    pub(crate) fn selectable(&self, status: &HashMap<AiProviderId, AgentStatus>) -> bool {
        match self {
            Self::Model(_) | Self::Group { .. } | Self::AgentModel { .. } => true,
            Self::Header | Self::Note(_) => false,
            Self::Agent(id) => status.get(id).is_some_and(|status| status.selectable()),
        }
    }
}

/// What the picker knows about one agent's models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentModels {
    Loading,
    Listed(Vec<AgentModelView>),
    Failed,
}

/// The groups an agent's models fall into, in the order first seen. The
/// agent's own `default` entry is the *Default* row, in no group.
pub(crate) fn model_groups(models: &[AgentModelView]) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for model in listed_models(models) {
        if !groups.contains(&model.group) {
            groups.push(model.group.clone());
        }
    }
    groups
}

/// An agent's models other than its own `default` entry, which is drawn as
/// the *Default* row at the top instead of a second time in the list.
pub(crate) fn listed_models(models: &[AgentModelView]) -> impl Iterator<Item = &AgentModelView> {
    models
        .iter()
        .filter(|model| model.id != AGENT_DEFAULT_MODEL)
}

/// A model's label: the agent's own name for it, or the id formatted.
pub(crate) fn model_label(model: &AgentModelView) -> String {
    model
        .name
        .clone()
        .unwrap_or_else(|| fallback_label(&model.id, &model.group))
}

/// A group's heading: the agent's own name for it, or the id formatted.
pub(crate) fn group_label(models: &[AgentModelView], group: &str) -> String {
    models
        .iter()
        .filter(|model| model.group == group)
        .find_map(|model| model.group_name.clone())
        .unwrap_or_else(|| fallback_label(group, ""))
}

/// The *Default* row's label: the agent's own `default` entry's name when
/// it lists one (Claude does), otherwise Koru's word.
pub(crate) fn default_model_label(models: &[AgentModelView], lang: Lang) -> String {
    models
        .iter()
        .find(|model| model.id == AGENT_DEFAULT_MODEL)
        .and_then(|model| model.name.clone())
        .unwrap_or_else(|| t(lang, Key::AiModelAgentDefault).to_owned())
}

/// A model id as a person would write it, when the agent sends no name. A
/// formatting rule rather than a table, so it never goes stale: the group
/// prefix (or else everything up to the last `/`) goes, `-`, `_` and spaces
/// split words, a date (`2025-01-31`) stays whole, runs of one- and
/// two-digit numbers join with `.`, and each word that starts with a letter
/// is capitalised. `claude-opus-4-1` reads `Claude Opus 4.1`;
/// `o3-mini-2025-01-31` reads `O3 Mini 2025-01-31`; `deepseek/v4-pro` under
/// `deepseek` reads `V4 Pro`.
pub(crate) fn fallback_label(id: &str, group: &str) -> String {
    let tail = id
        .strip_prefix(&format!("{group}/"))
        .filter(|_| !group.is_empty())
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id));
    let digits = |token: &str, lengths: std::ops::RangeInclusive<usize>| {
        lengths.contains(&token.len()) && token.bytes().all(|b| b.is_ascii_digit())
    };
    let raw: Vec<&str> = tail
        .split(['-', '_', ' '])
        .filter(|token| !token.is_empty())
        .collect();
    // A year, a month and a day in a row are one date, not a year and a
    // version number.
    let mut tokens: Vec<String> = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        if let [year, month, day, ..] = raw[index..]
            && digits(year, 4..=4)
            && digits(month, 2..=2)
            && digits(day, 2..=2)
        {
            tokens.push(format!("{year}-{month}-{day}"));
            index += 3;
        } else {
            tokens.push(raw[index].to_owned());
            index += 1;
        }
    }
    let mut words: Vec<String> = Vec::new();
    let mut previous_short_number = false;
    for token in &tokens {
        let token = token.as_str();
        let number = digits(token, 1..=2);
        match words.last_mut() {
            Some(last) if number && previous_short_number => {
                last.push('.');
                last.push_str(token);
            }
            _ => {
                let mut chars = token.chars();
                words.push(match chars.next() {
                    Some(first) if first.is_alphabetic() => {
                        first.to_uppercase().chain(chars).collect()
                    }
                    _ => token.to_owned(),
                });
            }
        }
        previous_short_number = number;
    }
    if words.is_empty() {
        id.to_owned()
    } else {
        words.join(" ")
    }
}

/// How one picker row draws.
pub(crate) struct PickerRowView {
    pub(crate) label: String,
    pub(crate) level: u8,
    pub(crate) dim: bool,
    pub(crate) current: bool,
    pub(crate) trailing: Trailing,
}

pub(crate) enum Trailing {
    None,
    Word(String),
    Caret { expanded: bool },
}

pub(crate) fn is_agent_provider(provider: &AiProviderId) -> bool {
    provider.as_str().starts_with("agent-")
}

/// A chat with no message yet may still change who answers it; after the
/// first turn, the conversation belongs to that provider (AGENTUX D1-2).
pub(crate) fn chat_is_empty(transcript: Option<&ChatTranscript>) -> bool {
    transcript.is_some_and(|transcript| transcript.messages.is_empty())
}

/// The chip's text for an agent chat: the agent's name, then its model's
/// label when one was chosen rather than the agent's own default. A model
/// no longer listed reads as its id formatted. When the label already
/// starts with the agent's brand (*Claude Opus 4.7* under *Claude Code*),
/// the brand is not said twice.
pub(crate) fn agent_chip_label(
    agents: &[AgentProviderView],
    provider: &AiProviderId,
    requested_model: &str,
    models: &[AgentModelView],
) -> String {
    let name = agents
        .iter()
        .find(|agent| &agent.id == provider)
        .map(|agent| agent.label.as_str())
        .unwrap_or(provider.as_str());
    if requested_model == AGENT_DEFAULT_MODEL || requested_model.is_empty() {
        return name.to_owned();
    }
    let label = models
        .iter()
        .find(|model| model.id == requested_model)
        .map(model_label)
        .unwrap_or_else(|| fallback_label(requested_model, ""));
    let first_word = |text: &str| text.split_whitespace().next().map(str::to_lowercase);
    if first_word(&label).is_some() && first_word(&label) == first_word(name) {
        label
    } else {
        format!("{name} · {label}")
    }
}

pub(crate) fn agent_label(agents: &[AgentProviderView], provider: &AiProviderId) -> String {
    agents
        .iter()
        .find(|agent| &agent.id == provider)
        .map(|agent| agent.label.clone())
        .unwrap_or_else(|| provider.as_str().to_owned())
}

/// Whether a durable row is a local agent's, and so reads by kind and
/// outcome rather than by a workspace tool's name.
pub(crate) fn is_agent_row(activity: &ToolActivity) -> bool {
    AgentToolKind::is_agent_token(&activity.tool_name)
}

/// The row's title: the agent's own wording when it gave any (stored with
/// the row since schema 8), the kind's localized name otherwise — never the
/// token.
pub(crate) fn agent_row_title(activity: &ToolActivity, lang: Lang) -> String {
    let kind = AgentToolKind::parse(&activity.tool_name).unwrap_or(AgentToolKind::Other);
    if let Some(summary) = activity.agent_summary.as_deref() {
        return format!("{} — {summary}", t(lang, Key::AiAgentAsks));
    }
    if let Some(label) = activity.agent_label.as_deref() {
        if activity.status == ToolActivityStatus::Running {
            return format!("{} — {label}", t(lang, Key::AiAgentWorking));
        }
        return label.to_owned();
    }
    let key = match kind {
        AgentToolKind::Tool | AgentToolKind::Other | AgentToolKind::Mcp => Key::AiAgentUsedTool,
        AgentToolKind::Command => Key::AiAgentRanCommand,
        AgentToolKind::FileChange => Key::AiAgentChangedFile,
        AgentToolKind::Search => Key::AiAgentSearched,
        AgentToolKind::Approval => Key::AiAgentAskedPermission,
    };
    t(lang, key).to_owned()
}

/// The row's detail: its outcome in the person's language, and, for a
/// failure, why — as the host classified it. The tool's own error text
/// never reaches the chat (UC17).
pub(crate) fn agent_row_detail(activity: &ToolActivity, lang: Lang) -> String {
    let outcome = agent_row_outcome(activity, lang);
    match (activity.status, activity.error_code) {
        (ToolActivityStatus::Failed, Some(code)) => {
            let reason = match code {
                koru_ai_core::ToolErrorCode::PermissionDenied => Key::AiToolFailedDenied,
                koru_ai_core::ToolErrorCode::Cancelled => Key::AiToolFailedCancelled,
                koru_ai_core::ToolErrorCode::ExecutionFailed => Key::AiToolFailedErrored,
            };
            format!("{outcome} · {}", t(lang, reason))
        }
        _ => outcome,
    }
}

/// The most lines of a call's input or output the row shows; the rest is
/// an ellipsis. The stored text is bounded in characters, not lines, and a
/// command's output is often a screen of them.
const CONTENT_LINES: usize = 12;

/// The row's body under its title: the outcome, the failure's own words
/// when the tool gave any, when it ran, and — since schema 8 (UC17) — what
/// the call was asked and what it produced, each under its own heading.
pub(crate) fn agent_row_body(activity: &ToolActivity, lang: Lang, now_ms: i64) -> String {
    let mut lines = vec![agent_row_detail(activity, lang)];
    if let Some(message) = activity
        .error_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        lines.push(message.to_owned());
    }
    lines.push(crate::tool_groups::activity_line(activity, lang, now_ms));
    for (key, content) in [
        (Key::AiToolInput, activity.agent_input.as_deref()),
        (Key::AiToolOutput, activity.agent_output.as_deref()),
    ] {
        let Some(content) = content.map(str::trim).filter(|content| !content.is_empty()) else {
            continue;
        };
        lines.push(format!("{}:", t(lang, key)));
        let mut shown = content.lines().take(CONTENT_LINES + 1).collect::<Vec<_>>();
        if shown.len() > CONTENT_LINES {
            shown.truncate(CONTENT_LINES);
            shown.push("…");
        }
        lines.extend(shown.iter().map(|line| line.to_string()));
    }
    lines.join("\n")
}

fn agent_row_outcome(activity: &ToolActivity, lang: Lang) -> String {
    let key = match (activity.status, activity.approval) {
        (ToolActivityStatus::ApprovalRequired, _) => Key::AiAgentOutcomeWaiting,
        (ToolActivityStatus::Running | ToolActivityStatus::Proposed, _) => {
            Key::AiAgentOutcomeRunning
        }
        (ToolActivityStatus::Complete, ToolApproval::Approved) => Key::AiAgentOutcomeAllowed,
        (ToolActivityStatus::Complete, _) => Key::AiAgentOutcomeDone,
        (ToolActivityStatus::Denied, _) => Key::AiAgentOutcomeRejected,
        (ToolActivityStatus::Failed, _) => Key::AiAgentOutcomeFailed,
        (ToolActivityStatus::Cancelled, _) => Key::AiAgentOutcomeCancelled,
    };
    t(lang, key).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_ai_core::{ChatId, InferenceAttemptId, MessageId, Timestamp, ToolInvocationId};
    use koru_ai_wire::Failure;

    fn validation(
        status: ProviderValidationStatus,
        code: Option<&str>,
    ) -> ProviderValidationResponse {
        ProviderValidationResponse {
            provider: AiProviderId::parse("agent-codex").unwrap(),
            status,
            models: Vec::new(),
            selected_model: None,
            failure: code.map(|code| Failure {
                code: code.into(),
                message: String::new(),
                retryable: true,
                details: Default::default(),
            }),
        }
    }

    #[test]
    fn readiness_words_follow_the_closed_codes() {
        assert_eq!(
            AgentStatus::from_validation(&validation(ProviderValidationStatus::Ready, None)),
            AgentStatus::Ready
        );
        assert_eq!(
            AgentStatus::from_validation(&validation(
                ProviderValidationStatus::Unavailable,
                Some("agent_signed_out")
            )),
            AgentStatus::SignedOut
        );
        assert_eq!(
            AgentStatus::from_validation(&validation(ProviderValidationStatus::Failed, None)),
            AgentStatus::CheckFailed
        );
        assert!(AgentStatus::Ready.word(Lang::En).is_none());
        for lang in Lang::ALL {
            assert!(!AgentStatus::NotInstalled.word(lang).unwrap().is_empty());
        }
    }

    fn model(id: &str, group: &str) -> AgentModelView {
        AgentModelView {
            id: id.into(),
            group: group.into(),
            name: None,
            description: None,
            group_name: None,
            legacy: false,
            reasoning: None,
        }
    }

    fn named(id: &str, group: &str, name: &str) -> AgentModelView {
        AgentModelView {
            name: Some(name.into()),
            ..model(id, group)
        }
    }

    #[test]
    fn models_group_in_first_seen_order_and_read_by_name_or_formatted_id() {
        let mut deepseek = named("deepseek/v4-pro", "deepseek", "DeepSeek V4 Pro");
        deepseek.group_name = Some("DeepSeek".into());
        let models = vec![
            named("default", "claude", "Default (recommended)"),
            deepseek,
            model("anthropic/claude", "anthropic"),
            model("deepseek/v4-flash", "deepseek"),
        ];
        assert_eq!(
            model_groups(&models),
            vec!["deepseek", "anthropic"],
            "the agent's own default is the Default row, in no group"
        );
        assert_eq!(model_label(&models[1]), "DeepSeek V4 Pro");
        assert_eq!(model_label(&models[3]), "V4 Flash");
        assert_eq!(group_label(&models, "deepseek"), "DeepSeek");
        assert_eq!(group_label(&models, "anthropic"), "Anthropic");
        assert_eq!(
            default_model_label(&models, Lang::En),
            "Default (recommended)"
        );
        assert_eq!(default_model_label(&models[1..], Lang::En), "Default");
        assert_eq!(
            listed_models(&models).count(),
            3,
            "an agent-listed default replaces the synthetic row"
        );
    }

    /// TECHNICAL §8's table, and the edges around it.
    #[test]
    fn a_model_id_is_formatted_by_rule_when_the_agent_sends_no_name() {
        for (id, group, label) in [
            ("claude-opus-4-1", "claude", "Claude Opus 4.1"),
            (
                "claude-sonnet-4-5-20250929",
                "claude",
                "Claude Sonnet 4.5 20250929",
            ),
            ("deepseek/v4-pro", "deepseek", "V4 Pro"),
            ("gpt-5.1-codex-mini", "openai", "Gpt 5.1 Codex Mini"),
            ("kimi-k2-0905", "kimi", "Kimi K2 0905"),
            ("o3-mini-2025-01-31", "openai", "O3 Mini 2025-01-31"),
            ("gpt-4o-2024-08-06-1", "openai", "Gpt 4o 2024-08-06 1"),
            ("model-2025-1-31", "g", "Model 2025 1.31"),
            (
                "openrouter/deepseek/deepseek-chat",
                "openrouter",
                "Deepseek/deepseek Chat",
            ),
            ("vendor/model_name", "other", "Model Name"),
            ("v-1-2-3", "", "V 1.2.3"),
            ("--", "g", "--"),
        ] {
            assert_eq!(fallback_label(id, group), label, "{id} under {group}");
        }
        assert_eq!(
            fallback_label("deepseek", ""),
            "Deepseek",
            "a group heading"
        );
    }

    #[test]
    fn the_chip_names_the_agent_and_a_chosen_models_label_once() {
        let agents = vec![
            AgentProviderView {
                id: AiProviderId::parse("agent-opencode").unwrap(),
                label: "OpenCode".into(),
            },
            AgentProviderView {
                id: AiProviderId::parse("agent-claude").unwrap(),
                label: "Claude Code".into(),
            },
        ];
        let opencode = AiProviderId::parse("agent-opencode").unwrap();
        let claude = AiProviderId::parse("agent-claude").unwrap();
        let listed = vec![
            named("opencode/beam-free", "opencode", "Beam Free"),
            named("opus", "claude", "Claude Opus 4.7"),
        ];
        assert_eq!(
            agent_chip_label(&agents, &opencode, AGENT_DEFAULT_MODEL, &listed),
            "OpenCode"
        );
        assert_eq!(
            agent_chip_label(&agents, &opencode, "opencode/beam-free", &listed),
            "OpenCode · Beam Free"
        );
        assert_eq!(
            agent_chip_label(&agents, &opencode, "deepseek/v4-pro", &[]),
            "OpenCode · V4 Pro",
            "a model no longer listed reads as its id formatted"
        );
        assert_eq!(
            agent_chip_label(&agents, &claude, "opus", &listed),
            "Claude Opus 4.7",
            "the brand is not said twice"
        );
    }

    fn row(tool_name: &str, status: ToolActivityStatus, approval: ToolApproval) -> ToolActivity {
        let now = Timestamp::now();
        ToolActivity {
            id: ToolInvocationId::new(),
            attempt_id: InferenceAttemptId::new(),
            chat_id: ChatId::new(),
            assistant_message_id: MessageId::new(),
            tool_name: tool_name.into(),
            read_only: false,
            status,
            approval,
            proposed_at: now,
            started_at: None,
            completed_at: None,
            result_bytes: None,
            error_code: None,
            target_workspace_id: None,
            target_document_id: None,
            error_message: None,
            agent_label: None,
            agent_summary: None,
            agent_input: None,
            agent_output: None,
            display_target: None,
            live: None,
        }
    }

    #[test]
    fn an_agent_row_reads_by_kind_and_outcome_once_the_wording_is_gone() {
        let done = row(
            "agent.command",
            ToolActivityStatus::Complete,
            ToolApproval::NotRequired,
        );
        assert!(is_agent_row(&done));
        assert_eq!(agent_row_title(&done, Lang::En), "Ran a command");
        assert_eq!(agent_row_detail(&done, Lang::En), "done");
        let mut failed = row(
            "agent.mcp",
            ToolActivityStatus::Failed,
            ToolApproval::NotRequired,
        );
        failed.error_code = Some(koru_ai_core::ToolErrorCode::ExecutionFailed);
        assert_eq!(
            agent_row_detail(&failed, Lang::En),
            "failed · The tool reported an error"
        );
        failed.error_code = Some(koru_ai_core::ToolErrorCode::PermissionDenied);
        assert_eq!(
            agent_row_detail(&failed, Lang::En),
            "failed · It was not allowed to run"
        );
        // The tool's own words sit between the outcome and the times, and
        // an empty message is no line at all.
        failed.error_message = Some("  Tool not found: koru__list_workspaces ".into());
        let body = agent_row_body(&failed, Lang::En, failed.proposed_at.millis());
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "failed · It was not allowed to run");
        assert_eq!(lines[1], "Tool not found: koru__list_workspaces");
        failed.error_message = Some("   ".into());
        assert_eq!(
            agent_row_body(&failed, Lang::En, failed.proposed_at.millis())
                .lines()
                .count(),
            2
        );
        let allowed = row(
            "agent.approval",
            ToolActivityStatus::Complete,
            ToolApproval::Approved,
        );
        assert_eq!(agent_row_title(&allowed, Lang::En), "Asked permission");
        assert_eq!(agent_row_detail(&allowed, Lang::En), "allowed");
        let mut live = row(
            "agent.tool",
            ToolActivityStatus::Running,
            ToolApproval::NotRequired,
        );
        live.agent_label = Some("Read koru.bat".into());
        assert_eq!(agent_row_title(&live, Lang::En), "Working — Read koru.bat");
        // Stored words survive the turn: the same row, ended, keeps its label.
        live.status = ToolActivityStatus::Complete;
        assert_eq!(agent_row_title(&live, Lang::En), "Read koru.bat");
        let mut asks = row(
            "agent.approval",
            ToolActivityStatus::ApprovalRequired,
            ToolApproval::Pending,
        );
        asks.agent_summary = Some("Run git status?".into());
        assert_eq!(
            agent_row_title(&asks, Lang::En),
            "Permission — Run git status?"
        );
        // UC17: the call's input and output, each under a heading, cut to
        // a screen of lines with an ellipsis.
        let mut ran = row(
            "agent.command",
            ToolActivityStatus::Complete,
            ToolApproval::NotRequired,
        );
        ran.agent_input = Some("git status".into());
        ran.agent_output = Some(
            (1..=20)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let body = agent_row_body(&ran, Lang::En, ran.proposed_at.millis());
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[2], "Input:");
        assert_eq!(lines[3], "git status");
        assert_eq!(lines[4], "Output:");
        assert_eq!(lines[5], "line 1");
        assert_eq!(lines.len(), 5 + 12 + 1);
        assert_eq!(lines.last().copied(), Some("…"));
        assert!(!is_agent_row(&row(
            "workspace_read",
            ToolActivityStatus::Complete,
            ToolApproval::NotRequired
        )));
    }
}

#[cfg(test)]
mod failure_words {
    use koru_i18n::Lang;

    fn failure(code: &str, message: &str) -> koru_ai_wire::Failure {
        koru_ai_wire::Failure {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: Default::default(),
        }
    }

    /// A code names the sentence in the person's language; the service's
    /// own words follow when they say more; an unknown code is those words.
    #[test]
    fn a_failure_reads_by_its_code_and_keeps_the_services_words() {
        assert_eq!(
            crate::failure_text(
                &failure("ai_provider", "the agent refused that model."),
                Lang::En
            ),
            "The provider reported an error · the agent refused that model"
        );
        assert_eq!(
            crate::failure_text(
                &failure("ai_network", "Koru could not reach the provider."),
                Lang::En
            ),
            "Koru could not reach the provider"
        );
        assert_eq!(
            crate::failure_text(&failure("ai_rate_limit", ""), Lang::De),
            "Der Anbieter ist ausgelastet. Versuche es gleich noch einmal"
        );
        assert_eq!(
            crate::failure_text(
                &failure("something_new", "Only the service knows."),
                Lang::En
            ),
            "Only the service knows"
        );
    }
}

pub(crate) fn is_agent_provider_str(provider: &str) -> bool {
    provider.starts_with("agent-")
}

/// How often a running check is asked about while it is in view.
const AGENT_CHECK_FOLLOW_UP: std::time::Duration = std::time::Duration::from_millis(1500);

/// Hit-test parts for offered choices start here; 1–3 are the workspace
/// tool's fixed buttons and 0 is the row itself.
pub(crate) const CHOICE_PART_BASE: u8 = 10;

pub(crate) fn offered_choice_count(activity: &ToolActivity) -> usize {
    activity.live.as_ref().map_or(0, |live| live.choices.len())
}

/// The buttons an agent's permission request offers, stacked so a long
/// label in any language keeps its full width. With no offered choices the
/// legacy pair, allow and reject, stands in.
pub(crate) fn agent_choice_rects(row: crate::Rect, activity: &ToolActivity) -> Vec<crate::Rect> {
    let count = offered_choice_count(activity).max(2);
    let top = row.y + row.h - count as f32 * (crate::BUTTON_H + 6.0) - 2.0;
    (0..count)
        .map(|index| {
            crate::Rect::new(
                row.x + 10.0,
                top + index as f32 * (crate::BUTTON_H + 6.0),
                (row.w - 20.0).max(1.0),
                crate::BUTTON_H,
            )
        })
        .collect()
}

pub(crate) fn agent_choice_at(
    row: crate::Rect,
    activity: &ToolActivity,
    x: f32,
    y: f32,
) -> Option<u8> {
    agent_choice_rects(row, activity)
        .into_iter()
        .position(|rect| rect.contains(x, y))
        .map(|index| CHOICE_PART_BASE + index as u8)
}

impl crate::ChatSurface {
    /// The provider the open setup card is about, for validation answers.
    pub(crate) fn setup_provider_id(&self) -> Option<AiProviderId> {
        match self.setup_provider {
            crate::SetupProvider::OpenAi => AiProviderId::parse(AiProviderId::OPENAI).ok(),
            crate::SetupProvider::Ollama => AiProviderId::parse(AiProviderId::OLLAMA).ok(),
            crate::SetupProvider::Koru => AiProviderId::parse(AiProviderId::KORU).ok(),
            crate::SetupProvider::Agents => self.setup_agent.clone(),
        }
    }

    /// The picker's rows. A chat that has begun offers its own provider's
    /// models. One with no message yet, or no chat yet, offers **only the
    /// local agents**, each with its readiness word: Koru AI, OpenAI and
    /// Ollama are not offered to start on (the user's call, 2026-10-31).
    pub(crate) fn chat_model_rows(&self) -> Vec<PickerRow> {
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        let open = self.transcript.is_none() || chat_is_empty(self.transcript.as_ref());
        let mut rows: Vec<PickerRow> = if open {
            Vec::new()
        } else {
            self.chat_model_options()
                .into_iter()
                .map(PickerRow::Model)
                .collect()
        };
        if !agents.is_empty() && open {
            rows.push(PickerRow::Header);
            for agent in agents {
                rows.push(PickerRow::Agent(agent.id.clone()));
                if self.expanded_agent.as_ref() == Some(&agent.id) {
                    self.push_agent_model_rows(&agent.id, &mut rows);
                }
            }
        }
        rows
    }

    /// Under an expanded agent: the *Default* row first — the agent's own
    /// `default` entry when it lists one — then its models, by group when
    /// there is more than one. A failed listing still offers the default.
    fn push_agent_model_rows(&self, agent: &AiProviderId, rows: &mut Vec<PickerRow>) {
        let default_row = PickerRow::AgentModel {
            agent: agent.clone(),
            id: AGENT_DEFAULT_MODEL.to_owned(),
        };
        match self.agent_models.get(agent) {
            None | Some(AgentModels::Loading) => {
                rows.push(PickerRow::Note(Key::AiAgentModelsLoading))
            }
            Some(AgentModels::Failed) => {
                rows.push(PickerRow::Note(Key::AiAgentModelsFailed));
                rows.push(default_row);
            }
            Some(AgentModels::Listed(models)) => {
                rows.push(default_row);
                let groups = model_groups(models);
                let model_row = |model: &AgentModelView| PickerRow::AgentModel {
                    agent: agent.clone(),
                    id: model.id.clone(),
                };
                if groups.len() <= 1 {
                    rows.extend(listed_models(models).map(model_row));
                    return;
                }
                for group in groups {
                    rows.push(PickerRow::Group {
                        agent: agent.clone(),
                        group: group.clone(),
                    });
                    if self.expanded_group.as_ref() == Some(&group) {
                        rows.extend(
                            listed_models(models)
                                .filter(|model| model.group == group)
                                .map(model_row),
                        );
                    }
                }
            }
        }
    }

    /// An agent model's levels, as its agent's listing carries them; `None`
    /// for any other provider, and for a model listed with none.
    pub(crate) fn agent_model_control(
        &self,
        model: &koru_ai_core::ModelSelection,
    ) -> Option<&koru_ai_core::ReasoningControl> {
        if !is_agent_provider(&model.provider) {
            return None;
        }
        self.listed_agent_models(&model.provider)
            .iter()
            .find(|listed| listed.id == model.requested_model)
            .and_then(|listed| listed.reasoning.as_ref())
    }

    /// The models listed for `agent`, empty while none are known.
    pub(crate) fn listed_agent_models(&self, agent: &AiProviderId) -> &[AgentModelView] {
        match self.agent_models.get(agent) {
            Some(AgentModels::Listed(models)) => models,
            _ => &[],
        }
    }

    pub(crate) fn picker_row_view(
        &self,
        row: &PickerRow,
        agents: &[AgentProviderView],
        current_provider: Option<&AiProviderId>,
        current_model: &str,
    ) -> PickerRowView {
        let on_agent = |agent: &AiProviderId| current_provider == Some(agent);
        match row {
            PickerRow::Model(model) => PickerRowView {
                label: model.clone(),
                level: 0,
                dim: false,
                current: current_provider.is_some_and(|provider| !is_agent_provider(provider))
                    && current_model == model,
                trailing: Trailing::None,
            },
            PickerRow::Header => PickerRowView {
                label: t(self.lang, Key::AiLocalAgents).to_owned(),
                level: 0,
                dim: true,
                current: false,
                trailing: Trailing::None,
            },
            PickerRow::Agent(agent) => {
                let status = self
                    .agent_status
                    .get(agent)
                    .copied()
                    .unwrap_or(AgentStatus::Checking);
                PickerRowView {
                    label: agent_label(agents, agent),
                    level: 0,
                    dim: !status.selectable(),
                    current: on_agent(agent) && current_model == AGENT_DEFAULT_MODEL,
                    trailing: match status.word(self.lang) {
                        Some(word) => Trailing::Word(word),
                        None => Trailing::Caret {
                            expanded: self.expanded_agent.as_ref() == Some(agent),
                        },
                    },
                }
            }
            PickerRow::Group { agent, group } => {
                let models = self.listed_agent_models(agent);
                PickerRowView {
                    label: group_label(models, group),
                    level: 1,
                    dim: false,
                    current: on_agent(agent)
                        && listed_models(models)
                            .any(|model| &model.group == group && model.id == current_model)
                        && self.expanded_group.as_ref() != Some(group),
                    trailing: Trailing::Caret {
                        expanded: self.expanded_group.as_ref() == Some(group),
                    },
                }
            }
            PickerRow::AgentModel { agent, id } => {
                let models = self.listed_agent_models(agent);
                let default = id == AGENT_DEFAULT_MODEL;
                let single_group = default || model_groups(models).len() <= 1;
                let label = if default {
                    default_model_label(models, self.lang)
                } else {
                    models
                        .iter()
                        .find(|model| &model.id == id)
                        .map(model_label)
                        .unwrap_or_else(|| fallback_label(id, ""))
                };
                PickerRowView {
                    label,
                    level: if single_group { 1 } else { 2 },
                    dim: false,
                    current: on_agent(agent) && current_model == id,
                    trailing: Trailing::None,
                }
            }
            PickerRow::Note(key) => PickerRowView {
                label: t(self.lang, *key).to_owned(),
                level: 1,
                dim: true,
                current: false,
                trailing: Trailing::None,
            },
        }
    }

    /// The picker row at `index`, as drawn.
    fn chat_model_row_rect(&self, index: usize) -> Option<crate::Rect> {
        let panel = self.chat_model_panel()?;
        let visible = index.checked_sub(self.chat_model_offset)?;
        Some(crate::Rect::new(
            panel.x + 4.0,
            panel.y + 4.0 + visible as f32 * 30.0,
            panel.w - 8.0,
            30.0,
        ))
    }

    /// A click on picker row `index`. The caret end of an agent row expands
    /// it and lists its models; anywhere else picks the agent with its own
    /// default model.
    pub(crate) fn click_picker_row(&mut self, index: usize, x: f32) -> bool {
        let Some(row) = self.chat_model_rows().get(index).cloned() else {
            return true;
        };
        if !row.selectable(&self.agent_status) {
            return true;
        }
        match row {
            PickerRow::Model(model) => {
                self.close_picker();
                self.change_chat_model(model)
            }
            PickerRow::Header | PickerRow::Note(_) => true,
            PickerRow::Agent(agent) => {
                let caret_end = self
                    .chat_model_row_rect(index)
                    .is_some_and(|rect| x >= rect.x + rect.w - 36.0);
                if caret_end {
                    self.toggle_agent_models(agent);
                    return true;
                }
                self.close_picker();
                self.choose_agent_model(agent, AGENT_DEFAULT_MODEL.to_owned())
            }
            PickerRow::Group { group, .. } => {
                self.expanded_group = if self.expanded_group.as_ref() == Some(&group) {
                    None
                } else {
                    Some(group)
                };
                true
            }
            PickerRow::AgentModel { agent, id } => {
                self.close_picker();
                self.choose_agent_model(agent, id)
            }
        }
    }

    pub(crate) fn close_picker(&mut self) {
        self.chat_model_open = false;
        self.chat_model_scroll_drag = None;
    }

    /// Expand or collapse an agent's row. Its models are usually already
    /// here; they are asked for when nothing is, or the last ask failed.
    fn toggle_agent_models(&mut self, agent: AiProviderId) {
        if self.expanded_agent.as_ref() == Some(&agent) {
            self.expanded_agent = None;
            return;
        }
        self.expanded_agent = Some(agent.clone());
        self.expanded_group = None;
        if !matches!(self.agent_models.get(&agent), Some(AgentModels::Listed(_))) {
            self.ask_agent_models(agent);
        }
    }

    /// Ask for every agent's models, so the picker opens on lists already
    /// known (agent-models D8): when settings arrive, and each time the
    /// picker opens. The host answers from memory.
    pub(crate) fn prefetch_agent_models(&mut self) {
        let agents: Vec<AiProviderId> = self
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .agents
                    .iter()
                    .map(|agent| agent.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for agent in agents {
            self.ask_agent_models(agent);
        }
    }

    /// Ask for one agent's models unless an ask is already out. A list on
    /// screen stays while the answer is on its way; only an agent with no
    /// list reads *Listing models…*.
    pub(crate) fn ask_agent_models(&mut self, agent: AiProviderId) {
        if self.agent_models_asked.contains(&agent) {
            return;
        }
        if !matches!(self.agent_models.get(&agent), Some(AgentModels::Listed(_))) {
            self.agent_models
                .insert(agent.clone(), AgentModels::Loading);
        }
        // Straight to the reader: a background ask is not something the
        // person is waiting on, so it neither marks the surface busy nor
        // clears its status line.
        let sent = self.reader.as_ref().is_some_and(|reader| {
            reader
                .send(crate::reader::Command::DiscoverModels(agent.clone()))
                .is_ok()
        });
        if sent {
            self.agent_models_asked.insert(agent);
        }
    }

    /// Ask the service how ready each agent is; the answers arrive as
    /// validations and fill `agent_status`. Cheap: the service answers from
    /// a cached snapshot.
    pub(crate) fn refresh_agent_status(&mut self) {
        self.agent_status_asked = Some(std::time::Instant::now());
        let ids: Vec<AiProviderId> = self
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .agents
                    .iter()
                    .map(|agent| agent.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.agent_status
                .entry(id.clone())
                .or_insert(AgentStatus::Checking);
            self.send_command(crate::reader::Command::Validate(id));
        }
    }

    /// While the picker or the *Local agents* card is showing and some agent
    /// is still being checked, ask again every little while: the service
    /// answers from a cached snapshot, and a re-probe it asked the host for
    /// lands there without anyone pressing anything.
    pub(crate) fn follow_agent_checks(&mut self, now: std::time::Instant) {
        let showing = self.chat_model_open
            || (self.setup_open && self.setup_provider == crate::SetupProvider::Agents);
        if !showing
            || !self
                .agent_status
                .values()
                .any(|status| *status == AgentStatus::Checking)
            || self
                .agent_status_asked
                .is_some_and(|asked| now.duration_since(asked) < AGENT_CHECK_FOLLOW_UP)
        {
            return;
        }
        self.refresh_agent_status();
    }

    /// Make a local agent the one who answers this chat, on the model
    /// named or its own default — or, with no chat yet, the one a new chat
    /// is made with. Only offered on a chat with no message yet, and only
    /// for an agent that is ready. The chat keeps its level when the new
    /// model offers it, and is back on *Default* otherwise (D18).
    pub(crate) fn choose_agent_model(
        &mut self,
        agent: AiProviderId,
        requested_model: String,
    ) -> bool {
        if self
            .transcript
            .as_ref()
            .is_some_and(|transcript| !chat_is_empty(Some(transcript)))
        {
            return true;
        }
        let mut model = koru_ai_core::ModelSelection {
            provider: agent,
            requested_model,
            default_reasoning: koru_ai_core::ReasoningChoice::Default,
        };
        let current = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.current_model.default_reasoning);
        if let (Some(current), Some(control)) = (current, self.agent_model_control(&model))
            && crate::effort::stops(control).contains(&current)
        {
            model.default_reasoning = current;
        }
        self.start_chat_on(model)
    }

    /// Answer an agent's permission request by the choice clicked.
    pub(crate) fn decide_agent_choice(
        &mut self,
        tool: koru_ai_core::ToolInvocationId,
        part: u8,
    ) -> bool {
        let Some(activity) = self.transcript.as_ref().and_then(|transcript| {
            transcript
                .tool_activities
                .iter()
                .find(|activity| activity.id == tool)
        }) else {
            return false;
        };
        let index = (part - CHOICE_PART_BASE) as usize;
        let choices = activity
            .live
            .as_ref()
            .map(|live| live.choices.as_slice())
            .unwrap_or_default();
        if choices.is_empty() {
            // The legacy pair: allow, then reject.
            return match index {
                0 => self.send_command(crate::reader::Command::ApproveTool(tool)),
                1 => self.send_command(crate::reader::Command::DenyTool(tool)),
                _ => false,
            };
        }
        let Some(choice) = choices.get(index) else {
            return false;
        };
        let approve = matches!(
            choice.intent,
            koru_ai_core::ChoiceIntent::AllowOnce | koru_ai_core::ChoiceIntent::AllowAlways
        );
        self.send_command(crate::reader::Command::DecideTool(
            tool,
            choice.id.clone(),
            approve,
        ))
    }

    /// The Settings card's agent rows: one per agent, its name and its
    /// readiness word, the picked one tinted.
    pub(crate) fn agent_row_rects(&self, area: crate::Rect) -> Vec<(AiProviderId, crate::Rect)> {
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        agents
            .iter()
            .enumerate()
            .map(|(index, agent)| {
                (
                    agent.id.clone(),
                    crate::Rect::new(area.x, area.y + index as f32 * 34.0, area.w, 30.0),
                )
            })
            .collect()
    }

    /// The agent row under the pointer on the *Local agents* card.
    pub(crate) fn agent_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let area = self.field_rects()[0];
        self.agent_row_rects(area)
            .iter()
            .position(|(_, rect)| rect.contains(x, y))
    }

    /// How much taller the card is than the two fields the others show:
    /// the rows, plus a breath before the buttons under them.
    pub(crate) fn agents_gap(&self) -> f32 {
        if self.setup_provider != crate::SetupProvider::Agents {
            return 0.0;
        }
        let rows = self
            .settings
            .as_ref()
            .map_or(0, |settings| settings.agents.len());
        (rows as f32 * 34.0 + 16.0 - 134.0).max(0.0)
    }

    /// *Kimi — Needs a supported version*: the row's own words as the status
    /// line, for a pick or a save the agent's readiness refuses.
    pub(crate) fn agent_not_ready_line(&self, agent: &AiProviderId) -> Option<String> {
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default();
        let word = self
            .agent_status
            .get(agent)
            .copied()
            .unwrap_or(AgentStatus::Checking)
            .word(self.lang)?;
        Some(format!("{} — {word}", agent_label(agents, agent)))
    }

    /// A row that is ready is picked; one that is not says why and is not.
    pub(crate) fn click_agent_row(&mut self, x: f32, y: f32) -> bool {
        let Some(index) = self.agent_row_at(x, y) else {
            return false;
        };
        let area = self.field_rects()[0];
        let Some((agent, _)) = self.agent_row_rects(area).into_iter().nth(index) else {
            return false;
        };
        if self
            .agent_status
            .get(&agent)
            .is_some_and(|status| status.selectable())
        {
            self.setup_agent = Some(agent);
            self.status = None;
        } else {
            self.status = self.agent_not_ready_line(&agent);
        }
        true
    }

    pub(crate) fn draw_agent_rows(
        &mut self,
        canvas: &mut vello::Scene,
        root: vello::kurbo::Affine,
        area: crate::Rect,
    ) {
        let p = crate::theme::p();
        let hint = self.engine.simple_layout_wrapped(
            &t(self.lang, Key::AiAgentsSettingsHint),
            crate::role::meta(),
            area.w,
        );
        crate::scene::draw_layout(canvas, root, &hint, area.x, area.y - 30.0, Some(p.text_dim));
        let rows = self.agent_row_rects(area);
        let agents = self
            .settings
            .as_ref()
            .map(|settings| settings.agents.clone())
            .unwrap_or_default();
        for (agent, rect) in rows {
            let status = self
                .agent_status
                .get(&agent)
                .copied()
                .unwrap_or(AgentStatus::Checking);
            let selected = self.setup_agent.as_ref() == Some(&agent);
            if selected || rect.contains(self.pointer.0, self.pointer.1) {
                crate::scene::fill(
                    canvas,
                    root,
                    if selected {
                        p.row_selected
                    } else {
                        p.hover_tint
                    },
                    &crate::scene::rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
                );
            }
            let line = self
                .engine
                .simple_layout(&agent_label(&agents, &agent), crate::role::row());
            crate::scene::draw_layout_first_line(
                canvas,
                root,
                &line,
                rect.x + 8.0,
                rect.y + (rect.h - line.height()) * 0.5,
                Some(if status.selectable() {
                    p.text
                } else {
                    p.text_dim
                }),
            );
            if let Some(word) = status.word(self.lang) {
                let word = self.engine.simple_layout(&word, crate::role::meta());
                crate::scene::draw_layout_first_line(
                    canvas,
                    root,
                    &word,
                    rect.x + rect.w - word.width() - 8.0,
                    rect.y + (rect.h - word.height()) * 0.5,
                    Some(p.text_dim),
                );
            }
        }
    }
}

/// The choices an agent offered for a pending permission request, as
/// buttons under the row; a rejection reads as the secondary style.
pub(crate) fn draw_agent_choices(
    canvas: &mut vello::Scene,
    root: vello::kurbo::Affine,
    engine: &mut crate::TextEngine,
    lang: Lang,
    pointer: (f32, f32),
    row: crate::Rect,
    activity: &ToolActivity,
) {
    let rects = agent_choice_rects(row, activity);
    let choices: Vec<(String, bool)> = match activity.live.as_ref() {
        Some(live) if !live.choices.is_empty() => live
            .choices
            .iter()
            .map(|choice| {
                (
                    choice.label.clone(),
                    matches!(
                        choice.intent,
                        koru_ai_core::ChoiceIntent::AllowOnce
                            | koru_ai_core::ChoiceIntent::AllowAlways
                    ),
                )
            })
            .collect(),
        _ => vec![
            (t(lang, Key::AiAgentAllow).to_owned(), true),
            (t(lang, Key::AiAgentReject).to_owned(), false),
        ],
    };
    for (rect, (label, allows)) in rects.into_iter().zip(choices) {
        crate::draw_surface_button(
            canvas,
            root,
            engine,
            rect,
            &label,
            if allows {
                crate::ButtonRole::Primary
            } else {
                crate::ButtonRole::Secondary
            },
            rect.contains(pointer.0, pointer.1),
            false,
        );
    }
}
