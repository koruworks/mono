// SPDX-License-Identifier: MPL-2.0

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use koru_store_core::model::Scope;
use serde_json::{Value, json};

use super::*;
use crate::{
    Backend, HostCapabilities, RequestContext, WorkspaceToolExecutor, workspace_tool_catalog,
};

/// A backend that records every call that reached it and answers a
/// two-workspace list.
#[derive(Default)]
struct Recording(Mutex<Vec<(String, Value)>>);

impl Backend for Recording {
    fn invoke(&self, _: &RequestContext, tool: &str, arguments: Value) -> Result<Value, ToolError> {
        self.0.lock().unwrap().push((tool.to_owned(), arguments));
        Ok(match tool {
            "list_workspaces" => json!({ "workspaces": [
                { "id": "11111111-1111-4111-8111-111111111111" },
                { "id": "22222222-2222-4222-8222-222222222222" },
            ] }),
            _ => json!({}),
        })
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        Ok(json!({}))
    }
}

/// A resolver that answers a fixed script and counts how often it is asked.
struct Scripted {
    answer: Mutex<Box<dyn Fn() -> AccessResolution + Send>>,
    asked: AtomicUsize,
    admitted_with: Mutex<Vec<Option<AccessRevision>>>,
}

impl Scripted {
    fn new(answer: impl Fn() -> AccessResolution + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Box::new(answer)),
            asked: AtomicUsize::new(0),
            admitted_with: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl AccessResolver for Scripted {
    fn resolve(&self, admitted: Option<&AccessRevision>) -> AccessResolution {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.admitted_with.lock().unwrap().push(admitted.copied());
        (self.answer.lock().unwrap())()
    }
}

fn a() -> StoreWorkspaceId {
    StoreWorkspaceId::parse("11111111-1111-4111-8111-111111111111").unwrap()
}

fn b() -> StoreWorkspaceId {
    StoreWorkspaceId::parse("22222222-2222-4222-8222-222222222222").unwrap()
}

const REVISION: AccessRevision = AccessRevision {
    placement_revision: 7,
    membership_epoch: 3,
    authorization_version: 2,
};

fn hosted(admitted: impl IntoIterator<Item = StoreWorkspaceId>) -> RequestContext {
    RequestContext::selected(
        "us",
        "app-connection:test",
        [Scope::ReadDocuments, Scope::EditDocuments],
        admitted,
    )
    .with_capabilities(HostCapabilities::rung1())
}

fn resolved(
    access: impl IntoIterator<Item = StoreWorkspaceId>,
    pending: Vec<PendingWorkspace>,
    placement_revision: u64,
) -> AccessResolution {
    AccessResolution::Resolved(ResolvedAccess {
        access: WorkspaceAccess::selected(access),
        pending,
        revision: AccessRevision {
            placement_revision,
            ..REVISION
        },
    })
}

/// **Hosted lookup after a move** (§7.4): the session was admitted with A;
/// the Workspace moved out of the granted Profile and B moved in. A UUID
/// named in a call is checked against the *current* set, so A is now
/// `permission_denied` and B is served, inside the same long session and
/// with no new tool input. The resolver was told what the call was admitted
/// under.
#[test]
fn a_workspace_moved_out_of_the_grant_is_refused_and_one_moved_in_is_served() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let resolver = Scripted::new(|| resolved([b()], Vec::new(), 8));
    let context = hosted([a()]).with_access_resolver(REVISION, resolver.clone());

    let refused = executor
        .invoke(&context, "list_documents", json!({ "workspace_id": a() }))
        .unwrap_err();
    assert_eq!(refused.code, "permission_denied");
    assert_eq!(refused.details.unwrap()["workspace_id"], json!(a()));
    executor
        .invoke(
            &context,
            "read_document",
            json!({ "workspace_id": b(), "document_id": "x" }),
        )
        .unwrap();
    assert_eq!(
        backend
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(tool, _)| tool.as_str())
            .collect::<Vec<_>>(),
        vec!["read_document"],
        "the moved-out Workspace never reached the backend"
    );
    assert_eq!(
        *resolver.admitted_with.lock().unwrap(),
        vec![Some(REVISION), Some(REVISION)]
    );
}

/// A resolver that answers `Current` leaves the admitted set in force, and
/// a UUID-granted session whose placement revision moves keeps its access:
/// the executor uses whatever set the resolver answers, which for a
/// Workspace grant is the same set.
#[test]
fn a_current_answer_keeps_the_admitted_set_and_a_pinned_grant_survives_a_move() {
    let executor = WorkspaceToolExecutor::new(Arc::new(Recording::default()));
    let current = Scripted::new(|| AccessResolution::Current);
    let context = hosted([a()]).with_access_resolver(REVISION, current.clone());
    executor
        .invoke(&context, "list_documents", json!({ "workspace_id": a() }))
        .unwrap();
    assert!(
        executor
            .invoke(&context, "list_documents", json!({ "workspace_id": b() }))
            .is_err()
    );
    assert_eq!(current.asked(), 2);

    // The same UUID, re-resolved under a newer placement revision.
    let moved = Scripted::new(|| resolved([a()], Vec::new(), 9));
    let pinned = hosted([a()]).with_access_resolver(REVISION, moved);
    executor
        .invoke(&pinned, "list_documents", json!({ "workspace_id": a() }))
        .unwrap();
}

