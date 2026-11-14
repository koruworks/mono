// SPDX-License-Identifier: MPL-2.0

use super::*;
use prost::Message;

#[test]
fn shared_profile_set_commands_preserve_exact_members_and_owner_fence() {
    let cases: &[(&[u8], usize)] = &[
        (
            include_bytes!("../../../../contracts/testdata/valid/account_select_profiles_empty.pb"),
            0,
        ),
        (
            include_bytes!("../../../../contracts/testdata/valid/account_select_profiles_one.pb"),
            1,
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/valid/account_select_profiles_subset.pb"
            ),
            2,
        ),
    ];
    for (bytes, count) in cases {
        let value = wire::AccountCommandRequest::decode(*bytes).unwrap();
        assert_eq!(command(&value), Ok(()));
        assert_eq!(value.action, wire::AccountAction::SelectProfiles as i32);
        assert_eq!(value.profile_ids.len(), *count);
        assert!(value.profile_id.is_empty());
        assert_eq!(value.expected_owner_scope, Some(9));
    }
    for (bytes, has_member) in [
        (
            include_bytes!("../../../../contracts/testdata/valid/account_retry_enrollment_all.pb")
                .as_slice(),
            false,
        ),
        (
            include_bytes!("../../../../contracts/testdata/valid/account_retry_enrollment_one.pb")
                .as_slice(),
            true,
        ),
    ] {
        let value = wire::AccountCommandRequest::decode(bytes).unwrap();
        assert_eq!(command(&value), Ok(()));
        assert_eq!(value.action, wire::AccountAction::RetryEnrollment as i32);
        assert_eq!(!value.profile_id.is_empty(), has_member);
        assert!(value.profile_ids.is_empty());
        assert_eq!(value.expected_owner_scope, Some(9));
    }
}

#[test]
fn malformed_sets_and_removed_single_selection_never_clear_the_current_set() {
    let cases: &[(&[u8], &str)] = &[
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profiles_duplicate.pb"
            ),
            "account_profile_invalid",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profiles_unsorted.pb"
            ),
            "account_profile_invalid",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profiles_noncanonical.pb"
            ),
            "account_profile_invalid",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profiles_uuid_version.pb"
            ),
            "account_profile_invalid",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profiles_missing_scope.pb"
            ),
            "account_scope_missing",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_select_profile_legacy.pb"
            ),
            "account_action_unknown",
        ),
        (
            include_bytes!(
                "../../../../contracts/testdata/invalid/account_profile_set_wrong_action.pb"
            ),
            "account_payload_invalid",
        ),
    ];
    for (bytes, error) in cases {
        let value = wire::AccountCommandRequest::decode(*bytes).unwrap();
        assert_eq!(command(&value), Err(*error));
    }
}

