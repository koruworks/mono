// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn mini_app_refinement_requires_exact_live_target_and_has_one_delivery() {
    let (tools, registry, mut context, mut call) = fixture();
    let mut host = context.mini_app.as_ref().unwrap().host;
    host.refinement = Some(mini_apps::RefinementTarget {
        tab_id: 17,
        surface_id: uuid::Uuid::new_v4(),
        base_revision: 8,
    });
    let target = host.refinement.unwrap();
    let mut declaration = Declaration::parse(mini_apps::FIXTURE.as_bytes())
        .unwrap()
        .envelope;
    declaration.surface.surface_id = target.surface_name();
    context.mini_app = Some(
        mini_apps::Authority::with_refinement(
            host,
            Some(mini_apps::RefinementContext {
                target,
                declaration,
            }),
        )
        .unwrap(),
    );
    let mut root = Declaration::parse(mini_apps::FIXTURE.as_bytes())
        .unwrap()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some("document-first".into());
    call.name = mini_apps::UPDATE_TOOL.into();
    call.arguments = json!({"version":"1.0","updateComponents":{"surfaceId":host.refinement.unwrap().surface_name(),"components":[root]}});
    let mut wrong = call.clone();
    wrong.arguments["updateComponents"]["surfaceId"] = "wrong-surface".into();
    assert!(
        tools
            .invoke(&context, &wrong, &Cancellation::default())
            .is_err()
    );
    let result = tools
        .invoke(&context, &call, &Cancellation::default())
        .unwrap();
    assert_eq!(result.content, call.arguments);
    assert!(
        matches!(creation_event(&context, &result).unwrap(), koru_ai_wire::PublicAiEvent::MiniAppUpdated {host: event_host, ..} if event_host == host)
    );
    assert!(creation_event(&context, &result).is_err());
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    assert!(registry.reads.load(std::sync::atomic::Ordering::Relaxed) > 0);
    host.refinement = None;
    context.mini_app = Some(mini_apps::Authority::new(host));
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
}
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
        refinement: None,
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

struct CompositionMetadata {
    reads: AtomicUsize,
    documents: Vec<DocumentId>,
}
impl MiniAppRegistry for CompositionMetadata {
    fn read(
        &self,
        _: &AiToolExecutionContext,
        lists: bool,
        documents: bool,
    ) -> Result<Registry, AiError> {
        assert!(!lists);
        assert!(documents);
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(Registry {
            lists: vec![],
            documents: self.documents.clone(),
        })
    }
}

#[test]
fn composition_creation_validates_action_only_documents_in_one_metadata_read() {
    let declaration = Declaration::parse(mini_apps::composition::FIXTURE.as_bytes()).unwrap();
    let documents = declaration.document_references();
    assert_eq!(documents.len(), 4);
    for missing_action_document in [false, true] {
        let (_, _, context, mut call) = fixture();
        let mut allowed = documents.clone();
        if missing_action_document {
            allowed.pop();
        }
        let metadata = Arc::new(CompositionMetadata {
            reads: AtomicUsize::new(0),
            documents: allowed,
        });
        let tools = MiniAppTools::new(metadata.clone());
        call.arguments = serde_json::to_value(&declaration.envelope).unwrap();
        let result = tools.invoke(&context, &call, &Cancellation::default());
        assert_eq!(result.is_err(), missing_action_document);
        assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
        if let Ok(result) = result {
            assert!(matches!(
                creation_event(&context, &result).unwrap(),
                koru_ai_wire::PublicAiEvent::MiniAppCreated { .. }
            ));
        }
    }
}

#[test]
fn composition_refinement_checks_snapshot_before_io_and_rechecks_before_delivery() {
    let (_, _, mut context, mut call) = fixture();
    let mut host = context.mini_app.as_ref().unwrap().host;
    let target = mini_apps::RefinementTarget {
        tab_id: 5,
        surface_id: uuid::Uuid::new_v4(),
        base_revision: 9,
    };
    host.refinement = Some(target);
    let mut declaration = Declaration::parse(mini_apps::composition::FIXTURE.as_bytes())
        .unwrap()
        .envelope;
    declaration.surface.surface_id = target.surface_name();
    let snapshot = mini_apps::RefinementContext {
        target,
        declaration: declaration.clone(),
    };
    let documents = snapshot.validate().unwrap().document_references();
    let metadata = Arc::new(CompositionMetadata {
        reads: AtomicUsize::new(0),
        documents: documents.clone(),
    });
    let tools = MiniAppTools::new(metadata.clone());
    context.mini_app =
        Some(mini_apps::Authority::with_refinement(host, Some(snapshot.clone())).unwrap());
    let mut components = declaration.surface.components;
    components
        .iter_mut()
        .find(|c| c.id == "root")
        .unwrap()
        .component = "Row".into();
    call.name = mini_apps::UPDATE_TOOL.into();
    call.arguments = json!({"version":"1.0","updateComponents":{"surfaceId":target.surface_name(),"components":components}});
    let mut redirect = call.clone();
    redirect.arguments["updateComponents"]["components"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["component"] == "Button")
        .unwrap()["action"]["document"]["documentId"] = json!(DocumentId::new());
    assert!(
        tools
            .invoke(&context, &redirect, &Cancellation::default())
            .is_err()
    );
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 0);
    let result = tools
        .invoke(&context, &call, &Cancellation::default())
        .unwrap();
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
    let mut tampered = result.clone();
    tampered.content = redirect.arguments;
    assert!(creation_event(&context, &tampered).is_err());
    assert!(matches!(
        creation_event(&context, &result).unwrap(),
        koru_ai_wire::PublicAiEvent::MiniAppUpdated { .. }
    ));

    let mut removed = documents;
    removed.pop();
    let metadata = Arc::new(CompositionMetadata {
        reads: AtomicUsize::new(0),
        documents: removed,
    });
    let tools = MiniAppTools::new(metadata.clone());
    context.mini_app = Some(mini_apps::Authority::with_refinement(host, Some(snapshot)).unwrap());
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
    context.mini_app = Some(mini_apps::Authority::new(host));
    assert!(
        tools
            .invoke(&context, &call, &Cancellation::default())
            .is_err()
    );
    assert_eq!(metadata.reads.load(Ordering::Relaxed), 1);
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
