// SPDX-License-Identifier: MPL-2.0

//! The questions a Profile's page asks: which removal *Delete profile…*
//! means, *Sign out…*, *Unlink…*, and the controller's own questions shown
//! as dialogs — each answered by pointer and keyboard, with Enter and Escape
//! as safe as they were on Manage Profiles.
use super::*;
use koru_agent_wire::{
    AccountAuthIdentityRow, AccountCommand, AccountOperationResult, AccountProfileRow, AccountState,
};
use koru_core::schema::WorkspaceDoc;

const IDENTITY: &str = "00000000-0000-4000-8000-000000000041";

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

fn manage(profile: &ProfileSummary) -> ManageView {
    let mut view = ManageView::new();
    view.delegate(|c| {
        c.take_request();
        c.set_profiles(vec![profile.clone()], profile.id, &[])
    });
    View::set_viewport(&mut view, 1000.0, 1000.0);
    view
}

fn identity(n: u8, email: &str) -> AccountAuthIdentityRow {
    AccountAuthIdentityRow {
        auth_identity_id: format!("google-{n}"),
        provider: "google".into(),
        verified_email: Some(email.into()),
    }
}

/// A Profile the signed-in account owns, with these Google Accounts.
fn synced(identities: Vec<AccountAuthIdentityRow>) -> (ManageView, ProfileSummary) {
    let synced = profile("Existing", Some(IDENTITY));
    let mut view = manage(&synced);
    *view.c.account_mut() = AccountState {
        stage: AccountStage::SignedIn,
        generation: 7,
        capability: true,
        active_account_id: Some(IDENTITY.into()),
        ..AccountState::default()
    };
    *view.c.account_profiles_mut() = vec![AccountProfileRow {
        koru_identity_id: IDENTITY.into(),
        profile_id: synced.id.to_storage(),
        name: Some("Existing".into()),
        locale: Some("en".into()),
        auth_identities: identities,
    }];
    view.c.set_account_profiles_loaded(true);
    view.c.clear_pending();
    view.delegate(|_| ());
    (view, synced)
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

fn press(view: &mut ManageView, control: profile_page::Control) {
    let (x, y) = view
        .laid()
        .placed(control)
        .unwrap_or_else(|| panic!("{control:?} is on the page"))
        .rect
        .centre();
    assert!(View::mouse_down(view, &mut doc(), x, y, false));
}

fn key(view: &mut ManageView, key: NamedKey) -> bool {
    View::key(
        view,
        &mut doc(),
        &Key::Named(key),
        None,
        ModifiersState::empty(),
    )
}

fn showing(view: &ManageView) -> Option<&'static str> {
    view.dialog().map(Dialog::name)
}

use profile_page::Control;

// ------------------------------------------------------------- Delete profile…

#[test]
fn delete_offers_removal_here_and_cancel_for_a_profile_that_cannot_leave_koru() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    press(&mut view, Control::Delete);
    let Some(Dialog::Delete {
        answers, choice, ..
    }) = view.dialog()
    else {
        panic!("the delete dialog");
    };
    assert_eq!(answers, &[DeleteAnswer::Remove, DeleteAnswer::Cancel]);
    assert_eq!(choice.title(), "Delete \u{201c}Local\u{201d}?");
    // Nothing is chosen until a button is: Enter is inert, Escape dismisses.
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(showing(&view), Some("Manage delete dialog"));
    assert!(key(&mut view, NamedKey::Escape));
    assert!(view.dialog().is_none());
    assert!(view.c.confirming_remove().is_none());
}

#[test]
fn an_unknown_count_removes_by_the_cascade_and_cancel_is_the_default() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    press(&mut view, Control::Delete);
    // The first answer: remove from this device.
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.confirming_remove(), Some(true));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the controller's question, as a dialog");
    };
    assert_eq!(confirm.title(), "Remove Local and all its Workspaces?");
    assert!(!confirm.body().contains("Koru account"));
    assert!(!confirm.body().contains("hosted Profile"));
    assert_eq!(confirm.default_action(), Some(ConfirmOutcome::Cancel));
    // Enter cancels it, and the controller hears that.
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.c.confirming_remove().is_none());
    assert!(view.dialog().is_none());
    assert_eq!(view.c.take_remove_request(), None);

    // Asked again and answered on purpose.
    press(&mut view, Control::Delete);
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::Enter));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.take_remove_request(), Some((local.id, true, None)));
    assert!(view.dialog().is_none());
}

#[test]
fn account_bound_removal_preserves_the_hosted_recovery_explanation() {
    let (mut view, _) = synced(Vec::new());
    press(&mut view, Control::Delete);
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::Enter));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("remove confirmation");
    };
    assert!(confirm.body().contains(REMOVE_HOSTED_BODY));
}

#[test]
fn an_empty_profile_is_removed_on_its_own() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    view.delegate(|c| c.set_workspace_counts([(local.id, 0)].into_iter().collect()));
    press(&mut view, Control::Delete);
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::Enter));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the remove question");
    };
    assert_eq!(confirm.title(), "Remove Local from this device?");
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.take_remove_request(), Some((local.id, false, None)));
}

