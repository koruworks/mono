// SPDX-License-Identifier: MPL-2.0

//! Behaviour of the controller alone: account results, intents and the
//! requests they raise. Where a control is drawn and which one has focus is
//! tested beside the Manage pages (`manage/*/tests.rs`).
use super::*;
use koru_workspace_host::onboarding::OnboardingView;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn scoped_account(scope: u64) -> AccountState {
    AccountState {
        active_scope: scope,
        stage: AccountStage::AwaitingAccountCreation,
        ..AccountState::default()
    }
}

/// The account surface's `index`th action, as the view would map a click on
/// it. Nothing happens when there is no such action, as before.
fn account_action(c: &mut ProfilesController, index: usize) {
    if let Some(action) = c
        .account_surface()
        .and_then(|surface| surface.actions.get(index).copied())
    {
        c.perform(Intent::AccountAction(action));
    }
}

fn offline_profile(name: &str) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    }
}

fn local_profile(profile: ProfileId, identity: &str) -> ProfileSummary {
    ProfileSummary {
        id: profile,
        name: "Existing".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: Some(format!("koru:{identity}")),
        folder: String::new(),
    }
}

fn form(c: &mut ProfilesController) -> &mut OnboardingView {
    c.profile_creation
        .as_mut()
        .map(std::cell::RefCell::get_mut)
        .expect("creation flow")
}

#[test]
fn parked_confirmation_admission_failure_cannot_change_the_visible_form() {
    let mut c = ProfilesController::new();
    c.take_request();
    c.set_account(scoped_account(1));
    c.submit_creation_decision(true);
    let (_, submission) = c.take_request_with_submission().unwrap();
    c.set_account(scoped_account(2));
    c.notice = Some("B form".into());
    assert!(
        c.account_command_failed(koru_workspace_host::account_ui::CommandFailure {
            submission: submission.unwrap(),
            uncertain: true
        })
    );
    assert_eq!(c.account.active_scope, 2);
    assert_eq!(c.notice.as_deref(), Some("B form"));
    assert_eq!(c.account.stage, AccountStage::AwaitingAccountCreation);
    c.set_account(scoped_account(1));
    assert!(!c.creation_decision_pending);
    assert!(c.creation_submission.is_some());
}

#[test]
fn inactive_authorization_ack_emits_local_cas_immediately_and_parks_followup() {
    let mut c = ProfilesController::new();
    c.take_request();
    let profile = offline_profile("A local Profile");
    let id = profile.id;
    c.set_retained_profiles(vec![profile]);
    c.set_account(scoped_account(1));
    c.pending_add_google = Some(PendingAddGoogle {
        candidate_id: "candidate".into(),
        profile_id: id.to_storage(),
        verified_email: "a@example.test".into(),
        profile_name: "A local Profile".into(),
        expected_identity_id: None,
        finishing: true,
        authorization_committed: false,
    });
    c.set_account(scoped_account(2));
    c.notice = Some("B form".into());
    c.complete_scoped(
        1,
        10,
        AccountOperationResult::GoogleAuthorizationAdded {
            koru_identity_id: "11111111-1111-4111-8111-111111111111".into(),
            profile_id: id.to_storage(),
            already_authorized: false,
        },
    );
    assert_eq!(
        c.take_bind_request(),
        Some((id, None, "11111111-1111-4111-8111-111111111111".into()))
    );
    assert_eq!(c.notice.as_deref(), Some("B form"));
    assert!(c.take_request().is_none());
    c.local_profile_account_bound(id);
    assert!(c.take_request().is_none());
    c.set_account(scoped_account(1));
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::ListProfiles)
    ));
    assert!(c.pending_add_google.is_none());
}

