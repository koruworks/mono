// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_agent_wire::{AccountCommand, AccountOperationResult, AccountStage, EnrollmentState};

fn id(n: u8) -> String {
    format!("10000000-0000-4000-8000-{n:012}")
}
fn state(scope: u64) -> AccountState {
    AccountState {
        active_scope: scope,
        generation: 10,
        stage: AccountStage::SignedIn,
        active_account_id: Some(id(scope as u8)),
        held_accounts: vec![koru_agent_wire::AccountHeldAccount {
            scope_id: scope,
            account_id: id(scope as u8),
            auth_identity_id: Some(id(90 + scope as u8)),
            stage: AccountStage::SignedIn,
            credential_ready: true,
            failure_code: None,
            active_generations: vec![],
        }],
        ..Default::default()
    }
}
fn choices(scope: u64) -> Vec<AccountProfileChoice> {
    (1..=3)
        .map(|n| AccountProfileChoice {
            profile: AccountProfileRow {
                koru_identity_id: id(scope as u8),
                profile_id: id(n + scope as u8 * 10),
                name: Some(format!("Profile {n}")),
                locale: None,
                auth_identities: vec![koru_agent_wire::AccountAuthIdentityRow {
                    auth_identity_id: id(90 + scope as u8),
                    provider: "google".into(),
                    verified_email: None,
                }],
            },
            available_on_this_device: n == 1,
            available_on_another_device: n == 2,
        })
        .collect()
}
fn view() -> OnboardingView {
    let mut view = OnboardingView::default();
    view.select_account_context(&state(1));
    view.show_account_profile_choices("", choices(1));
    view
}
fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// **A Profile already on this computer is not an import choice.** Its row
/// says it is here and offers *Open local content*; it has no checkbox, is
/// not counted in *N selected*, and is never sent in a selection. It used to
/// be ticked like the rest, which read as importing it a second time.
#[test]
fn a_profile_already_here_has_no_checkbox_and_opens_from_its_own_button() {
    let mut view = view();
    view.account_state.enrollments = vec![koru_agent_wire::AccountProfileEnrollment {
        profile_id: id(11),
        state: EnrollmentState::Complete,
        failure_code: None,
        retry_at_millis: None,
    }];
    view.set_viewport(900.0, 650.0);
    let row = view.selection_layout().rows[0].rect;
    assert_eq!(
        view.selection_hit(row.x + 20.0, row.y + 18.0),
        None,
        "a press on its body ticks nothing"
    );
    assert!(!view.selection_controls().contains(&Hit::Existing(0)));
    view.activate(Hit::Existing(0));
    assert!(view.selection_ids().is_empty());
    assert_eq!(view.selection_count(), "0 selected");
    assert!(!view.selection_controls().contains(&Hit::Primary));
    assert_eq!(
        view.selection_layout().rows[0].source,
        "Already on this device"
    );

    assert!(view.selection_controls().contains(&Hit::OpenExisting(0)));
    view.activate(Hit::OpenExisting(0));
    assert_eq!(
        view.take_action(),
        Some(Action::RestoreLocalProfile { profile_id: id(11) })
    );
}

/// A tick left on a Profile from before it arrived here does not survive it
/// arriving: the next list prunes it, so the count and the selection agree
/// with what is drawn.
#[test]
fn a_tick_on_a_profile_that_has_since_arrived_here_is_dropped() {
    let mut view = view();
    view.activate(Hit::Existing(1));
    assert_eq!(view.selection_ids(), vec![id(12)]);
    let mut arrived = choices(1);
    arrived[1].available_on_this_device = true;
    view.show_account_profile_choices("", arrived);
    assert!(view.selection_ids().is_empty());
    assert!(!view.selection.checked.contains(&id(12)));
}

#[test]
fn a_selection_sends_only_profiles_that_are_not_here_and_opens_the_first_once_durable() {
    let mut view = view();
    view.account_state.enrollments = vec![koru_agent_wire::AccountProfileEnrollment {
        profile_id: id(11),
        state: EnrollmentState::Complete,
        failure_code: None,
        retry_at_millis: None,
    }];
    view.activate(Hit::Existing(0));
    view.activate(Hit::Existing(1));
    view.activate(Hit::Primary);
    assert_eq!(
        view.take_action(),
        Some(Action::SelectAccountProfiles {
            profile_ids: vec![id(12)]
        }),
        "the Profile already here is not sent"
    );
    view.profiles_selected(vec![koru_agent_wire::AccountProfileEnrollment {
        profile_id: id(12),
        state: EnrollmentState::Waiting,
        failure_code: None,
        retry_at_millis: None,
    }]);
    assert_eq!(
        view.take_action(),
        Some(Action::RestoreLocalProfile { profile_id: id(12) })
    );
}

