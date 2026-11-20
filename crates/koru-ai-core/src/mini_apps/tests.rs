// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn declaration_decode_failures_are_closed_and_do_not_echo_private_fields() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let private = Capability::mint().0;
    let mut missing = fixture.clone();
    missing.as_object_mut().unwrap().remove("version");
    let mut unknown = fixture.clone();
    unknown[private.clone()] = private.clone().into();
    let mut wrong_type = fixture.clone();
    wrong_type["version"] = false.into();
    let mut bad_list = fixture.clone();
    bad_list["createSurface"]["components"][2]
        .as_object_mut()
        .unwrap()
        .remove("bucket");
    bad_list["createSurface"]["components"][2]["list"] =
        json!({"workspace":"current","listId":private});
    let mut bad_document = fixture;
    bad_document["createSurface"]["components"][4]["document"] =
        json!({"workspace":"current","documentId":private});
    for (value, expected) in [
        (missing, "missing declaration field"),
        (unknown, "unknown declaration field"),
        (wrong_type, "invalid declaration type"),
        (bad_list, "invalid declaration value"),
        (bad_document, "invalid declaration value"),
    ] {
        let reason = Declaration::from_value(&value).unwrap_err();
        assert_eq!(reason, expected);
        assert!(!reason.contains(&private));
    }
    let duplicate = FIXTURE.replacen(
        "\"version\": \"1.0\"",
        "\"version\": \"1.0\", \"version\": \"1.0\"",
        1,
    );
    assert_eq!(
        Declaration::parse(duplicate.as_bytes()).unwrap_err(),
        "duplicate declaration field"
    );
    assert_eq!(
        Declaration::parse(b"{").unwrap_err(),
        "malformed declaration JSON"
    );
    let unknown = serde_json::Error::io(std::io::Error::other(private));
    assert_eq!(
        declaration_decode_reason(&unknown),
        "unsupported declaration"
    );
}

#[test]
fn mini_app_capability_is_runtime_only_redacted_and_keys_cannot_echo_it() {
    let capability = Capability::mint();
    let context = RuntimeContext {
        capability: capability.clone(),
        workspace_id: WorkspaceId::new(),
        refinement_surface: None,
        refinement: None,
    };
    let options = crate::AiRequestOptions {
        mini_app: Some(context),
        ..Default::default()
    };
    assert!(!format!("{options:?}").contains(&capability.0));
    assert!(
        !serde_json::to_string(&options)
            .unwrap()
            .contains(&capability.0)
    );
    assert!(
        serde_json::to_value(&options)
            .unwrap()
            .get("mini_app")
            .is_none()
    );
    let event = crate::AiEvent::ToolCalls {
        attempt_id: crate::InferenceAttemptId::new(),
        calls: vec![crate::AiToolCall {
            call_id: capability.0.clone(),
            name: "ordinary".into(),
            arguments: json!({capability.0.clone():capability.0.clone()}),
            thought_signatures: vec![],
        }],
        stop_reason: None,
        at: crate::Timestamp::now(),
    };
    let output = serde_json::to_string(&sanitize_agent_event(event)).unwrap();
    assert!(!output.contains(&capability.0));
    assert!(output.contains("redacted mini-app permission"));
    assert!(Capability::in_text(&format!("old: {}", capability.0)));
    assert!(!Capability::in_text("koru-mini-app documentation"));
}