#[test]
fn delete_from_koru_is_offered_where_allowed_and_asks_the_controllers_question() {
    let (mut view, profile) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::Delete);
    let Some(Dialog::Delete { answers, .. }) = view.dialog() else {
        panic!("the delete dialog");
    };
    assert_eq!(
        answers,
        &[
            DeleteAnswer::Remove,
            DeleteAnswer::DeleteFromKoru,
            DeleteAnswer::Cancel
        ]
    );
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.dialog().is_none());
    let confirm = view.c.prompts().next().expect("Delete from Koru?");
    assert_eq!(
        confirm.title(),
        "Delete \u{201c}Existing\u{201d} from Koru?"
    );
    assert_eq!(confirm.default_action(), Some(ConfirmOutcome::Cancel));
    // The controller's prompt takes the keyboard before anything else.
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::InspectProfileLifecycle {
            profile_id: profile.id.to_storage(),
        })
    );
    assert!(view.c.hosted_delete().is_some());
}

#[test]
fn a_press_answers_the_dialog_where_its_button_is_drawn() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    press(&mut view, Control::Delete);
    // A press on the page under the scrim reaches the dialog, not the page:
    // the header's name is under it, and outside the panel dismisses.
    let (x, y) = view.laid().placed(Control::Name).unwrap().rect.centre();
    assert!(View::mouse_down(&mut view, &mut doc(), x, y, false));
    assert!(view.dialog().is_none());
    assert_eq!(view.take_request(), None, "the name was not pressed");
}

// ------------------------------------------------------------- the account

#[test]
fn sign_out_asks_first_and_signs_this_computer_out() {
    let (mut view, _) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::SignOut);
    let Some(Dialog::SignOut(confirm)) = view.dialog() else {
        panic!("the sign-out question");
    };
    assert!(confirm.body().contains("Every signed-in profile"));
    assert!(key(&mut view, NamedKey::Enter), "Enter is Cancel");
    assert!(view.dialog().is_none());
    assert_eq!(view.c.take_request(), None);

    press(&mut view, Control::SignOut);
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.take_request(), Some(AccountCommand::Logout));
}

#[test]
fn unlinking_one_of_several_google_accounts_asks_then_unlinks_only_it() {
    let (mut view, profile) = synced(vec![
        identity(1, "one@example.test"),
        identity(2, "two@example.test"),
    ]);
    press(&mut view, Control::Unlink(1));
    let Some(Dialog::Unlink { confirm, .. }) = view.dialog() else {
        panic!("the unlink question");
    };
    assert_eq!(confirm.title(), "Unlink two@example.test?");
    assert!(key(&mut view, NamedKey::Escape));
    assert_eq!(view.c.take_request(), None);

    press(&mut view, Control::Unlink(1));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::RemoveProfileAuthorization {
            profile_id: profile.id.to_storage(),
            auth_identity_id: "google-2".into(),
            confirm_profile_offline_only: false,
        })
    );
}

#[test]
fn make_offline_only_is_the_last_accounts_unlink_and_its_question() {
    let (mut view, profile) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::MakeOffline);
    assert!(view.c.confirming_offline());
    let Some(Dialog::Offline(confirm)) = view.dialog() else {
        panic!("the offline-only question");
    };
    assert_eq!(confirm.title(), "Make Existing offline-only?");
    assert!(key(&mut view, NamedKey::Escape));
    assert!(!view.c.confirming_offline());
    assert!(view.dialog().is_none());

    // The chip's own Unlink… is the same question for the last account.
    press(&mut view, Control::Unlink(0));
    assert!(matches!(view.dialog(), Some(Dialog::Offline(_))));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::RemoveProfileAuthorization {
            profile_id: profile.id.to_storage(),
            auth_identity_id: "google-1".into(),
            confirm_profile_offline_only: true,
        })
    );
    assert!(view.dialog().is_none());
}

#[test]
fn add_google_confirms_the_candidate_in_a_dialog_and_binds_before_success() {
    let (mut view, profile) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::AddGoogle);
    let _ = view.c.take_request();
    view.delegate(|c| {
        c.complete(AccountOperationResult::AddGoogleCandidate {
            candidate_id: "candidate-one".into(),
            profile_id: profile.id.to_storage(),
            verified_email: "person@example.com".into(),
            expires_at_millis: 123,
        })
    });
    let Some(Dialog::AddGoogle(confirm)) = view.dialog() else {
        panic!("the candidate's question");
    };
    assert_eq!(confirm.title(), "Connect person@example.com to Existing?");
    assert_eq!(showing(&view), Some("Manage add Google dialog"));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::ConfirmAddGoogle {
            candidate_id: "candidate-one".into(),
            profile_id: profile.id.to_storage(),
        })
    );
    // Finishing is the controller's notice, not a dialog.
    assert!(view.dialog().is_none());
    assert_eq!(view.c.notice(), Some("Finishing..."));
}

#[test]
fn escape_cancels_a_google_account_candidate() {
    let (mut view, profile) = synced(vec![identity(1, "one@example.test")]);
    view.c
        .offer_add_google(profile.id, "person@example.com", "Existing");
    view.delegate(|_| ());
    assert!(matches!(view.dialog(), Some(Dialog::AddGoogle(_))));
    assert!(key(&mut view, NamedKey::Escape));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::CancelAddGoogle {
            candidate_id: "candidate".into(),
            profile_id: profile.id.to_storage(),
        })
    );
}

#[test]
fn a_dialog_is_named_for_the_inspector_and_never_opens_under_another_question() {
    let (mut view, _) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::SignOut);
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, w, h));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    let snapshot = frame.finish();
    assert_eq!(
        snapshot
            .records
            .iter()
            .filter(|r| r.name == "Manage sign-out dialog")
            .count(),
        1
    );
    // A second question waits for the first.
    view.ask_how_to_delete(view.laid().profile.unwrap());
    assert!(matches!(view.dialog(), Some(Dialog::SignOut(_))));
}
