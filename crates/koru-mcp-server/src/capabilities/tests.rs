// SPDX-License-Identifier: MPL-2.0

use std::sync::{Arc, Mutex};

use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::Scope;
use serde_json::{Value, json};

use super::*;
use crate::{Backend, RequestContext, Toolset, WorkspaceToolExecutor};

/// A backend that records every call that reached it.
#[derive(Default)]
struct Recording(Mutex<Vec<String>>);

impl Backend for Recording {
    fn invoke(
        &self,
        _context: &RequestContext,
        tool: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        self.0.lock().unwrap().push(tool.to_owned());
        Ok(json!({}))
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        Ok(json!({}))
    }
}

fn hosted(capabilities: HostCapabilities) -> (RequestContext, WorkspaceId) {
    let workspace = WorkspaceId::new();
    let context = RequestContext::selected(
        "us",
        "app-connection:test",
        [Scope::ReadDocuments, Scope::EditDocuments],
        [workspace],
    )
    .with_capabilities(capabilities);
    (context, workspace)
}

fn names(tools: &[WorkspaceTool]) -> Vec<String> {
    tools.iter().map(|tool| tool.name.clone()).collect()
}

/// TECHNICAL §14, *Capabilities (F6)*: every tool in the shared catalog
/// outside rung 1, called by name with a Read & edit grant, is refused before
/// any backend work.
#[test]
fn every_tool_outside_rung_one_is_refused_before_backend_work() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let (context, workspace) = hosted(HostCapabilities::rung1());
    let outside: Vec<String> = names(&workspace_tool_catalog())
        .into_iter()
        .filter(|tool| !RUNG1_TOOLS.contains(&tool.as_str()))
        .collect();
    assert!(
        outside.len() >= 10,
        "files, versions, chats, add_image and update_task are all outside: {outside:?}"
    );
    for tool in &outside {
        // Arguments that would pass every later check, so the refusal can
        // only be the capability's.
        let refused = executor
            .invoke(&context, tool, json!({ "workspace_id": workspace }))
            .expect_err(tool);
        assert_eq!(refused.code, "tool_not_available", "{tool}");
        assert_eq!(refused.details, Some(json!({ "tool": tool })), "{tool}");
    }
    assert!(
        backend.0.lock().unwrap().is_empty(),
        "no refused tool reached the backend"
    );
}

/// The read set refuses every mutation the same way, scopes notwithstanding.
#[test]
fn the_read_set_refuses_every_rung_one_mutation() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let (context, workspace) = hosted(HostCapabilities::rung1_reads());
    for tool in ["create_document", "edit_document", "extract_to_document"] {
        let refused = executor
            .invoke(&context, tool, json!({ "workspace_id": workspace }))
            .expect_err(tool);
        assert_eq!(refused.code, "tool_not_available", "{tool}");
    }
    assert!(backend.0.lock().unwrap().is_empty());
}

/// A name that is no tool at all is refused the same way on a hosted host, so
/// a caller cannot tell a deferred tool from a misspelled one by probing.
#[test]
fn an_unknown_name_is_not_available_either() {
    let executor = WorkspaceToolExecutor::new(Arc::new(Recording::default()));
    let (context, _) = hosted(HostCapabilities::rung1());
    let refused = executor
        .invoke(&context, "drop_tables", json!({}))
        .unwrap_err();
    assert_eq!(refused.code, "tool_not_available");
}

/// A tool the host serves goes on to the ordinary checks and the backend.
#[test]
fn a_served_tool_still_meets_scope_and_workspace_authority() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let (context, workspace) = hosted(HostCapabilities::rung1());
    executor
        .invoke(
            &context,
            "list_documents",
            json!({ "workspace_id": workspace }),
        )
        .unwrap();
    let elsewhere = executor
        .invoke(
            &context,
            "list_documents",
            json!({ "workspace_id": WorkspaceId::new() }),
        )
        .unwrap_err();
    assert_eq!(elsewhere.code, "permission_denied");
    let read_only = RequestContext::selected(
        "us",
        "app-connection:read",
        [Scope::ReadDocuments],
        [workspace],
    )
    .with_capabilities(HostCapabilities::rung1());
    let no_scope = executor
        .invoke(
            &read_only,
            "edit_document",
            json!({ "workspace_id": workspace }),
        )
        .unwrap_err();
    assert_eq!(no_scope.code, "permission_denied");
    assert_eq!(*backend.0.lock().unwrap(), vec!["list_documents"]);
}

/// One set publishes and admits: the hosted catalog is exactly the tools it
/// admits, every name in the sets is a real tool, and the schemas are the
/// shared catalog's own.
#[test]
fn the_catalog_and_admission_are_one_set() {
    let shared = workspace_tool_catalog();
    for set in [RUNG1_TOOLS, RUNG1_READ_TOOLS] {
        for tool in set {
            assert!(
                shared.iter().any(|published| published.name == *tool),
                "{tool} is not a published tool"
            );
        }
    }
    let hosted = HostCapabilities::rung1().catalog();
    let mut listed = names(&hosted);
    listed.sort();
    let mut expected: Vec<String> = RUNG1_TOOLS.iter().map(|tool| tool.to_string()).collect();
    expected.sort();
    assert_eq!(listed, expected);
    for tool in &hosted {
        let original = shared.iter().find(|t| t.name == tool.name).unwrap();
        assert_eq!(tool, original, "{} is published unchanged", tool.name);
    }
    for tool in RUNG1_READ_TOOLS {
        assert!(RUNG1_TOOLS.contains(tool), "{tool}: the reads are a subset");
    }
}

/// **The local catalog is unchanged** (TECHNICAL §5.4 item 1): local hosts
/// serve everything, and their catalog is the shared one.
#[test]
fn the_local_catalog_is_unchanged() {
    assert_eq!(RequestContext::local().capabilities, HostCapabilities::All);
    assert_eq!(
        RequestContext::selected(
            "local",
            "koru-ai",
            [Scope::ReadDocuments],
            [WorkspaceId::new()]
        )
        .capabilities,
        HostCapabilities::All
    );
    assert_eq!(HostCapabilities::default(), HostCapabilities::All);
    assert_eq!(HostCapabilities::All.catalog(), workspace_tool_catalog());
}

/// Capabilities and toolsets compose rather than overlap: every hosted tool
/// sits in some toolset, so a bridge narrowed by `--toolsets` and a host
/// narrowed by capabilities intersect, and neither has a tool the other
/// cannot name.
#[test]
fn every_hosted_tool_belongs_to_a_toolset() {
    for tool in RUNG1_TOOLS {
        assert!(Toolset::of(tool).is_some(), "{tool}");
    }
}
