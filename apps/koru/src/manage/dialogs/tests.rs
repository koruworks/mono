// SPDX-License-Identifier: MPL-2.0

//! The questions a Profile's page asks: the footer's two removals — the
//! shared deletion and the device-local one, each named for what it does
//! and worded from the string table — *Sign out…*, *Unlink…*, and the
//! controller's own questions shown as dialogs; each answered by pointer
//! and keyboard, with Enter and Escape as safe as they were on Manage
//! Profiles.
use super::*;
use koru_agent_wire::{
    AccountAuthIdentityRow, AccountCommand, AccountOperationResult, AccountProfileRow, AccountState,
};
use koru_core::schema::WorkspaceDoc;
use koru_i18n::Key as I18n;

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

// ------------------------------------------------------- the footer's removals

fn t(key: I18n) -> &'static str {
    koru_i18n::t(crate::format::lang(), key)
}

fn preview(targets: Vec<koru_store_core::WorkspaceId>) -> koru_api_client::ProfileDeletionPreview {
    koru_api_client::ProfileDeletionPreview {
        targets,
        placement_revision: 7,
    }
}

/// The footer's controls with their labels, in the order they are placed.
fn footer(view: &ManageView) -> Vec<(Control, String)> {
    view.laid()
        .controls
        .iter()
        .filter(|placed| {
            matches!(
                placed.control,
                Control::Delete | Control::RemoveFromDevice | Control::DeleteFromKoru
            )
        })
        .map(|placed| (placed.control, placed.label.clone()))
        .collect()
}

#[test]
fn the_footer_offers_the_shared_deletion_then_the_local_removal_each_named_for_what_it_does() {
    let local = profile("Local", None);
    let view = manage(&local);
    assert_eq!(
        footer(&view),
        vec![
            (Control::Delete, t(I18n::ProfileDeleteAction).to_owned()),
            (
                Control::RemoveFromDevice,
                t(I18n::ProfileRemoveFromDeviceAction).to_owned()
            ),
        ],
        "a Profile the account cannot delete offers the two, and no Delete from Koru"
    );
    // *Delete from Koru…* joins them, last, where the account allows it.
    let (view, _) = synced(vec![identity(1, "one@example.test")]);
    assert_eq!(
        footer(&view)
            .into_iter()
            .map(|(c, _)| c)
            .collect::<Vec<_>>(),
        vec![
            Control::Delete,
            Control::RemoveFromDevice,
            Control::DeleteFromKoru
        ]
    );
    assert_eq!(
        footer(&view)[2].1,
        t(I18n::ProfileDeleteFromKoruAction),
        "named from the table, like its neighbours"
    );
}

#[test]
fn the_shared_deletion_asks_the_store_first_and_says_so_when_it_cannot() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    press(&mut view, Control::Delete);
    // No Store client here: the question never opens over a guess, and the
    // device-local removal is not a fallback — it is a different action.
    assert!(view.dialog().is_none());
    assert!(view.c.confirming_remove().is_none());
    assert_eq!(view.c.notice(), Some(t(I18n::ProfileDeletePreviewFailed)));
    assert_eq!(view.c.take_remove_request(), None);
}

#[test]
fn the_deletion_question_is_worded_from_the_table_over_the_captured_list() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    let (a, b) = (
        koru_store_core::WorkspaceId::new(),
        koru_store_core::WorkspaceId::new(),
    );
    view.delegate(|c| c.deletion_preview_arrived(preview(vec![a, b])));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the deletion question, as a dialog");
    };
    assert_eq!(
        confirm.title(),
        koru_i18n::fill(t(I18n::ProfileDeleteTitle), &["Local"])
    );
    let lang = crate::format::lang();
    let names = format!("{}, {}", a.to_storage(), b.to_storage());
    let targets = koru_i18n::fill(
        t(I18n::ProfileDeleteTargetsMany),
        &[&koru_i18n::number::decimal(lang, 2), &names],
    );
    assert_eq!(
        confirm.body(),
        format!("{}\n\n{targets}", t(I18n::ProfileDeleteBody)),
        "the body is the table's sentence and the captured list, nothing else"
    );
    assert_eq!(confirm.default_action(), Some(ConfirmOutcome::Cancel));
    // Enter cancels, and the preview goes with the question: the next
    // removal request must not inherit it and become the batch.
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.dialog().is_none());
    assert!(view.c.confirming_remove().is_none());
    assert!(view.c.deletion_preview().is_none());
    assert_eq!(view.c.take_remove_request(), None);

    // Confirmed on purpose, the request carries the preview — the host's
    // sign that this is `delete_profile` and not the local cascade.
    view.delegate(|c| c.deletion_preview_arrived(preview(vec![a, b])));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    let (id, cascade, carried) = view.c.take_remove_request().expect("the request");
    assert_eq!((id, cascade), (local.id, true));
    assert_eq!(carried.map(|preview| preview.targets), Some(vec![a, b]));
    assert!(view.dialog().is_none());
}

