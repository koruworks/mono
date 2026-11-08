// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A controller with every field set away from its default, so a swap that
/// missed one — or moved one it should have left — shows up as a value in the
/// wrong place.
fn everything_set() -> ProfilesController {
    let profile = ProfileId::new();
    let row = ProfileSummary {
        id: profile,
        name: "Work".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    };
    let mut c = ProfilesController::new();
    // Per scope.
    c.cloud.status = cloud::Status::Manage;
    c.selected = Some(profile);
    c.account.stage = AccountStage::SignedIn;
    c.account.generation = 7;
    c.account_profiles = vec![AccountProfileRow {
        koru_identity_id: "identity".into(),
        profile_id: profile.to_storage(),
        name: None,
        locale: None,
        auth_identities: Vec::new(),
    }];
    c.account_profiles_loaded = true;
    c.signed_in_identity = Some(("google".into(), "a@example.test".into()));
    c.pending_unlink = Some(PendingUnlink {
        profile_id: profile.to_storage(),
        auth_identity_id: "auth".into(),
        profile_name: "Work".into(),
        awaiting_fresh_google: false,
    });
    c.pending_add_google = Some(PendingAddGoogle {
        candidate_id: "candidate".into(),
        profile_id: profile.to_storage(),
        verified_email: "a@example.test".into(),
        profile_name: "Work".into(),
        expected_identity_id: None,
        finishing: false,
        authorization_committed: false,
    });
    c.pending_rebind = Some((profile, "erased".into()));
    c.attach_prompt = Some((profile, Confirm::new("title", "body", "go")));
    c.pending_attach = Some(PendingAttach {
        profile,
        submission: koru_workspace_host::account_ui::CommandSubmissionId::next(),
        awaiting_fresh_sign_in: false,
    });
    c.profiles = vec![row.clone()];
    c.account_profiles_loaded = true;
    c.inline_connect = Some(inline_connect::PendingConnect::for_test(profile));
    c.account_warning_dismissed = true;
    c.creation_decision_pending = true;
    c.creation_submission = Some(koru_workspace_host::account_ui::CommandSubmissionId::next());
    c.different_account_dismissed = true;
    c.profile_creation = Some(Default::default());
    c.profile_creation_request = Some(ConnectedProfileDraft {
        id: koru_core::ProfileId::new(),
        owner_display_name: String::new(),
        profile_name: String::new(),
        workspace_name: String::new(),
        template: String::new(),
        workspace_icon: None,
        workspace_color: None,
        profile_icon: None,
        profile_color: None,
        connect_account: false,
    });
    c.profile_restore_request = Some(profile);
    c.profile_switch_request = Some(profile);
    c.uri_request = Some("https://example.test");
    c.confirm_offline = true;
    c.confirm_remove = true;
    c.confirm_hosted_delete = Some(removal::confirmation(profile, "Work"));
    c.pending_hosted_delete = Some(PendingHostedDelete {
        profile,
        profile_name: "Work".into(),
        stage: HostedDeleteStage::Deleting,
    });
    c.discovered_hosted_delete = Some(DiscoveredHostedDeletion {
        profile,
        profile_name: "Work".into(),
        identity_id: "identity".into(),
        lifecycle_revision: 1,
    });
    c.lifecycle_reconciled_generation = Some(7);
    c.pending = Some(AccountCommand::ListProfiles);
    c.remove_all_workspaces = true;
    c.notice = Some("notice".into());
    // Shared.
    c.cloud_uri = Some("https://www.koru.do/".into());
    c.cloud_visible = true;
    c.retained_profiles = vec![row];
    c.hidden_profile_count = 3;
    c.current = Some(profile);
    c.current_workspace_count = 2;
    c.workspace_counts.insert(profile, 2);
    c.remove_request = Some((profile, true));
    c.hosted_delete_request = Some((profile, true));
    c.continue_offline_request = Some((profile, "identity".into()));
    c.bind_request = Some((profile, None, "identity".into()));
    c.hints.reset_focus = true;
    c
}

/// What a scope swap must leave in place, compared field by field.
fn shared(c: &ProfilesController) -> String {
    let device = (
        c.cloud_uri.clone(),
        c.cloud_visible,
        c.retained_profiles.len(),
        c.hidden_profile_count,
        c.profiles.len(),
        c.current,
        c.current_workspace_count,
    );
    let lane = (
        c.workspace_counts.clone(),
        c.remove_request,
        c.hosted_delete_request,
        c.continue_offline_request.clone(),
        c.bind_request.clone(),
        c.hints,
    );
    format!("{device:?} {lane:?}")
}

