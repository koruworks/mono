// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_agent_wire::CloudDocumentInspectionPayload;
use serde_json::json;
fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}
fn selection() -> CloudDocumentSelection {
    CloudDocumentSelection {
        account_id: id(1),
        workspace_id: id(2),
        incarnation_id: id(3),
        cloud_unit_id: id(4),
        document_id: id(5),
        source_head_digest: "a".repeat(64),
    }
}
fn payload(selected: &CloudDocumentSelection) -> CloudDocumentInspectionPayload {
    CloudDocumentInspectionPayload::new(json!({"selection":selected,"document":{
        "id":id(5),"workspace_id":id(2),"title":"Stored copy","revision":1,"blocks":[{
            "id":id(6),"parent_id":null,"kind":"file","in_trash":false,"properties":{"done":false,"alignment":"start"},
            "blob":{"id":"hash","mime_type":"image/png","size":null,"width":null,"height":null}
        }]},"can_request_removal":true,"removable_files":[{"node_id":id(6),"in_trash":false,"mode":"attachment_leaf"}]
    }).to_string()).unwrap()
}
fn admit(state: &mut Inspection, generation: u64) -> AccountCommand {
    let command = state.take(Instant::now(), 0).unwrap();
    let id = match &command {
        AccountCommand::InspectCloudDocument { request_id, .. }
        | AccountCommand::RemoveCloudFile { request_id, .. } => *request_id,
        _ => panic!(),
    };
    assert!(state.admit(CommandAdmission {
        owner_scope: 1,
        generation,
        submission: state.submission(id).unwrap()
    }));
    command
}
fn inspect_result(command: AccountCommand) -> AccountOperationResult {
    let AccountCommand::InspectCloudDocument {
        request_id,
        selection,
    } = command
    else {
        panic!()
    };
    let value = CloudDocumentSelection::from_json(selection.as_str()).unwrap();
    AccountOperationResult::CloudDocumentInspection {
        request_id,
        selection,
        payload: Some(payload(&value)),
        failure: None,
    }
}
#[test]
fn inspection_and_removal_bind_selection_generation_and_remote_ack() {
    let mut state = Inspection::default();
    state.open(1, selection());
    let command = admit(&mut state, 10);
    let result = inspect_result(command);
    assert_eq!(state.result(9, &result), None);
    assert_eq!(state.result(10, &result), Some(false));
    let key = state.view.key().unwrap().clone();
    assert_eq!(state.view.phase(), Phase::Ready);
    state
        .view
        .activate(&key, &crate::manage::cloud::copy::Control::Remove(id(6)));
    let intent = state
        .view
        .answer(&key, koru_ui::confirm::Outcome::Confirm)
        .unwrap();
    state.queue(intent);
    let AccountCommand::RemoveCloudFile {
        request_id,
        selection,
        node_id,
    } = admit(&mut state, 11)
    else {
        panic!()
    };
    assert_eq!(state.view.phase(), Phase::Removing);
    assert_eq!(
        state.result(
            11,
            &AccountOperationResult::CloudFileRemoval {
                request_id,
                selection,
                node_id,
                removed: true,
                failure: None
            }
        ),
        Some(true)
    );
    assert_eq!(state.view.phase(), Phase::Acknowledged);
}
#[test]
fn timeout_cancels_and_never_retries_an_uncertain_write() {
    let mut state = Inspection::default();
    state.open(1, selection());
    let command = admit(&mut state, 10);
    state.result(10, &inspect_result(command));
    let key = state.view.key().unwrap().clone();
    state
        .view
        .activate(&key, &crate::manage::cloud::copy::Control::Remove(id(6)));
    let intent = state
        .view
        .answer(&key, koru_ui::confirm::Outcome::Confirm)
        .unwrap();
    state.queue(intent);
    admit(&mut state, 11);
    assert!(state.maintain(Instant::now() + Duration::from_secs(31)));
    assert_eq!(state.view.phase(), Phase::OutcomeUnknown);
    assert!(matches!(
        state.take_cancel(),
        Some(AccountCommand::Cancel { generation: 11 })
    ));
    assert!(state.take(Instant::now(), 0).is_none());
    assert!(
        state
            .view
            .activate(&key, &crate::manage::cloud::copy::Control::Remove(id(6)))
            .is_none()
    );
}
#[test]
fn actual_account_switch_clears_copy_and_cancels_late_admission_under_original_owner() {
    let mut c = ProfilesController::new();
    c.set_account(AccountState {
        active_scope: 1,
        active_account_id: Some(id(1)),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    c.cloud.inspection.open(1, selection());
    let command = c.cloud.inspection.take(Instant::now(), 0).unwrap();
    let AccountCommand::InspectCloudDocument { request_id, .. } = &command else {
        panic!()
    };
    let submission = c.cloud.inspection.submission(*request_id).unwrap();
    c.set_account(AccountState {
        active_scope: 2,
        active_account_id: Some(id(20)),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    assert!(!c.cloud_copy().visible());
    assert!(c.inventory_cancellations.admit(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 10
    }));
    assert!(matches!(
        c.inventory_cancellations.take(),
        Some((1, AccountCommand::Cancel { generation: 10 }))
    ));
    assert!(c.complete_scoped(1, 10, inspect_result(command)));
    assert!(!c.cloud_copy().visible());
    assert_eq!(c.inventory_cancellations.len(), 0);
}
#[test]
fn new_view_epoch_survives_controller_resets_and_wrong_selection_is_unavailable() {
    let mut state = Inspection::default();
    state.open(1, selection());
    let old = state.view.key().unwrap().clone();
    state = Inspection::default();
    state.open(1, selection());
    assert_ne!(old, state.view.key().unwrap().clone());
    let command = admit(&mut state, 10);
    let mut result = inspect_result(command);
    if let AccountOperationResult::CloudDocumentInspection { selection, .. } = &mut result {
        let mut changed = super::tests::selection();
        changed.source_head_digest = "b".repeat(64);
        *selection =
            CloudDocumentSelectionPayload::new(serde_json::to_string(&changed).unwrap()).unwrap();
    }
    assert_eq!(state.result(10, &result), Some(false));
    assert_eq!(state.view.phase(), Phase::Unavailable);
}

#[test]
fn acknowledged_and_retired_removals_refresh_authority_without_reopening() {
    for close in [false, true] {
        let mut c = ProfilesController::new();
        c.set_account(AccountState {
            active_scope: 1,
            active_account_id: Some(id(1)),
            stage: AccountStage::SignedIn,
            ..Default::default()
        });
        c.cloud.inventory.open();
        c.cloud.inspection.open(1, selection());
        let command = admit(&mut c.cloud.inspection, 10);
        assert!(c.complete_scoped(1, 10, inspect_result(command)));
        let key = c.cloud_copy().view.key().unwrap().clone();
        c.cloud_copy_mut()
            .view
            .activate(&key, &crate::manage::cloud::copy::Control::Remove(id(6)));
        let intent = c
            .cloud_copy_mut()
            .view
            .answer(&key, koru_ui::confirm::Outcome::Confirm)
            .unwrap();
        c.cloud_copy_intent(intent);
        let AccountCommand::RemoveCloudFile {
            request_id,
            selection,
            node_id,
        } = admit(&mut c.cloud.inspection, 11)
        else {
            panic!()
        };
        if close {
            c.cloud_copy_mut().close();
        }
        assert!(c.complete_scoped(
            1,
            11,
            AccountOperationResult::CloudFileRemoval {
                request_id,
                selection,
                node_id,
                removed: !close,
                failure: close.then_some(CloudInspectionFailure::OutcomeUnknown)
            }
        ));
        assert_eq!(c.cloud_copy().visible(), !close);
        assert!(matches!(
            c.cloud.storage.take(Instant::now()),
            Some(AccountCommand::InspectCloudPlan { .. })
        ));
        assert!(matches!(
            c.cloud.inventory.take(Instant::now()),
            Some(AccountCommand::InspectCloudUsageWorkspaces { cursor: None, .. })
        ));
    }
}

#[test]
fn usage_change_discards_any_meter_read_started_before_the_remote_change() {
    let mut storage = storage::Storage::default();
    let AccountCommand::InspectCloudPlan { request_id } = storage.take(Instant::now()).unwrap()
    else {
        panic!()
    };
    storage.usage_changed();
    assert!(storage.snapshot.is_none());
    assert!(storage.take(Instant::now()).is_none());
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/koru-account-wire/src/cloud_plan.fixtures.json"
    )))
    .unwrap();
    let payload =
        koru_agent_wire::CloudPlanPayload::new(fixture["fixtures"][0]["response"].to_string())
            .unwrap();
    assert!(storage.result(1, request_id, Some(&payload), 0, Instant::now()));
    assert!(storage.snapshot.is_none());
    assert!(
        matches!(storage.take(Instant::now()),Some(AccountCommand::InspectCloudPlan{request_id:next}) if next != request_id)
    );
}
