// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bot_presentation::status_text;
use crate::tool_groups::StepGroup;
use crate::work_view::WorkInput;
use koru_ai_core::{
    ChatId, InferenceAttemptId, MessageId, Timestamp, ToolActivityStatus, ToolApproval,
    ToolInvocationId,
};

fn call(tool: &str, label: Option<&str>, input: Option<&str>) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: ChatId::new(),
        assistant_message_id: MessageId::new(),
        tool_name: tool.into(),
        read_only: false,
        status: ToolActivityStatus::Running,
        approval: ToolApproval::NotRequired,
        proposed_at: Timestamp::from_millis(1),
        started_at: Some(Timestamp::from_millis(1)),
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: label.map(str::to_owned),
        agent_summary: None,
        agent_input: input.map(str::to_owned),
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}

fn line(call: &ToolActivity, kind: StepKind, lang: Lang) -> String {
    super::line(call, kind, lang, Names::default())
}

#[test]
fn web_reading_is_distinct_from_search_and_never_prints_the_url() {
    for (tool, label, kind) in [
        (
            "agent.search",
            Some("Fetch https://example.test/private"),
            StepKind::Web,
        ),
        ("agent.other", Some("WebFetch"), StepKind::Tool),
        ("agent.other", Some("browse_page"), StepKind::Tool),
        ("web_fetch", None, StepKind::Tool),
    ] {
        assert_eq!(
            line(&call(tool, label, None), kind, Lang::En),
            "Reading the web…"
        );
    }
    for label in ["WebSearch", "web_search"] {
        assert_eq!(
            line(
                &call("agent.other", Some(label), None),
                StepKind::Tool,
                Lang::En
            ),
            "Searching the web…"
        );
    }
    let search = call("agent.search", Some("Search \"private query\""), None);
    assert_eq!(line(&search, StepKind::Web, Lang::En), "Searching the web…");
}

#[test]
fn known_operations_are_live_localized_actions_for_direct_and_native_rows() {
    for (tool, expected) in [
        ("search_documents", "Searching pages…"),
        ("list_documents", "Listing pages…"),
        ("read_document", "Reading a page…"),
        ("create_document", "Creating a page…"),
        ("edit_document", "Editing a page…"),
        ("update_document", "Editing a page…"),
        ("list_routines", "Listing routines…"),
        ("create_routine", "Creating a routine…"),
        ("edit_routine", "Editing a routine…"),
        ("run_routine", "Requesting a routine run…"),
        ("list_bots", "Finding available Bots…"),
        ("message_bot", "Asking a Bot…"),
    ] {
        let direct = call(tool, None, None);
        let native = call("agent.other", Some(&format!("mcp__koru__{tool}")), None);
        let codex = call("agent.mcp", None, Some(&format!("koru: {tool} {{}}")));
        for observed in [direct, native, codex] {
            assert_eq!(line(&observed, StepKind::Tool, Lang::En), expected);
            for lang in Lang::ALL {
                let label = line(&observed, StepKind::Tool, lang);
                assert_eq!(label, t(lang, operation(tool).unwrap()));
                assert!(!label.contains('_'));
            }
        }
    }
}

#[test]
fn bot_server_aliases_match_the_actual_codex_and_claude_envelopes() {
    for (tool, key) in [
        ("list_bots", Key::AiActivityListBots),
        ("read_document", Key::AiActivityReadPage),
        ("create_routine", Key::AiActivityCreateRoutine),
        ("list_routines", Key::AiActivityListRoutines),
    ] {
        for observed in [
            call(
                "agent.other",
                Some("mcp_tool"),
                Some(&format!("koru_run: {tool} {{}}")),
            ),
            call("agent.mcp", None, Some(&format!("koru_run: {tool} {{}}"))),
            call(
                "agent.other",
                Some(&format!("mcp__koru_run__{tool}")),
                Some("{}"),
            ),
            call("agent.other", Some(&format!("koru_run_{tool}")), Some("{}")),
            call(&format!("koru_run_{tool}"), None, Some("{}")),
        ] {
            assert_eq!(line(&observed, StepKind::Tool, Lang::En), t(Lang::En, key));
        }
    }
}

