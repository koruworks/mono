// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::{Value, json};

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
fn workspace_page(next: Option<&str>) -> Value {
    let mut page = fixture(false);
    page["next_cursor"] = json!(next);
    page
}
fn request(state: &mut Inventory, now: Instant) -> u64 {
    match state.take(now).unwrap() {
        AccountCommand::InspectCloudUsageWorkspaces { request_id, .. }
        | AccountCommand::InspectCloudUsageDocuments { request_id, .. } => request_id,
        _ => panic!(),
    }
}
fn result(id: u64, page: Value) -> AccountOperationResult {
    if page["contract"] == "cloud-usage-workspaces-v1" {
        AccountOperationResult::CloudUsageWorkspaces {
            request_id: id,
            payload: Some(CloudInventoryPayload::new(page.to_string()).unwrap()),
            failure: None,
        }
    } else {
        AccountOperationResult::CloudUsageDocuments {
            request_id: id,
            workspace_id: page["workspace_id"].as_str().unwrap().into(),
            incarnation_id: page["incarnation_id"].as_str().unwrap().into(),
            payload: Some(CloudInventoryPayload::new(page.to_string()).unwrap()),
            failure: None,
        }
    }
}
#[test]
fn page_epochs_restart_once_and_first_resets_the_episode() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    let id = request(&mut state, now);
    assert!(state.result(1, &result(id, workspace_page(Some("next")))));
    state.next();
    let id = request(&mut state, now);
    let mut changed = workspace_page(None);
    changed["catalog_revision_digest"] = json!("b".repeat(64));
    assert!(state.result(2, &result(id, changed)));
    assert!(state.restarted);
    assert!(state.page.is_none());
    assert_eq!(state.cursor, None);
    let id = request(&mut state, now);
    assert!(state.result(
        3,
        &AccountOperationResult::CloudUsageWorkspaces {
            request_id: id,
            payload: None,
            failure: Some(CloudInventoryFailure::RevisionChanged)
        }
    ));
    assert!(state.failed);
    assert!(state.take(now).is_none());
    state.first();
    assert!(!state.restarted);
    assert!(state.take(now).is_some());
}
#[test]
fn document_proof_change_returns_to_fresh_workspace_selection() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    let page: CloudUsageDocumentsResponseV1 = serde_json::from_value(fixture(true)).unwrap();
    state.selection = Selection::Documents {
        workspace: page.workspace_id,
        incarnation: page.incarnation_id,
    };
    let id = request(&mut state, now);
    let mut first = fixture(true);
    first["next_cursor"] = json!("next");
    assert!(state.result(1, &result(id, first)));
    state.next();
    let id = request(&mut state, now);
    let mut changed = fixture(true);
    changed["manifest"]["proof_digest"] = json!("b".repeat(64));
    assert!(state.result(2, &result(id, changed)));
    assert_eq!(state.selection, Selection::Workspaces);
    assert!(state.restarted);
    assert!(matches!(
        state.take(now),
        Some(AccountCommand::InspectCloudUsageWorkspaces { cursor: None, .. })
    ));
}
#[test]
fn history_is_bounded_and_first_is_always_available() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    for n in 0..41 {
        let id = request(&mut state, now);
        assert!(state.result(
            n + 1,
            &result(id, workspace_page(Some(&format!("next{n}"))))
        ));
        if n < 40 {
            state.next();
        }
    }
    assert_eq!(state.history.len(), 32);
    for n in 0..32 {
        state.previous();
        let id = request(&mut state, now);
        assert!(state.result(50 + n, &result(id, workspace_page(None))));
    }
    assert!(!state.can_previous());
    assert!(!state.first_page());
    state.first();
    assert!(state.first_page());
    assert!(state.take(now).is_some());
}
#[test]
fn timeout_hide_and_late_admission_cancel_only_the_owned_read() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    let id = request(&mut state, now);
    let submission = state.submission(id).unwrap();
    state.hide();
    assert!(state.admit(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 17
    }));
    assert_eq!(
        state.take_cancel(),
        Some(AccountCommand::Cancel { generation: 17 })
    );
    assert!(state.result(17, &result(id, workspace_page(None))));
    assert!(state.page.is_none());
    assert!(state.retired.is_empty());
    state.open();
    let id = request(&mut state, now);
    let submission = state.submission(id).unwrap();
    state.admit(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 18,
    });
    assert!(!state.maintain(now + Duration::from_secs(29)));
    assert!(state.maintain(now + Duration::from_secs(30)));
    assert!(state.failed);
    assert_eq!(
        state.take_cancel(),
        Some(AccountCommand::Cancel { generation: 18 })
    );
    assert!(state.take(now + Duration::from_secs(100)).is_none());
}