/// **Lag is pending, not fallback** (§7.4): a call naming a Workspace the
/// resolver lists as pending answers `authorization_unavailable` with the
/// retry, before any permission check and before the backend; a listing or
/// a search simply does not include it, and names no other Workspace in
/// its place.
#[test]
fn a_pending_workspace_answers_the_typed_retry_and_is_absent_from_listings() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let pending = PendingWorkspace {
        workspace_id: b(),
        retry_after_seconds: 5,
    };
    let resolver = Scripted::new(move || resolved([a()], vec![pending], 8));
    let context = hosted([a(), b()]).with_access_resolver(REVISION, resolver.clone());

    let retry = executor
        .invoke(&context, "edit_document", json!({ "workspace_id": b() }))
        .unwrap_err();
    assert_eq!(retry.code, AUTHORIZATION_UNAVAILABLE);
    let details = retry.details.unwrap();
    assert_eq!(details["reason"], "placement_pending");
    assert_eq!(details["retry_after_seconds"], 5);
    assert_eq!(details["workspace_id"], json!(b()));
    assert!(retry.message.contains("nothing was changed"));

    let listed = executor
        .invoke(&context, "list_workspaces", json!({}))
        .unwrap();
    assert_eq!(listed["workspaces"], json!([{ "id": a() }]));

    executor
        .invoke(&context, "search_documents", json!({ "query": "x" }))
        .unwrap();
    let calls = backend.0.lock().unwrap();
    let (_, search) = calls
        .iter()
        .find(|(tool, _)| tool == "search_documents")
        .unwrap();
    assert_eq!(search["workspace_ids"], json!([a()]));
    assert!(
        calls.iter().all(|(tool, _)| tool != "edit_document"),
        "the pending call never reached the backend"
    );
    // Once per list and search call, and once for the refused edit.
    assert_eq!(resolver.asked(), 3);
}

/// A revoked or unreadable authority refuses with its own code, before the
/// backend, whatever the admitted set says.
#[test]
fn revoked_and_unavailable_authority_refuse_before_the_backend() {
    let backend = Arc::new(Recording::default());
    let executor = WorkspaceToolExecutor::new(backend.clone());
    let revoked =
        hosted([a()]).with_access_resolver(REVISION, Scripted::new(|| AccessResolution::Revoked));
    let refused = executor
        .invoke(&revoked, "list_documents", json!({ "workspace_id": a() }))
        .unwrap_err();
    assert_eq!(refused.code, AUTHORIZATION_REVOKED);

    let unavailable = hosted([a()]).with_access_resolver(
        REVISION,
        Scripted::new(|| AccessResolution::Unavailable {
            retry_after_seconds: 2,
        }),
    );
    let refused = executor
        .invoke(&unavailable, "list_workspaces", json!({}))
        .unwrap_err();
    assert_eq!(refused.code, AUTHORIZATION_UNAVAILABLE);
    assert_eq!(refused.details.unwrap()["reason"], "authority_unavailable");
    assert!(backend.0.lock().unwrap().is_empty());
}

/// `get_documentation` needs no content scope and never asks the resolver;
/// a local context has no resolver and `All` access, unchanged.
#[test]
fn documentation_skips_the_resolver_and_local_contexts_have_none() {
    let executor = WorkspaceToolExecutor::new(Arc::new(Recording::default()));
    let resolver = Scripted::new(|| AccessResolution::Revoked);
    let context = hosted([a()]).with_access_resolver(REVISION, resolver.clone());
    executor
        .invoke(&context, "get_documentation", json!({}))
        .unwrap();
    assert_eq!(resolver.asked(), 0);

    let local = RequestContext::local();
    assert!(local.access_revision.is_none());
    assert!(local.access_resolver.0.is_none());
    assert!(matches!(local.workspace_access, WorkspaceAccess::All));
    assert_eq!(
        format!("{:?}", local.access_resolver),
        "AccessResolverSeam(none)"
    );
    assert_eq!(
        format!("{:?}", context.access_resolver),
        "AccessResolverSeam(set)"
    );
    let selected = RequestContext::selected("local", "koru-ai", [Scope::ReadDocuments], [a()]);
    assert!(selected.access_resolver.0.is_none());
}

/// **Workspace-only clients see no private unit** (Plan 3.3): the MCP
/// surface is Workspace UUIDs and documents. No published tool — local or
/// hosted — takes or mentions a Profile, a personal unit, a catalog unit or
/// a user scope, in its input schema or its description, and the tool list
/// is unchanged by this plan: no `profile_id` parameter, no organization
/// tool.
#[test]
fn no_tool_input_names_a_profile_personal_unit_or_user_scope() {
    const PRIVATE: [&str; 7] = [
        "profile",
        "personal",
        "catalog",
        "user_scope",
        "placement",
        "root_unit",
        "unit_ref",
    ];
    let mut tools = workspace_tool_catalog();
    tools.extend(HostCapabilities::rung1().catalog());
    assert!(!tools.is_empty());
    for tool in &tools {
        let schema = serde_json::to_string(&tool.input_schema)
            .unwrap()
            .to_lowercase();
        let description = tool.description.to_lowercase();
        for word in PRIVATE {
            assert!(!schema.contains(word), "{}'s input names {word}", tool.name);
            assert!(
                !description.contains(word),
                "{}'s description names {word}",
                tool.name
            );
        }
        assert!(
            !tool.name.contains("profile")
                && !tool.name.contains("organization")
                && !tool.name.contains("move"),
            "{} is an organization tool",
            tool.name
        );
    }
}