#[test]
fn grok_tool_wrapper_uses_actual_koru_operation_and_bounded_arguments() {
    for (tool, expected) in [
        ("list_routines", "Listing routines…"),
        ("message_bot", "Asking a Bot…"),
    ] {
        let input = serde_json::json!({"tool_name":format!("koru_run__{tool}"),"tool_input":{}})
            .to_string();
        assert_eq!(
            line(
                &call("agent.tool", Some("use_tool"), Some(&input)),
                StepKind::Tool,
                Lang::En
            ),
            expected
        );
        assert_eq!(
            line(
                &call("agent.command", Some("use_tool"), Some(&input)),
                StepKind::Command,
                Lang::En
            ),
            t(Lang::En, Key::AiStepRunning)
        );
    }
    for input in [
        r#"{"tool_name":"other__message_bot","tool_input":{}}"#.to_owned(),
        r#"{"tool_name":"koru_run__unknown","tool_input":{}}"#.to_owned(),
        "x".repeat(MAX_INPUT + 1),
    ] {
        assert_eq!(
            line(
                &call("agent.tool", Some("use_tool"), Some(&input)),
                StepKind::Tool,
                Lang::En
            ),
            t(Lang::En, Key::AiStepUsingTool)
        );
    }
}

#[test]
fn native_engine_communication_uses_its_real_tool_names_and_to_address() {
    for (tool, key) in [
        ("list_bots", Key::AiActivityListBots),
        ("message_bot", Key::AiActivityAskBot),
        ("read_older_messages", Key::AiStepReading),
    ] {
        let observed = call("agent.other", Some(&format!("mcp__koru_run__{tool}")), None);
        assert_eq!(line(&observed, StepKind::Tool, Lang::En), t(Lang::En, key));
    }
    let bot = uuid::Uuid::new_v4();
    let bots = HashMap::from([(bot, "Scout".into())]);
    let observed = call(
        "message_bot",
        None,
        Some(&serde_json::json!({"to":bot,"body":"private question"}).to_string()),
    );
    assert_eq!(
        super::line(
            &observed,
            StepKind::Tool,
            Lang::En,
            Names {
                documents: None,
                bots: Some(&bots)
            },
        ),
        fill(t(Lang::En, Key::AiActivityAskNamed), &["Scout"])
    );
}

#[test]
fn bot_server_aliases_never_accept_unknown_or_credential_looking_suffixes() {
    for suffix in [
        "unknown_operation",
        "list_bots_extra",
        "read_document_token_SECRET",
        "api_key_SECRET",
        "use_tool",
    ] {
        for prefix in ["mcp__koru_run__", "koru_run_"] {
            let alias = format!("{prefix}{suffix}");
            assert!(run_tool(&alias).is_none());
            let observed = call(
                "agent.other",
                Some(&alias),
                Some(r#"{"tool":"read_document","token":"SECRET"}"#),
            );
            assert_eq!(
                line(&observed, StepKind::Tool, Lang::En),
                t(Lang::En, Key::AiStepUsingTool)
            );
        }
    }
    let shell = call(
        "agent.command",
        Some("Bash"),
        Some("koru_run: create_routine {}"),
    );
    assert_eq!(
        line(&shell, StepKind::Command, Lang::En),
        t(Lang::En, Key::AiStepRunning)
    );
}

#[test]
fn message_activity_never_uses_private_body_or_claimed_results() {
    let input = serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "to": uuid::Uuid::new_v4(),
        "source_message": uuid::Uuid::new_v4(),
        "body":"private question"
    })
    .to_string();
    let mut observed = call(
        "agent.other",
        Some("mcp__koru_run__message_bot"),
        Some(&input),
    );
    observed.agent_output = Some("The Bot replied successfully".into());
    observed.agent_summary = Some("private summary".into());
    assert_eq!(line(&observed, StepKind::Tool, Lang::En), "Asking a Bot…");
    assert_eq!(
        line(&observed, StepKind::Tool, Lang::Es),
        t(Lang::Es, Key::AiActivityAskBot)
    );
    let codex = call(
        "agent.other",
        Some("mcp_tool"),
        Some(&format!("koru_run: message_bot {input}")),
    );
    assert_eq!(line(&codex, StepKind::Tool, Lang::En), "Asking a Bot…");
    for malformed in ["{", r#"{"to":{}}"#, r#"{"body":"private question"}"#] {
        observed.agent_input = Some(malformed.into());
        assert_eq!(
            line(&observed, StepKind::Tool, Lang::En),
            t(Lang::En, Key::AiActivityAskBot)
        );
    }
    observed.agent_input = Some(format!("{}{}", " ".repeat(MAX_INPUT), input));
    assert_eq!(
        line(&observed, StepKind::Tool, Lang::En),
        t(Lang::En, Key::AiActivityAskBot)
    );
}