#[test]
fn status_and_result_keep_each_members_progress_and_ignore_removed_result_field() {
    let status = wire::MobileAccountStatus::decode(
        include_bytes!("../../../../contracts/testdata/valid/account_enrollment_status.pb")
            .as_slice(),
    )
    .unwrap();
    let result = wire::MobileAccountResult::decode(
        include_bytes!("../../../../contracts/testdata/valid/account_profiles_selected.pb")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(status.active_scope, result.owner_scope);
    assert_eq!(status.generation, result.generation);
    assert_eq!(result.requester, 7);
    assert_eq!(result.kind, "profiles_selected");
    assert_eq!(status.enrollments, result.enrollments);
    assert_eq!(result.enrollments.len(), 3);
    assert_eq!(result.enrollments[0].retry_at_millis, Some(45));
    assert_eq!(result.enrollments[1].stage, "complete");
    assert_eq!(result.enrollments[1].failure_code, None);
    assert_eq!(result.enrollments[2].retry_at_millis, Some(90));
    let old = wire::MobileAccountResult::decode(
        include_bytes!(
            "../../../../contracts/testdata/forward_compat/account_removed_selected_field.pb"
        )
        .as_slice(),
    )
    .unwrap();
    assert_eq!(old.kind, "selected");
    assert!(old.enrollments.is_empty());
    assert_eq!(old.owner_scope, result.owner_scope);
    assert_eq!(old.generation, result.generation);
}

#[test]
fn explicit_creation_commands_share_the_platform_fixture() {
    for (bytes, action) in [
        (
            include_bytes!("../../../../contracts/testdata/valid/account_creation_confirm.pb")
                .as_slice(),
            wire::AccountAction::ConfirmAccountCreation,
        ),
        (
            include_bytes!("../../../../contracts/testdata/valid/account_creation_cancel.pb")
                .as_slice(),
            wire::AccountAction::CancelAccountCreation,
        ),
    ] {
        let value = wire::AccountCommandRequest::decode(bytes).unwrap();
        assert_eq!(command(&value), Ok(()));
        assert_eq!(value.action, action as i32);
        assert!(value.profile_id.is_empty());
    }
}

#[test]
fn zero_profile_account_has_an_explicit_authoritative_summary() {
    let value = wire::MobileAccountResult::decode(
        include_bytes!("../../../../contracts/testdata/valid/account_empty_profiles.pb").as_slice(),
    )
    .unwrap();
    assert_eq!(value.account.unwrap().display_name, "Existing account");
    assert!(value.profiles.is_empty());
}

#[test]
fn shared_account_fixtures_fail_closed_without_rejecting_future_fields() {
    let valid = include_bytes!("../../../../contracts/testdata/valid/account_start.pb");
    let future = include_bytes!(
        "../../../../contracts/testdata/forward_compat/account_start_unknown_field.pb"
    );
    for bytes in [valid.as_slice(), future.as_slice()] {
        let value = wire::AccountCommandRequest::decode(bytes).unwrap();
        assert_eq!(command(&value), Ok(()));
        assert_eq!(value.requester, 7);
        assert_eq!(value.action, wire::AccountAction::StartGoogle as i32);
    }
    for bytes in [
        include_bytes!("../../../../contracts/testdata/invalid/account_unknown_action.pb")
            .as_slice(),
        include_bytes!("../../../../contracts/testdata/invalid/account_missing_requester.pb")
            .as_slice(),
    ] {
        assert!(command(&wire::AccountCommandRequest::decode(bytes).unwrap()).is_err());
    }
}

#[test]
fn secret_payloads_are_bounded_and_cancel_cannot_smuggle_a_token() {
    let mut value = wire::AccountPlatformRequest {
        version: 1,
        requester: 7,
        response: Some(wire::account_platform_request::Response::SignIn(
            wire::NativeSignInReply {
                generation: 1,
                request_id: 1,
                id_token: Some(vec![65; MAX_TOKEN_BYTES + 1]),
                cancelled: false,
            },
        )),
        ..Default::default()
    };
    assert!(platform(&value).is_err());
    if let Some(wire::account_platform_request::Response::SignIn(reply)) = &mut value.response {
        reply.id_token = Some(vec![65]);
        reply.cancelled = true;
    }
    assert!(platform(&value).is_err());
}

#[test]
fn cloud_inventory_requires_exact_selectors_and_bounded_cursor_only_on_read_actions() {
    let mut request = wire::AccountCommandRequest {
        version: 1,
        requester: 42,
        expected_owner_scope: Some(9),
        action: wire::AccountAction::InspectCloudUsageWorkspaces as i32,
        ..Default::default()
    };
    assert_eq!(command(&request), Ok(()));
    request.cloud_cursor = Some("aB9_-".into());
    assert_eq!(command(&request), Ok(()));
    for cursor in ["".into(), "x".repeat(2049), "a/b".into(), "a=b".into()] {
        request.cloud_cursor = Some(cursor);
        assert!(command(&request).is_err());
    }
    request.cloud_cursor = None;
    request.action = wire::AccountAction::InspectCloudUsageDocuments as i32;
    assert!(command(&request).is_err());
    request.cloud_workspace_id = Some("00000000-0000-0000-0000-000000000050".into());
    request.cloud_incarnation_id = Some("00000000-0000-0000-0000-000000000051".into());
    assert_eq!(command(&request), Ok(())); // Read contract does not require UUID v4.
    let bytes = request.encode_to_vec();
    assert_eq!(
        command(&wire::AccountCommandRequest::decode(bytes.as_slice()).unwrap()),
        Ok(())
    );
    request.cloud_workspace_id = Some("00000000000000000000000000000050".into());
    assert!(command(&request).is_err());
    request.cloud_workspace_id = Some("00000000-0000-0000-0000-000000000050".into());
    request.action = wire::AccountAction::InspectCloudUsageWorkspaces as i32;
    assert!(command(&request).is_err());
    request.action = wire::AccountAction::InspectCloudPlan as i32;
    assert!(command(&request).is_err());
}
