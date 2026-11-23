// SPDX-License-Identifier: MPL-2.0
use super::*;

fn fixture() -> Declaration {
    Declaration::parse(composition::FIXTURE.as_bytes()).unwrap()
}

#[test]
fn saved_v2_separates_portable_slots_from_ids_and_reopens_with_fresh_lifetime() {
    let workspace = WorkspaceId::new();
    let original = fixture();
    let saved = SavedEnvelope::capture(workspace, &original).unwrap();
    let bytes = saved.encode().unwrap();
    assert!(bytes.len() <= MAX_FILE_BYTES);
    assert_eq!(SavedEnvelope::decode(&bytes).unwrap(), saved);
    assert_eq!(saved.document_references().len(), 4);
    let SavedEnvelope::Composition(mut rebound) = saved.clone() else {
        panic!()
    };
    let definition = serde_json::to_string(&rebound.definition).unwrap();
    for document in original.document_references() {
        assert!(!definition.contains(&document.as_uuid().to_string()));
    }
    for forbidden in ["surfaceId", "renderer", "request", "revision", "task_list"] {
        assert!(!definition.contains(forbidden));
    }
    rebound.bindings.workspace = WorkspaceId::new();
    for binding in &mut rebound.bindings.documents {
        binding.document = DocumentId::new();
    }
    let rebound = SavedEnvelope::Composition(rebound);
    let live = rebound
        .instantiate(rebound.workspace(), "fresh-runtime".into())
        .unwrap();
    assert!(rebound.matches_declaration(rebound.workspace(), &live));
    assert_ne!(live.document_references(), original.document_references());
    assert!(
        saved
            .instantiate(rebound.workspace(), "wrong".into())
            .is_err()
    );
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("travel-workspace")
    );
}

#[test]
fn canonical_save_ignores_flat_array_order_but_preserves_child_order() {
    let workspace = WorkspaceId::new();
    let original = fixture();
    let captured = SavedEnvelope::capture(workspace, &original).unwrap();
    let mut reordered = original.envelope.clone();
    reordered.surface.components.reverse();
    let reordered = Declaration::validate(reordered).unwrap();
    assert_eq!(
        captured,
        SavedEnvelope::capture(workspace, &reordered).unwrap()
    );
    let mut rearranged = original.envelope;
    rearranged.surface.components[3]
        .children
        .as_mut()
        .unwrap()
        .reverse();
    assert_ne!(
        captured,
        SavedEnvelope::capture(workspace, &Declaration::validate(rearranged).unwrap()).unwrap()
    );
}

#[test]
fn slot_identity_survives_rebinding_with_reversed_document_uuid_order() {
    let saved = SavedEnvelope::capture(WorkspaceId::new(), &fixture()).unwrap();
    let SavedEnvelope::Composition(mut rebound) = saved else {
        panic!()
    };
    let original_definition = rebound.definition.clone();
    for (index, binding) in rebound.bindings.documents.iter_mut().enumerate() {
        binding.document = DocumentId::from_uuid(uuid::Uuid::from_u128(100 - index as u128));
    }
    let rebound = SavedEnvelope::Composition(rebound);
    let reopened = rebound
        .instantiate(rebound.workspace(), "new-install".into())
        .unwrap();
    let recaptured = SavedEnvelope::capture(rebound.workspace(), &reopened).unwrap();
    let SavedEnvelope::Composition(recaptured) = recaptured else {
        panic!()
    };
    assert_eq!(recaptured.definition, original_definition);
    assert!(rebound.matches_declaration(rebound.workspace(), &reopened));
}

#[test]
fn near_live_limit_is_savable_with_long_normalized_surface_identity() {
    let mut value = serde_json::to_value(fixture().envelope).unwrap();
    value["createSurface"]["surfaceId"] = format!("mini-app-{}", uuid::Uuid::new_v4()).into();
    // Compact live JSON stays below its 4096-byte ingress bound; v2's
    // explicit portable kinds/default metadata is allowed to expand it.
    for i in 0..12 {
        let id = format!("description_{i}");
        value["createSurface"]["components"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"id":id,"component":"Text","text":"Planning details"}));
        value["createSurface"]["components"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(id.into());
    }
    loop {
        let length = mini_apps::bounded_json(&value).unwrap().len();
        if length >= mini_apps::MAX_BYTES - 80 {
            break;
        }
        let components = value["createSurface"]["components"].as_array_mut().unwrap();
        let target = components
            .iter_mut()
            .skip(10)
            .find(|n| n["text"].as_str().unwrap().len() < composition::MAX_TEXT_BYTES)
            .unwrap();
        let text = target["text"].as_str().unwrap().to_owned();
        target["text"] = format!("{text}x").into();
    }
    let live = Declaration::from_value(&value).unwrap();
    let workspace = WorkspaceId::new();
    let saved = SavedEnvelope::capture(workspace, &live).unwrap();
    let bytes = saved.encode().unwrap();
    assert!(bytes.len() > MAX_BYTES);
    assert!(bytes.len() < MAX_FILE_BYTES);
    let reopened = SavedEnvelope::decode(&bytes)
        .unwrap()
        .instantiate(workspace, format!("mini-app-{}", uuid::Uuid::new_v4()))
        .unwrap();
    assert!(saved.matches_declaration(workspace, &reopened));
}

#[test]
fn version_one_still_has_identical_bytes_through_new_codec() {
    let workspace = WorkspaceId::new();
    let original = Declaration::parse(mini_apps::FIXTURE.as_bytes()).unwrap();
    let legacy = Envelope::capture(workspace, &original).unwrap();
    let bytes = legacy.encode().unwrap();
    let decoded = SavedEnvelope::decode(&bytes).unwrap();
    assert_eq!(decoded, SavedEnvelope::TaskDocument(legacy));
    assert_eq!(decoded.encode().unwrap(), bytes);
    assert_eq!(
        SavedEnvelope::capture(workspace, &original)
            .unwrap()
            .encode()
            .unwrap(),
        bytes
    );
}

#[test]
fn v2_rejects_missing_extra_duplicate_nil_and_unknown_bindings_or_shapes() {
    let saved = SavedEnvelope::capture(WorkspaceId::new(), &fixture()).unwrap();
    let SavedEnvelope::Composition(value) = saved else {
        panic!()
    };
    for bad in [
        {
            let mut v = value.clone();
            v.bindings.documents.pop();
            v
        },
        {
            let mut v = value.clone();
            let mut b = v.bindings.documents[0].clone();
            b.parameter = "extra".into();
            v.bindings.documents.push(b);
            v
        },
        {
            let mut v = value.clone();
            v.bindings.documents.push(v.bindings.documents[0].clone());
            v
        },
        {
            let mut v = value.clone();
            v.bindings.documents[0].document = DocumentId::from_uuid(uuid::Uuid::nil());
            v
        },
        {
            let mut v = value.clone();
            v.format_version = 3;
            v
        },
        {
            let mut v = value.clone();
            v.definition.nodes[0].id = "not-root".into();
            v
        },
    ] {
        assert!(SavedEnvelope::Composition(bad).encode().is_err());
    }
    let mut json = serde_json::to_value(&value).unwrap();
    json["definition"]["document_text"] = "forbidden".into();
    assert!(SavedEnvelope::decode(&serde_json::to_vec(&json).unwrap()).is_err());
}