#[test]
fn creation_choice_emits_one_correlated_command_and_never_starts_google_again() {
    for confirm in [false, true] {
        let mut c = ProfilesController::new();
        c.take_request();
        c.complete(AccountOperationResult::AccountCreationRequired);
        account_action(&mut c, if confirm { 0 } else { 1 });
        assert_eq!(
            c.take_request(),
            Some(if confirm {
                AccountCommand::ConfirmAccountCreation
            } else {
                AccountCommand::CancelAccountCreation
            })
        );
        assert!(c.account_surface().unwrap().actions.is_empty());
        account_action(&mut c, 0);
        c.perform(Intent::Session);
        assert_eq!(c.take_request(), None);
        assert!(c.take_profile_creation_request().is_none());
        assert!(c.take_bind_request().is_none());
        c.complete(AccountOperationResult::Cancelled);
        assert_eq!(c.account.stage, AccountStage::Cancelled);
        assert!(!c.creation_decision_pending);
    }
}

#[test]
fn local_profile_selection_and_offline_continue_remain_available_during_creation() {
    let mut c = ProfilesController::new();
    c.take_request();
    let profile = offline_profile("Local fixture");
    let id = profile.id;
    c.profiles = vec![profile];
    c.complete(AccountOperationResult::AccountCreationRequired);
    c.perform(Intent::Select(id));
    assert_eq!(c.selected, Some(id));
    assert!(c.account_surface().is_some());
    account_action(&mut c, 2);
    assert!(c.account_surface().is_none());
    assert_eq!(c.take_request(), None);
    assert_eq!(c.profiles[0].account, None);
    assert!(c.take_profile_creation_request().is_none());
}

#[test]
fn creation_expiry_uses_expired_copy_and_restarts_only_on_explicit_action() {
    let mut c = ProfilesController::new();
    c.take_request();
    c.complete(AccountOperationResult::AccountCreationRequired);
    c.complete(AccountOperationResult::Failed {
        code: "account_confirmation_expired".into(),
    });
    let surface = c.account_surface().unwrap();
    assert_eq!(
        surface.title,
        koru_i18n::t(
            crate::format::lang(),
            koru_i18n::Key::AccountCreationExpiredTitle
        )
    );
    assert_eq!(
        surface.body,
        koru_i18n::t(
            crate::format::lang(),
            koru_i18n::Key::AccountCreationExpiredBody
        )
    );
    assert_eq!(c.take_request(), None);
    account_action(&mut c, 0);
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::StartGoogle { .. })
    ));
}

#[test]
fn account_actions_are_commands_not_inline_io() {
    let mut c = ProfilesController::new();
    c.perform(Intent::Session);
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::StartGoogle { .. })
    ));
    c.account.stage = AccountStage::SignedIn;
    c.perform(Intent::Session);
    assert_eq!(c.take_request(), Some(AccountCommand::Logout));
}

#[test]
fn session_email_is_targeted_requested_and_does_not_steal_focus() {
    let mut c = ProfilesController::new();
    assert_eq!(c.take_request(), Some(AccountCommand::Status));
    c.complete(AccountOperationResult::SignedIn);
    assert_eq!(c.account.stage, AccountStage::SignedIn);
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::InspectAccountSession)
    );
    c.complete(AccountOperationResult::SignedInIdentity {
        provider: "google".into(),
        verified_email: "person@example.test".into(),
    });
    assert_eq!(
        c.signed_in_identity,
        Some(("google".into(), "person@example.test".into()))
    );
    assert!(!c.take_hints().reset_focus, "focus stays where it was");
    assert_eq!(c.take_request(), Some(AccountCommand::ListProfiles));

    c.complete(AccountOperationResult::SignedOut);
    assert_eq!(c.account.stage, AccountStage::SignedOut);
    assert!(c.signed_in_identity.is_none());
}

#[test]
fn last_identity_requires_explicit_offline_confirmation() {
    let mut c = ProfilesController::new();
    assert_eq!(c.take_request(), Some(AccountCommand::Status));
    let profile = ProfileId::new();
    c.selected = Some(profile);
    c.account_profiles = vec![AccountProfileRow {
        koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
        profile_id: profile.to_storage(),
        name: Some("Koru".into()),
        locale: Some("en".into()),
        auth_identities: vec![koru_agent_wire::AccountAuthIdentityRow {
            auth_identity_id: "auth_google_1".into(),
            provider: "google".into(),
            verified_email: Some("person@example.com".into()),
        }],
    }];

    c.perform(Intent::Unlink(profile, 0));
    assert!(c.confirm_offline);
    assert_eq!(c.take_request(), None);
    c.perform(Intent::ConfirmOffline);
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::RemoveProfileAuthorization {
            confirm_profile_offline_only: true,
            ..
        })
    ));
}

