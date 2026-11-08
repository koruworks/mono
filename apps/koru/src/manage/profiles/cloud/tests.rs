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
            current: Some(true)
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
            current: None
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
            current: Some(false)
        }
    ));
    assert!(c.complete_scoped(
        1,
        8,
        AccountOperationResult::CloudSubscription {
            request_id: new_id,
            current: Some(true)
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
            current: Some(true)
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
            current: Some(true)
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
// ------------------------------------------------- the Profile prompt (D258)

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

/// **Turning it on is the Profile being spoken about** (D258), so that is
/// when the question about its Workspaces is asked — and the hosted switch
/// goes out either way, because the two are different decisions.
#[test]
fn switching_a_profile_on_asks_about_its_workspaces() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);

    let (asked, confirm) = c.cloud.move_prompt.as_ref().expect("the prompt");
    assert_eq!(*asked, profile);
    assert_eq!(
        confirm.title(),
        "Move this Profile's Workspaces to Koru Cloud?"
    );
    assert!(
        confirm.body().contains("Work"),
        "the question names the Profile it is about: {}",
        confirm.body(),
    );
    // **Enter answers nothing until a button is chosen** (ADR-265, row four).
    // The corner answer starts sending gigabytes.
    assert_eq!(confirm.default_action(), None);

    // The hosted half is a different decision and still goes out.
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::SetProfileCloudSync {
            profile_id,
            enabled: true,
            ..
        }) if profile_id == profile.to_storage()
    ));
}

/// **Move everything** carries the Workspaces; **Just this Profile** moves the
/// Profile alone. Both write, because the Profile is moving either way — the
/// person said so with the switch — and the prompt only ever asked about its
/// children.
///
/// This test used to assert that `Cancel` wrote `(profile, false)`. That was
/// pinning D259's third defect: the dialog had no dismissal, so Escape and the
/// close gesture committed a Profile to Cloud. *Just this Profile* is now the
/// middle answer and carries that meaning; `Cancel` is asserted below to write
/// nothing at all.
#[test]
fn the_two_writing_answers_are_with_the_workspaces_and_without_them() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;

    c.cloud_sync_activate(profile);
    assert!(c.settle_move_prompt(ConfirmOutcome::Confirm));
    assert_eq!(c.cloud.move_request, Some((profile, true)));
    assert!(c.cloud.move_prompt.is_none());

    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    assert!(c.settle_move_prompt(ConfirmOutcome::Alternative));
    assert_eq!(
        c.cloud.move_request,
        Some((profile, false)),
        "its Workspaces stay where they are, capped beneath it",
    );
}

/// **Cancel cancels** (D259). No write, no mode change, and the question can
/// be asked again.
///
/// Escape is the same answer — `Confirm::key` maps it to `Cancel`, as it does
/// in every modal in this app — so this covers the keyboard and the button at
/// once, which is the whole reason `Confirm` has one `answer` function.
#[test]
fn cancelling_the_move_prompt_writes_nothing_and_changes_no_mode() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    assert!(c.cloud.move_prompt.is_some());
    // The hosted switch is its own decision and has already gone out; the
    // dialog's dismissal must not take it back and must not add to it.
    let hosted = c.take_request();
    assert!(matches!(
        hosted,
        Some(AccountCommand::SetProfileCloudSync { enabled: true, .. })
    ));

    assert!(c.settle_move_prompt(ConfirmOutcome::Cancel));
    assert!(c.cloud.move_prompt.is_none(), "the dialog is dismissed");
    assert_eq!(c.cloud.move_request, None, "no transport write queued");
    assert!(!c.cloud_move_pending(), "nothing is in flight");
    assert_eq!(c.take_request(), None, "no second hosted command");
    assert_eq!(
        c.notice.as_deref(),
        Some("Nothing moved. Turn Cloud sync off and on again to be asked once more."),
    );

    // And it may be asked again — the switch is the trigger, and dismissing
    // the question did not answer it.
    c.cloud.set_request = None;
    c.cloud.sync_pending = None;
    c.cloud_sync_activate(profile);
    assert!(
        c.cloud.move_prompt.is_some(),
        "the question comes back rather than having been answered by Escape",
    );
}

