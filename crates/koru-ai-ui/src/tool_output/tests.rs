// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    ChatId, InferenceAttemptId, MessageId, Timestamp, ToolActivity, ToolActivityStatus,
    ToolApproval, ToolErrorCode, ToolInvocationId,
};
use koru_i18n::Lang;

use super::*;

const NOW: i64 = 10_000;

fn row(tool: &str, label: Option<&str>, status: ToolActivityStatus) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: ChatId::new(),
        assistant_message_id: MessageId::new(),
        tool_name: tool.into(),
        read_only: false,
        status,
        approval: ToolApproval::NotRequired,
        proposed_at: Timestamp::from_millis(2_000),
        started_at: Some(Timestamp::from_millis(2_000)),
        completed_at: status.is_terminal().then(|| Timestamp::from_millis(3_000)),
        result_bytes: None,
        error_code: (status == ToolActivityStatus::Failed)
            .then_some(ToolErrorCode::ExecutionFailed),
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: label.map(str::to_owned),
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}

fn done(tool: &str, label: &str, input: Option<&str>, output: &str) -> ToolActivity {
    let mut row = row(tool, Some(label), ToolActivityStatus::Complete);
    row.agent_input = input.map(str::to_owned);
    row.agent_output = Some(output.to_owned());
    row
}

/// Every word a detail shows, piece by piece, as a person would read it.
fn words(pieces: &[Piece]) -> Vec<String> {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Meta(text)
            | Piece::Markdown(text)
            | Piece::Toggle(text)
            | Piece::Header { text, .. } => text.clone(),
            Piece::Code(lines) => lines
                .iter()
                .map(|line| format!("{}{}", line.gutter, line.text))
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect()
}

/// What the chat's Markdown renderer draws for `markdown`, block by block.
fn rendered(markdown: &str) -> Vec<String> {
    let mut engine = koru_draw::text::TextEngine::new();
    koru_ai_core::cut_markdown(markdown)
        .into_iter()
        .map(|block| {
            match koru_markdown::render_block(
                &mut engine,
                &block.markdown,
                400.0,
                &koru_markdown::Styles::ui(),
            ) {
                koru_markdown::Rendered::Text { text, .. } => text,
                _ => String::new(),
            }
        })
        .collect()
}

#[test]
fn a_fenced_output_is_drawn_as_a_code_block_without_its_fences() {
    let output = "```console\nOn branch main\nnothing to commit\n```";
    let pieces = pieces(
        &done("agent.other", "Custom tool", None, output),
        Lang::En,
        NOW,
        false,
    );
    let Some(Piece::Markdown(markdown)) = pieces.first() else {
        panic!("Markdown first: {pieces:?}");
    };
    let drawn = rendered(markdown);
    assert_eq!(drawn, ["On branch main\nnothing to commit"]);
    assert!(drawn.iter().all(|text| !text.contains("```")));
    // And it sits on the code background.
    let mut engine = koru_draw::text::TextEngine::new();
    let lines = layout::lay_out(&mut engine, &pieces, 400.0);
    assert!(lines[0].panel);
}

/// D17: a failure's words were shown under the outcome and again as the
/// output, each inside raw fences. Now once, unfenced, in the error color.
#[test]
fn the_error_is_shown_once() {
    let mut failed = row(
        "agent.other",
        Some("mcp__koru__list_workspaces"),
        ToolActivityStatus::Failed,
    );
    failed.error_message = Some("```\nTool not found\n```".into());
    failed.agent_output = Some("```\nTool not found\n```".into());
    failed.agent_input = Some("{}".into());
    let pieces = pieces(&failed, Lang::En, NOW, false);
    let words = words(&pieces);
    assert_eq!(
        words
            .iter()
            .filter(|text| text.contains("Tool not found"))
            .count(),
        1,
        "{words:?}"
    );
    assert!(words.iter().all(|text| !text.contains("```")), "{words:?}");
    let error = pieces
        .iter()
        .find_map(|piece| match piece {
            Piece::Code(lines) => Some(lines),
            _ => None,
        })
        .unwrap();
    assert!(error.iter().all(|line| line.ink == Ink::Error));
    // The tool said why, so the host's generic reason is not repeated.
    assert_eq!(words.last().unwrap(), "Failed · 1s · Just now");
}