#[test]
fn refinement_accepts_only_a_complete_root_replacement_without_upstream_revision() {
    let mut root = Declaration::parse(FIXTURE.as_bytes())
        .unwrap()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some("document-first".into());
    let valid = json!({"version":"1.0","updateComponents":{"surfaceId":"mini-app-fixture","components":[root]}});
    assert!(
        UpdateEnvelope::from_value(&valid)
            .unwrap()
            .arrangement()
            .unwrap()
    );
    for value in [
        json!({"version":"1.0","baseRevision":1,"updateComponents":valid["updateComponents"]}),
        json!({"version":"1.0","updateComponents":{"surfaceId":"mini-app-fixture","components":[]}}),
        json!({"version":"1.0","updateComponents":{"surfaceId":"mini-app-fixture","components":[{"id":"document","component":"DocumentHost"}]}}),
        json!({"version":"1.0","updateComponents":{"surfaceId":"mini-app-fixture","components":[{"id":"root","component":"TaskDocument","children":["tasks"],"arrangement":"document-first"}]}}),
        json!({"version":"1.0","deleteSurface":{"surfaceId":"mini-app-fixture"}}),
    ] {
        assert!(UpdateEnvelope::from_value(&value).is_err());
    }
    let mut too_large = valid;
    too_large["updateComponents"]["surfaceId"] = "x".repeat(MAX_BYTES + 1).into();
    assert!(UpdateEnvelope::from_value(&too_large).is_err());
}
#[test]
fn strict_catalog_accepts_order_independent_reference_only_components() {
    let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
    value["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(
        Declaration::from_value(&value).unwrap().swapped(),
        Some(false)
    );
    let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
    value["createSurface"]["components"][0]["arrangement"] = "document-first".into();
    assert_eq!(
        Declaration::from_value(&value).unwrap().swapped(),
        Some(true)
    );
    assert!(Declaration::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    for path in ["dataModel", "metadata", "updateComponents"] {
        let mut invalid = value.clone();
        invalid["createSurface"][path] = json!({});
        assert!(Declaration::from_value(&invalid).is_err());
    }
    for field in ["text", "rows", "title", "style", "@catalogId", "template"] {
        let mut invalid = value.clone();
        invalid["createSurface"]["components"][2][field] = "content".into();
        assert!(Declaration::from_value(&invalid).is_err());
    }
    value["createSurface"]["components"][4]["id"] = "tasks".into();
    assert!(Declaration::from_value(&value).is_err());
}
#[test]
fn authority_allows_one_success_and_revokes_in_flight_jobs() {
    let host = HostContext {
        renderer_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        workspace_id: WorkspaceId::new(),
        refinement: None,
    };
    let authority = Authority::new(host);
    assert!(authority.begin());
    assert!(!authority.begin());
    assert!(authority.finish(false));
    assert!(authority.begin());
    assert!(authority.finish(true));
    assert!(!authority.begin());
    let other = Authority::new(host);
    assert!(other.begin());
    other.clone().revoke();
    assert!(!other.finish(true));
    assert!(!other.live());
}

#[test]
fn refinement_authority_requires_the_exact_snapshot_and_redacts_debug() {
    let target = RefinementTarget {
        tab_id: 8,
        surface_id: Uuid::new_v4(),
        base_revision: 12,
    };
    let mut declaration = Declaration::parse(composition::FIXTURE.as_bytes())
        .unwrap()
        .envelope;
    declaration.surface.surface_id = target.surface_name();
    declaration
        .surface
        .components
        .iter_mut()
        .find(|c| c.component == "Button")
        .unwrap()
        .text = Some("PRIVATE_UI_LABEL".into());
    let context = RefinementContext {
        target,
        declaration,
    };
    let host = HostContext {
        renderer_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        workspace_id: WorkspaceId::new(),
        refinement: Some(target),
    };
    let authority = Authority::with_refinement(host, Some(context.clone())).unwrap();
    assert!(!format!("{authority:?}").contains("PRIVATE_UI_LABEL"));
    let runtime = RuntimeContext {
        capability: Capability::mint(),
        workspace_id: host.workspace_id,
        refinement_surface: Some(target.surface_name()),
        refinement: Some(context.clone()),
    };
    let options = crate::AiRequestOptions {
        mini_app: Some(runtime),
        ..Default::default()
    };
    assert!(!format!("{options:?}").contains("PRIVATE_UI_LABEL"));
    assert!(
        !serde_json::to_string(&options)
            .unwrap()
            .contains("PRIVATE_UI_LABEL")
    );
    let instructions = context.model_instructions().unwrap();
    assert!(instructions.contains("PRIVATE_UI_LABEL"));
    assert!(!instructions.contains("base_revision"));
    assert!(!instructions.contains("tab_id"));
    let wrong = HostContext {
        refinement: Some(RefinementTarget {
            base_revision: 13,
            ..target
        }),
        ..host
    };
    assert!(Authority::with_refinement(wrong, Some(context.clone())).is_err());
    assert!(
        Authority::with_refinement(
            HostContext {
                refinement: None,
                ..host
            },
            Some(context)
        )
        .is_err()
    );
    assert!(Authority::with_refinement(host, None).is_ok());
}