#[test]
fn first_selected_profile_opens_as_soon_as_it_is_durable_while_others_continue() {
    let mut view = view();
    view.activate(Hit::Existing(1));
    view.activate(Hit::Existing(2));
    view.activate(Hit::Primary);
    view.take_action();
    view.profiles_selected(vec![
        koru_agent_wire::AccountProfileEnrollment {
            profile_id: id(12),
            state: EnrollmentState::Waiting,
            failure_code: None,
            retry_at_millis: None,
        },
        koru_agent_wire::AccountProfileEnrollment {
            profile_id: id(13),
            state: EnrollmentState::Complete,
            failure_code: None,
            retry_at_millis: None,
        },
    ]);
    assert_eq!(
        view.take_action(),
        Some(Action::RestoreLocalProfile { profile_id: id(12) })
    );
    let mut projection = state(1);
    projection.enrollments = view.account_state.enrollments.clone();
    view.show_account_state(&projection);
    assert!(
        view.take_action().is_none(),
        "do not launch a second restore"
    );
}

#[test]
fn checkboxes_capture_only_explicit_current_grant_choices() {
    let mut draft = SelectionDraft::default();
    draft.toggle("b");
    draft.toggle("a");
    assert_eq!(draft.choice(&set(&["a", "b"]), &set(&[])), vec!["a", "b"]);
    draft.toggle("c");
    let submitted = draft.choice(&set(&["a", "b", "c"]), &set(&["b"]));
    assert_eq!(submitted, vec!["a", "c"]);
    assert!(draft.begin(submitted));
    draft.reconcile(&set(&["c", "d"]));
    assert_eq!(
        draft.submitted.as_ref().unwrap(),
        &vec!["a".to_owned(), "c".to_owned()]
    );
    assert!(!draft.begin(vec!["d".into()]));
}

#[test]
fn completion_during_cancellation_does_not_open_a_profile() {
    let mut view = view();
    view.activate(Hit::Existing(1));
    view.activate(Hit::Primary);
    view.take_action();
    view.account_command_failed(crate::account_ui::CommandFailure {
        submission: view.selection.submission.unwrap(),
        uncertain: true,
    });
    view.activate(Hit::CancelSelection);
    assert_eq!(
        view.take_action(),
        Some(Action::Account(AccountCommand::CancelEnrollment))
    );
    let mut projection = state(1);
    projection.enrollments = vec![koru_agent_wire::AccountProfileEnrollment {
        profile_id: id(12),
        state: EnrollmentState::Complete,
        failure_code: None,
        retry_at_millis: None,
    }];
    view.show_account_state(&projection);
    assert!(view.take_action().is_none());
    assert_eq!(view.screen, Screen::ExistingProfiles);
}

#[test]
fn grant_removal_and_completed_members_cannot_be_resubmitted() {
    let mut draft = SelectionDraft::default();
    draft.toggle("a");
    draft.toggle("b");
    draft.reconcile(&set(&["b", "c"]));
    assert_eq!(draft.checked, set(&["b"]));
    draft.toggle("c");
    assert_eq!(draft.choice(&set(&["b", "c"]), &set(&["b"])), vec!["c"]);
}

#[test]
fn admission_failure_preserves_draft_and_uncertainty_preserves_exact_operation() {
    let mut draft = SelectionDraft::default();
    draft.toggle("a");
    draft.begin(vec!["a".into()]);
    let submission = draft.submission.unwrap();
    assert!(!draft.failed(crate::account_ui::CommandFailure {
        submission: crate::account_ui::CommandSubmissionId::next(),
        uncertain: false
    }));
    assert!(draft.failed(crate::account_ui::CommandFailure {
        submission,
        uncertain: true
    }));
    draft.toggle("b");
    assert_eq!(draft.checked, set(&["a"]));
    assert_eq!(draft.submitted, Some(vec!["a".into()]));
    draft.settled();
    assert!(draft.begin(vec!["a".into()]));
    let submission = draft.submission.unwrap();
    assert!(draft.failed(crate::account_ui::CommandFailure {
        submission,
        uncertain: false
    }));
    assert!(!draft.locked());
    assert_eq!(draft.checked, set(&["a"]));
}

