// SPDX-License-Identifier: MPL-2.0

use super::*;

const ACCOUNT: &str = "10000000-0000-4000-8000-000000000001";
const OTHER_ACCOUNT: &str = "20000000-0000-4000-8000-000000000002";

fn signed_in() -> ProfilesController {
    let mut c = ProfilesController::new();
    c.pending = None;
    c.account = AccountState {
        active_scope: 1,
        active_account_id: Some(ACCOUNT.into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    };
    c.set_cloud_visible(true);
    c
}
fn start(c: &mut ProfilesController) -> (u64, CommandSubmissionId) {
    let (AccountCommand::InspectCloudSubscription { request_id }, Some(submission)) =
        c.take_request_with_submission().unwrap()
    else {
        panic!("expected cloud inspection");
    };
    (request_id, submission)
}
fn profile(name: &str, account: Option<&str>) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: account.map(|id| format!("koru:{id}")),
        folder: String::new(),
    }
}
/// A signed-in c with a confirmed subscription and two account Profiles plus
/// one local-only Profile.
fn subscribed() -> ProfilesController {
    let mut c = signed_in();
    c.profiles = vec![
        profile("Work", Some(ACCOUNT)),
        profile("Home", Some(ACCOUNT)),
        profile("Scratch", None),
    ];
    let (id, submission) = start(&mut c);
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 0
    }));
    assert!(c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSubscription {
            request_id: id,
            current: Some(true),
            access: Some(true),
        }
    ));
    c
}
/// Drain the `InspectCloudSync` the confirmed subscription asks for, admit it,
/// and answer it with `profiles`.
fn answer_sync(c: &mut ProfilesController, profiles: Vec<CloudSyncProfile>) {
    let (AccountCommand::InspectCloudSync { request_id }, Some(submission)) =
        c.take_request_with_submission().unwrap()
    else {
        panic!("expected a per-Profile cloud read");
    };
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 0
    }));
    assert!(c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSync {
            request_id,
            profiles
        }
    ));
}
fn row(profile: &ProfileSummary, enabled: bool, synced: u32, total: u32) -> CloudSyncProfile {
    CloudSyncProfile {
        profile_id: profile.id.to_storage(),
        enabled,
        workspaces: total,
        synced_workspaces: synced,
        error: None,
    }
}
fn subtitle(c: &ProfilesController, index: usize) -> Option<String> {
    let profile = c.profiles[index].clone();
    c.cloud_sync_subtitle(&profile)
}
#[test]
fn email_session_updates_settle_before_cloud_subscription_and_sync_reads() {
    let mut c = signed_in();
    c.set_cloud_visible(false);
    let work = profile("Work", Some(ACCOUNT));
    c.profiles = vec![work.clone()];
    let mut account = c.account.clone();
    account.generation = 1;
    c.set_account(account.clone());
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::InspectAccountSession)
    );
    c.complete_scoped(
        1,
        1,
        AccountOperationResult::SignedInIdentity {
            provider: "email".into(),
            verified_email: "person@example.test".into(),
        },
    );
    assert_eq!(c.take_request(), Some(AccountCommand::ListProfiles));
    for generation in 2..34 {
        account.generation = generation;
        c.set_account(account.clone());
        assert!(c.take_request().is_none(), "session inspection must settle");
    }

    c.set_cloud_visible(true);
    let (request_id, submission) = start(&mut c);
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 34,
    }));
    c.complete_scoped(
        1,
        34,
        AccountOperationResult::CloudSubscription {
            request_id,
            current: Some(true),
            access: Some(true),
        },
    );
    assert_eq!(c.cloud_status(), Status::Manage);
    answer_sync(&mut c, vec![row(&work, true, 1, 1)]);
    assert!(c.cloud_sync_on(work.id));
    assert!(c.take_request().is_none());
}

#[test]
fn cloud_startup_and_outage_leave_local_profile_details_accessible() {
    use koru_agent_wire::{
        AccountProfileCloud, AccountProfileEnrollment, CloudConnectionStage, EnrollmentState,
    };
    let mut c = signed_in();
    let work = profile("Work", Some(ACCOUNT));
    c.selected = Some(work.id);
    c.profiles = vec![work.clone()];
    c.account.enrollments = vec![AccountProfileEnrollment {
        profile_id: work.id.to_string(),
        state: EnrollmentState::Waiting,
        failure_code: Some("enrollment_unavailable".into()),
        retry_at_millis: None,
    }];
    // No subscription inspection or hosted sync command has answered yet.
    c.account.cloud_profiles = vec![AccountProfileCloud {
        discovery: None,
        profile_id: work.id.to_string(),
        stage: CloudConnectionStage::Connecting,
    }];
    assert!(c.cloud_sync_on(work.id));
    assert!(c.account_surface().is_none());
    assert!(c.profile_connection_status(work.id).is_none());
    assert_eq!(
        subtitle(&c, 0).as_deref(),
        Some("Connecting to Koru Cloud…")
    );
    c.account.cloud_profiles[0].stage = CloudConnectionStage::Retrying;
    c.account.stage = AccountStage::Unavailable;
    assert!(c.account_surface().is_none());
    assert_eq!(
        subtitle(&c, 0).as_deref(),
        Some("Koru Cloud unavailable · local content is available")
    );
    c.account.stage = AccountStage::SignedIn;
    c.account.cloud_profiles.clear();
    assert!(
        c.account_surface().is_some(),
        "non-Cloud enrollment retains its surface"
    );
}