#[test]
fn an_input_that_says_nothing_is_not_shown() {
    for input in ["{}", "[]", "null", "  "] {
        let pieces = pieces(
            &done("agent.other", "Custom tool", Some(input), "a result"),
            Lang::En,
            NOW,
            false,
        );
        assert_eq!(
            words(&pieces),
            ["a result", "Done · 1s · Just now"],
            "input {input:?}"
        );
    }
    // An input that says something is shown, pretty, under its heading.
    let pieces = pieces(
        &done(
            "agent.other",
            "Custom tool",
            Some(r#"{"city":"Lisbon"}"#),
            "Sunny",
        ),
        Lang::En,
        NOW,
        false,
    );
    assert_eq!(
        words(&pieces),
        [
            "Input",
            "{\n  \"city\": \"Lisbon\"\n}",
            "Output",
            "Sunny",
            "Done · 1s · Just now"
        ]
    );
}

#[test]
fn a_failure_without_words_says_why_on_its_status_line() {
    let failed = row("agent.other", Some("x"), ToolActivityStatus::Failed);
    let pieces = pieces(&failed, Lang::En, NOW, false);
    assert_eq!(
        words(&pieces),
        ["Failed · The tool reported an error · 1s · Just now"]
    );
}

#[test]
fn a_long_block_folds_to_its_head_and_tail() {
    let lines = (1..=40)
        .map(|n| CodeLine::plain(format!("line {n}"), Ink::Text))
        .collect::<Vec<_>>();
    let folded = code_block(lines.clone(), false, Lang::En);
    let [Piece::Code(shown), Piece::Toggle(toggle)] = folded.as_slice() else {
        panic!("{folded:?}");
    };
    assert_eq!(shown.len(), HEAD_LINES + 1 + TAIL_LINES);
    assert_eq!(shown[0].text, "line 1");
    assert_eq!(shown[HEAD_LINES].text, "\u{22EF} 28 more lines");
    assert_eq!(shown.last().unwrap().text, "line 40");
    assert_eq!(toggle, "Show all 40 lines");

    let open = code_block(lines.clone(), true, Lang::En);
    let [Piece::Code(all), Piece::Toggle(toggle)] = open.as_slice() else {
        panic!("{open:?}");
    };
    assert_eq!(all.len(), 40);
    assert_eq!(toggle, "Show fewer lines");

    // A block that would hide only a line or two is never folded.
    assert_eq!(
        code_block(lines[..14].to_vec(), false, Lang::En),
        vec![Piece::Code(lines[..14].to_vec())]
    );
}

#[test]
fn a_fence_is_taken_off_only_when_it_wraps_the_whole_text() {
    assert_eq!(unfence("```console\n$ ls\n```"), (Some("console"), "$ ls"));
    assert_eq!(unfence("```\n```"), (None, ""));
    // A wider fence wraps text that has fences of its own.
    assert_eq!(
        unfence("````\n```\ninside\n```\n````"),
        (None, "```\ninside\n```")
    );
    // Cut off by the row's bound: the opening fence still goes.
    assert_eq!(unfence("```diff\n+a\n+b"), (Some("diff"), "+a\n+b"));
    // Two blocks, or text around a block, are left as they are.
    let two = "```\na\n```\nthen\n```\nb\n```";
    assert_eq!(unfence(two), (None, two));
    assert_eq!(unfence("plain"), (None, "plain"));
}

#[test]
fn terminal_escapes_are_taken_out() {
    assert_eq!(
        strip_ansi("\u{1b}[31merror\u{1b}[0m: \u{1b}]0;title\u{7}done"),
        "error: done"
    );
    assert_eq!(strip_ansi("plain"), "plain");
}

/// A detail is laid out once: the same pieces at the same width have the
/// same fingerprint, and anything that changes what is drawn changes it.
#[test]
fn a_detail_is_fingerprinted_by_what_it_draws() {
    let row = done("agent.other", "x", None, "out");
    let a = pieces(&row, Lang::En, NOW, false);
    assert_eq!(
        layout::fingerprint(&a, 300.0),
        layout::fingerprint(&pieces(&row, Lang::En, NOW, false), 300.0)
    );
    assert_ne!(
        layout::fingerprint(&a, 300.0),
        layout::fingerprint(&a, 301.0)
    );
    let other = done("agent.other", "x", None, "out 2");
    assert_ne!(
        layout::fingerprint(&a, 300.0),
        layout::fingerprint(&pieces(&other, Lang::En, NOW, false), 300.0)
    );
}

// ----- commands -----------------------------------------------------------

/// Claude's command: its input names the command, its output comes in a
/// `console` fence. The header says what ran; the fence is gone.
#[test]
fn a_command_reads_as_its_line_over_what_it_printed() {
    let ran = done(
        "agent.command",
        "git status",
        Some(r#"{"command":"git status","description":"Show the tree's status"}"#),
        "```console\nOn branch main\nnothing to commit\n```",
    );
    let pieces = pieces(&ran, Lang::En, NOW, false);
    assert_eq!(
        words(&pieces),
        [
            "$ git status",
            "On branch main\nnothing to commit",
            "Done · 1s · Just now"
        ]
    );
    assert_eq!(
        pieces[0],
        Piece::Header {
            text: "$ git status".into(),
            mono: true
        }
    );
}

/// A failed command: its words once, in the error color, and its exit code
/// on the status line rather than as the first line of its output.
#[test]
fn a_failed_command_shows_its_exit_code_and_its_error_once() {
    let mut failed = row(
        "agent.command",
        Some("Terminal"),
        ToolActivityStatus::Failed,
    );
    failed.agent_input = Some(r#"{"command":"git status"}"#.into());
    failed.error_message = Some("```\nExit code 128\nfatal: not a git repository\n```".into());
    failed.agent_output = failed.error_message.clone();
    let pieces = pieces(&failed, Lang::En, NOW, false);
    assert_eq!(
        words(&pieces),
        [
            "$ git status",
            "fatal: not a git repository",
            "Failed · Exit code 128 · 1s · Just now"
        ]
    );
    let Piece::Code(lines) = &pieces[1] else {
        panic!("{pieces:?}")
    };
    assert_eq!(lines[0].ink, Ink::Error);
}

/// Codex's command is its input itself; a long output folds.
#[test]
fn a_long_command_output_folds() {
    let output = (1..=100)
        .map(|n| format!("\u{1b}[32mok\u{1b}[0m test {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let ran = done(
        "agent.command",
        "command_execution",
        Some("cargo test"),
        &output,
    );
    let pieces = pieces(&ran, Lang::En, NOW, false);
    assert_eq!(
        pieces[0],
        Piece::Header {
            text: "$ cargo test".into(),
            mono: true
        }
    );
    let Piece::Code(lines) = &pieces[1] else {
        panic!("{pieces:?}")
    };
    assert_eq!(lines.len(), HEAD_LINES + 1 + TAIL_LINES);
    assert_eq!(lines[0].text, "ok test 1", "escapes are taken out");
    assert_eq!(pieces[2], Piece::Toggle("Show all 100 lines".into()));
    let open = super::pieces(&ran, Lang::En, NOW, true);
    let Piece::Code(lines) = &open[1] else {
        panic!("{open:?}")
    };
    assert_eq!(lines.len(), 100);
}

/// A command whose call never said what it ran, and printed nothing, is
/// its status line alone — no `$ Terminal`, no `{}`.
#[test]
fn a_command_that_said_nothing_is_its_status_alone() {
    let mut ran = row(
        "agent.command",
        Some("Terminal"),
        ToolActivityStatus::Complete,
    );
    ran.agent_input = Some("{}".into());
    assert_eq!(
        words(&pieces(&ran, Lang::En, NOW, false)),
        ["Done · 1s · Just now"]
    );
}

// ----- diffs --------------------------------------------------------------

/// What Koru's ACP adapter keeps for an edit that arrived as ACP `diff`
/// content (`acp::events::diff_text`, tested there from the content).
const EDIT: &str =
    "--- src/a.rs\n+++ src/a.rs\n@@ -40,3 +40,3 @@\n fn a() {\n-    old();\n+    new();\n }";

#[test]
fn a_unified_diff_parses_to_numbered_lines() {
    let files = diff::parse(EDIT).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path.as_deref(), Some("src/a.rs"));
    let lines = files[0]
        .lines
        .iter()
        .map(|line| (line.kind, line.number, line.text.as_str()))
        .collect::<Vec<_>>();
    use diff::DiffKind::*;
    assert_eq!(
        lines,
        [
            (Hunk, None, "@@ -40,3 +40,3 @@"),
            (Context, Some(40), "fn a() {"),
            (Removed, Some(41), "    old();"),
            (Added, Some(41), "    new();"),
            (Context, Some(42), "}"),
        ]
    );
    assert!(diff::parse("just text\n+ not a diff").is_none());
}

/// An edit reads as its path, then green and red lines with their numbers.
#[test]
fn an_edit_from_acp_diff_content_reads_as_a_diff() {
    let edit = done(
        "agent.file_change",
        "Edit src/a.rs",
        Some(r#"{"file_path":"src/a.rs","old_string":"old();","new_string":"new();"}"#),
        EDIT,
    );
    let pieces = pieces(&edit, Lang::En, NOW, false);
    assert_eq!(
        pieces[0],
        Piece::Header {
            text: "src/a.rs".into(),
            mono: false
        }
    );
    let Piece::Code(lines) = &pieces[1] else {
        panic!("{pieces:?}")
    };
    let shown = lines
        .iter()
        .map(|line| (line.gutter.as_str(), line.text.as_str(), line.ink))
        .collect::<Vec<_>>();
    assert_eq!(
        shown,
        [
            ("40", "  fn a() {", Ink::Text),
            ("41", "-     old();", Ink::Removed),
            ("41", "+     new();", Ink::Added),
            ("42", "  }", Ink::Text),
        ]
    );
}

/// A `diff` fence in any tool's output is drawn the same way; the prose
/// around it stays Markdown.
#[test]
fn a_diff_fence_in_text_output_renders_as_a_diff() {
    let output = format!("Applied:\n\n```diff\n{EDIT}\n```\n\nDone.");
    let pieces = pieces(
        &done("agent.other", "apply_patch", None, &output),
        Lang::En,
        NOW,
        false,
    );
    assert_eq!(pieces[0], Piece::Markdown("Applied:".into()));
    assert!(matches!(&pieces[1], Piece::Header { text, .. } if text == "src/a.rs"));
    assert!(
        matches!(&pieces[2], Piece::Code(lines) if lines.iter().any(|line| line.ink == Ink::Added))
    );
    assert_eq!(pieces[3], Piece::Markdown("Done.".into()));
}

/// A row from before Koru kept a diff (`path` over the new text) reads as
/// its path over that text, not as a diff it is not.
#[test]
fn an_edit_from_before_diffs_reads_as_its_path_and_text() {
    let edit = done(
        "agent.file_change",
        "Edit README.md",
        None,
        "README.md\nnew words",
    );
    assert_eq!(
        words(&pieces(&edit, Lang::En, NOW, false)),
        ["README.md", "new words", "Done · 1s · Just now"]
    );
}
