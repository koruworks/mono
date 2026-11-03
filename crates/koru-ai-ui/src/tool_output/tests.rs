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
            | Piece::Path(text)
            | Piece::Header { text, .. } => text.clone(),
            Piece::Link { title, site, .. } => format!("{title} · {site}"),
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
    assert_eq!(pieces[0], Piece::Path("src/a.rs".into()));
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
    assert_eq!(pieces[1], Piece::Path("src/a.rs".into()));
    assert!(
        matches!(&pieces[2], Piece::Code(lines) if lines.iter().any(|line| line.ink == Ink::Added))
    );
    assert_eq!(pieces[3], Piece::Markdown("Done.".into()));
}

/// **A diff's path is a link that opens the file** (D62): in the link
/// color, a press target of its own, carrying the path as the agent wrote
/// it; each file of a diff its own.
#[test]
fn a_diffs_path_is_a_link_to_the_file() {
    let two = format!("{EDIT}\n--- a/docs/b.md\n+++ b/docs/b.md\n@@ -1,1 +1,1 @@\n-old\n+new");
    let edit = done("agent.file_change", "Edit", None, &two);
    let pieces = pieces(&edit, Lang::En, NOW, false);
    let paths: Vec<&Piece> = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Path(_)))
        .collect();
    assert_eq!(
        paths,
        [
            &Piece::Path("src/a.rs".into()),
            &Piece::Path("docs/b.md".into())
        ]
    );
    let mut engine = koru_draw::text::TextEngine::new();
    let lines = layout::lay_out(&mut engine, &pieces, 400.0);
    let links: Vec<_> = lines
        .iter()
        .filter(|line| matches!(line.hit, Some(layout::DetailHit::Path(_))))
        .collect();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].hit, Some(layout::DetailHit::Path(0)));
    assert_eq!(links[1].hit, Some(layout::DetailHit::Path(1)));
    assert_eq!(links[1].url.as_deref(), Some("docs/b.md"));
    assert!(links.iter().all(|line| line.ink == Some(Ink::Link)));
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

// ----- file reads ---------------------------------------------------------