/// Escape reaches `settle_move_prompt` as `Cancel`, from the dialog itself.
///
/// Asserted on `Confirm` rather than inferred, because the mapping is the
/// thing that was wrong: the code was right on both sides of it and the
/// sentence in between — *Cancel means Just this Profile* — was the defect.
#[test]
fn escape_answers_the_move_prompt_with_cancel() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    let (_, confirm) = c.cloud.move_prompt.as_mut().expect("the prompt");
    let outcome = confirm.key(&winit::keyboard::Key::Named(
        winit::keyboard::NamedKey::Escape,
    ));
    assert_eq!(outcome, ConfirmOutcome::Cancel);
    assert!(c.settle_move_prompt(outcome));
    assert_eq!(c.cloud.move_request, None);
}

/// **Turning it off raises nothing.** That is D227's immediate stop; it
/// already has its behaviour, and a second dialog there would be a second
/// answer to a settled question. The row says plainly what it does instead.
#[test]
fn switching_a_profile_off_asks_nothing_and_says_what_it_does() {
    let mut c = subscribed();
    let rows = vec![
        row(&c.profiles[0].clone(), true, 3, 3),
        row(&c.profiles[1].clone(), false, 0, 1),
    ];
    answer_sync(&mut c, rows);
    let profile = c.profiles[0].id;

    c.cloud_sync_activate(profile);
    assert!(c.cloud.move_prompt.is_none());
    assert_eq!(c.cloud.move_request, None);
    assert_eq!(
        subtitle(&c, 0).as_deref(),
        Some("Turning off — transfers stop on this device now"),
    );
}

/// **On, answered, then off leaves the durable state the person last chose.**
/// Switching off is D227's immediate stop on the hosted side and writes no
/// transport mode at all, so nothing here queues a second group — the rows
/// keep what the prompt committed, and turning the switch back on asks again
/// rather than guessing.
#[test]
fn on_then_off_queues_no_second_transport_write() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;

    c.cloud_sync_activate(profile);
    assert!(c.settle_move_prompt(ConfirmOutcome::Confirm));
    assert_eq!(c.cloud.move_request, Some((profile, true)));
    c.drain_transport_answer();
    assert_eq!(c.cloud.move_request, None);

    // The switch settles on, then the person turns it off again.
    let rows = vec![
        row(&c.profiles[0].clone(), true, 3, 3),
        row(&c.profiles[1].clone(), false, 0, 1),
    ];
    c.cloud.sync = Some(rows);
    c.cloud.set_request = None;
    c.cloud.sync_pending = None;
    c.cloud_sync_activate(profile);

    assert!(c.cloud.move_prompt.is_none(), "off asks nothing");
    assert_eq!(c.cloud.move_request, None, "off writes no mode");
    assert!(!c.cloud_move_pending());
}

/// A window that never discovered a Store claims nothing. The hosted toggle
/// has done its half; the device-local mode has not been written, and the
/// notice carries the same kind of code a refusal would.
#[test]
fn an_answer_with_no_store_reports_a_refusal_rather_than_success() {
    let mut c = ready_to_switch_on();
    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    assert!(c.settle_move_prompt(ConfirmOutcome::Confirm));

    assert!(c.drain_transport_answer());
    assert_eq!(c.cloud.move_request, None);
    assert_eq!(
        c.notice.as_deref(),
        Some("Could not move to Koru Cloud: store_unreachable"),
    );
}

/// The prompt is the only clock this c contributes, and it is gone the
/// moment the answer has been dealt with — a settings tab nobody is waiting
/// on wakes the loop for nothing.
#[test]
fn an_idle_profiles_tab_contributes_no_clock() {
    let mut c = ready_to_switch_on();
    assert_eq!(c.maintenance_deadline(), None);

    let profile = c.profiles[0].id;
    c.cloud_sync_activate(profile);
    c.settle_move_prompt(ConfirmOutcome::Confirm);
    assert!(c.maintenance_deadline().is_some());

    c.drain_transport_answer();
    assert_eq!(c.maintenance_deadline(), None);
}

/// One question at a time. A second switch while the first is unanswered is
/// dropped rather than queued — the Store's answer is the truth, and two
/// groups in flight would race each other.
#[test]
fn a_second_switch_does_not_stack_a_second_prompt() {
    let mut c = ready_to_switch_on();
    let first = c.profiles[0].id;
    let second = c.profiles[1].id;
    c.cloud_sync_activate(first);
    c.cloud_sync_activate(second);
    let (asked, _) = c.cloud.move_prompt.as_ref().expect("one prompt");
    assert_eq!(*asked, first);
}