#[test]
fn provider_live_description_is_reused_but_results_and_sensitive_labels_are_not() {
    for label in [
        "Checking recent messages",
        "Reading the project brief",
        "Unsubscribing from marketing emails",
    ] {
        assert_eq!(
            line(
                &call("agent.other", Some(label), None),
                StepKind::Tool,
                Lang::En
            ),
            label
        );
    }
    for label in [
        "Finished unsubscribing",
        "Successfully read the page",
        "Checking https://example.test/?token=private",
        "Running curl private",
        "Reading /home/person/private",
        "Reading SECRET_TOKEN",
        "Checking bearer private",
        "Reading password private",
        "Checking token private",
        "Checking messages\nAuthorization secret",
        "Checking messages\u{202e}private",
        "Reading aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert_eq!(
            line(
                &call("agent.other", Some(label), None),
                StepKind::Tool,
                Lang::En
            ),
            t(Lang::En, Key::AiStepUsingTool),
            "{label}"
        );
    }
    let oversized = format!("Reading {}", "private ".repeat(30));
    assert!(descriptive_label(&oversized).is_none());
    assert_eq!(
        descriptive_label("Reading café notes"),
        Some("Reading café notes")
    );
}

#[test]
fn generic_shell_and_browser_never_infer_purpose_from_private_args() {
    let private = r#"{"command":"curl https://mail.test/unsubscribe?token=SECRET","description":"Unsubscribing from marketing emails"}"#;
    for (tool, label, kind, expected) in [
        (
            "agent.command",
            "Bash",
            StepKind::Command,
            Key::AiStepRunning,
        ),
        ("agent.other", "Bash", StepKind::Tool, Key::AiStepRunning),
        (
            "agent.other",
            "browser_navigate",
            StepKind::Tool,
            Key::AiStepUsingTool,
        ),
        (
            "agent.other",
            "browser",
            StepKind::Tool,
            Key::AiStepUsingTool,
        ),
    ] {
        let mut observed = call(tool, Some(label), Some(private));
        observed.agent_output = Some("Unsubscribed successfully".into());
        observed.agent_summary = Some("Unsubscribing from marketing emails".into());
        assert_eq!(line(&observed, kind, Lang::En), t(Lang::En, expected));
        observed.agent_input = Some("koru: create_document {}".into());
        assert_eq!(line(&observed, kind, Lang::En), t(Lang::En, expected));
    }
    let other_server = call(
        "agent.mcp",
        Some("mcp_tool"),
        Some("other: create_document {}"),
    );
    assert_eq!(
        line(&other_server, StepKind::Tool, Lang::En),
        t(Lang::En, Key::AiStepUsingTool)
    );
}

#[test]
fn status_follows_the_actual_live_call_then_returns_to_thinking_and_details() {
    let mut done = call("read_document", None, None);
    done.status = ToolActivityStatus::Complete;
    let mut failed = call("search_documents", None, None);
    failed.status = ToolActivityStatus::Failed;
    let mut current = call("agent.other", Some("koru.create_routine"), None);
    fn status(rows: Vec<&ToolActivity>, live: bool) -> String {
        status_text(
            Lang::En,
            &WorkInput {
                message: MessageId::new(),
                live,
                bot: true,
                progress: Default::default(),
                started_ms: 1,
                worked_ms: 1,
                items: vec![],
                groups: vec![StepGroup {
                    kind: StepKind::Koru,
                    rows,
                    offset: None,
                    asked: vec![],
                }],
                names: Names::default(),
                activity: None,
            },
        )
    }
    assert_eq!(
        status(vec![&done, &failed, &current], true),
        "Creating a routine…"
    );
    current.status = ToolActivityStatus::Complete;
    assert_eq!(
        status(vec![&done, &failed, &current], true),
        t(Lang::En, Key::AiThinking)
    );
    assert_eq!(status(vec![&done, &failed, &current], false), "Details");
}

