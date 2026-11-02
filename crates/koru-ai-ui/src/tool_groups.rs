// SPDX-License-Identifier: MPL-2.0

//! **What an agent did for a reply, in a few quiet lines** — the pure half
//! of the work view (`crate::work_view` draws it).
//!
//! A reply's tool calls are summarized, not listed: consecutive calls of one
//! kind are one line (*Read 4 files*, *Ran 2 commands*), a failure is a count
//! on its line (*· 1 failed*), plumbing a person has no use for is not shown,
//! and a pending permission request is never a line at all — it stays a card.
//!
//! Where each call began in the reply's text (`ToolActivity::text_offset`,
//! schema 10) splits the reply: the text before the last call is the agent's
//! running commentary, folded with the steps under *Worked for …*; the text
//! after it is the answer. A reply whose rows carry no offset — written
//! before schema 10, or by Koru's own tools — folds its steps and keeps all
//! of its text as the answer.

use koru_ai_core::{AgentToolKind, CallShape, MessageId, ToolActivity, ToolActivityStatus};
use koru_i18n::{Key, Lang, fill, t};

use crate::agents::is_agent_row;

/// What a step line says a group of calls did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    Read,
    SearchFiles,
    Web,
    Command,
    Edit,
    Koru,
    Tool,
    Permission,
}

/// A pending permission request: a card with its choices, never folded.
pub(crate) fn is_card(activity: &ToolActivity) -> bool {
    activity.status == ToolActivityStatus::ApprovalRequired
        && activity.approval == koru_ai_core::ToolApproval::Pending
}

/// The line a call is summarized under; `None` for a call that is not a
/// step — a pending permission request, or plumbing such as a lookup of
/// which tools exist.
pub(crate) fn step_kind(activity: &ToolActivity) -> Option<StepKind> {
    if is_card(activity) {
        return None;
    }
    if !is_agent_row(activity) {
        // Koru's own tools, called by Koru's own models.
        return Some(StepKind::Koru);
    }
    let shape = activity.agent_label.as_deref().and_then(CallShape::of);
    match shape {
        Some(CallShape::ToolDiscovery) => return None,
        Some(CallShape::Koru(_)) => return Some(StepKind::Koru),
        _ => {}
    }
    let kind = AgentToolKind::parse(&activity.tool_name).unwrap_or(AgentToolKind::Other);
    Some(match kind {
        AgentToolKind::Approval => StepKind::Permission,
        AgentToolKind::Command => StepKind::Command,
        AgentToolKind::FileChange => StepKind::Edit,
        AgentToolKind::Read => StepKind::Read,
        AgentToolKind::Search => match shape {
            Some(CallShape::Search) => StepKind::SearchFiles,
            _ => StepKind::Web,
        },
        AgentToolKind::Tool | AgentToolKind::Other | AgentToolKind::Mcp => match shape {
            Some(CallShape::Read) => StepKind::Read,
            Some(CallShape::Search) => StepKind::SearchFiles,
            Some(CallShape::Fetch) => StepKind::Web,
            _ => StepKind::Tool,
        },
    })
}

/// Consecutive calls of one kind, with no text said between them.
pub(crate) struct StepGroup<'a> {
    pub kind: StepKind,
    pub rows: Vec<&'a ToolActivity>,
    /// Where in the reply the group's first call began, in bytes.
    pub offset: Option<usize>,
}

/// A reply's steps, grouped in the order they happened. `offset` is where a
/// call began — the stored offset, or the live one the surface noted.
pub(crate) fn group_steps<'a>(
    rows: &[&'a ToolActivity],
    offset: impl Fn(&ToolActivity) -> Option<usize>,
) -> Vec<StepGroup<'a>> {
    let mut groups: Vec<StepGroup<'a>> = Vec::new();
    for row in rows {
        let Some(kind) = step_kind(row) else {
            continue;
        };
        let at = offset(row);
        match groups.last_mut() {
            Some(last) if last.kind == kind && last.offset == at => last.rows.push(row),
            _ => groups.push(StepGroup {
                kind,
                rows: vec![row],
                offset: at,
            }),
        }
    }
    groups
}