#[test]
fn real_view_submits_one_subset_and_all_once_and_keeps_unavailable_members() {
    for (indices, expected) in [
        (vec![2], vec![id(13)]),
        (vec![1, 2], vec![id(12), id(13)]),
        // The row at 0 is already on this computer: ticking it does nothing.
        (vec![0, 1, 2], vec![id(12), id(13)]),
    ] {
        let mut view = view();
        for index in indices {
            view.activate(Hit::Existing(index));
        }
        view.activate(Hit::Primary);
        assert_eq!(
            view.take_action(),
            Some(Action::SelectAccountProfiles {
                profile_ids: expected.clone()
            })
        );
        view.activate(Hit::Primary);
        assert_eq!(view.take_action(), None);
        let mut next = choices(1);
        next.remove(2);
        view.show_account_profile_choices("", next);
        assert_eq!(view.selection.submitted, Some(expected));
        assert!(!view.selection.checked.contains(&id(13)));
    }
}

#[test]
fn other_auth_and_other_account_rows_neither_appear_nor_enter_all() {
    let mut view = view();
    let mut rows = choices(1);
    rows[0].profile.auth_identities[0].auth_identity_id = id(99);
    rows[1].profile.koru_identity_id = id(2);
    view.show_account_profile_choices("", rows);
    assert_eq!(view.account_profiles.len(), 1);
    view.activate(Hit::Existing(0));
    view.activate(Hit::Primary);
    assert_eq!(
        view.take_action(),
        Some(Action::SelectAccountProfiles {
            profile_ids: vec![id(13)]
        })
    );
}

#[test]
fn mixed_progress_keeps_local_open_and_exact_member_retry_without_reenrolling_complete() {
    let mut view = view();
    let mut projection = state(1);
    projection.enrollments = vec![
        koru_agent_wire::AccountProfileEnrollment {
            profile_id: id(11),
            state: EnrollmentState::Complete,
            failure_code: None,
            retry_at_millis: None,
        },
        koru_agent_wire::AccountProfileEnrollment {
            profile_id: id(13),
            state: EnrollmentState::Waiting,
            failure_code: Some("no_peer_online".into()),
            retry_at_millis: Some(1000),
        },
    ];
    view.show_account_state(&projection);
    assert!(view.controls().contains(&Hit::OpenExisting(0)));
    assert!(view.controls().contains(&Hit::RetryExisting(2)));
    view.activate(Hit::Existing(1));
    view.activate(Hit::Existing(2));
    assert_eq!(view.selection_ids(), vec![id(12), id(13)]);
    view.activate(Hit::RetryExisting(2));
    assert_eq!(
        view.take_action(),
        Some(Action::Account(AccountCommand::RetryEnrollment {
            profile_id: Some(id(13))
        }))
    );
    view.activate(Hit::OpenExisting(0));
    assert_eq!(
        view.take_action(),
        Some(Action::RestoreLocalProfile { profile_id: id(11) })
    );
}

#[test]
fn parked_drafts_intersect_new_grants_without_changing_visible_account() {
    let mut view = view();
    view.activate(Hit::Existing(0));
    view.activate(Hit::Existing(2));
    view.select_account_context(&state(2));
    view.show_account_profile_choices("", choices(2));
    view.activate(Hit::Existing(1));
    let mut refreshed = choices(1);
    refreshed.remove(0);
    view.accept_parked_account_result(
        1,
        &AccountOperationResult::ProfileChoices {
            display_name: String::new(),
            choices: refreshed,
        },
    );
    assert_eq!(view.account_scope(), 2);
    assert_eq!(view.selection.checked, set(&[&id(22)]));
    view.select_account_context(&state(1));
    assert_eq!(view.selection.checked, set(&[&id(13)]));
    view.activate(Hit::Primary);
    assert_eq!(
        view.take_action(),
        Some(Action::SelectAccountProfiles {
            profile_ids: vec![id(13)]
        })
    );
}