#[test]
fn embedded_welcome_links_leave_manage_profiles_as_typed_uri_requests() {
    let mut c = ProfilesController::new();
    c.begin_profile_creation();
    let creation = form(&mut c);

    // From the address field: password, its eye, Continue, Google,
    // Apple and offline; the next stop is the Terms of Service link.
    for _ in 0..7 {
        assert!(creation.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(),));
    }
    assert!(creation.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
    c.drain_profile_creation_action();

    assert_eq!(c.take_uri_request(), Some(koru_ui::welcome::TERMS_URL));
}

#[test]
fn signed_in_profile_creation_requests_content_availability_choices() {
    let mut c = ProfilesController::new();
    c.begin_profile_creation();
    c.complete(AccountOperationResult::SignedInIdentity {
        provider: "google".into(),
        verified_email: "person@example.invalid".into(),
    });

    assert_eq!(c.take_request(), Some(AccountCommand::ListProfileChoices));
}

fn choose_first_local_profile(c: &mut ProfilesController) {
    let creation = form(c);
    assert!(creation.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(),));
    assert!(creation.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
    c.drain_profile_creation_action();
}

#[test]
fn retained_hosted_profile_leaves_as_a_typed_restore_request() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    c.begin_profile_creation();
    c.complete(AccountOperationResult::ProfileChoices {
        display_name: "Steven".into(),
        choices: vec![koru_agent_wire::AccountProfileChoice {
            profile: AccountProfileRow {
                koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                profile_id: profile.to_storage(),
                name: Some("Retained".into()),
                locale: Some("en".into()),
                auth_identities: Vec::new(),
            },
            available_on_this_device: true,
            available_on_another_device: false,
        }],
    });

    choose_first_local_profile(&mut c);

    assert_eq!(c.take_profile_restore_request(), Some(profile));
    assert!(c.profile_creation.is_some());
}

#[test]
fn already_visible_hosted_profile_closes_creation_without_restoring_again() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    c.profiles = vec![offline_profile("Already here")];
    c.profiles[0].id = profile;
    c.begin_profile_creation();
    c.complete(AccountOperationResult::ProfileChoices {
        display_name: "Steven".into(),
        choices: vec![koru_agent_wire::AccountProfileChoice {
            profile: AccountProfileRow {
                koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                profile_id: profile.to_storage(),
                name: Some("Already here".into()),
                locale: Some("en".into()),
                auth_identities: Vec::new(),
            },
            available_on_this_device: true,
            available_on_another_device: false,
        }],
    });

    choose_first_local_profile(&mut c);

    assert_eq!(c.take_profile_restore_request(), None);
    assert_eq!(c.take_profile_switch_request(), Some(profile));
    assert_eq!(c.selected(), Some(profile));
    assert_eq!(c.notice.as_deref(), Some("Profile selected"));
    assert!(c.profile_creation.is_none());
}

#[test]
fn add_google_cancel_consumes_the_exact_candidate_on_server() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    c.pending_add_google = Some(PendingAddGoogle {
        candidate_id: "candidate-two".into(),
        profile_id: profile.to_storage(),
        verified_email: "person@example.com".into(),
        profile_name: "Steven".into(),
        expected_identity_id: None,
        finishing: false,
        authorization_committed: false,
    });
    c.perform(Intent::CancelAddGoogle);
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::CancelAddGoogle {
            candidate_id: "candidate-two".into(),
            profile_id: profile.to_storage(),
        })
    );
    assert!(c.pending_add_google.as_ref().unwrap().finishing);
}

