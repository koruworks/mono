// SPDX-License-Identifier: MPL-2.0

use super::*;

fn waiting() -> (OnboardingView, CommandSubmissionId) {
    let mut view = OnboardingView::default();
    view.select_account_context(&AccountState {
        active_scope: 7,
        generation: 2,
        ..Default::default()
    });
    view.begin_browser_signin(BrowserProvider::Google);
    let submission = view
        .command_submission(&AccountCommand::StartGoogle {
            locale: "en".into(),
        })
        .unwrap();
    assert_eq!(view.take_action(), Some(Action::StartGoogle));
    (view, submission)
}

#[test]
fn cancel_before_start_leaves_no_command_or_browser_wait() {
    let mut view = OnboardingView::default();
    view.begin_browser_signin(BrowserProvider::Google);
    view.cancel_browser_signin();
    assert_eq!(view.screen, Screen::Entry);
    assert!(view.take_action().is_none());
    assert!(view.browser_signin.is_none());
}

#[test]
fn cancel_waits_for_exact_start_admission_and_uses_its_generation() {
    let (mut view, submission) = waiting();
    let (_, frame) = view.browser_modal_frame();
    let cancel = frame
        .node(super::super::browser_modal::KEY_CANCEL)
        .expect("cancel link")
        .rect
        .centre();
    assert!(view.mouse_down(cancel.0, cancel.1));
    assert!(view.take_action().is_none());
    assert!(!view.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 8,
        generation: 88
    }));
    assert!(view.take_action().is_none());
    assert!(view.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 7,
        generation: 44
    }));
    assert_eq!(view.take_action(), Some(Action::CancelGoogle));
    assert_eq!(view.account_generation(), 44);
    let cancel_command = AccountCommand::Cancel {
        generation: view.account_generation(),
    };
    assert!(view.command_submission(&cancel_command).is_some());
    assert_eq!(
        view.screen,
        Screen::Browser,
        "admission does not prove cancellation"
    );
    view.cancel_browser_signin();
    assert!(
        view.take_action().is_none(),
        "duplicate cancellation is suppressed"
    );
    view.account_cancelled();
    assert_eq!(view.screen, Screen::Entry);
}

#[test]
fn escape_cancels_admitted_signin_without_using_another_state_generation() {
    let (mut view, submission) = waiting();
    view.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 7,
        generation: 44,
    });
    view.account_state.generation = 99;
    assert!(view.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    assert_eq!(view.take_action(), Some(Action::CancelGoogle));
    assert_eq!(view.account_generation(), 44);
}

#[test]
fn rejected_start_returns_to_usable_entry_instead_of_waiting_for_google() {
    let (mut view, submission) = waiting();
    assert!(view.account_command_failed(CommandFailure {
        submission,
        uncertain: false
    }));
    assert_eq!(view.screen, Screen::Entry);
    assert!(view.error.is_some());
    assert!(view.browser_signin.is_none());
    view.activate(Hit::Primary);
    assert_eq!(view.screen, Screen::Setup);
}

#[test]
fn uncertain_start_offers_offline_escape_without_another_signin() {
    let (mut view, submission) = waiting();
    assert!(view.account_command_failed(CommandFailure {
        submission,
        uncertain: true
    }));
    assert_eq!(view.screen, Screen::AccountStatus);
    assert_eq!(
        view.account_surface.as_ref().unwrap().actions,
        vec![crate::account_ui::Action::ContinueOffline]
    );
    assert!(view.take_action().is_none());
    view.activate_account_action(0);
    assert_eq!(view.screen, Screen::Setup);
    assert!(view.take_action().is_none());
}

#[test]
fn rejected_cancel_retains_exact_operation_for_retry() {
    let (mut view, submission) = waiting();
    view.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 7,
        generation: 44,
    });
    view.cancel_browser_signin();
    view.take_action();
    let submission = view
        .command_submission(&AccountCommand::Cancel { generation: 44 })
        .unwrap();
    assert!(view.account_command_failed(CommandFailure {
        submission,
        uncertain: false
    }));
    assert_eq!(view.screen, Screen::AccountStatus);
    view.activate_account_action(1);
    assert_eq!(view.take_action(), Some(Action::CancelGoogle));
    assert_eq!(view.account_generation(), 44);
}

#[test]
fn browser_modal_preserves_provider_across_waiting_state_and_dispatches_reopen_and_copy() {
    for provider in [
        BrowserProvider::Google,
        BrowserProvider::Apple,
        BrowserProvider::Email,
    ] {
        let mut view = OnboardingView::default();
        view.show_workspace_setup();
        view.begin_browser_signin(provider);
        let _ = view.take_action();
        view.show_account_state(&AccountState {
            stage: koru_agent_wire::AccountStage::WaitingForBrowser,
            ..Default::default()
        });
        assert_eq!(view.screen, Screen::Browser);
        assert_eq!(view.browser_provider, provider);

        let (_, frame) = view.browser_modal_frame();
        let reopen = frame
            .node(super::super::browser_modal::KEY_REOPEN)
            .expect("reopen button")
            .rect
            .centre();
        let copy = frame
            .node(super::super::browser_modal::KEY_COPY)
            .expect("copy link")
            .rect
            .centre();
        assert!(view.mouse_down(reopen.0, reopen.1));
        assert_eq!(view.take_action(), Some(Action::ReopenBrowserSignin));
        assert!(view.mouse_down(copy.0, copy.1));
        assert_eq!(view.take_action(), Some(Action::CopyBrowserSigninLink));
    }
}
