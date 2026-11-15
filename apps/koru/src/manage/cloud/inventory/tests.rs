// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_agent_wire::{AccountOperationResult, AccountState, CloudInventoryPayload};
use koru_workspace_host::accessibility::{SemanticNode, ViewSlot, WindowSlot};
use serde_json::{Value, json};
use std::time::Instant;

fn fixture(documents: bool) -> Value {
    let all: Value = serde_json::from_str(if documents {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/koru-account-wire/src/cloud_documents.fixtures.json"
        ))
    } else {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/koru-account-wire/src/cloud_plan.fixtures.json"
        ))
    })
    .unwrap();
    if documents {
        all["fixtures"][0]["response"].clone()
    } else {
        all["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "workspace_page")
            .unwrap()["response"]
            .clone()
    }
}
fn view() -> ManageView {
    let mut view = ManageView::new();
    view.c.clear_pending();
    view.c.set_account(AccountState {
        active_scope: 1,
        active_account_id: Some("account".into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    View::set_viewport(&mut view, 520.0, 900.0);
    view.show(Page::Cloud);
    view.set_in_front(true);
    assert!(view.activate_control(Control::CloudInventory));
    view
}
fn respond(view: &mut ManageView, page: Value) {
    let command = view
        .c
        .cloud_inventory_mut()
        .take(Instant::now())
        .expect("read requested");
    let result = match command {
        profiles::AccountCommand::InspectCloudUsageWorkspaces { request_id, .. } => {
            AccountOperationResult::CloudUsageWorkspaces {
                request_id,
                payload: Some(CloudInventoryPayload::new(page.to_string()).unwrap()),
                failure: None,
            }
        }
        profiles::AccountCommand::InspectCloudUsageDocuments {
            request_id,
            workspace_id,
            incarnation_id,
            ..
        } => AccountOperationResult::CloudUsageDocuments {
            request_id,
            workspace_id,
            incarnation_id,
            payload: Some(CloudInventoryPayload::new(page.to_string()).unwrap()),
            failure: None,
        },
        _ => panic!(),
    };
    assert!(view.c.complete_scoped(1, 1, result));
    view.sync();
}
fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}
fn semantics(view: &ManageView) -> Vec<SemanticNode> {
    let cx = accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(0.0, 0.0, 520.0, 900.0),
    };
    let mut nodes = Vec::new();
    flatten(&view.semantics(&cx), &mut nodes);
    nodes
}

fn mounted_copy() -> (ManageView, uuid::Uuid) {
    let mut view = view();
    view.c.set_account(AccountState {
        active_scope: 1,
        active_account_id: Some(uuid::Uuid::from_u128(1).to_string()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    view.c.cloud_inventory_mut().open();
    let doc = fixture(true);
    view.c.cloud_inventory_mut().selection = Selection::Documents {
        workspace: ReadUuid(uuid::Uuid::parse_str(doc["workspace_id"].as_str().unwrap()).unwrap()),
        incarnation: ReadUuid(
            uuid::Uuid::parse_str(doc["incarnation_id"].as_str().unwrap()).unwrap(),
        ),
    };
    respond(&mut view, doc);
    let unit = match &view.c.cloud_inventory().page {
        Some(PageData::Documents(page)) => {
            page.items
                .iter()
                .find(|r| {
                    r.membership.state != MembershipState::Structural
                        && r.document_id.is_some()
                        && r.source_head_digest.is_some()
                })
                .unwrap()
                .cloud_unit_id
                .0
        }
        _ => panic!(),
    };
    assert!(
        view.laid
            .placed(Control::CloudInventoryDocument(unit))
            .is_some()
    );
    assert!(view.activate_control(Control::CloudInventoryDocument(unit)));
    let profiles::AccountCommand::InspectCloudDocument {
        request_id,
        selection,
    } = view.c.cloud_copy_mut().take(Instant::now(), 0).unwrap()
    else {
        panic!()
    };
    let selected: koru_store_wire::cloud_document::CloudDocumentSelection =
        serde_json::from_str(selection.as_str()).unwrap();
    let node = uuid::Uuid::from_u128(999);
    let payload = json!({"selection":selected,"document":{"id":selected.document_id,"workspace_id":selected.workspace_id,
        "title":"Exact Cloud copy","revision":1,"blocks":[{"id":node.to_string(),"parent_id":null,"kind":"file","in_trash":false,
        "properties":{"name":"Only this file","done":false,"alignment":"start"},"blob":{"id":"hash","mime_type":"image/png","size":null,"width":null,"height":null}}]},
        "can_request_removal":true,"removable_files":[{"node_id":node.to_string(),"in_trash":false,"mode":"attachment_leaf"}]});
    assert!(view.c.complete_scoped(
        1,
        10,
        AccountOperationResult::CloudDocumentInspection {
            request_id,
            selection,
            payload: Some(
                koru_agent_wire::CloudDocumentInspectionPayload::new(payload.to_string()).unwrap()
            ),
            failure: None
        }
    ));
    view.sync();
    (view, node)
}

#[test]
fn mounted_cloud_copy_routes_real_confirmation_and_ack_to_authoritative_refresh() {
    let (mut view, node) = mounted_copy();
    assert!(view.laid.placed(Control::CloudCopyRemove(node)).is_some());
    let nodes = semantics(&view);
    assert!(
        nodes
            .iter()
            .any(|n| n.label.as_deref() == Some("Exact Cloud copy"))
    );
    assert!(view.activate_control(Control::CloudCopyRemove(node)));
    assert!(matches!(
        view.dialog(),
        Some(dialogs::Dialog::CloudFile { .. })
    ));
    view.dialog_key(&Key::Named(NamedKey::Enter));
    assert_eq!(
        view.c.cloud_copy().view.phase(),
        super::super::copy::Phase::Ready
    );
    view.dialog_key(&Key::Named(NamedKey::Tab));
    view.dialog_key(&Key::Named(NamedKey::Enter));
    assert!(view.dialog().is_none());
    let profiles::AccountCommand::RemoveCloudFile {
        request_id,
        selection,
        node_id,
    } = view.c.cloud_copy_mut().take(Instant::now(), 0).unwrap()
    else {
        panic!()
    };
    assert_eq!(node_id, node.to_string());
    assert_eq!(
        view.c.cloud_copy().view.phase(),
        super::super::copy::Phase::Removing
    );
    assert!(view.c.complete_scoped(
        1,
        11,
        AccountOperationResult::CloudFileRemoval {
            request_id,
            selection,
            node_id,
            removed: true,
            failure: None
        }
    ));
    view.sync();
    assert_eq!(
        view.c.cloud_copy().view.phase(),
        super::super::copy::Phase::Acknowledged
    );
    assert!(view.laid.placed(Control::CloudCopyRemove(node)).is_none());
    assert!(matches!(
        view.c.cloud_inventory().selection,
        Selection::Workspaces
    ));
    assert!(matches!(
        view.c.cloud_inventory_mut().take(Instant::now()),
        Some(profiles::AccountCommand::InspectCloudUsageWorkspaces { cursor: None, .. })
    ));
}

#[test]
fn mounted_confirmation_disappears_on_account_switch_and_cannot_target_identical_copy() {
    let (mut view, node) = mounted_copy();
    assert!(view.activate_control(Control::CloudCopyRemove(node)));
    let old = view.c.cloud_copy().view.key().unwrap().clone();
    view.c.set_account(AccountState {
        active_scope: 2,
        active_account_id: Some(uuid::Uuid::from_u128(2).to_string()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    view.sync();
    assert!(view.dialog().is_none());
    assert!(!view.c.cloud_copy().visible());
    assert!(
        view.c
            .cloud_copy_mut()
            .view
            .answer(&old, ConfirmOutcome::Confirm)
            .is_none()
    );
}
#[test]
fn actual_workspace_navigation_uses_incarnation_and_one_frame_for_every_control() {
    let mut view = view();
    let mut page = fixture(false);
    page["next_cursor"] = Value::Null;
    respond(&mut view, page);
    let row = match &view.c.cloud_inventory().page {
        Some(PageData::Workspaces(p)) => p.items[0].clone(),
        _ => panic!(),
    };
    let control = Control::CloudInventoryWorkspace(row.workspace_id.0, row.incarnation_id.0);
    let placed = view.laid.placed(control).unwrap().clone();
    assert_eq!(
        view.laid
            .inventory
            .as_ref()
            .unwrap()
            .frame
            .borrow()
            .node(control_key(control).unwrap())
            .unwrap()
            .rect,
        placed.rect
    );
    let node = semantics(&view)
        .into_iter()
        .find(|n| {
            n.occurrence.locator().ends_with("#inventory-workspace")
                && n.bounds == Some(placed.rect)
        })
        .unwrap();
    assert_eq!(node.role, accesskit::Role::Button);
    assert!(node.actions.contains(&accesskit::Action::Click));
    assert!(view.activate_control(control));
    assert!(
        matches!(view.c.cloud_inventory_mut().take(Instant::now()),Some(profiles::AccountCommand::InspectCloudUsageDocuments{workspace_id,incarnation_id,cursor:None,..}) if workspace_id==row.workspace_id.0.to_string() && incarnation_id==row.incarnation_id.0.to_string())
    );
}
#[test]
fn all_membership_groups_are_visible_without_an_unscoped_open_or_additive_total() {
    let mut view = view();
    let mut doc = fixture(true);
    doc["items"].as_array_mut().unwrap().push(json!({
        "cloud_unit_id":"00000000-0000-0000-0000-000000000006",
        "document_id":null,"unit_role":"unknown","source_head_digest":null,"display_name":null,
        "membership":{"state":"unknown","reason":"unit_identity_unknown"},
        "usage":{"state":"unknown","reason":"usage_unknown","files_bytes":null}
    }));
    view.c.cloud_inventory_mut().selection = Selection::Documents {
        workspace: ReadUuid(uuid::Uuid::parse_str(doc["workspace_id"].as_str().unwrap()).unwrap()),
        incarnation: ReadUuid(
            uuid::Uuid::parse_str(doc["incarnation_id"].as_str().unwrap()).unwrap(),
        ),
    };
    respond(&mut view, doc);
    let nodes = semantics(&view);
    let lang = crate::format::lang();
    for key in [
        TextKey::CloudInventoryDocuments,
        TextKey::CloudInventoryTrash,
        TextKey::CloudInventoryUnfiled,
        TextKey::CloudInventoryStructure,
        TextKey::CloudInventoryDocumentsUnavailable,
        TextKey::CloudInventoryNonAdditive,
        TextKey::CloudInventoryRecovery,
    ] {
        assert!(
            nodes
                .iter()
                .any(|n| n.label.as_deref() == Some(koru_i18n::t(lang, key))),
            "{key:?}"
        );
    }
    assert!(
        view.laid
            .controls
            .iter()
            .all(|p| !matches!(p.control, Control::CloudInventoryWorkspace(..)))
    );
    let frame = view.laid.inventory.as_ref().unwrap().frame.borrow();
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    assert!(
        frame
            .nodes
            .iter()
            .filter(|n| n.key.is_some())
            .all(|n| n.rect.w <= view.column().w)
    );
}
#[test]
fn equal_profile_and_workspace_uuid_bytes_keep_distinct_details_and_inspector_ancestry() {
    let mut view = view();
    let mut page = fixture(false);
    page["next_cursor"] = Value::Null;
    let id = page["items"][0]["workspace_id"].clone();
    page["items"][0]["placement"] = json!({"state":"assigned","profile_id":id});
    page["items"][0]["display_name"] = Value::Null;
    respond(&mut view, page);
    let id = uuid::Uuid::parse_str(id.as_str().unwrap()).unwrap();
    assert!(view.activate_control(Control::CloudInventoryProfileDetails(id)));
    assert!(view.c.cloud_inventory().profile_details.contains(&id));
    assert!(!view.c.cloud_inventory().details.contains(&id));
    assert!(view.activate_control(Control::CloudInventoryDetails(id)));
    assert!(view.c.cloud_inventory().details.contains(&id));
    assert!(
        view.laid
            .inventory
            .as_ref()
            .unwrap()
            .frame
            .borrow()
            .diagnostics
            .is_empty()
    );
    let capture = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 520.0, 900.0));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    let snapshot = capture.finish();
    assert!(
        snapshot
            .records
            .iter()
            .any(|r| r.name == "Cloud inventory page")
    );
    assert!(snapshot.records.iter().any(|r| r.name == "Cloud Workspace"));
    assert!(snapshot.records.iter().any(|r| r.name == "KoruButton"));
    assert!(snapshot.records.iter().enumerate().any(|(index, r)| {
        if r.name != "KoruButton" {
            return false;
        }
        let mut ancestor = snapshot.records[index].parent;
        let mut workspace = false;
        while let Some(index) = ancestor {
            let record = &snapshot.records[index];
            workspace |= record.name == "Cloud Workspace";
            if record.name == "Cloud inventory page" {
                return workspace;
            }
            ancestor = record.parent;
        }
        false
    }));
}

#[test]
fn pointer_repaint_reuses_frame_and_changes_invalidate_it() {
    let mut view = view();
    respond(&mut view, fixture(false));
    let first = view.laid.inventory.as_ref().unwrap().frame.clone();
    view.page_hover = Some(Control::CloudInventoryBack);
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    assert!(Rc::ptr_eq(
        &first,
        &view.laid.inventory.as_ref().unwrap().frame
    ));
    view.c.cloud_inventory_mut().first();
    view.sync();
    assert!(!Rc::ptr_eq(
        &first,
        &view.laid.inventory.as_ref().unwrap().frame
    ));
    let next = view.laid.inventory.as_ref().unwrap().frame.clone();
    View::set_viewport(&mut view, 460.0, 900.0);
    assert!(!Rc::ptr_eq(
        &next,
        &view.laid.inventory.as_ref().unwrap().frame
    ));
}
#[test]
fn paging_controls_are_disabled_without_a_next_page_and_back_is_real_navigation() {
    let mut view = view();
    let mut page = fixture(false);
    page["next_cursor"] = Value::Null;
    respond(&mut view, page);
    assert!(view.laid.disabled.contains(&Control::CloudInventoryNext));
    assert!(view.activate_control(Control::CloudInventoryNext));
    assert!(!view.c.cloud_inventory().checking());
    let node = semantics(&view)
        .into_iter()
        .find(|n| n.occurrence.locator().ends_with("#inventory-next"))
        .unwrap();
    assert!(node.disabled);
    assert!(!node.actions.contains(&accesskit::Action::Click));
    assert!(view.activate_control(Control::CloudInventoryBack));
    assert!(!view.c.cloud_inventory().visible);
    assert!(view.laid.cloud.is_some());
    assert!(view.laid.inventory.is_none());
}