#[test]
fn deleting_an_empty_profile_is_the_profile_fact_alone_and_says_so() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    view.delegate(|c| c.deletion_preview_arrived(preview(Vec::new())));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the deletion question");
    };
    assert_eq!(
        confirm.title(),
        koru_i18n::fill(t(I18n::ProfileDeleteEmptyTitle), &["Local"])
    );
    assert!(confirm.body().ends_with(t(I18n::ProfileDeleteTargetsNone)));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    let (id, _, carried) = view.c.take_remove_request().expect("the request");
    assert_eq!(id, local.id);
    assert!(carried.is_some(), "still the batch, over an empty list");
}

#[test]
fn remove_from_this_device_asks_at_once_and_never_carries_a_preview() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    press(&mut view, Control::RemoveFromDevice);
    assert_eq!(
        view.c.confirming_remove(),
        Some(true),
        "an unknown count takes the cascade"
    );
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the controller's question, as a dialog");
    };
    assert_eq!(
        confirm.title(),
        koru_i18n::fill(t(I18n::ProfileRemoveTitle), &["Local"])
    );
    assert_eq!(confirm.body(), t(I18n::ProfileRemoveCascadeBody));
    assert_eq!(confirm.default_action(), Some(ConfirmOutcome::Cancel));
    // Enter cancels it, and the controller hears that.
    assert!(key(&mut view, NamedKey::Enter));
    assert!(view.c.confirming_remove().is_none());
    assert!(view.dialog().is_none());
    assert_eq!(view.c.take_remove_request(), None);

    // Asked again and answered on purpose: no preview travels, so the host
    // runs the device-local path.
    press(&mut view, Control::RemoveFromDevice);
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.take_remove_request(), Some((local.id, true, None)));
    assert!(view.dialog().is_none());
}

#[test]
fn an_empty_profile_is_removed_from_this_device_on_its_own() {
    let local = profile("Local", None);
    let mut view = manage(&local);
    view.delegate(|c| c.set_workspace_counts([(local.id, 0)].into_iter().collect()));
    press(&mut view, Control::RemoveFromDevice);
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("the remove question");
    };
    assert_eq!(
        confirm.title(),
        koru_i18n::fill(t(I18n::ProfileRemoveTitle), &["Local"])
    );
    assert_eq!(confirm.body(), t(I18n::ProfileRemoveEmptyBody));
    assert!(key(&mut view, NamedKey::Tab));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.c.take_remove_request(), Some((local.id, false, None)));
}

#[test]
fn an_account_bound_profile_keeps_the_hosted_recovery_sentence_in_both_questions() {
    let (mut view, _) = synced(Vec::new());
    press(&mut view, Control::RemoveFromDevice);
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("remove confirmation");
    };
    assert_eq!(
        confirm.body(),
        format!(
            "{} {}",
            t(I18n::ProfileRemoveCascadeBody),
            t(I18n::ProfileRemoveHostedBody)
        )
    );
    assert!(key(&mut view, NamedKey::Escape));

    view.delegate(|c| c.deletion_preview_arrived(preview(Vec::new())));
    let Some(Dialog::Remove(confirm)) = view.dialog() else {
        panic!("deletion confirmation");
    };
    assert_eq!(
        confirm.body(),
        format!(
            "{} {}\n\n{}",
            t(I18n::ProfileDeleteBody),
            t(I18n::ProfileRemoveHostedBody),
            t(I18n::ProfileDeleteTargetsNone)
        ),
        "the hosted sentence before the list"
    );
}

#[test]
fn delete_from_koru_is_offered_where_allowed_and_asks_the_controllers_question() {
    let (mut view, profile) = synced(vec![identity(1, "one@example.test")]);
    press(&mut view, Control::DeleteFromKoru);
    assert!(
        view.dialog().is_none(),
        "the controller's prompt, not a page dialog"
    );
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
    press(&mut view, Control::RemoveFromDevice);
    // A press on the page under the scrim reaches the dialog, not the page:
    // the header's name is under it, and outside the panel dismisses.
    let (x, y) = view.laid().placed(Control::Name).unwrap().rect.centre();
    assert!(View::mouse_down(&mut view, &mut doc(), x, y, false));
    assert!(view.dialog().is_none());
    assert_eq!(view.take_request(), None, "the name was not pressed");
    assert_eq!(view.c.take_remove_request(), None);
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
    view.ask_to_remove_from_device(view.laid().profile.unwrap());
    assert!(matches!(view.dialog(), Some(Dialog::SignOut(_))));
    assert!(view.c.confirming_remove().is_none());
}
