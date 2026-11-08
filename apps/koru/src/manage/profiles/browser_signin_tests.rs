// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_workspace_host::account_ui::CommandAdmission;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[test]
fn admission_drains_deferred_cancel_from_the_reused_form_without_another_click() {
    let mut c = ProfilesController::new();
    c.set_account(AccountState {
        active_scope: 7,
        generation: 2,
        stage: AccountStage::SignedOut,
        ..Default::default()
    });
    c.take_request();
    c.begin_profile_creation();
    let form = c
        .profile_creation
        .as_mut()
        .map(std::cell::RefCell::get_mut)
        .unwrap();
    // Past the welcome screen's email form (address, password, its eye,
    // Continue) to the Google button, which is what this test is about.
    for _ in 0..4 {
        assert!(form.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty()));
    }
    assert!(form.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
    c.drain_profile_creation_action();
    let (command, submission) = c.take_request_with_submission().unwrap();
    assert!(matches!(command, AccountCommand::StartGoogle { .. }));
    let submission = submission.unwrap();
    assert!(
        c.profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
            .unwrap()
            .key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty())
    );
    c.drain_profile_creation_action();
    assert!(c.take_request().is_none());
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 7,
        generation: 44
    }));
    let (cancel, tracked) = c.take_request_with_submission().unwrap();
    assert_eq!(cancel, AccountCommand::Cancel { generation: 44 });
    assert!(tracked.is_some());
}
