// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{ChatId, InferenceAttemptId, ProfileId, ToolInvocationId, WorkspaceId};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Fake {
    reads: AtomicUsize,
    profile: ProfileId,
    workspace: WorkspaceId,
    list: uuid::Uuid,
    document: DocumentId,
}
impl MiniAppRegistry for Fake {
    fn read(&self, c: &AiToolExecutionContext, _: bool, _: bool) -> Result<Registry, AiError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if c.profile_id != Some(self.profile)
            || c.mini_app.as_ref().unwrap().host.workspace_id != self.workspace
        {
            return Err(unavailable());
        }
        Ok(Registry {
            lists: (0..25)
                .map(|n| {
                    (
                        if n == 0 {
                            self.list
                        } else {
                            uuid::Uuid::new_v4()
                        },
                        format!("Empty list {n}"),
                    )
                })
                .collect(),
            documents: vec![self.document],
        })
    }
}
fn fixture() -> (MiniAppTools, Arc<Fake>, AiToolExecutionContext, AiToolCall) {
    let registry = Arc::new(Fake {
        reads: AtomicUsize::new(0),
        profile: ProfileId::new(),
        workspace: WorkspaceId::new(),
        list: uuid::Uuid::new_v4(),
        document: DocumentId::new(),
    });
    let host = mini_apps::HostContext {
        renderer_id: uuid::Uuid::new_v4(),
        request_id: uuid::Uuid::new_v4(),
        workspace_id: registry.workspace,
    };
    let context = AiToolExecutionContext {
        chat_id: ChatId::new(),
        attempt_id: InferenceAttemptId::new(),
        invocation_id: ToolInvocationId::new(),
        profile_id: Some(registry.profile),
        workspace_ids: vec![registry.workspace],
        mini_app: Some(mini_apps::Authority::new(host)),
    };
    let call = AiToolCall {
        call_id: "call-1".into(),
        name: mini_apps::CREATE_TOOL.into(),
        arguments: serde_json::from_str(mini_apps::FIXTURE).unwrap(),
        thought_signatures: Vec::new(),
    };
    (MiniAppTools::new(registry.clone()), registry, context, call)
}
#[test]
fn mini_app_bounds_and_scope_are_checked_before_registry_access() {
    let (tools, registry, mut context, mut call) = fixture();
    call.arguments["content"] = "x".repeat(5000).into();
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    assert_eq!(registry.reads.load(Ordering::Relaxed), 0);
    call.arguments = serde_json::from_str(mini_apps::FIXTURE).unwrap();
    context.workspace_ids.clear();
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    assert_eq!(registry.reads.load(Ordering::Relaxed), 0);
    context.workspace_ids.push(registry.workspace);
    context.profile_id = Some(ProfileId::new());
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
}
#[test]
fn mini_app_named_empty_lists_are_explicit_bounded_metadata() {
    let (tools, _, context, mut call) = fixture();
    call.name = mini_apps::LIST_TOOL.into();
    call.arguments = json!({"query":"Empty","limit":20});
    let result = tools
        .invoke(&context, &call, &Cancellation::default())
        .unwrap();
    assert_eq!(result.content["lists"].as_array().unwrap().len(), 20);
    assert_eq!(result.content["next_offset"], 20);
    assert_eq!(result.content["lists"][0].as_object().unwrap().len(), 2);
    call.arguments = json!({"query":"Empty","offset":20,"limit":20});
    assert_eq!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .unwrap()
            .content["lists"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    for invalid in [
        json!({"query":"x".repeat(129)}),
        json!({"query":"","limit":21}),
        json!({"query":"","offset":1000001}),
    ] {
        call.arguments = invalid;
        assert!(
            tools
                .invoke(&context, &call, &Cancellation::default())
                .is_err()
        );
    }
}
#[test]
fn mini_app_references_and_one_success_use_the_registered_validator() {
    let (tools, registry, context, mut call) = fixture();
    call.arguments["createSurface"]["components"][4]["document"] =
        json!({"workspace":"current","documentId":DocumentId::new()});
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    call.arguments["createSurface"]["components"][4]["document"]["documentId"] =
        json!(registry.document);
    call.arguments["createSurface"]["components"][2]
        .as_object_mut()
        .unwrap()
        .remove("bucket");
    call.arguments["createSurface"]["components"][2]["list"] =
        json!({"workspace":"current","listId":registry.list});
    let result = tools
        .invoke(&context, &call, &Cancellation::default())
        .unwrap();
    let event = creation_event(&context, &result).unwrap();
    assert!(matches!(
        event,
        koru_ai_wire::PublicAiEvent::MiniAppCreated { .. }
    ));
    assert!(creation_event(&context, &result).is_err());
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
}