#[test]
fn complimentary_access_enables_sync_without_purchase_actions() {
    let mut c = signed_in();
    let work = profile("Work", Some(ACCOUNT));
    c.profiles = vec![work.clone()];
    let (request_id, _) = start(&mut c);
    c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSubscription {
            request_id,
            current: Some(false),
            access: Some(true),
        },
    );
    assert_eq!(c.cloud_status(), Status::Active);
    assert_eq!(c.cloud_access(), Some(true));
    assert_eq!(c.cloud_body_text(Lang::En), "Subscribed");
    assert!(!c.cloud_actionable());
    assert!(!c.cloud_learn_more_visible());
    assert!(matches!(
        c.cloud_sync_availability(&work),
        SyncAvailability::Enabled
    ));
    c.cloud_activate();
    assert!(c.take_cloud_uri().is_none());
    answer_sync(&mut c, vec![row(&work, true, 1, 1)]);
    assert_eq!(
        c.cloud_body_text(Lang::En),
        "Subscribed · syncing 1 of 1 profiles"
    );
}

#[test]
fn an_inactive_purchase_keeps_billing_management_but_not_sync_access() {
    let mut c = signed_in();
    let work = profile("Work", Some(ACCOUNT));
    let (request_id, _) = start(&mut c);
    c.complete_scoped(
        1,
        0,
        AccountOperationResult::CloudSubscription {
            request_id,
            current: Some(true),
            access: Some(false),
        },
    );
    assert_eq!(c.cloud_access(), Some(false));
    assert_eq!(c.cloud_status(), Status::Manage);
    assert!(c.cloud_action_is_link());
    assert_eq!(c.cloud_body_text(Lang::En), "Cloud access is inactive.");
    assert!(matches!(
        c.cloud_sync_availability(&work),
        SyncAvailability::Disabled(TextKey::CloudAccessInactive)
    ));
    assert!(c.take_request().is_none());
}

