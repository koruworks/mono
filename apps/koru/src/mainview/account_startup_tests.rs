// SPDX-License-Identifier: MPL-2.0

use super::*;
use winit::keyboard::NamedKey;

#[test]
fn account_state_reaches_manage_in_an_inactive_split() {
    let mut view = MainView::new();
    // The account-wide page: Koru Cloud.
    view.open_manage(crate::manage::Page::Cloud);
    view.panes.split(
        &panes::PanePath::root(),
        panes::SplitDir::Right,
        Tab::welcome(),
    );
    assert!(View::set_account_state(
        &mut view,
        koru_agent_wire::AccountState {
            active_scope: 41,
            generation: 9,
            stage: koru_agent_wire::AccountStage::SignedIn,
            ..Default::default()
        },
    ));
    let manager = view
        .panes
        .iter_groups()
        .flat_map(|(_, group)| group.tabs().iter())
        .find_map(|tab| match &tab.view {
            TabView::Manage(manager) => Some(manager),
            _ => None,
        })
        .unwrap();
    assert_eq!(manager.controller().account_scope(), 41);
    assert!(
        !view
            .tabs()
            .iter()
            .any(|tab| matches!(tab.view, TabView::Manage(_)))
    );
}

#[test]
fn window_wrapper_connects_missing_profile_and_stays_in_manager_after_local_ack() {
    use koru_agent_wire::{
        AccountOperationResult, AccountProfileEnrollment, AccountProfileRow, AccountStage,
        AccountState, EnrollmentState,
    };
    use koru_workspace_host::account_ui::CommandAdmission;
    for early_result in [false, true] {
        let mut view = crate::manager_view::ManagerView::Container(MainView::new());
        let local = ProfileId::new();
        View::set_pane_context(
            &mut view,
            vec![ProfileSummary {
                id: local,
                name: "Local".into(),
                email: String::new(),
                avatar: None,
                icon: None,
                color: None,
                account: None,
                folder: String::new(),
            }],
            local,
            vec![],
            None,
        );
        View::set_account_state(
            &mut view,
            AccountState {
                active_scope: 41,
                generation: 10,
                stage: AccountStage::SignedIn,
                ..Default::default()
            },
        );
        let crate::manager_view::ManagerView::Container(inner) = &mut view else {
            unreachable!()
        };
        // Koru Cloud: the account's Profiles that are not here are rows of
        // *Which profiles sync*, each with a switch, where Tab reaches them.
        inner.open_manage(crate::manage::Page::Cloud);
        while View::take_pane_request(&mut view).is_some() {}
        let profile = ProfileId::new();
        View::account_scoped_result(
            &mut view,
            41,
            10,
            AccountOperationResult::Profiles {
                profiles: vec![AccountProfileRow {
                    profile_id: profile.to_storage(),
                    koru_identity_id: "10000000-0000-4000-8000-000000000041".into(),
                    name: Some("Other Profile".into()),
                    locale: None,
                    auth_identities: vec![],
                }],
            },
        );
        while View::take_pane_request(&mut view).is_some() {}
        let mut workspace = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Test").unwrap();
        // Tab through the nav and the page to the missing Profile's switch.
        let on_the_switch = |view: &crate::manager_view::ManagerView| {
            let crate::manager_view::ManagerView::Container(inner) = view else {
                unreachable!()
            };
            inner.tabs().iter().any(|tab| match &tab.view {
                TabView::Manage(manage) => {
                    format!("{:?}", manage.page_focus()).contains("ProfileBring")
                }
                _ => false,
            })
        };
        for _ in 0..40 {
            if on_the_switch(&view) {
                break;
            }
            assert!(View::key(
                &mut view,
                &mut workspace,
                &Key::Named(NamedKey::Tab),
                None,
                ModifiersState::empty(),
            ));
        }
        assert!(on_the_switch(&view), "Tab reaches the Profile's switch");
        assert!(View::key(
            &mut view,
            &mut workspace,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));
        let Some(PaneRequest::AccountTracked {
            submission,
            owner_scope,
            command,
        }) = View::take_pane_request(&mut view)
        else {
            panic!("Connect must submit through the wrapper")
        };
        assert_eq!(owner_scope, 41);
        assert_eq!(
            command,
            koru_agent_wire::AccountCommand::SelectProfiles {
                profile_ids: vec![profile.to_storage()]
            }
        );
        let result = AccountOperationResult::ProfilesSelected {
            profiles: vec![AccountProfileEnrollment {
                profile_id: profile.to_storage(),
                state: EnrollmentState::Waiting,
                failure_code: None,
                retry_at_millis: None,
            }],
        };
        if early_result {
            assert!(View::account_scoped_result(
                &mut view,
                41,
                11,
                result.clone()
            ));
            assert!(View::take_pane_request(&mut view).is_none());
        }
        assert!(View::account_command_admitted(
            &mut view,
            CommandAdmission {
                submission,
                owner_scope: 41,
                generation: 11
            }
        ));
        if !early_result {
            assert!(View::account_scoped_result(&mut view, 41, 11, result));
        }
        assert!(View::take_pane_request(&mut view).is_none());
        View::set_pane_context(
            &mut view,
            [local, profile]
                .into_iter()
                .map(|id| ProfileSummary {
                    id,
                    name: "Profile".into(),
                    email: String::new(),
                    avatar: None,
                    icon: None,
                    color: None,
                    account: None,
                    folder: String::new(),
                })
                .collect(),
            local,
            vec![],
            None,
        );
        assert!(
            View::take_pane_request(&mut view).is_none(),
            "catalog readiness never navigates"
        );
        let crate::manager_view::ManagerView::Container(inner) = &view else {
            unreachable!()
        };
        assert!(
            inner
                .tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Manage(_)))
        );
    }
}