#[test]
fn collision_never_binds_and_keeps_exact_retry_separate_from_account_switch() {
    let mut c = ProfilesController::new();
    c.account = AccountState {
        stage: AccountStage::Failed,
        capability: true,
        failure_code: Some("collision_required".into()),
        ..AccountState::default()
    };

    account_action(&mut c, 0);
    assert_eq!(c.take_request(), Some(AccountCommand::Retry));
    assert_eq!(c.take_bind_request(), None);

    c.account_warning_dismissed = false;
    account_action(&mut c, 1);
    assert!(matches!(
        c.take_request(),
        Some(AccountCommand::StartGoogle { .. })
    ));
    assert_eq!(c.take_bind_request(), None);
}

#[test]
fn an_action_the_surface_does_not_offer_does_nothing() {
    use koru_workspace_host::account_ui::Action;
    let mut c = ProfilesController::new();
    c.take_request();
    c.account.stage = AccountStage::SignedIn;
    assert!(c.account_surface().is_none());
    c.perform(Intent::AccountAction(Action::Retry));
    c.perform(Intent::AccountAction(Action::ContactSupport));
    assert_eq!(c.take_request(), None);
    assert_eq!(c.take_effect(), None);
}

#[test]
fn erased_account_rebind_uses_exact_old_and_new_identity_ids_for_local_cas() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    let erased = "00000000-0000-4000-8000-000000000041";
    let replacement = "00000000-0000-4000-8000-000000000042";
    c.profiles = vec![ProfileSummary {
        id: profile,
        name: "Steven".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: Some(format!("koru:{erased}")),
        folder: String::new(),
    }];
    c.selected = Some(profile);
    c.account = AccountState {
        stage: AccountStage::Failed,
        capability: true,
        failure_code: Some("account_erased".into()),
        ..AccountState::default()
    };

    account_action(&mut c, 0);
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::RebindProfile {
            profile_id: profile.to_storage(),
            erased_identity_id: erased.into(),
        })
    );
    assert_eq!(c.take_bind_request(), None);

    c.complete(AccountOperationResult::Profile {
        profile: AccountProfileRow {
            koru_identity_id: replacement.into(),
            profile_id: profile.to_storage(),
            name: Some("Steven".into()),
            locale: Some("en".into()),
            auth_identities: Vec::new(),
        },
    });
    assert_eq!(
        c.take_bind_request(),
        Some((profile, Some(erased.into()), replacement.into()))
    );
    assert!(c.local_profile_account_bound(profile));
    assert!(c.pending_rebind.is_none());
}

#[test]
fn exact_hosted_identity_match_is_not_a_different_account() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    let identity = "00000000-0000-4000-8000-000000000041";
    c.profiles = vec![local_profile(profile, identity)];
    c.account = AccountState {
        stage: AccountStage::SignedIn,
        ..AccountState::default()
    };
    c.signed_in_identity = Some(("google".into(), "person@example.test".into()));
    c.account_profiles_loaded = true;
    c.account_profiles = vec![AccountProfileRow {
        koru_identity_id: identity.into(),
        profile_id: profile.to_storage(),
        name: Some("Existing".into()),
        locale: Some("en".into()),
        auth_identities: Vec::new(),
    }];

    assert!(c.different_account_email().is_none());
    assert!(!c.profile_is_connected_to_other_account(&c.profiles[0]));
}

#[test]
fn authorization_is_exact_profile_scoped_even_inside_one_koru_identity() {
    let mut c = ProfilesController::new();
    let allowed = ProfileId::new();
    let other = ProfileId::new();
    let identity = "00000000-0000-4000-8000-000000000041";
    c.profiles = vec![
        local_profile(allowed, identity),
        local_profile(other, identity),
    ];
    c.account = AccountState {
        stage: AccountStage::SignedIn,
        ..AccountState::default()
    };
    c.signed_in_identity = Some(("google".into(), "person@example.test".into()));
    c.account_profiles_loaded = true;
    c.account_profiles = vec![AccountProfileRow {
        koru_identity_id: identity.into(),
        profile_id: allowed.to_storage(),
        name: Some("Allowed".into()),
        locale: Some("en".into()),
        auth_identities: Vec::new(),
    }];

    assert!(!c.profile_is_connected_to_other_account(&c.profiles[0]));
    assert!(c.profile_is_connected_to_other_account(&c.profiles[1]));
}