#[test]
fn hidden_cards_and_busy_command_lanes_never_issue_a_background_cloud_read() {
    let mut c = signed_in();
    c.set_cloud_visible(false);
    assert!(c.take_request().is_none());
    c.pending = Some(AccountCommand::ListProfiles);
    assert_eq!(c.take_request(), Some(AccountCommand::ListProfiles));
    assert!(c.take_request().is_none());
    c.set_cloud_visible(true);
    let _ = start(&mut c);
    c.refresh_cloud();
    assert!(c.take_request().is_none());
}
#[test]
fn targeted_results_before_admission_do_not_touch_profile_forms_and_late_replies_are_fenced() {
    let mut c = signed_in();
    let (id, submission) = start(&mut c);
    c.notice = Some("unrelated profile work".into());
    assert!(c.complete_scoped(
        1,
        7,
        AccountOperationResult::CloudSubscription {
            request_id: id,
            current: None,
            access: None,
        }
    ));
    assert_eq!(c.notice.as_deref(), Some("unrelated profile work"));
    assert!(!c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 7
    }));
    assert!(c.cloud.status == Status::Unavailable);
    c.cloud_activate();
    assert!(c.take_cloud_uri().is_none());
    let (new_id, submission) = start(&mut c);
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 8
    }));
    c.account.generation = 100;
    assert!(!c.complete_scoped(
        1,
        7,
        AccountOperationResult::CloudSubscription {
            request_id: id,
            current: Some(false),
            access: Some(false),
        }
    ));
    assert!(c.complete_scoped(
        1,
        8,
        AccountOperationResult::CloudSubscription {
            request_id: new_id,
            current: Some(true),
            access: Some(true),
        }
    ));
    assert!(c.cloud.status == Status::Manage);
    c.cloud_activate();
    assert!(c.take_cloud_uri().unwrap().ends_with("/dashboard"));
    c.refresh_cloud();
    let (late, _) = start(&mut c);
    c.set_account(AccountState {
        active_scope: 1,
        stage: AccountStage::SignedOut,
        ..Default::default()
    });
    assert!(!c.complete_scoped(
        1,
        9,
        AccountOperationResult::CloudSubscription {
            request_id: late,
            current: Some(true),
            access: Some(true),
        }
    ));
    assert!(c.cloud.status == Status::Unknown);
}
#[test]
fn switching_accounts_keeps_targeted_status_off_the_selected_profile_and_new_account() {
    let mut c = signed_in();
    let (id, _) = start(&mut c);
    c.set_account(AccountState {
        active_scope: 2,
        active_account_id: Some(OTHER_ACCOUNT.into()),
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    assert!(c.complete_scoped(
        1,
        3,
        AccountOperationResult::CloudSubscription {
            request_id: id,
            current: Some(true),
            access: Some(true),
        }
    ));
    assert_eq!(c.account.active_scope, 2);
    assert!(c.cloud.status == Status::Unknown);
    assert!(c.take_cloud_uri().is_none());
}
#[test]
fn transitional_signin_stages_never_offer_a_mislabeled_cancel_action() {
    for stage in [
        AccountStage::Restoring,
        AccountStage::Refreshing,
        AccountStage::WaitingForBrowser,
        AccountStage::Exchanging,
        AccountStage::AwaitingAccountCreation,
    ] {
        let mut c = signed_in();
        c.account.stage = stage;
        assert!(!c.cloud_actionable());
        c.cloud_activate();
        assert!(c.pending.is_none());
        assert!(c.take_cloud_request().is_none());
    }
    let mut c = signed_in();
    c.account.stage = AccountStage::SignedOut;
    assert!(c.cloud_actionable());
    c.cloud_activate();
    assert!(matches!(
        c.pending,
        Some(AccountCommand::StartGoogle { .. })
    ));
}
#[test]
fn learn_more_opens_the_pricing_page_and_the_subscribed_link_still_opens_the_dashboard() {
    let mut c = signed_in();
    c.cloud.status = Status::Subscribe;
    c.perform(Intent::CloudLearnMore);
    let url = c.take_cloud_uri().unwrap();
    assert!(url.ends_with("/pricing"), "{url}");
    assert!(url.starts_with("https://www.koru.do/"), "{url}");
    // The primary button on the same card goes to the same place.
    c.perform(Intent::CloudPrimary);
    assert_eq!(c.take_cloud_uri().unwrap(), url);

    c.cloud.status = Status::Manage;
    // Learn more is gone with the subscribe card; the action is now the link.
    c.perform(Intent::CloudLearnMore);
    assert!(c.take_cloud_uri().is_none());
    assert!(!c.cloud_learn_more_visible());
    c.perform(Intent::CloudPrimary);
    assert!(c.take_cloud_uri().unwrap().ends_with("/dashboard"));
}
#[test]
fn a_refused_write_shows_the_agents_code_verbatim_rather_than_spinning() {
    let mut c = subscribed();
    let work = c.profiles[0].clone();
    answer_sync(&mut c, vec![row(&work, false, 0, 1)]);
    c.perform(Intent::CloudSync(work.id));
    let (_, Some(submission)) = c.take_request_with_submission().unwrap() else {
        panic!("expected a tracked write");
    };
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 4
    }));
    assert!(c.complete_scoped(
        1,
        4,
        AccountOperationResult::Failed {
            code: "workspace_not_authorized".into()
        }
    ));
    assert_eq!(c.cloud_sync_pending_for(work.id), None);
    assert_eq!(
        subtitle(&c, 0).as_deref(),
        Some("Cloud sync: workspace_not_authorized")
    );
    // A command that never reached the Agent is the same story.
    let mut c = subscribed();
    let work = c.profiles[0].clone();
    answer_sync(&mut c, vec![row(&work, false, 0, 1)]);
    c.perform(Intent::CloudSync(work.id));
    let (_, Some(submission)) = c.take_request_with_submission().unwrap() else {
        panic!("expected a tracked write");
    };
    assert!(c.account_command_failed(CommandFailure {
        submission,
        uncertain: false
    }));
    assert_eq!(c.cloud_sync_pending_for(work.id), None);
    assert_eq!(
        subtitle(&c, 0).as_deref(),
        Some("Cloud sync: cloud_sync_unavailable")
    );
}
// ------------------------------------------------- the switch asks nothing

/// A subscribed c with both account Profiles reported off.
fn ready_to_switch_on() -> ProfilesController {
    let mut c = subscribed();
    let rows = vec![
        row(&c.profiles[0].clone(), false, 0, 3),
        row(&c.profiles[1].clone(), false, 0, 1),
    ];
    answer_sync(&mut c, rows);
    c
}

/// **The switch is the whole act** (2026-11-11). Turning it on sends the one
/// command and raises no question — the Agent opens the Store for a Profile
/// whose Cloud sync is on — and turning it off never did.
#[test]
fn turning_the_switch_on_or_off_asks_nothing() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    assert_eq!(c.prompts().count(), 0);
    assert!(matches!(
        c.take_cloud_request(),
        Some(AccountCommand::SetProfileCloudSync { enabled: true, .. })
    ));
}