#[test]
fn retired_result_before_admission_releases_backlog_without_restoring_rows() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    let id = request(&mut state, now);
    let submission = state.submission(id).unwrap();
    state.hide();
    assert!(state.result(17, &result(id, workspace_page(None))));
    assert!(state.retired.is_empty());
    assert!(state.page.is_none());
    assert!(!state.admit(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 17
    }));
    assert!(state.take_cancel().is_none());
}
#[test]
fn malformed_and_mixed_results_never_become_empty_known_pages() {
    let now = Instant::now();
    let mut state = Inventory::default();
    state.open();
    let id = request(&mut state, now);
    assert!(state.result(
        1,
        &AccountOperationResult::CloudUsageWorkspaces {
            request_id: id,
            payload: Some(CloudInventoryPayload::new("{}".into()).unwrap()),
            failure: None
        }
    ));
    assert!(state.failed);
    assert!(state.page.is_none());
    state.first();
    let id = request(&mut state, now);
    assert!(state.result(
        2,
        &AccountOperationResult::CloudUsageWorkspaces {
            request_id: id,
            payload: Some(CloudInventoryPayload::new(workspace_page(None).to_string()).unwrap()),
            failure: Some(CloudInventoryFailure::RevisionChanged)
        }
    ));
    assert!(state.failed);
    assert!(!state.restarted);
}
#[test]
fn complete_profile_total_requires_known_placement_and_never_sums_documents() {
    let mut page: CloudUsageWorkspacesResponseV1 =
        serde_json::from_value(workspace_page(None)).unwrap();
    let row = page
        .items
        .iter()
        .find(|r| r.placement.profile_id.is_some())
        .unwrap()
        .clone();
    let profile = row.placement.profile_id.unwrap();
    page.items = vec![WorkspaceUsageItemV1 {
        usage: WorkspaceUsageV1 {
            state: UsageState::Known,
            reason: None,
            files_bytes: Some(Decimal::new(9_007_199_254_740_993).unwrap()),
            last_known: None,
        },
        ..row.clone()
    }];
    let mut state = Inventory {
        page: Some(PageData::Workspaces(page.clone())),
        ..Default::default()
    };
    assert_eq!(state.profile_total(profile), Some(9_007_199_254_740_993));
    state.cursor = Some("later".into());
    assert_eq!(state.profile_total(profile), None);
    state.cursor = None;
    page.items.push(WorkspaceUsageItemV1 {
        placement: PlacementV1 {
            state: PlacementState::Unknown,
            profile_id: None,
        },
        ..row
    });
    state.page = Some(PageData::Workspaces(page));
    assert_eq!(state.profile_total(profile), None);
    state.page = Some(PageData::Documents(
        serde_json::from_value(fixture(true)).unwrap(),
    ));
    assert_eq!(state.profile_total(profile), None);
}
#[test]
fn actual_controller_rejects_old_account_and_wrong_generation() {
    let now = Instant::now();
    let mut c = ProfilesController::new();
    c.set_account(AccountState {
        active_scope: 1,
        active_account_id: Some("one".into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    c.cloud_inventory_mut().open();
    let id = request(c.cloud_inventory_mut(), now);
    let submission = c.cloud_inventory().submission(id).unwrap();
    c.cloud_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 5,
    });
    assert!(!c.complete_scoped(1, 4, result(id, workspace_page(None))));
    assert!(c.complete_scoped(1, 5, result(id, workspace_page(None))));
    c.set_account(AccountState {
        active_scope: 2,
        active_account_id: Some("two".into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    assert!(c.cloud_inventory().page.is_none());
    assert!(!c.complete_scoped(1, 5, result(id, workspace_page(None))));
    assert!(c.cloud_inventory().page.is_none());
}
