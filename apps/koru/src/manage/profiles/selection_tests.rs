// SPDX-License-Identifier: MPL-2.0

use super::*;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[test]
fn missing_profiles_are_account_scoped_and_exclude_retained_local_content() {
    let mut c = ProfilesController::new();
    let local = ProfileId::new();
    let remote = ProfileId::new();
    c.set_account(AccountState {
        active_scope: 1,
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    c.take_request();
    c.complete(AccountOperationResult::Profiles {
        profiles: [local, remote]
            .into_iter()
            .map(|id| AccountProfileRow {
                profile_id: id.to_storage(),
                koru_identity_id: "10000000-0000-4000-8000-000000000090".into(),
                name: Some("Profile".into()),
                locale: None,
                auth_identities: vec![],
            })
            .collect(),
    });
    c.retained_profiles = vec![ProfileSummary {
        id: local,
        name: "Local".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    }];
    assert_eq!(
        c.missing_account_profiles()
            .iter()
            .map(|p| p.profile_id.clone())
            .collect::<Vec<_>>(),
        vec![remote.to_storage()]
    );
    let mut state = c.account.clone();
    state.generation += 1;
    c.set_account(state);
    assert_eq!(
        c.missing_account_profiles().len(),
        1,
        "ordinary account results keep the loaded list"
    );
    c.set_account(AccountState {
        active_scope: 2,
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    assert!(
        c.missing_account_profiles().is_empty(),
        "another account must not see the old account's choices"
    );
}
use koru_workspace_host::account_ui::{CommandAdmission, CommandFailure};

fn key(c: &mut ProfilesController, key: NamedKey) {
    assert!(
        c.profile_creation
            .as_mut()
            .map(std::cell::RefCell::get_mut)
            .unwrap()
            .key(&Key::Named(key), None, ModifiersState::empty())
    );
    c.drain_profile_creation_action();
}

#[test]
fn signed_in_manager_can_import_another_profile_without_reauthenticating() {
    let mut c = ProfilesController::new();
    c.set_account(AccountState {
        active_scope: 7,
        generation: 10,
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    c.take_request();
    let ids: Vec<_> = (1..=2)
        .map(|n| format!("10000000-0000-4000-8000-{n:012}"))
        .collect();
    c.complete(AccountOperationResult::Profiles {
        profiles: ids
            .iter()
            .map(|id| AccountProfileRow {
                profile_id: id.clone(),
                koru_identity_id: "10000000-0000-4000-8000-000000000090".into(),
                name: Some("Profile".into()),
                locale: None,
                auth_identities: vec![],
            })
            .collect(),
    });
    c.take_request();
    c.perform(Intent::Connect(ProfileId::parse(&ids[1]).unwrap()));
    assert!(
        c.profile_creation.is_none(),
        "Connect stays in Manage Profiles"
    );
    let (command, submission) = c.take_request_with_submission().unwrap();
    assert_eq!(
        command,
        AccountCommand::SelectProfiles {
            profile_ids: vec![ids[1].clone()]
        }
    );
    assert!(submission.is_some());
}

#[test]
fn all_selection_crosses_real_manager_dispatch_and_parked_admission_without_touching_b() {
    let mut c = ProfilesController::new();
    c.set_account(AccountState {
        active_scope: 1,
        generation: 10,
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    c.begin_profile_creation();
    let ids: Vec<_> = (1..=3)
        .map(|n| format!("10000000-0000-4000-8000-{n:012}"))
        .collect();
    c.complete(AccountOperationResult::Profiles {
        profiles: ids
            .iter()
            .map(|id| AccountProfileRow {
                profile_id: id.clone(),
                koru_identity_id: "10000000-0000-4000-8000-000000000090".into(),
                name: Some("Authorized Profile".into()),
                locale: None,
                auth_identities: vec![],
            })
            .collect(),
    });
    // Each checked row contributes to the set directly; Connect follows them.
    for _ in 0..3 {
        key(&mut c, NamedKey::Enter);
        key(&mut c, NamedKey::Tab);
    }
    key(&mut c, NamedKey::Enter);
    let (command, submission) = c.take_request_with_submission().unwrap();
    assert_eq!(command, AccountCommand::SelectProfiles { profile_ids: ids });
    let submission = submission.unwrap();
    key(&mut c, NamedKey::Enter);
    assert!(
        c.take_request().is_none(),
        "duplicate input does not resubmit a pending set"
    );
    let b = AccountState {
        active_scope: 2,
        generation: 20,
        stage: AccountStage::SignedIn,
        ..Default::default()
    };
    c.set_account(b.clone());
    assert_eq!(
        c.take_request(),
        Some(AccountCommand::InspectAccountSession),
        "B requests its own session projection on activation"
    );
    assert_eq!(c.account.active_scope, 2);
    assert!(c.account_command_failed(CommandFailure {
        submission,
        uncertain: true
    }));
    assert_eq!(c.account, b);
    assert!(c.account_command_admitted(CommandAdmission {
        submission,
        owner_scope: 1,
        generation: 11
    }));
    assert!(c.complete_scoped(
        1,
        11,
        AccountOperationResult::Failed {
            code: "enrollment_unavailable".into()
        }
    ));
    assert_eq!(c.account, b);
    assert!(c.profile_switch_request.is_none());
    assert!(c.take_request().is_none());
    c.set_account(AccountState {
        active_scope: 1,
        generation: 11,
        stage: AccountStage::SignedIn,
        ..Default::default()
    });
    assert!(
        c.profile_creation
            .as_ref()
            .unwrap()
            .borrow()
            .command_submission(&AccountCommand::SelectProfiles {
                profile_ids: vec![]
            })
            .is_none()
    );
}
