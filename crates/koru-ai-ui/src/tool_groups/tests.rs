// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::{ChatId, InferenceAttemptId, Timestamp, ToolApproval, ToolInvocationId};

pub(crate) fn row(
    tool_name: &str,
    label: Option<&str>,
    status: ToolActivityStatus,
    offset: Option<u32>,
) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: ChatId::new(),
        assistant_message_id: MessageId::new(),
        tool_name: tool_name.into(),
        read_only: false,
        status,
        approval: ToolApproval::NotRequired,
        proposed_at: Timestamp::from_millis(1_000),
        started_at: None,
        completed_at: status.is_terminal().then(|| Timestamp::from_millis(2_000)),
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: label.map(str::to_owned),
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: offset,
        display_target: None,
        live: None,
    }
}

fn stored(activity: &ToolActivity) -> Option<usize> {
    activity.text_offset.map(|offset| offset as usize)
}

fn lines(rows: &[&ToolActivity]) -> Vec<String> {
    group_steps(rows, stored)
        .iter()
        .map(|group| group_line(group, Lang::En))
        .collect()
}

const DONE: ToolActivityStatus = ToolActivityStatus::Complete;

#[test]
fn consecutive_calls_of_one_kind_are_one_line() {
    let reads = [
        row("agent.read", Some("Read a.rs"), DONE, Some(0)),
        row("agent.other", Some("Read b.rs"), DONE, Some(0)),
        row("agent.read", Some("Read c.rs"), DONE, Some(0)),
    ];
    let commands = [
        row("agent.command", Some("`cargo test`"), DONE, Some(0)),
        row("agent.command", Some("`git status`"), DONE, Some(0)),
    ];
    let edit = row("agent.file_change", Some("Edit `a.rs`"), DONE, Some(0));
    let web = row("agent.search", None, DONE, Some(0));
    let found = row("agent.other", Some("Find `*.md`"), DONE, Some(0));
    let all: Vec<&ToolActivity> = reads
        .iter()
        .chain(&commands)
        .chain([&edit, &web, &found])
        .collect();
    assert_eq!(
        lines(&all),
        [
            "Read 3 files",
            "Ran 2 commands",
            "Edited a file",
            "Searched the web",
            "Searched files",
        ]
    );
    // The same kind on either side of something said is two lines.
    let split = [
        row("agent.read", Some("Read a.rs"), DONE, Some(0)),
        row("agent.read", Some("Read b.rs"), DONE, Some(12)),
    ];
    assert_eq!(
        lines(&[&split[0], &split[1]]),
        ["Read a file", "Read a file"]
    );
}

#[test]
fn a_failure_is_counted_on_its_line_and_a_running_call_says_what_it_is_doing() {
    let ok = row("agent.command", None, DONE, None);
    let failed = row("agent.command", None, ToolActivityStatus::Failed, None);
    assert_eq!(lines(&[&ok, &failed]), ["Ran 2 commands · 1 failed"]);
    let running = row(
        "agent.tool",
        Some("Read big.log"),
        ToolActivityStatus::Running,
        None,
    );
    assert_eq!(
        lines(&[&ok, &running]),
        ["Ran a command", "Reading… big.log"]
    );
}

/// D17: an answered permission is a note on the call it was asked for —
/// the call whose label its summary repeats — not a line between steps;
/// the calls on either side of it read as one step again.
#[test]
fn an_answered_permission_is_a_note_on_its_call() {
    let first = row("agent.command", Some("cargo test"), DONE, Some(0));
    let mut asked = row("agent.approval", None, DONE, Some(0));
    asked.approval = ToolApproval::Approved;
    asked.agent_summary = Some("cargo test".into());
    let second = row("agent.command", Some("git status"), DONE, Some(0));
    let rows = [&first, &asked, &second];
    assert_eq!(lines(&rows), ["Ran 2 commands · asked"]);
    let groups = group_steps(&rows, stored);
    assert_eq!(
        step_call_line(&groups[0], &first, Lang::En),
        "cargo test · asked"
    );
    assert_eq!(step_call_line(&groups[0], &second, Lang::En), "git status");

    // Refused, it says so.
    let mut refused = asked.clone();
    refused.status = ToolActivityStatus::Denied;
    refused.approval = ToolApproval::Denied;
    assert_eq!(lines(&[&first, &refused]), ["Ran a command · refused"]);

    // A permission no call answers to keeps its own line.
    let mut stray = asked.clone();
    stray.agent_summary = Some("permissions".into());
    assert_eq!(
        lines(&[&first, &stray, &second]),
        ["Ran a command", "Asked permission", "Ran a command"]
    );
}