#[test]
fn enrollment_failure_routes_directly_to_enrollment_retry() {
    let mut c = ProfilesController::new();
    c.account = AccountState {
        enrollment: koru_agent_wire::EnrollmentState::Failed,
        enrollment_failure_code: Some("trust_persistence_failed".into()),
        ..AccountState::default()
    };
    account_action(&mut c, 0);
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::RetryEnrollment { profile_id: None })
    );
}

#[test]
fn selected_profile_retry_and_progress_do_not_borrow_sibling_state() {
    let mut c = ProfilesController::new();
    let first = ProfileId::new();
    let second = ProfileId::new();
    c.selected = Some(first);
    c.account = AccountState {
        stage: AccountStage::SignedIn,
        enrollment: koru_agent_wire::EnrollmentState::Failed,
        enrollments: vec![
            koru_agent_wire::AccountProfileEnrollment {
                profile_id: first.to_string(),
                state: koru_agent_wire::EnrollmentState::Failed,
                failure_code: Some("trust_persistence_failed".into()),
                retry_at_millis: None,
            },
            koru_agent_wire::AccountProfileEnrollment {
                profile_id: second.to_string(),
                state: koru_agent_wire::EnrollmentState::Complete,
                failure_code: None,
                retry_at_millis: None,
            },
        ],
        ..AccountState::default()
    };
    account_action(&mut c, 0);
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::RetryEnrollment {
            profile_id: Some(first.to_string())
        })
    );
    c.selected = Some(second);
    c.account_warning_dismissed = false;
    assert!(c.account_surface().is_none_or(|surface| {
        !surface
            .actions
            .contains(&koru_workspace_host::account_ui::Action::RetryEnrollment)
    }));
    // The surface is asked of a Profile, not only of the selection.
    assert!(c.account_surface_for(Some(first)).is_some_and(|surface| {
        surface
            .actions
            .contains(&koru_workspace_host::account_ui::Action::RetryEnrollment)
    }));
}

#[test]
fn unavailable_account_is_nonblocking_and_keeps_retry_distinct() {
    let mut c = ProfilesController::new();
    let surface = c.account_surface().expect("unavailable surface");
    assert_eq!(surface.title, "Koru can't connect right now");
    assert_eq!(
        surface.actions,
        vec![
            koru_workspace_host::account_ui::Action::Retry,
            koru_workspace_host::account_ui::Action::ContinueOffline,
        ]
    );
    account_action(&mut c, 0);
    assert_eq!(c.take_request(), Some(AccountCommand::Retry));
    assert_eq!(c.take_bind_request(), None);
}

// ---- the one way out ---------------------------------------------------------

/// `take_effect` drains every slot, in the order the window always asked in: a
/// page before anything else, the local Store write before the Agent command
/// that follows it, the command last.
#[test]
fn effects_leave_in_the_order_the_window_asked_for_them() {
    let mut c = ProfilesController::new();
    let profile = ProfileId::new();
    c.pending = Some(AccountCommand::ListProfiles);
    c.continue_offline_request = Some((profile, "identity".into()));
    c.hosted_delete_request = Some((profile, true));
    c.remove_request = Some((profile, false));
    c.bind_request = Some((profile, None, "next".into()));
    c.profile_switch_request = Some(profile);
    c.profile_restore_request = Some(profile);
    c.uri_request = Some(koru_ui::welcome::CHAT_URL);
    c.cloud_uri = Some("https://www.koru.do/pricing".into());

    let mut order = Vec::new();
    while let Some(effect) = c.take_effect() {
        order.push(effect);
    }
    assert_eq!(
        order,
        vec![
            Effect::OpenWebsite("https://www.koru.do/pricing".into()),
            Effect::OpenUri(koru_ui::welcome::CHAT_URL),
            Effect::RestoreLocalProfile(profile),
            Effect::SwitchProfile(profile),
            Effect::BindAccount {
                profile,
                expected_identity_id: None,
                new_identity_id: "next".into(),
            },
            Effect::RemoveLocalProfile {
                profile,
                delete_workspaces: false,
                preview: None,
            },
            Effect::FinishHostedDeletion {
                profile,
                discovered_after_offline: true,
            },
            Effect::ContinueDeletedOffline {
                profile,
                expected_identity_id: "identity".into(),
            },
            Effect::Account {
                owner_scope: 0,
                command: AccountCommand::ListProfiles,
                submission: None,
            },
        ]
    );
}