/// A group's line: what it did and how often, or — while a call in it is
/// still running — what it is doing; a failure is counted after a dot.
pub(crate) fn group_line(group: &StepGroup<'_>, lang: Lang) -> String {
    let live = group.rows.iter().any(|row| !row.status.is_terminal());
    let count = group.rows.len();
    let text = if live {
        t(
            lang,
            match group.kind {
                StepKind::Read => Key::AiStepReading,
                StepKind::SearchFiles => Key::AiStepSearchingFiles,
                StepKind::Web => Key::AiStepSearchingWeb,
                StepKind::Command => Key::AiStepRunning,
                StepKind::Edit => Key::AiStepEditing,
                StepKind::Koru => Key::AiStepUsingKoru,
                StepKind::Tool => Key::AiStepUsingTool,
                StepKind::Permission => Key::AiStepWaitingPermission,
            },
        )
        .to_owned()
    } else {
        let (one, many) = match group.kind {
            StepKind::Read => (Key::AiStepReadOne, Key::AiStepReadMany),
            StepKind::SearchFiles => (Key::AiStepSearchFilesOne, Key::AiStepSearchFilesMany),
            StepKind::Web => (Key::AiStepWebOne, Key::AiStepWebMany),
            StepKind::Command => (Key::AiAgentRanCommand, Key::AiStepCommandMany),
            StepKind::Edit => (Key::AiStepEditOne, Key::AiStepEditMany),
            StepKind::Koru => (Key::AiStepKoruOne, Key::AiStepKoruMany),
            StepKind::Tool => (Key::AiAgentUsedTool, Key::AiStepToolMany),
            StepKind::Permission => (Key::AiAgentAskedPermission, Key::AiStepPermissionMany),
        };
        if count == 1 {
            t(lang, one).to_owned()
        } else {
            fill(t(lang, many), &[&count.to_string()])
        }
    };
    let failed = group
        .rows
        .iter()
        .filter(|row| row.status == ToolActivityStatus::Failed)
        .count();
    if failed == 0 {
        text
    } else {
        format!(
            "{text} · {}",
            fill(t(lang, Key::AiStepFailed), &[&failed.to_string()])
        )
    }
}

/// One call's line inside an opened group: what the agent called it (its
/// first line), one of Koru's tools by its friendly name, or its kind.
pub(crate) fn call_line(activity: &ToolActivity, lang: Lang) -> String {
    if !is_agent_row(activity) {
        return koru_tool_line(&activity.tool_name, lang);
    }
    if let Some(label) = activity
        .agent_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        if let Some(CallShape::Koru(tool)) = CallShape::of(label) {
            return koru_tool_line(&tool, lang);
        }
        return label.lines().next().unwrap_or(label).to_owned();
    }
    if let Some(summary) = activity.agent_summary.as_deref() {
        return summary.lines().next().unwrap_or(summary).to_owned();
    }
    let kind = step_kind(activity).unwrap_or(StepKind::Tool);
    let group = StepGroup {
        kind,
        rows: vec![activity],
        offset: None,
    };
    group_line(&group, lang)
}

/// One of Koru's own tools, as a person would say it.
pub(crate) fn koru_tool_line(tool: &str, lang: Lang) -> String {
    let key = match tool {
        "read_document" => Key::AiKoruReadDocument,
        "search_documents" => Key::AiKoruSearchDocuments,
        "list_documents" => Key::AiKoruListDocuments,
        "list_workspaces" => Key::AiKoruListWorkspaces,
        "create_document" => Key::AiKoruCreateDocument,
        "edit_document" | "update_document" => Key::AiKoruEditDocument,
        other => return fill(t(lang, Key::AiKoruTool), &[&other.replace('_', " ")]),
    };
    t(lang, key).to_owned()
}

/// What the work view shows between its header and the answer, in order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WorkItem<'a> {
    /// What the agent said between calls, as it wrote it.
    Text(&'a str),
    /// A step group, by its index.
    Group(usize),
}

/// The reply cut at its calls: the work items in order, and where the
/// answer begins. Without an offset on every group the reply is not cut —
/// the items are the groups and the whole text is the answer.
pub(crate) fn split_reply<'a>(
    markdown: &'a str,
    groups: &[StepGroup<'_>],
) -> (Vec<WorkItem<'a>>, usize) {
    if groups.is_empty() || groups.iter().any(|group| group.offset.is_none()) {
        return ((0..groups.len()).map(WorkItem::Group).collect(), 0);
    }
    let mut items = Vec::new();
    let mut from = 0;
    for (index, group) in groups.iter().enumerate() {
        let to = floor_boundary(markdown, group.offset.unwrap_or(0)).max(from);
        let text = markdown[from..to].trim();
        if !text.is_empty() {
            items.push(WorkItem::Text(text));
        }
        items.push(WorkItem::Group(index));
        from = to;
    }
    (items, from)
}