#[test]
fn result_before_ack_and_old_failure_after_cancel_are_correlated_by_submission() {
    let mut view = view();
    view.activate(Hit::Existing(1));
    view.activate(Hit::Primary);
    view.take_action();
    let original = view.selection.submission.unwrap();
    assert!(view.selection_result(
        1,
        11,
        &AccountOperationResult::Failed {
            code: "enrollment_unavailable".into()
        }
    ));
    assert!(view.selection.waiting);
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission: original,
        owner_scope: 1,
        generation: 11,
    });
    assert!(!view.selection.locked());
    view.activate(Hit::Primary);
    view.take_action();
    let original = view.selection.submission.unwrap();
    view.account_command_failed(crate::account_ui::CommandFailure {
        submission: original,
        uncertain: true,
    });
    assert!(view.selection.uncertain);
    assert!(view.controls().contains(&Hit::CancelSelection));
    view.activate(Hit::CancelSelection);
    assert_eq!(
        view.take_action(),
        Some(Action::Account(AccountCommand::CancelEnrollment))
    );
    let cancel = view.selection.submission.unwrap();
    view.selection_result(
        1,
        12,
        &AccountOperationResult::Failed {
            code: "old_selection_failure".into(),
        },
    );
    assert!(view.selection.waiting);
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission: cancel,
        owner_scope: 1,
        generation: 13,
    });
    assert!(
        view.selection.waiting,
        "old selection failure cannot settle Cancel"
    );
    view.selection_result(1, 13, &AccountOperationResult::Cancelled);
    assert!(!view.selection.locked());
    assert_eq!(view.screen, Screen::ExistingProfiles);
}

#[test]
fn every_locale_wraps_controls_and_progress_inside_supported_small_windows() {
    for lang in koru_i18n::Lang::ALL {
        for (width, height) in [(760.0, 520.0), (960.0, 640.0), (1200.0, 800.0)] {
            let mut view = view();
            view.language = lang;
            view.set_viewport(width, height);
            view.account_profiles[0].name=Some("A deliberately long Profile title that must remain inside its row and never cover another action".repeat(3));
            let layout = view.selection_layout();
            assert!(
                layout.viewport.h >= 30.0,
                "{lang:?} {width}x{height}: {}",
                layout.viewport.h
            );
            assert!(layout.body.y + layout.body.h < layout.viewport.y);
            for (_, rect, _, _) in &layout.footer {
                assert!(
                    rect.x >= 0.0
                        && rect.x + rect.w <= width
                        && rect.y >= layout.viewport.y + layout.viewport.h
                        && rect.y + rect.h <= height
                );
            }
            for row in &layout.rows {
                if let Some((_, rect, _)) = row.action {
                    assert!(
                        rect.x >= row.rect.x
                            && rect.x + rect.w <= row.rect.x + row.rect.w
                            && rect.y + rect.h <= row.rect.y + row.rect.h
                    );
                }
            }
            let mut scene = Scene::new();
            view.draw_selection(&mut scene, Affine::IDENTITY);
        }
    }
}

