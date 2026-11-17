// SPDX-License-Identifier: MPL-2.0
use super::*;

struct Never;
impl Backend for Never {
    fn doctor(&self) -> Result<Value, ToolError> {
        panic!("unexpected doctor")
    }
    fn invoke(&self, _: &RequestContext, _: &str, _: Value) -> Result<Value, ToolError> {
        panic!("unauthorized tool reached backend")
    }
}

#[test]
fn bot_tools_are_local_bridge_only() {
    let executor = WorkspaceToolExecutor::new(Arc::new(Never));
    let selected = RequestContext::selected(
        "local",
        "model",
        [Scope::ReadDocuments, Scope::EditDocuments],
        [],
    );
    let hosted = RequestContext::local().with_capabilities(HostCapabilities::rung1());
    assert!(allowed(&RequestContext::local()));
    for tool in BOT_TOOL_NAMES {
        assert_eq!(
            executor
                .invoke(&selected, tool, json!({}))
                .unwrap_err()
                .code,
            "permission_denied"
        );
        assert_eq!(
            executor.invoke(&hosted, tool, json!({})).unwrap_err().code,
            "tool_not_available"
        );
        assert!(Toolsets::parse("docs").unwrap().refusal(tool).is_some());
        assert!(Toolsets::parse("bots").unwrap().refusal(tool).is_none());
        assert!(workspace_tool_catalog().iter().any(|t| t.name == *tool));
    }
    assert!(!BOT_SCOPES.contains(&"bots.manage"));
    assert!(!BOT_SCOPES.contains(&"ai.chat"));
}

/// `m8_mcp_team_paused` (M8 piece D): when the person has paused every Bot,
/// `start_bot_task` answers the gateway's refusal as it is, so an assistant
/// reads the code, the sentence and `details.reason`. Pausing and resuming
/// the team are the person's controls: no tool here does either, and the
/// scopes this bridge asks for cannot reach the route that does.
#[test]
fn m8_mcp_team_paused_reaches_the_caller_and_no_tool_changes_the_pause() {
    let mut details = std::collections::BTreeMap::new();
    details.insert("reason".to_owned(), json!("team_paused"));
    let refused = ai_tool_error(koru_ai_wire::Failure {
        code: koru_ai_wire::bot_team_codes::TEAM_PAUSED.into(),
        message: "The person has paused all Bots.".into(),
        retryable: false,
        details,
    });
    assert_eq!(refused.code, "team_paused");
    assert_eq!(refused.message, "The person has paused all Bots.");
    assert_eq!(refused.details, Some(json!({ "reason": "team_paused" })));

    assert_eq!(
        BOT_TOOL_NAMES,
        [
            "list_bots",
            "start_bot_task",
            "read_bot_task",
            "stop_bot_task"
        ]
    );
    for tool in workspace_tool_catalog() {
        let name = tool.name.to_ascii_lowercase();
        assert!(
            !(name.contains("team") || name.contains("pause") || name.contains("resume")),
            "{name}: no tool pauses or resumes Bots"
        );
    }
    assert_eq!(
        BOT_SCOPES,
        ["bots.read", "bots.run", "store.read_documents"]
    );
}