/// A reply that used tools, cut for its work view.
pub(crate) struct Cut<'a, T> {
    pub groups: Vec<StepGroup<'a>>,
    pub items: Vec<WorkItem<'a>>,
    /// Where the answer begins in the reply's text; 0 when it is not cut.
    pub answer_start: usize,
    /// The answer's blocks.
    pub blocks: Vec<(T, String)>,
}

/// `markdown` and its `rows` as a work view and an answer; `None` when no
/// row is a step and the reply reads as it always did.
pub(crate) fn cut_reply<'a, T: Copy>(
    markdown: &'a str,
    rows: &[&'a ToolActivity],
    offset: impl Fn(&ToolActivity) -> Option<usize>,
    blocks: &[(T, String)],
) -> Option<Cut<'a, T>> {
    let groups = group_steps(rows, offset);
    if groups.is_empty() {
        return None;
    }
    let (items, answer_start) = split_reply(markdown, &groups);
    let kept = if answer_start == 0 {
        None
    } else {
        answer_blocks(markdown, answer_start, blocks)
    };
    Some(match kept {
        Some(blocks) => Cut {
            groups,
            items,
            answer_start,
            blocks,
        },
        // Uncut: nothing before the first call, or blocks that do not
        // line up with the text — the answer is all of it.
        None => Cut {
            items: (0..groups.len()).map(WorkItem::Group).collect(),
            groups,
            answer_start: 0,
            blocks: blocks.to_vec(),
        },
    })
}

/// The answer's blocks, cut where the answer begins: a block wholly before
/// it is commentary and dropped; a block it starts inside of — the agent's
/// words before and after a call run together in what was stored — keeps
/// only its answer half. `None` when a block cannot be found in the text,
/// and the reply is shown uncut.
pub(crate) fn answer_blocks<T: Copy>(
    markdown: &str,
    answer_start: usize,
    blocks: &[(T, String)],
) -> Option<Vec<(T, String)>> {
    let mut kept = Vec::new();
    let mut cursor = 0;
    for (id, block) in blocks {
        let start = cursor + markdown[cursor..].find(block.as_str())?;
        let end = start + block.len();
        cursor = end;
        if end <= answer_start {
            continue;
        }
        if start >= answer_start {
            kept.push((*id, block.clone()));
        } else {
            let tail = markdown[answer_start..end].trim_start();
            if !tail.is_empty() {
                kept.push((*id, tail.to_owned()));
            }
        }
    }
    Some(kept)
}

/// Commentary as plain text: the Markdown's words without its marks.
pub(crate) fn plain_text(markdown: &str) -> String {
    koru_ai_core::finalize_markdown(MessageId::new(), markdown)
        .iter()
        .map(|block| block.plain_text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The header: *Working for 6s* while the reply runs, *Worked for 1m 34s*
/// once it is done.
pub(crate) fn header(live: bool, elapsed_ms: i64, lang: Lang) -> String {
    fill(
        t(
            lang,
            if live {
                Key::AiWorkWorkingFor
            } else {
                Key::AiToolsWorkedFor
            },
        ),
        &[&seconds(elapsed_ms)],
    )
}

/// How long a finished reply worked: from the reply's start to its end,
/// or — when the message does not say — across its calls.
pub(crate) fn worked_ms(
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
    rows: &[&ToolActivity],
) -> i64 {
    if let (Some(started), Some(ended)) = (started_ms, ended_ms)
        && ended >= started
    {
        return ended - started;
    }
    let started = rows.iter().map(|row| row.proposed_at.millis()).min();
    let ended = rows
        .iter()
        .filter_map(|row| row.completed_at.map(|at| at.millis()))
        .max();
    match (started, ended) {
        (Some(started), Some(ended)) if ended >= started => ended - started,
        _ => 0,
    }
}

/// A row's own activity line under its outcome: when it started, relative
/// to now, and how long it took once it ended.
pub(crate) fn activity_line(activity: &ToolActivity, lang: Lang, now_ms: i64) -> String {
    let started = fill(
        t(lang, Key::AiToolsStarted),
        &[&koru_i18n::time::relative_long(
            lang,
            activity.proposed_at.millis(),
            now_ms,
        )],
    );
    match activity.completed_at {
        Some(ended) => format!(
            "{started} · {}",
            fill(
                t(lang, Key::AiToolsElapsed),
                &[&seconds(ended.millis() - activity.proposed_at.millis())]
            )
        ),
        None => started,
    }
}

/// `28s`, `1m 05s`: a duration as a person reads one, under a minute in
/// whole seconds.
pub(crate) fn seconds(ms: i64) -> String {
    let total = (ms.max(0) + 500) / 1000;
    if total < 60 {
        format!("{total}s")
    } else {
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

#[cfg(test)]
mod tests;