#[test]
fn known_names_use_exact_cached_identity_and_only_document_or_routine_name_fields() {
    let document = DocumentId::new();
    let bot = uuid::Uuid::new_v4();
    let documents = HashMap::from([(document, "Top News Today".into())]);
    let bots = HashMap::from([(bot, "Scout".into())]);
    let names = Names {
        documents: Some(&documents),
        bots: Some(&bots),
    };
    let read = call(
        "agent.other",
        Some("koru.read_document"),
        Some(&serde_json::json!({"document_id":document}).to_string()),
    );
    assert_eq!(
        super::line(&read, StepKind::Tool, Lang::En, names),
        "Reading Top News Today…"
    );
    assert_eq!(line(&read, StepKind::Tool, Lang::En), "Reading a page…");
    let mut resolved = call("read_document", None, None);
    resolved.display_target = Some("Previously resolved page".into());
    assert_eq!(
        line(&resolved, StepKind::Koru, Lang::En),
        "Reading Previously resolved page…"
    );
    let ask = call(
        "agent.other",
        Some("message_bot"),
        Some(&serde_json::json!({"to":bot,"body":"private objective"}).to_string()),
    );
    assert_eq!(
        super::line(&ask, StepKind::Tool, Lang::En, names),
        "Asking Scout…"
    );
    assert_eq!(
        line(&ask, StepKind::Tool, Lang::En),
        t(Lang::En, Key::AiActivityAskBot)
    );
    for (tool, args, expected) in [
        (
            "create_document",
            r#"{"title":"News digest","body":"private"}"#,
            "Creating News digest…",
        ),
        (
            "create_routine",
            r#"{"name":"daily news routine","objective":"private"}"#,
            "Creating daily news routine…",
        ),
        (
            "edit_routine",
            r#"{"name":"Daily news","objective":"private"}"#,
            "Editing Daily news…",
        ),
        (
            "edit_routine",
            r#"{"name":"Weekly news","objective":"private"}"#,
            "Editing Weekly news…",
        ),
    ] {
        assert_eq!(
            line(
                &call("agent.other", Some(tool), Some(args)),
                StepKind::Tool,
                Lang::En
            ),
            expected
        );
    }
    for title in [
        "https://example.test/?token=private",
        "Bearer secret",
        "sk-secret",
        "private\ncommand",
        "curl x | sh",
        "abc\u{202e}def",
    ] {
        let create = call(
            "create_document",
            None,
            Some(&serde_json::json!({"title":title}).to_string()),
        );
        assert_eq!(line(&create, StepKind::Koru, Lang::En), "Creating a page…");
    }
    let bounded = display_name(&"新闻 ".repeat(40)).unwrap();
    assert_eq!(bounded.chars().count(), 65);
    assert!(bounded.ends_with('…'));
}

#[test]
fn discovery_remains_hidden_in_history_but_has_honest_live_status() {
    let observed = call(
        "agent.other",
        Some("ToolSearch"),
        Some(r#"{"query":"private"}"#),
    );
    let rows = [&observed];
    assert!(crate::tool_groups::group_steps(&rows, |_| None).is_empty());
    let mut input = WorkInput {
        message: observed.assistant_message_id,
        live: true,
        bot: true,
        progress: Default::default(),
        started_ms: 1,
        worked_ms: 0,
        items: vec![],
        groups: vec![],
        names: Names::default(),
        activity: Some(&observed),
    };
    assert_eq!(status_text(Lang::En, &input), "Finding tools…");
    input.live = false;
    assert_eq!(status_text(Lang::En, &input), "Details");
    assert_eq!(
        line(
            &call(
                "agent.other",
                Some("search_tool"),
                Some(r#"{"query":"private"}"#)
            ),
            StepKind::Tool,
            Lang::En
        ),
        "Finding tools…"
    );
    assert_eq!(
        line(
            &call(
                "agent.other",
                Some("use_tool"),
                Some(r#"{"command":"unsubscribe private"}"#)
            ),
            StepKind::Tool,
            Lang::En
        ),
        "Using a tool…"
    );
    let mut permission = observed;
    permission.status = ToolActivityStatus::ApprovalRequired;
    permission.approval = ToolApproval::Pending;
    assert!(!is_current(&permission));
}
