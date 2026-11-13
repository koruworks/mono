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
