// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_draw::{
    el::{FrameCx, TextCache},
    geometry::Rect,
    inspect,
};
use serde_json::json;
use vello::{Scene, kurbo::Affine};
use winit::keyboard::{Key, NamedKey};

fn view() -> CloudDocumentInspectionV1 {
    serde_json::from_value(json!({
        "selection":{"account_id":"00000000-0000-0000-0000-000000000001","workspace_id":"00000000-0000-0000-0000-000000000002",
            "incarnation_id":"00000000-0000-0000-0000-000000000003","cloud_unit_id":"00000000-0000-0000-0000-000000000004",
            "document_id":"00000000-0000-0000-0000-000000000005","source_head_digest":"a".repeat(64)},
        "document":{"id":"00000000-0000-0000-0000-000000000005","workspace_id":"00000000-0000-0000-0000-000000000002",
            "title":"Stored Cloud title","revision":9007199254740993u64,"blocks":[{
                "id":"00000000-0000-0000-0000-000000000006","parent_id":null,"kind":"canvas","in_trash":true,
                "properties":{"name":"Canvas file","done":false,"alignment":"start"},
                "blob":{"id":"b3:abc","mime_type":"image/png","size":null,"width":null,"height":null},
                "reference":{"kind":"label","value":"Retained target"}
            }]},"can_request_removal":true,"removable_files":[{"node_id":"00000000-0000-0000-0000-000000000006","in_trash":true}]
    })).unwrap()
}
fn ready() -> (Presentation, ViewKey, String) {
    let view = view();
    let key = ViewKey {
        owner_scope: 1,
        account_generation: 7,
        selection: view.selection.clone(),
    };
    let node = view.removable_files[0].node_id.clone();
    let mut state = Presentation::default();
    assert!(state.open(key.clone()));
    assert!(state.install(&key, &view.to_json().unwrap()));
    (state, key, node)
}

#[test]
fn exact_scope_and_source_fence_all_installation_and_confirmation() {
    let (mut state, key, node) = ready();
    state.activate(&key, &Control::Remove(node.clone()));
    assert!(state.confirmation().is_some());
    let mut changed = key.clone();
    changed.owner_scope += 1;
    state.open(changed.clone());
    assert!(state.confirmation().is_none());
    assert!(!state.install(&key, &view().to_json().unwrap()));
    assert!(state.answer(&key, Outcome::Confirm).is_none());
    assert_eq!(state.phase(), Phase::Loading);
    let (mut state, key, node) = ready();
    state.activate(&key, &Control::Remove(node));
    let mut changed = key.clone();
    changed.selection.source_head_digest = "b".repeat(64);
    assert!(state.answer(&changed, Outcome::Confirm).is_none());
    assert_eq!(state.phase(), Phase::Ready);
}

#[test]
fn file_confirmation_is_explicit_then_only_remote_ack_clears_the_copy() {
    let (mut state, key, node) = ready();
    assert!(
        state
            .activate(&key, &Control::Remove(node.clone()))
            .is_none()
    );
    let answer = state
        .confirmation()
        .unwrap()
        .key(&Key::Named(NamedKey::Enter));
    assert_eq!(answer, Outcome::Keep);
    assert!(state.answer(&key, answer).is_none());
    assert_eq!(
        state.answer(&key, Outcome::Confirm),
        Some(Intent::Remove {
            key: key.clone(),
            node: node.clone()
        })
    );
    assert_eq!(state.phase(), Phase::Removing);
    assert!(state.activate(&key, &Control::Remove(node)).is_none());
    assert!(!state.install(&key, &view().to_json().unwrap()));
    assert!(state.acknowledged(&key));
    assert_eq!(state.phase(), Phase::Acknowledged);
    assert!(state.view.is_none());
}

#[test]
fn unknown_outcome_requires_reload_and_read_only_or_note_rows_have_no_controls() {
    let (mut state, key, node) = ready();
    state.failed(&key, CloudDocumentFailure::OutcomeUnknown);
    assert!(state.view.is_none());
    assert!(
        state
            .activate(&key, &Control::Remove(node.clone()))
            .is_none()
    );
    assert_eq!(
        state.activate(&key, &Control::Reload),
        Some(Intent::Reload(key.clone()))
    );
    let mut value = view();
    value.can_request_removal = false;
    value.removable_files.clear();
    state.install(&key, &value.to_json().unwrap());
    assert!(state.activate(&key, &Control::Remove(node)).is_none());
    assert!(state.confirmation().is_none());
    value.can_request_removal = true;
    value.document.blocks[0].kind = "document".into();
    value.removable_files = view().removable_files;
    state.open(key.clone());
    assert!(state.install(&key, &serde_json::to_string(&value).unwrap()));
    assert_eq!(state.phase(), Phase::Unavailable);
}

#[test]
fn passive_composition_uses_one_bounded_semantic_frame_and_existing_buttons() {
    let (state, _, _) = ready();
    let mut engine = TextEngine::new();
    let composition = state.compose(&mut engine);
    let frame = composition.tree.layout(
        &mut engine,
        &mut TextCache::default(),
        Rect::new(0.0, 0.0, 380.0, 1200.0),
        FrameCx::root(),
    );
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    for (_, key) in &composition.controls {
        assert!(frame.node(*key).is_some());
    }
    let labels: Vec<_> = frame
        .nodes
        .iter()
        .filter_map(|n| match &n.label {
            Some(el::Name::Text(value)) => Some(value.as_str()),
            Some(el::Name::Key(key)) => Some(t(*key)),
            None => None,
        })
        .collect();
    assert!(labels.contains(&"Canvas file"));
    assert!(labels.contains(&"Retained target"));
    assert!(labels.contains(&t(TextKey::CloudInventoryTrash)));
    assert!(labels.contains(&t(TextKey::CloudCopyPreviewUnavailable)));
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, 380.0, 1200.0),
    );
    frame.paint(
        &composition.tree,
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut engine,
    );
    let snapshot = capture.finish();
    assert!(snapshot.records.iter().enumerate().any(|(index, record)| {
        if record.name != "KoruButton" {
            return false;
        }
        let mut parent = snapshot.records[index].parent;
        while let Some(index) = parent {
            if snapshot.records[index].name == "Cloud copy page" {
                return true;
            }
            parent = snapshot.records[index].parent;
        }
        false
    }));
}

#[test]
fn passive_inline_labels_preserve_unicode_and_have_no_navigation_targets() {
    let rich: RichText = serde_json::from_value(json!({"text":"é\u{fffc}","marks":[],"inline_objects":[{
        "id":"00000000-0000-0000-0000-000000000009","anchor_utf8":2,"role":"link","label":"Stored label",
        "reference":{"kind":"url","url":"https://example.test"}
    }]})).unwrap();
    assert_eq!(passive_text(&rich), "éStored label");
}