/// A creation draft names the form it came from, so it is created for that
/// account even after another one became active.
#[test]
fn a_creation_draft_leaves_with_its_owner_scope() {
    let mut c = ProfilesController::new();
    c.take_request();
    c.set_account(scoped_account(4));
    c.take_request();
    c.profile_creation_request = Some(ConnectedProfileDraft {
        id: koru_core::ProfileId::new(),
        owner_display_name: "Steven".into(),
        profile_name: "Work".into(),
        workspace_name: "Notes".into(),
        template: Default::default(),
        workspace_icon: None,
        workspace_color: None,
        profile_icon: None,
        profile_color: None,
        connect_account: false,
    });
    assert!(matches!(
        c.take_effect(),
        Some(Effect::CreateConnectedProfile { owner_scope: 4, .. })
    ));
}

/// A cascade refused for want of a first-party grant fails the same way on
/// every retry, so it must not say "try again" — the sentence a cascade that
/// stopped part-way rightly says. The two codes are two sentences.
#[test]
fn a_cascade_this_window_may_not_run_does_not_ask_for_a_retry() {
    let mut c = ProfilesController::new();
    let profile = offline_profile("Work");
    let id = profile.id;
    c.profiles = vec![profile];
    c.selected = Some(id);
    let lang = crate::format::lang();

    assert!(c.local_profile_remove_failed_for(id, "profile_cascade_not_permitted"));
    assert_eq!(
        c.notice(),
        Some(koru_i18n::t(
            lang,
            koru_i18n::Key::ProfilesRemoveNotPermitted
        ))
    );

    assert!(c.local_profile_remove_failed_for(id, "profile_cascade_incomplete"));
    assert_eq!(
        c.notice(),
        Some(koru_i18n::t(
            lang,
            koru_i18n::Key::ProfilesRemoveCascadeIncomplete
        ))
    );

    let refused = koru_i18n::t(
        koru_i18n::Lang::En,
        koru_i18n::Key::ProfilesRemoveNotPermitted,
    );
    assert!(!refused.contains("Try again"), "{refused}");
    assert!(refused.contains("Nothing was deleted"), "{refused}");
    assert!(
        koru_i18n::t(
            koru_i18n::Lang::En,
            koru_i18n::Key::ProfilesRemoveCascadeIncomplete
        )
        .contains("Try again")
    );
}

/// The notice is drawn under the selected Profile's header. "Profile removed
/// from this device" stays while the page settles on its own, and goes as soon
/// as the person picks a different Profile — under which it would read as that
/// Profile's news.
#[test]
fn the_removal_notice_does_not_follow_the_person_to_another_profile() {
    let mut c = ProfilesController::new();
    let profiles = vec![
        offline_profile("Gone"),
        offline_profile("First"),
        offline_profile("Other"),
    ];
    let (gone, first, other) = (profiles[0].id, profiles[1].id, profiles[2].id);
    c.profiles = profiles;
    c.selected = Some(gone);

    assert!(c.local_profile_removed(gone));
    let removed = koru_i18n::t(
        crate::format::lang(),
        koru_i18n::Key::ProfilesRemovedFromDevice,
    );
    assert_eq!(c.selected(), Some(first));
    assert_eq!(c.notice(), Some(removed));

    // The page falling back by itself, and re-selecting the same row, are not
    // the person moving on.
    c.follow_page(other);
    assert_eq!(c.notice(), Some(removed), "a settling page lost the result");
    c.perform(Intent::Select(other));
    assert_eq!(c.notice(), Some(removed), "re-selecting cleared the result");

    c.perform(Intent::Select(first));
    assert_eq!(c.selected(), Some(first));
    assert_eq!(
        c.notice(),
        None,
        "the last Profile's notice stayed under a different one"
    );
}
