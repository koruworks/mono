// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::json;

fn fixture() -> CloudDocumentInspectionV1 {
    serde_json::from_value(json!({
        "selection": {
            "account_id":"00000000-0000-0000-0000-000000000001",
            "workspace_id":"00000000-0000-0000-0000-000000000002",
            "incarnation_id":"00000000-0000-0000-0000-000000000003",
            "cloud_unit_id":"00000000-0000-0000-0000-000000000004",
            "document_id":"00000000-0000-0000-0000-000000000005",
            "source_head_digest":"ab".repeat(32)
        },
        "document": {
            "id":"00000000-0000-0000-0000-000000000005",
            "workspace_id":"00000000-0000-0000-0000-000000000002",
            "title":"Cloud copy", "revision":1,
            "blocks":[{
                "id":"00000000-0000-0000-0000-000000000006", "kind":"image",
                "parent_id":null, "in_trash":false, "properties":{"done":false,"alignment":"start"},
                "blob":{"id":format!("b3:{}","a".repeat(64)),"mime_type":"image/png","size":null,"width":null,"height":null}
            }]
        },
        "can_request_removal":true,
        "removable_files":[{"node_id":"00000000-0000-0000-0000-000000000006","in_trash":false}]
    })).unwrap()
}
#[test]
fn inspection_preserves_exact_selection_and_never_turns_unknown_size_into_zero() {
    let value = fixture();
    let json = value.to_json().unwrap();
    let back = CloudDocumentInspectionV1::from_json(&json).unwrap();
    assert_eq!(back.selection, value.selection);
    assert_eq!(back.document.blocks[0].blob.as_ref().unwrap().size, None);
    assert!(!format!("{back:?}").contains("Cloud copy"));
    assert!(!format!("{:?}", back.selection).contains("00000000"));
}
#[test]
fn inspection_refuses_scope_hierarchy_and_nonleaf_removal_targets() {
    let mut value = fixture();
    value.document.workspace_id = value.selection.account_id.clone();
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.blocks[0].parent_id = Some(value.document.blocks[0].id.clone());
    assert!(value.to_json().is_err());
    let mut value = fixture();
    let mut child = value.document.blocks[0].clone();
    child.id = "00000000-0000-0000-0000-000000000007".into();
    child.parent_id = Some(value.document.blocks[0].id.clone());
    value.document.blocks.push(child);
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.blocks[0].kind = "document".into();
    assert!(value.to_json().is_err());
}
#[test]
fn inspection_enforces_whole_payload_text_block_and_mark_budgets() {
    assert!(CloudDocumentInspectionV1::from_json(&" ".repeat(MAX_INSPECTION_BYTES + 1)).is_err());
    let mut value = fixture();
    value.document.title = "x".repeat(MAX_INSPECTION_TEXT_BYTES + 1);
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.blocks = vec![value.document.blocks[0].clone(); MAX_INSPECTION_BLOCKS + 1];
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.blocks[0].rich_text = Some(RichText {
        text: "x".into(),
        marks: vec![
            TextMark {
                start_utf8: 0,
                end_utf8: 1,
                kind: "bold".into(),
                value: None
            };
            MAX_INSPECTION_MARKS + 1
        ],
        inline_objects: vec![],
    });
    assert!(value.to_json().is_err());
}
#[test]
fn inspection_rejects_unknown_fields_bad_ids_and_non_utf8_boundaries() {
    let mut raw = serde_json::to_value(fixture()).unwrap();
    raw["selection"]["extra"] = true.into();
    assert!(CloudDocumentInspectionV1::from_json(&raw.to_string()).is_err());
    let mut value = fixture();
    value.selection.source_head_digest = "AB".repeat(32);
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.blocks[0].rich_text = Some(RichText {
        text: String::from(char::from_u32(0x20ac).unwrap()),
        marks: vec![TextMark {
            start_utf8: 1,
            end_utf8: 3,
            kind: "bold".into(),
            value: None,
        }],
        inline_objects: vec![],
    });
    assert!(value.to_json().is_err());
}

#[test]
fn escaped_text_cannot_cross_the_encoded_frame_limit() {
    let mut value = fixture();
    value.document.title = String::from(char::from(0)).repeat(200_000);
    assert!(value.validate().is_ok());
    assert!(value.to_json().is_err());
    let mut value = fixture();
    value.document.revision = u64::MAX;
    assert!(value.to_json().is_err());
    assert!(
        !CloudDocumentAction::RemoveFile {
            node_id: "wrong".into()
        }
        .validate()
    );
    assert!(
        CloudDocumentAction::RemoveFile {
            node_id: fixture().removable_files[0].node_id.clone()
        }
        .validate()
    );
}

#[test]
fn removal_controls_match_visible_trash_and_actual_write_presentation_gate() {
    let mut value = fixture();
    value.removable_files[0].in_trash = true;
    assert!(value.to_json().is_err());
    value.document.blocks[0].in_trash = true;
    assert!(value.to_json().is_ok());
    value.can_request_removal = false;
    assert!(value.to_json().is_err());
    value.removable_files.clear();
    assert!(value.to_json().is_ok());
}