/// **Every per-scope field moves with its scope, and every shared one stays.**
///
/// Which side of the line a field is on is checked by the compiler:
/// `AccountForm::swap` destructures both structs without `..`, so a new field
/// does not build until it is classified. This checks that each one on the
/// per-scope side actually moves.
#[test]
fn every_controller_field_is_swapped_with_its_scope_or_deliberately_shared() {
    let mut c = everything_set();
    let before = shared(&c);
    let mut parked = AccountForm::default();
    parked.swap(&mut c);

    // The active side now holds a fresh form.
    assert!(c.cloud.status == cloud::Status::Unknown);
    assert!(c.selected.is_none());
    assert_eq!(c.account, AccountState::default());
    assert!(c.account_profiles.is_empty());
    assert!(!c.account_profiles_loaded);
    assert!(c.signed_in_identity.is_none());
    assert!(c.pending_unlink.is_none());
    assert!(c.pending_add_google.is_none());
    assert!(c.pending_rebind.is_none());
    assert!(c.attach_prompt.is_none());
    assert!(c.pending_attach.is_none());
    assert!(c.inline_connect.is_none());
    assert!(!c.account_warning_dismissed);
    assert!(!c.creation_decision_pending);
    assert!(c.creation_submission.is_none());
    assert!(!c.different_account_dismissed);
    assert!(c.profile_creation.is_none());
    assert!(c.profile_creation_request.is_none());
    assert!(c.profile_restore_request.is_none());
    assert!(c.profile_switch_request.is_none());
    assert!(c.uri_request.is_none());
    assert!(!c.confirm_offline);
    assert!(!c.confirm_remove);
    assert!(c.confirm_hosted_delete.is_none());
    assert!(c.pending_hosted_delete.is_none());
    assert!(c.discovered_hosted_delete.is_none());
    assert!(c.lifecycle_reconciled_generation.is_none());
    assert!(c.pending.is_none());
    assert!(!c.remove_all_workspaces);
    assert!(c.notice.is_none());
    // And nothing shared moved.
    assert_eq!(shared(&c), before);

    // Back again: everything returns.
    parked.swap(&mut c);
    assert!(c.cloud.status == cloud::Status::Manage);
    assert!(c.selected.is_some());
    assert_eq!(c.account.generation, 7);
    assert_eq!(c.account_profiles.len(), 1);
    assert!(c.account_profiles_loaded);
    assert!(c.signed_in_identity.is_some());
    assert!(c.pending_unlink.is_some());
    assert!(c.pending_add_google.is_some());
    assert!(c.pending_rebind.is_some());
    assert!(c.attach_prompt.is_some());
    assert!(c.pending_attach.is_some());
    assert!(c.inline_connect.is_some());
    assert!(c.account_warning_dismissed);
    assert!(c.creation_decision_pending);
    assert!(c.creation_submission.is_some());
    assert!(c.different_account_dismissed);
    assert!(c.profile_creation.is_some());
    assert!(c.profile_creation_request.is_some());
    assert!(c.profile_restore_request.is_some());
    assert!(c.profile_switch_request.is_some());
    assert!(c.uri_request.is_some());
    assert!(c.confirm_offline);
    assert!(c.confirm_remove);
    assert!(c.confirm_hosted_delete.is_some());
    assert!(c.pending_hosted_delete.is_some());
    assert!(c.discovered_hosted_delete.is_some());
    assert_eq!(c.lifecycle_reconciled_generation, Some(7));
    assert!(c.pending.is_some());
    assert!(c.remove_all_workspaces);
    assert!(c.notice.is_some());
    assert_eq!(shared(&c), before);
}

/// Changing scope asks the view to move focus and hover off controls that
/// belonged to the other form, as the view used to do itself.
#[test]
fn changing_scope_asks_the_view_to_reset_focus_and_hover() {
    let mut c = ProfilesController::new();
    c.take_hints();
    c.activate_scope(0);
    assert_eq!(
        c.take_hints(),
        ViewHints::default(),
        "same scope, no change"
    );
    c.activate_scope(5);
    let hints = c.take_hints();
    assert!(hints.reset_focus && hints.clear_hover);
}