#[test]
fn window_wrapper_delivers_cached_sign_in_and_identity_results_to_later_opened_manager() {
    let mut view = crate::manager_view::ManagerView::Container(MainView::new());
    let profile = ProfileId::new();
    View::set_pane_context(
        &mut view,
        vec![ProfileSummary {
            id: profile,
            name: "Work".into(),
            email: String::new(),
            avatar: None,
            icon: None,
            color: None,
            account: Some("koru:10000000-0000-4000-8000-000000000041".into()),
            folder: String::new(),
        }],
        profile,
        vec![],
        None,
    );
    View::set_account_state(
        &mut view,
        koru_agent_wire::AccountState {
            active_scope: 41,
            generation: 9,
            stage: koru_agent_wire::AccountStage::SignedIn,
            ..Default::default()
        },
    );
    while View::take_pane_request(&mut view).is_some() {}
    let crate::manager_view::ManagerView::Container(inner) = &mut view else {
        unreachable!()
    };
    inner.open_manage(crate::manage::Page::Cloud);
    let manager = inner
        .tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::Manage(view) => Some(view),
            _ => None,
        })
        .unwrap();
    assert_eq!(manager.controller().account_scope(), 41);
    assert!(matches!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::AccountOwned {
            owner_scope: 41,
            command: koru_agent_wire::AccountCommand::InspectAccountSession,
        })
    ));
    assert!(View::account_scoped_result(
        &mut view,
        41,
        10,
        koru_agent_wire::AccountOperationResult::SignedInIdentity {
            provider: "google".into(),
            verified_email: "account@example.test".into(),
        },
    ));
    assert!(matches!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::AccountOwned {
            owner_scope: 41,
            command: koru_agent_wire::AccountCommand::ListProfiles,
        })
    ));
    View::set_account_state(
        &mut view,
        koru_agent_wire::AccountState {
            active_scope: 41,
            generation: 11,
            stage: koru_agent_wire::AccountStage::SignedIn,
            ..Default::default()
        },
    );
    assert!(
        View::take_pane_request(&mut view).is_none(),
        "an ordinary command completion must not start another reconciliation"
    );
}