#[test]
fn keyboard_space_toggles_a_checked_row_and_escape_uses_owned_cancel() {
    let mut view = view();
    view.focus = view
        .controls()
        .iter()
        .position(|hit| *hit == Hit::Existing(1))
        .unwrap();
    assert!(view.key(&Key::Character(" ".into()), None, ModifiersState::empty()));
    assert_eq!(view.selection.checked, set(&[&id(12)]));
    view.activate(Hit::Primary);
    view.take_action();
    let submission = view.selection.submission.unwrap();
    view.account_command_failed(crate::account_ui::CommandFailure {
        submission,
        uncertain: true,
    });
    assert!(view.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    assert_eq!(
        view.take_action(),
        Some(Action::Account(AccountCommand::CancelEnrollment))
    );
}

#[test]
fn all_captures_the_explicit_choice_and_new_grants_do_not_expand_it_before_submit() {
    let mut view = view();
    view.activate(Hit::Existing(0));
    view.activate(Hit::Existing(1));
    view.activate(Hit::Existing(2));
    let mut expanded = choices(1);
    let mut added = expanded[0].clone();
    added.profile.profile_id = id(14);
    expanded.push(added);
    view.show_account_profile_choices("", expanded);
    assert_eq!(view.selection.checked, set(&[&id(12), &id(13)]));
    view.activate(Hit::Primary);
    assert_eq!(
        view.take_action(),
        Some(Action::SelectAccountProfiles {
            profile_ids: vec![id(12), id(13)]
        })
    );
}

fn submit(view: &mut OnboardingView) -> crate::account_ui::CommandSubmissionId {
    view.activate(Hit::Existing(1));
    view.activate(Hit::Primary);
    view.take_action();
    view.selection.submission.unwrap()
}

#[test]
fn same_account_success_or_cancel_before_ack_only_settles_its_exact_submission() {
    for cancel in [false, true] {
        let mut a = view();
        let mut b = view();
        let mut sa = submit(&mut a);
        let mut sb = submit(&mut b);
        if cancel {
            for view in [&mut a, &mut b] {
                let submission = view.selection.submission.unwrap();
                view.account_command_failed(crate::account_ui::CommandFailure {
                    submission,
                    uncertain: true,
                });
                view.activate(Hit::CancelSelection);
                view.take_action();
            }
            sa = a.selection.submission.unwrap();
            sb = b.selection.submission.unwrap();
        }
        let result = if cancel {
            AccountOperationResult::Cancelled
        } else {
            AccountOperationResult::ProfilesSelected {
                profiles: vec![koru_agent_wire::AccountProfileEnrollment {
                    profile_id: id(12),
                    state: EnrollmentState::Waiting,
                    failure_code: None,
                    retry_at_millis: None,
                }],
            }
        };
        for view in [&mut a, &mut b] {
            assert!(view.selection_result(1, 11, &result));
            assert!(view.selection.waiting);
        }
        a.account_command_admitted(crate::account_ui::CommandAdmission {
            submission: sa,
            owner_scope: 1,
            generation: 11,
        });
        assert!(!a.selection.locked());
        b.account_command_admitted(crate::account_ui::CommandAdmission {
            submission: sb,
            owner_scope: 1,
            generation: 12,
        });
        assert!(
            b.selection.waiting,
            "another form's pre-ACK result cannot finish B"
        );
        b.selection_result(1, 12, &result);
        assert!(!b.selection.locked());
    }
}

#[test]
fn choice_refresh_preserves_cancel_kind_and_uncertainty_until_exact_cancel_result() {
    let mut view = view();
    let submission = submit(&mut view);
    view.account_command_failed(crate::account_ui::CommandFailure {
        submission,
        uncertain: true,
    });
    let error = view.selection.notice;
    view.show_account_profile_choices("", choices(1));
    assert_eq!(view.selection.notice, error);
    assert!(view.selection.uncertain);
    view.activate(Hit::CancelSelection);
    view.take_action();
    let submission = view.selection.submission.unwrap();
    view.show_account_profile_choices("", choices(1));
    assert!(view.enrollment_cancelling);
    assert!(view.selection.waiting);
    view.selection_result(1, 12, &AccountOperationResult::Cancelled);
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 12,
    });
    assert!(!view.selection.locked());
    assert!(!view.enrollment_cancelling);
}

#[test]
fn early_result_overflow_is_bounded_uncertainty_with_an_explicit_cancel_escape() {
    let mut view = view();
    submit(&mut view);
    for generation in 11..=19 {
        view.selection_result(
            1,
            generation,
            &AccountOperationResult::Failed {
                code: "other_form".into(),
            },
        );
    }
    assert_eq!(view.selection.early_results.len(), 8);
    assert!(view.selection.uncertain);
    assert!(!view.selection.waiting);
    assert!(view.account_state.enrollments.is_empty());
    assert!(view.controls().contains(&Hit::CancelSelection));
    view.activate(Hit::CancelSelection);
    view.take_action();
    let submission = view.selection.submission.unwrap();
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 30,
    });
    view.selection_result(1, 30, &AccountOperationResult::Cancelled);
    assert!(!view.selection.locked());
}