/// A call titled only by its kind — Codex's `command_execution`, Claude's
/// *Terminal* from before its input arrived — says what it did from its
/// input instead.
#[test]
fn a_call_line_says_what_the_call_did() {
    let mut codex = row("agent.command", Some("command_execution"), DONE, None);
    codex.agent_input = Some("cargo test --workspace".into());
    assert_eq!(call_line(&codex, Lang::En), "cargo test --workspace");
    let mut claude = row("agent.command", Some("Terminal"), DONE, None);
    claude.agent_input = Some(r#"{"command":"git status","description":"Status"}"#.into());
    assert_eq!(call_line(&claude, Lang::En), "git status");
    let mut search = row("agent.search", Some("web_search"), DONE, None);
    search.agent_input = Some("koru notes".into());
    assert_eq!(call_line(&search, Lang::En), "\u{201C}koru notes\u{201D}");
    // With nothing to go on, the kind's word stays.
    let mut bare = row("agent.command", Some("Terminal"), DONE, None);
    bare.agent_input = Some("{}".into());
    assert_eq!(call_line(&bare, Lang::En), "Terminal");
}

#[test]
fn plumbing_is_hidden_koru_is_named_and_a_pending_permission_is_a_card() {
    let discovery = row("agent.other", Some("ToolSearch"), DONE, None);
    let koru = row("agent.mcp", Some("mcp__koru__read_document"), DONE, None);
    let own = row("search_documents", None, DONE, None);
    let mut pending = row(
        "agent.approval",
        None,
        ToolActivityStatus::ApprovalRequired,
        None,
    );
    pending.approval = ToolApproval::Pending;
    assert!(is_card(&pending));
    assert_eq!(step_kind(&pending), None);
    assert_eq!(step_kind(&discovery), None);
    assert_eq!(
        lines(&[&discovery, &koru, &own, &pending]),
        ["Used Koru (2)"]
    );
    assert_eq!(call_line(&koru, Lang::En), "Read a document");
    assert_eq!(call_line(&own, Lang::En), "Searched documents");
    assert_eq!(koru_tool_line("list_tasks", Lang::En), "Koru: list tasks");
    let answered = row("agent.approval", None, DONE, None);
    assert_eq!(lines(&[&answered]), ["Asked permission"]);
}

#[test]
fn a_call_line_is_its_labels_first_line() {
    let call = row("agent.command", Some("`ls`\n-la"), DONE, None);
    assert_eq!(call_line(&call, Lang::En), "`ls`");
    let bare = row("agent.command", None, DONE, None);
    assert_eq!(call_line(&bare, Lang::En), "Ran a command");
}

#[test]
fn the_reply_is_cut_at_its_calls_and_the_answer_follows_the_last() {
    let text = "Looking at the file.Now the tests.All good: it works.";
    let read = row("agent.read", None, DONE, Some(20));
    let run = row("agent.command", None, DONE, Some(34));
    let groups = group_steps(&[&read, &run], stored);
    let (items, answer) = split_reply(text, &groups);
    assert_eq!(
        items,
        [
            WorkItem::Text("Looking at the file."),
            WorkItem::Group(0),
            WorkItem::Text("Now the tests."),
            WorkItem::Group(1),
        ]
    );
    assert_eq!(&text[answer..], "All good: it works.");
}

#[test]
fn a_reply_without_offsets_folds_only_its_steps() {
    let text = "Here is the answer.";
    let read = row("agent.read", None, DONE, None);
    let groups = group_steps(&[&read], stored);
    let (items, answer) = split_reply(text, &groups);
    assert_eq!(items, [WorkItem::Group(0)]);
    assert_eq!(answer, 0);
    // An offset past the text, or inside a character, is held to the text.
    let late = row("agent.read", None, DONE, Some(99));
    let groups = group_steps(&[&late], stored);
    assert_eq!(split_reply("é!", &groups).1, "é!".len());
    let inside = row("agent.read", None, DONE, Some(1));
    let groups = group_steps(&[&inside], stored);
    assert_eq!(split_reply("é!", &groups).1, 0);
}

#[test]
fn the_answers_blocks_start_where_the_answer_does() {
    let text = "First I look.\n\nRan it.Done, it works.\n\n- one";
    let blocks = vec![
        (1, "First I look.".to_owned()),
        (2, "Ran it.Done, it works.".to_owned()),
        (3, "- one".to_owned()),
    ];
    let start = text.find("Done").unwrap();
    assert_eq!(
        answer_blocks(text, start, &blocks).unwrap(),
        vec![(2, "Done, it works.".to_owned()), (3, "- one".to_owned())]
    );
    assert_eq!(answer_blocks(text, 0, &blocks).unwrap(), blocks);
    let missing = vec![(9, "not here".to_owned())];
    assert!(answer_blocks(text, start, &missing).is_none());
}

#[test]
fn the_header_says_how_long_the_reply_worked() {
    assert_eq!(header(true, 6_200, Lang::En), "Working for 6s");
    assert_eq!(header(false, 94_000, Lang::En), "Worked for 1m 34s");
    let a = row("agent.command", None, DONE, None);
    assert_eq!(worked_ms(Some(1_000), Some(4_000), &[&a]), 3_000);
    assert_eq!(worked_ms(None, None, &[&a]), 1_000);
    assert_eq!(plain_text("**Bold** and `code`."), "Bold and code.");
    for lang in Lang::ALL {
        assert!(!header(false, 1_000, lang).is_empty());
    }
}

#[test]
fn a_rows_activity_line_names_its_start_and_its_length() {
    let mut done = row("agent.command", None, DONE, None);
    done.completed_at = Some(Timestamp::from_millis(2_400));
    let line = activity_line(&done, Lang::En, 1_000 + 3 * 60_000);
    assert!(line.starts_with("Started "), "{line}");
    assert!(line.ends_with("Elapsed 1s"), "{line}");
    let running = row("agent.command", None, ToolActivityStatus::Running, None);
    assert!(!activity_line(&running, Lang::En, 2_000).contains("Elapsed"));
    assert_eq!(seconds(65_400), "1m 05s");
}