/// Claude's read: the lines it asked for in its input, the file's lines
/// numbered `N<tab>` inside a fence (claude-agent-acp's `markdownEscape`).
#[test]
fn a_ranged_read_reads_as_its_path_lines_and_numbers() {
    let read = done(
        "agent.read",
        "Read src/a.rs (40 - 42)",
        Some(r#"{"file_path":"src/a.rs","offset":40,"limit":3}"#),
        "```\n40\tfn a() {\n41\t    new();\n42\t}\n```",
    );
    let pieces = pieces(&read, Lang::En, NOW, false);
    assert_eq!(
        pieces[0],
        Piece::Header {
            text: "src/a.rs · lines 40–42".into(),
            mono: false
        }
    );
    let Piece::Code(lines) = &pieces[1] else {
        panic!("{pieces:?}")
    };
    assert_eq!(
        lines
            .iter()
            .map(|line| (line.gutter.as_str(), line.text.as_str()))
            .collect::<Vec<_>>(),
        [("40", "fn a() {"), ("41", "    new();"), ("42", "}")]
    );
}

/// A whole file is not *ranged*; a read from before Step 0, with no input,
/// still finds its path in its title; the older `→` numbering reads too.
#[test]
fn a_whole_read_has_no_range_and_its_title_names_it() {
    let read = done(
        "agent.other",
        "Read README.md",
        Some("{}"),
        "     1\u{2192}# Koru\n     2\u{2192}A workspace.",
    );
    let pieces = pieces(&read, Lang::En, NOW, false);
    assert_eq!(
        words(&pieces),
        [
            "README.md",
            "1# Koru\n2A workspace.",
            "Done · 1s · Just now"
        ]
    );
    // A file whose lines are not numbered keeps them as they are.
    assert_eq!(
        file::numbered("plain\ntext"),
        [(None, "plain".to_owned()), (None, "text".to_owned())]
    );
}

// ----- search results -----------------------------------------------------

/// Claude's web search (claude-agent-acp's `formatWebSearchHit`): one
/// `Title (url)` line per result becomes one row a person can open.
#[test]
fn a_web_search_reads_as_its_query_and_one_row_per_result() {
    let search = done(
        "agent.search",
        "Search \"koru notes\"",
        Some(r#"{"query":"koru notes"}"#),
        "Koru — notes that think (https://www.koru.works/notes)\nKoru on GitHub (https://github.com/koruworks)",
    );
    let pieces = pieces(&search, Lang::En, NOW, false);
    assert_eq!(
        words(&pieces),
        [
            "\u{201C}koru notes\u{201D}",
            "Koru — notes that think · koru.works",
            "Koru on GitHub · github.com",
            "Done · 1s · Just now"
        ]
    );
    assert!(matches!(&pieces[1], Piece::Link { url, .. } if url == "https://www.koru.works/notes"));
    // Each row opens its own link.
    let mut engine = koru_draw::text::TextEngine::new();
    let lines = layout::lay_out(&mut engine, &pieces, 400.0);
    assert_eq!(lines[1].hit, Some(layout::DetailHit::Link(0)));
    assert_eq!(
        lines[2].url.as_deref(),
        Some("https://github.com/koruworks")
    );
}

/// An older CLI's dump: the `Links:` line is read; prose that lists no
/// results is drawn as text under the query.
#[test]
fn a_web_search_reads_what_it_can_and_falls_back_to_text() {
    let dump = "Web search results for query: \"koru\"\n\nLinks: [{\"title\":\"Koru\",\"url\":\"https://koru.works\"}]";
    assert_eq!(
        search::web_hits(dump),
        [search::WebHit {
            title: "Koru".into(),
            url: "https://koru.works".into()
        }]
    );
    let prose = done(
        "agent.search",
        "web_search",
        Some("koru"),
        "Koru is a workspace.",
    );
    assert_eq!(
        words(&pieces(&prose, Lang::En, NOW, false)),
        [
            "\u{201C}koru\u{201D}",
            "Koru is a workspace.",
            "Done · 1s · Just now"
        ]
    );
}

/// Claude's Grep: `path:line:text` rows under the command it ran.
#[test]
fn a_local_search_reads_as_path_line_and_text_rows() {
    let grep = done(
        "agent.search",
        "grep -n \"todo\" src",
        Some(r#"{"pattern":"todo","path":"src","-n":true}"#),
        "src/a.rs:12:    // todo: this\nC:\\koru\\b.rs:3:todo!()",
    );
    assert_eq!(
        words(&pieces(&grep, Lang::En, NOW, false)),
        [
            "grep -n \"todo\" src",
            "src/a.rs:12  // todo: this\nC:\\koru\\b.rs:3  todo!()",
            "Done · 1s · Just now"
        ]
    );
    // Glob's paths after its count; prose is not a list of matches.
    assert_eq!(
        search::matches("Found 2 files\nsrc/a.rs\nsrc/b.rs")
            .unwrap()
            .len(),
        2
    );
    assert!(search::matches("I looked everywhere and found nothing").is_none());
}

// ----- MCP tools ----------------------------------------------------------

/// One of Koru's own tools, called by Claude: one friendly line from the
/// step lines' table, with the JSON behind it one click away.
#[test]
fn a_koru_tool_reads_as_one_friendly_line() {
    let result = r#"{"document_id":"d","root_node_id":"r","nodes":[
        {"node_id":"r","kind":"document","content":{"type":"text","text":"Trip to Portugal"}},
        {"node_id":"a","kind":"paragraph","content":{"type":"text","text":"Lisbon"}},
        {"node_id":"b","kind":"paragraph","content":{"type":"text","text":"Porto"}}]}"#;
    let read = done(
        "agent.other",
        "mcp__koru__read_document",
        Some(r#"{"workspace_id":"w","document_id":"d"}"#),
        result,
    );
    let folded = pieces(&read, Lang::En, NOW, false);
    assert_eq!(
        folded[0],
        Piece::Header {
            text: "Read a document · Trip to Portugal · 2 blocks".into(),
            mono: false
        }
    );
    assert!(matches!(&folded[1], Piece::Toggle(text) if text.starts_with("Show all")));
    let open = pieces(&read, Lang::En, NOW, true);
    assert!(
        matches!(&open[1], Piece::Code(lines) if lines.iter().any(|line| line.text.contains("Porto")))
    );
    assert_eq!(open[2], Piece::Toggle("Show fewer lines".into()));
}

/// Another MCP server's tool (Codex's `mcpToolCall` result) is its JSON,
/// pretty; a result cut off by the row's bound reads as text.
#[test]
fn another_mcp_tools_result_is_pretty_json_or_text() {
    let call = done(
        "agent.mcp",
        "mcp_tool",
        Some(r#"github: search_issues {"q":"bug"}"#),
        r#"{"content":[{"type":"text","text":"{\"total\":2}"}]}"#,
    );
    assert_eq!(
        words(&pieces(&call, Lang::En, NOW, false)),
        [
            "github · search_issues",
            "{\n  \"total\": 2\n}",
            "Done · 1s · Just now"
        ]
    );
    let cut = done(
        "agent.mcp",
        "mcp__github__get",
        None,
        r#"{"items":[{"a":1},{"#,
    );
    assert_eq!(
        words(&pieces(&cut, Lang::En, NOW, false)),
        [r#"{"items":[{"a":1},{"#, "Done · 1s · Just now"]
    );
}

/// A document too long for the row arrives as its summary (`_truncated`):
/// the line counts the whole document from the marker, not the few nodes
/// kept, and says how large the result was instead of offering it all.
#[test]
fn a_summarized_koru_result_counts_from_its_marker_and_says_its_size() {
    let summary = r#"{"document_id":"d","nodes":[
        {"node_id":"r","kind":"document","content":{"type":"text","text":"Trip to Portugal"}},
        {"node_id":"a","kind":"paragraph","content":{"type":"text","text":"Lisbon"}}],
        "_truncated":{"bytes":51234,"lengths":{"nodes":213},"counts":{"blocks":212,"tasks":12}}}"#;
    let read = done("agent.other", "mcp__koru__read_document", None, summary);
    let folded = pieces(&read, Lang::En, NOW, false);
    assert_eq!(
        folded[0],
        Piece::Header {
            text: "Read a document · Trip to Portugal · 212 blocks".into(),
            mono: false
        }
    );
    assert_eq!(
        folded[1],
        Piece::Meta("Result was 51 KB; showing a summary".into())
    );
    assert!(!folded.iter().any(|piece| matches!(piece, Piece::Toggle(_))));

    // Any other list: its full length from `lengths`.
    let listed = r#"{"workspaces":[{"id":"a"},{"id":"b"}],
        "_truncated":{"bytes":9000,"lengths":{"workspaces":40}}}"#;
    let list = done("agent.other", "mcp__koru__list_workspaces", None, listed);
    let Piece::Header { text, .. } = &pieces(&list, Lang::En, NOW, false)[0] else {
        panic!("a header first");
    };
    assert!(text.ends_with("40 results"), "{text}");
}

/// Another MCP tool's summary shows the summary, without its marker.
#[test]
fn an_mcp_summary_is_shown_without_its_marker() {
    let call = done(
        "agent.mcp",
        "mcp__github__search",
        None,
        r#"{"total":2,"_truncated":{"bytes":9000,"lengths":{}}}"#,
    );
    let shown = words(&pieces(&call, Lang::En, NOW, false)).join("\n");
    assert!(shown.contains("\"total\": 2"));
    assert!(!shown.contains("_truncated"), "{shown}");
}