#[test]
fn local_form_and_its_return_choice_survive_late_selection_completion() {
    let mut view = view();
    let submission = submit(&mut view);
    view.activate(Hit::ContinueOffline);
    assert_eq!(view.screen, Screen::Profile);
    view.owner_name
        .set_text(&mut view.engine, "My edited display name");
    view.profile_name
        .set_text(&mut view.engine, "My edited Profile name");
    let mut progress = state(1);
    progress.stage = AccountStage::Refreshing;
    view.show_account_state(&progress);
    assert_eq!(view.screen, Screen::Profile);
    view.show_account_profile_choices("A refreshed provider display name", choices(1));
    assert_eq!(view.screen, Screen::Profile);
    assert_eq!(view.owner_name.value(), "My edited display name");
    assert_eq!(view.profile_name.value(), "My edited Profile name");
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 11,
    });
    view.selection_result(
        1,
        11,
        &AccountOperationResult::ProfilesSelected { profiles: vec![] },
    );
    assert_eq!(view.screen, Screen::Profile);
    assert!(view.reopen_account_creation());
    assert_eq!(view.screen, Screen::ExistingProfiles);
}

#[test]
fn separately_owned_created_profile_failure_preserves_its_retry_with_a_pending_selection() {
    let mut view = view();
    let submission = submit(&mut view);
    view.activate(Hit::CreateNew);
    assert_eq!(view.screen, Screen::Profile);
    let profile = koru_core::ProfileId::new();
    let retry = AccountCommand::CreateProfile {
        profile_id: profile.to_storage(),
        owner_display_name: "Owner".into(),
        name: "New local Profile".into(),
        locale: Some("en".into()),
    };
    // The host first routes its independently matched creation failure through
    // this existing exact-Profile recovery seam, before chooser-only results.
    view.connection_paused_scoped(1, profile, retry.clone());
    assert_eq!(view.created_profile_retry, Some((profile, retry.clone())));
    assert_eq!(view.screen, Screen::AccountStatus);
    assert_eq!(view.selection.submission, Some(submission));
    view.account_command_admitted(crate::account_ui::CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 11,
    });
    view.selection_result(
        1,
        11,
        &AccountOperationResult::ProfilesSelected { profiles: vec![] },
    );
    assert_eq!(view.created_profile_retry, Some((profile, retry)));
    assert_eq!(view.screen, Screen::AccountStatus);
}

#[test]
fn background_failed_cancelled_and_admission_failure_do_not_interrupt_local_form_edits() {
    for cancel in [false, true] {
        let mut view = view();
        let mut submission = submit(&mut view);
        if cancel {
            view.account_command_failed(crate::account_ui::CommandFailure {
                submission,
                uncertain: true,
            });
            view.activate(Hit::CancelSelection);
            view.take_action();
            submission = view.selection.submission.unwrap();
        }
        view.activate(Hit::ContinueOffline);
        view.error = Some("local validation remains owned by this form".into());
        view.account_command_admitted(crate::account_ui::CommandAdmission {
            submission,
            owner_scope: 1,
            generation: 12,
        });
        view.selection_result(
            1,
            12,
            &if cancel {
                AccountOperationResult::Cancelled
            } else {
                AccountOperationResult::Failed {
                    code: "enrollment_unavailable".into(),
                }
            },
        );
        assert_eq!(view.screen, Screen::Profile);
        assert_eq!(
            view.error.as_deref(),
            Some("local validation remains owned by this form")
        );
        if !cancel {
            assert_eq!(
                view.selection.notice,
                Some(koru_i18n::Key::ProfileChooserRefused)
            );
        }
    }
    let mut view = view();
    let submission = submit(&mut view);
    view.activate(Hit::CreateNew);
    view.account_command_failed(crate::account_ui::CommandFailure {
        submission,
        uncertain: true,
    });
    assert_eq!(view.screen, Screen::Profile);
    assert_eq!(
        view.selection.notice,
        Some(koru_i18n::Key::ProfileChooserUncertain)
    );
}

#[test]
fn named_space_activates_the_focused_checkbox_without_losing_keyboard_focus() {
    let mut view = view();
    view.focus = view
        .controls()
        .iter()
        .position(|hit| *hit == Hit::Existing(1))
        .unwrap();
    assert!(view.key(&Key::Named(NamedKey::Space), None, ModifiersState::empty()));
    assert_eq!(view.selection.checked, set(&[&id(12)]));
    assert_eq!(view.controls().get(view.focus), Some(&Hit::Existing(1)));
}
