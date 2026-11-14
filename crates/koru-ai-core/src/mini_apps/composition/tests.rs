// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mini_apps::{Declaration, RefinementContext, RefinementTarget, UpdateEnvelope};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

#[test]
fn reference_is_bounded_typed_and_includes_all_unique_action_documents() {
    assert!(FIXTURE.len() <= crate::mini_apps::MAX_BYTES);
    let declaration = Declaration::parse(FIXTURE.as_bytes()).unwrap();
    assert!(declaration.task_selection().is_none());
    assert_eq!(declaration.swapped(), None);
    let tree = declaration.composition().unwrap();
    assert_eq!(tree.document_host(), "document");
    assert_eq!(tree.nodes().len(), 10);
    assert_eq!(declaration.document_references().len(), 4);
    assert!(
        declaration
            .document_references()
            .contains(&declaration.initial_document().unwrap())
    );
    let mut reordered = fixture();
    reordered["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert!(Declaration::from_value(&reordered).is_ok());
}

#[test]
fn tiny_surface_identity_must_leave_room_for_normalized_delivery() {
    let mut value = fixture();
    value["createSurface"]["surfaceId"] = "x".into();
    for i in 0..12 {
        let id = format!("description_{i}");
        value["createSurface"]["components"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":id,"component":"Text","text":"Planning details"}));
        value["createSurface"]["components"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(id.into());
    }
    while crate::mini_apps::bounded_json(&value).unwrap().len() < crate::mini_apps::MAX_BYTES - 10 {
        let target = value["createSurface"]["components"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .skip(10)
            .find(|n| n["text"].as_str().unwrap().len() < MAX_TEXT_BYTES)
            .unwrap();
        let text = target["text"].as_str().unwrap().to_owned();
        target["text"] = format!("{text}x").into();
    }
    assert!(crate::mini_apps::bounded_json(&value).is_ok());
    assert_eq!(
        Declaration::from_value(&value).unwrap_err(),
        "surface exceeds byte limit"
    );
    let text = value["createSurface"]["components"][10]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    value["createSurface"]["components"][10]["text"] = text[..text.len() - 44].into();
    let accepted = Declaration::from_value(&value).unwrap();
    let normalized = accepted.normalize(crate::ToolInvocationId::new());
    assert!(Declaration::validate(normalized.envelope).is_ok());
}

#[test]
fn graph_identity_ownership_targets_and_properties_are_closed() {
    let original = fixture();
    for bad in [
        {
            let mut v = original.clone();
            v["createSurface"]["components"][1]["id"] = "root".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][0]["children"][0] = "root".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][0]["children"][0] = "navigation".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][0]["children"][0] = "missing".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][4]["action"]["target"] = "heading".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][4]["action"]["document"]["workspace"] = "other".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][4]["action"]["document"]["documentId"] =
                uuid::Uuid::nil().to_string().into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][4]["action"]["type"] = "Execute".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][1]["bucket"] = "today".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][4]["layout"] = json!({"gap":"S3"});
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][0]["layout"]["gap"] = 12.into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][0]["layout"]["gap"] = "S9".into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][3]["layout"]["wrap"] = true.into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["components"][1]["text"] = "x".repeat(MAX_TEXT_BYTES + 1).into();
            v
        },
        {
            let mut v = original.clone();
            v["createSurface"]["sendDataModel"] = true.into();
            v
        },
    ] {
        assert!(Declaration::from_value(&bad).is_err(), "accepted: {bad}");
    }
    let mut disconnected = original.clone();
    disconnected["createSurface"]["components"][0]["children"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    assert!(Declaration::from_value(&disconnected).is_err());
    let mut two_hosts = original;
    let mut extra = two_hosts["createSurface"]["components"][9].clone();
    extra["id"] = "second-editor".into();
    two_hosts["createSurface"]["components"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    two_hosts["createSurface"]["components"][0]["children"]
        .as_array_mut()
        .unwrap()
        .push("second-editor".into());
    assert!(Declaration::from_value(&two_hosts).is_err());
}

#[test]
fn depth_and_node_count_are_checked_independently_of_byte_bound() {
    let host = fixture()["createSurface"]["components"][9].clone();
    let mut v = fixture();
    let mut nodes = vec![host.clone()];
    for i in (0..MAX_DEPTH).rev() {
        nodes.push(json!({"id": if i == 0 { "root".to_owned() } else { format!("c{i}") }, "component":"Column", "children":[if i + 1 == MAX_DEPTH { "document".to_owned() } else { format!("c{}", i + 1) }]}));
    }
    v["createSurface"]["components"] = nodes.into();
    assert!(crate::mini_apps::bounded_json(&v).is_ok());
    assert!(Declaration::from_value(&v).is_err());
    let mut nodes = vec![host];
    let mut children = vec!["document".to_owned()];
    for i in 0..MAX_NODES - 1 {
        let id = format!("s{i}");
        children.push(id.clone());
        nodes.push(json!({"id":id,"component":"Spacer","space":"S1"}));
    }
    nodes.push(json!({"id":"root","component":"Column","children":children}));
    v["createSurface"]["components"] = nodes.into();
    assert!(crate::mini_apps::bounded_json(&v).is_ok());
    assert!(Declaration::from_value(&v).is_err());
}

#[test]
fn refinement_preserves_native_ids_actions_bindings_and_exact_surface() {
    let current = Declaration::parse(FIXTURE.as_bytes()).unwrap();
    let mut update = UpdateEnvelope {
        version: "1.0".into(),
        update: crate::mini_apps::ComponentUpdate {
            surface_id: current.envelope.surface.surface_id.clone(),
            components: current.envelope.surface.components.clone(),
        },
    };
    update.update.components[0].layout.as_mut().unwrap().gap = Some(Space::S3);
    update.update.components[3].component = "Column".into();
    update.update.components[3]
        .children
        .as_mut()
        .unwrap()
        .reverse();
    let refined = update.refine(&current).unwrap();
    assert_ne!(refined, current);
    assert_eq!(refined.document_references().len(), 4);
    let mut invalid = update.clone();
    if let Some(Action::SelectDocument { document, .. }) = &mut invalid.update.components[4].action
    {
        document.document_id = crate::DocumentId::new();
    }
    assert!(invalid.refine(&current).is_err());
    update.update.surface_id = "another-surface".into();
    assert!(update.refine(&current).is_err());
}

#[test]
fn refinement_context_requires_exact_surface_and_validated_tree() {
    let target = RefinementTarget {
        tab_id: 1,
        surface_id: uuid::Uuid::new_v4(),
        base_revision: 0,
    };
    let mut context = RefinementContext {
        target,
        declaration: Declaration::parse(FIXTURE.as_bytes()).unwrap().envelope,
    };
    assert!(context.validate().is_err());
    context.declaration.surface.surface_id = target.surface_name();
    assert!(context.validate().is_ok());
    context.declaration.surface.components[0].children = None;
    assert!(context.validate().is_err());
}
