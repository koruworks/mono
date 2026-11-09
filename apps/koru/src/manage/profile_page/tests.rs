// SPDX-License-Identifier: MPL-2.0

//! A Profile's page: what it offers in each state, what pressing each
//! control does, keyboard order, what the inspector sees and what a screen
//! reader is offered. The questions it raises are tested beside
//! `manage/dialogs.rs`.
use super::*;
use koru_agent_wire::{
    AccountAuthIdentityRow, AccountCommand, AccountOperationResult, AccountProfileRow, AccountState,
};
use koru_core::schema::WorkspaceDoc;
use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};

const IDENTITY: &str = "00000000-0000-4000-8000-000000000041";

fn profile(name: &str, account: Option<&str>) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: Some("teal".into()),
        account: account.map(|id| format!("koru:{id}")),
        folder: String::new(),
    }
}

/// A Manage tab over these Profiles, the first open, on the first's page.
fn manage(profiles: &[ProfileSummary]) -> ManageView {
    let mut view = ManageView::new();
    let current = profiles[0].id;
    view.delegate(|c| {
        c.take_request();
        c.set_profiles(profiles.to_vec(), current, &[])
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

/// Signed in to the account that owns `profile`, which has these Google
/// Accounts.
fn signed_in(
    view: &mut ManageView,
    profile: &ProfileSummary,
    identities: Vec<AccountAuthIdentityRow>,
) {
    *view.c.account_mut() = AccountState {
        stage: AccountStage::SignedIn,
        generation: 7,
        capability: true,
        active_account_id: Some(IDENTITY.into()),
        ..AccountState::default()
    };
    *view.c.account_profiles_mut() = vec![AccountProfileRow {
        koru_identity_id: IDENTITY.into(),
        profile_id: profile.id.to_storage(),
        name: Some(profile.name.clone()),
        locale: Some("en".into()),
        auth_identities: identities,
    }];
    view.c.set_account_profiles_loaded(true);
    view.c.clear_pending();
    view.delegate(|_| ());
}

fn signed_out(view: &mut ManageView) {
    view.c.account_mut().stage = AccountStage::SignedOut;
    view.delegate(|_| ());
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

fn controls(view: &ManageView) -> Vec<Control> {
    view.laid().controls.iter().map(|p| p.control).collect()
}

/// Press a control where it is drawn.
fn press(view: &mut ManageView, control: Control) -> bool {
    let rect = view
        .laid()
        .placed(control)
        .unwrap_or_else(|| panic!("{control:?} is on the page"))
        .rect;
    let (x, y) = rect.centre();
    assert_eq!(
        view.page_hit(x, y),
        Some(control),
        "{control:?} is hit where drawn"
    );
    View::mouse_down(view, &mut doc(), x, y, false)
}

fn key(view: &mut ManageView, key: NamedKey, mods: ModifiersState) -> bool {
    View::key(view, &mut doc(), &Key::Named(key), None, mods)
}

fn capture(view: &mut ManageView) -> inspect::Snapshot {
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, w, h));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    frame.finish()
}

fn named(snapshot: &inspect::Snapshot, name: &str) -> usize {
    snapshot.records.iter().filter(|r| r.name == name).count()
}

// ------------------------------------------------------------- the header

#[test]
fn the_header_asks_the_window_to_rename_and_to_change_icon_and_color() {
    let personal = profile("Personal", None);
    let mut view = manage(std::slice::from_ref(&personal));
    assert!(press(&mut view, Control::Name));
    assert!(press(&mut view, Control::Tile));
    assert_eq!(view.take_request(), Some(Request::Rename(personal.id)));
    assert_eq!(
        view.take_request(),
        Some(Request::ChangeIconAndColor(personal.id))
    );
    assert_eq!(view.take_request(), None);
    // The rename field goes over the name, in the tab's coordinates.
    let name = view.laid().header.name;
    let field = view.profile_name_rect(personal.id).expect("its page shows");
    assert_eq!((field.x, field.y), (name.x, name.y));
    assert!(view.profile_name_rect(ProfileId::new()).is_none());
}

#[test]
fn the_status_line_says_which_profile_is_open_and_switch_opens_another() {
    let personal = profile("Personal", None);
    let work = profile("Work", None);
    let mut view = manage(&[personal.clone(), work.clone()]);
    assert!(
        view.laid()
            .header
            .status_text
            .starts_with("Current profile · ")
    );
    assert!(
        !controls(&view).contains(&Control::Switch),
        "it is open already"
    );

    view.show(Page::Profile(work.id));
    assert!(
        !view
            .laid()
            .header
            .status_text
            .starts_with("Current profile")
    );
    assert!(press(&mut view, Control::Switch));
    assert_eq!(view.take_effect(), Some(Effect::SwitchProfile(work.id)));
}

#[test]
fn the_account_wide_outage_is_a_pointer_to_koru_cloud_not_a_card() {
    let synced = profile("Synced", Some(IDENTITY));
    let local = profile("Local", None);
    let mut view = manage(&[synced.clone(), local.clone()]);
    // The default account state: Koru cannot be reached.
    assert_eq!(view.c.account().stage, AccountStage::Unavailable);
    let section = view.laid().sign_in.clone().expect("the section");
    assert!(section.repair.is_none(), "not this Profile's to repair");
    assert!(press(&mut view, Control::CloudPointer));
    assert_eq!(view.page(), Some(Page::Cloud));

    view.show(Page::Profile(local.id));
    assert!(
        !controls(&view).contains(&Control::CloudPointer),
        "a Profile only on this computer has nothing to reach"
    );
}

// ------------------------------------------------------------- sign-in

#[test]
fn a_local_profile_signs_in_with_google_while_signed_out() {
    let local = profile("Local", None);
    let mut view = manage(std::slice::from_ref(&local));
    signed_out(&mut view);
    let row = &view.laid().sign_in.as_ref().unwrap().rows[0];
    assert_eq!(row.label.as_deref(), Some("On this computer only"));
    let session = view.laid().placed(Control::Session).unwrap();
    assert_eq!(session.look, Look::Google);
    assert_eq!(session.label, "Sign in with Google");
    assert!(!controls(&view).contains(&Control::SignOut));
    assert!(press(&mut view, Control::Session));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::StartGoogle {
            locale: "en".into()
        })
    );
}

#[test]
fn a_local_profile_connects_to_the_signed_in_account_through_the_attach_question() {
    let local = profile("Local", None);
    let mut view = manage(std::slice::from_ref(&local));
    signed_in(&mut view, &profile("Elsewhere", Some(IDENTITY)), Vec::new());
    assert!(controls(&view).contains(&Control::Attach));
    assert!(controls(&view).contains(&Control::SignOut));
    assert!(press(&mut view, Control::Attach));
    assert!(view.c.prompts().next().is_some(), "nothing binds unasked");
    // Enter answers Cancel, as the attach question always has.
    assert!(key(&mut view, NamedKey::Enter, ModifiersState::empty()));
    assert!(view.c.prompts().next().is_none());
    assert_eq!(view.c.take_request(), None);
}

#[test]
fn a_synced_profile_lists_its_google_accounts_and_the_account_actions() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    signed_in(
        &mut view,
        &synced,
        vec![
            identity(1, "one@example.test"),
            identity(2, "two@example.test"),
        ],
    );
    let section = view.laid().sign_in.clone().unwrap();
    let labels: Vec<_> = section.rows.iter().map(|r| r.label.clone()).collect();
    assert_eq!(labels[0].as_deref(), Some("one@example.test"));
    assert_eq!(labels[1].as_deref(), Some("two@example.test"));
    assert_eq!(labels[2], None, "the strip of account actions");
    let order = controls(&view);
    for control in [
        Control::Unlink(0),
        Control::Unlink(1),
        Control::AddGoogle,
        Control::SignOut,
    ] {
        assert!(order.contains(&control), "{control:?}");
    }
    assert!(
        !order.contains(&Control::MakeOffline),
        "two Google Accounts: unlink one first"
    );
    // Every control of *Sign-in and sync* sits inside its card; the
    // Workspaces section above it has its own.
    let workspaces = view.laid().workspaces.as_ref().map(|w| w.rect).unwrap();
    for placed in &view.laid().controls {
        let r = placed.rect;
        if matches!(placed.look, Look::Header)
            || placed.control == Control::Delete
            || r.y + r.h <= workspaces.y + workspaces.h
        {
            continue;
        }
        assert!(
            r.y >= section.rect.y && r.y + r.h <= section.rect.y + section.rect.h,
            "{:?} escapes its card",
            placed.control
        );
    }

    assert!(press(&mut view, Control::AddGoogle));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::AddGoogle {
            profile_id: synced.id.to_storage(),
            locale: "en".into(),
        })
    );
}

#[test]
fn the_cloud_row_is_the_shared_switch_or_the_way_to_koru_cloud() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    signed_in(&mut view, &synced, vec![identity(1, "one@example.test")]);
    view.c.set_cloud_status(profiles::CloudStatus::Subscribe);
    view.delegate(|_| ());
    assert!(!controls(&view).contains(&Control::CloudSync));
    assert!(press(&mut view, Control::GetCloud));
    assert_eq!(view.page(), Some(Page::Cloud));

    view.show(Page::Profile(synced.id));
    view.c.set_cloud_status(profiles::CloudStatus::Manage);
    view.delegate(|_| ());
    let switch = view.laid().placed(Control::CloudSync).unwrap().clone();
    let Look::Toggle(toggle) = switch.look else {
        panic!("the shared switch")
    };
    assert_eq!(toggle, Toggle::new(false));
    // Turning it on raises D258's question about the Profile's Workspaces.
    assert!(press(&mut view, Control::CloudSync));
    assert!(view.c.prompts().next().is_some());
}

#[test]
fn a_profiles_own_repair_is_an_attention_card_and_another_profiles_is_not() {
    use koru_agent_wire::{AccountProfileEnrollment, EnrollmentState};
    let hosted = profile("Hosted", Some(IDENTITY));
    let local = profile("Local", None);
    let mut view = manage(&[hosted.clone(), local.clone()]);
    signed_in(&mut view, &hosted, vec![identity(1, "one@example.test")]);
    let state = view.c.account_mut();
    state.enrollment = EnrollmentState::Waiting;
    state.enrollment_failure_code = Some("no_peer_online".into());
    state.enrollments = vec![AccountProfileEnrollment {
        profile_id: hosted.id.to_storage(),
        state: EnrollmentState::Waiting,
        failure_code: Some("no_peer_online".into()),
        retry_at_millis: None,
    }];
    view.delegate(|_| ());

    let section = view.laid().sign_in.clone().unwrap();
    assert!(section.repair.is_some());
    assert!(section.rows.is_empty(), "the repair is the section");
    let cancel = view
        .laid()
        .controls
        .iter()
        .find(|p| p.label == "Cancel connection")
        .map(|p| p.control)
        .expect("the enrollment's cancel");
    assert!(matches!(cancel, Control::Repair(_)));
    assert!(press(&mut view, cancel));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::CancelEnrollment)
    );

    view.show(Page::Profile(local.id));
    assert!(view.laid().sign_in.as_ref().unwrap().repair.is_none());
    assert!(controls(&view).contains(&Control::Attach));
}

// ------------------------------------------------------------- attention

#[test]
fn a_failed_deletion_is_the_page_until_it_is_answered() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    view.c
        .start_hosted_delete(synced.id, "Synced", HostedDeleteStage::Failed);
    view.delegate(|_| ());
    let laid = view.laid().clone();
    assert!(laid.attention.is_some());
    assert!(laid.sign_in.is_none() && laid.footer.is_none());
    let answers: Vec<Control> = controls(&view)
        .into_iter()
        .filter(|control| matches!(control, Control::Attention(_)))
        .collect();
    assert_eq!(
        answers,
        [
            Control::Attention(Attention::RetryDelete),
            Control::Attention(Attention::RemoveAfterFailure),
            Control::Attention(Attention::CancelFailure),
        ]
    );
    assert!(press(
        &mut view,
        Control::Attention(Attention::RemoveAfterFailure)
    ));
    assert_eq!(view.c.take_remove_request(), Some((synced.id, true, None)));
}

#[test]
fn a_failed_cleanup_finishes_from_its_card() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    view.c
        .start_hosted_delete(synced.id, "Synced", HostedDeleteStage::CleanupFailed);
    view.delegate(|_| ());
    assert!(press(
        &mut view,
        Control::Attention(Attention::FinishCleanup)
    ));
    assert_eq!(
        view.c.take_hosted_delete_request(),
        Some((synced.id, false))
    );
    assert_eq!(
        view.c.hosted_delete().map(|(_, stage)| stage),
        Some(HostedDeleteStage::CleanupRunning)
    );
}

#[test]
fn a_deletion_found_on_another_device_offers_its_three_answers() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    signed_in(&mut view, &synced, vec![identity(1, "one@example.test")]);
    view.delegate(|c| {
        c.complete(AccountOperationResult::ProfileLifecycleReconciliation {
            profiles: vec![koru_agent_wire::AccountProfileLifecycleRow {
                profile_id: synced.id.to_storage(),
                lifecycle_state: koru_agent_wire::AccountProfileLifecycleState::Deleted,
                lifecycle_revision: 1,
                operation_id: Some("10000000-0000-4000-8000-000000000001".into()),
                cleanup_stage: None,
                failure_code: None,
            }],
        })
    });
    let words = view.laid().attention.clone().expect("the card");
    assert_eq!(words.title, "\u{201c}Synced\u{201d} was deleted from Koru");
    assert!(press(&mut view, Control::Attention(Attention::Restore)));
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::RestoreProfile {
            profile_id: synced.id.to_storage(),
            expected_lifecycle_revision: 1,
            name_snapshot: "Synced".into(),
            locale_snapshot: Some("en".into()),
        })
    );
    assert!(press(
        &mut view,
        Control::Attention(Attention::ContinueOffline)
    ));
    assert_eq!(
        view.c.take_continue_offline_request(),
        Some((synced.id, IDENTITY.into()))
    );
}

// ------------------------------------------------------------- keyboard

#[test]
fn tab_walks_the_nav_then_the_page_and_enter_presses_what_it_is_on() {
    let personal = profile("Personal", None);
    let mut view = manage(std::slice::from_ref(&personal));
    signed_out(&mut view);
    let nav = view.focus_order().len();
    let page = view.page_focus_order();
    assert_eq!(
        page,
        vec![
            Control::Tile,
            Control::Name,
            Control::ImportWorkspaces,
            Control::NewWorkspace,
            Control::Session,
            Control::Delete
        ]
    );
    // The first Tab lands on the open page's row, then walks on.
    let none = ModifiersState::empty();
    assert!(key(&mut view, NamedKey::Tab, none));
    assert_eq!(view.focus(), Some(nav::Entry::Profile(personal.id)));
    for _ in 1..nav {
        assert!(key(&mut view, NamedKey::Tab, none));
    }
    assert!(key(&mut view, NamedKey::Tab, none));
    assert_eq!(view.page_focus(), Some(Control::Tile));
    assert_eq!(view.focus(), None, "the keyboard is in one place");
    // Arrows stay on the page while the keyboard is there.
    assert!(key(&mut view, NamedKey::ArrowDown, none));
    assert_eq!(view.page_focus(), Some(Control::Name));
    assert!(key(&mut view, NamedKey::Enter, none));
    assert_eq!(view.take_request(), Some(Request::Rename(personal.id)));
    // Shift+Tab from the first control goes back to the nav's last row.
    assert!(key(&mut view, NamedKey::ArrowUp, none));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert_eq!(view.focus(), Some(nav::Entry::Cloud));
    // Past the last control, Tab wraps to the nav's first row.
    for _ in 0..page.len() + 1 {
        assert!(key(&mut view, NamedKey::Tab, none));
    }
    assert_eq!(view.focus(), Some(nav::Entry::Profile(personal.id)));
}

#[test]
fn escape_puts_the_profiles_repair_aside() {
    let personal = profile("Personal", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&personal));
    view.c.account_mut().stage = AccountStage::AwaitingAccountCreation;
    view.delegate(|_| ());
    assert!(view.laid().sign_in.as_ref().unwrap().repair.is_some());
    assert!(key(&mut view, NamedKey::Escape, ModifiersState::empty()));
    // Account creation's own cancel answered it.
    assert_eq!(
        view.c.take_request(),
        Some(AccountCommand::CancelAccountCreation)
    );
}

// ------------------------------------------------------------- inspection

#[test]
fn the_inspector_names_the_pages_parts() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    signed_in(&mut view, &synced, vec![identity(1, "one@example.test")]);
    let snapshot = capture(&mut view);
    assert_eq!(named(&snapshot, "Manage profile header"), 1);
    assert_eq!(named(&snapshot, "Manage sign-in and sync"), 1);
    assert_eq!(named(&snapshot, "Manage account actions"), 1);
    assert_eq!(named(&snapshot, "Manage delete profile"), 1);
    assert_eq!(named(&snapshot, "Manage attention"), 0);
    assert!(named(&snapshot, "KoruCardRow") >= 1, "shared rows");

    view.c
        .start_hosted_delete(synced.id, "Synced", HostedDeleteStage::Failed);
    view.delegate(|_| ());
    let snapshot = capture(&mut view);
    assert_eq!(named(&snapshot, "Manage attention"), 1);
    assert_eq!(named(&snapshot, "Manage sign-in and sync"), 0);
}

// ------------------------------------------------------------- accessibility

#[test]
fn a_screen_reader_hears_the_pages_controls_as_buttons_and_presses_them() {
    let synced = profile("Synced", Some(IDENTITY));
    let mut view = manage(std::slice::from_ref(&synced));
    signed_in(&mut view, &synced, vec![identity(1, "one@example.test")]);
    view.c.set_cloud_status(profiles::CloudStatus::Manage);
    view.delegate(|_| ());
    let cx = accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(40.0, 60.0, 1000.0, 1000.0),
    };
    let tree = view.semantics(&cx);
    let delete = tree
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Delete profile…"))
        .expect("the footer is a button");
    assert_eq!(delete.role, accesskit::Role::Button);
    let placed = view.laid().placed(Control::Delete).unwrap().rect;
    let bounds = delete.bounds.unwrap();
    assert_eq!((bounds.x, bounds.y), (placed.x + 40.0, placed.y + 60.0));
    let switch = tree
        .children
        .iter()
        .find(|node| node.role == accesskit::Role::Switch)
        .expect("Cloud sync is a switch");
    assert_eq!(switch.label.as_deref(), Some("Cloud sync"));
    let unlink = tree
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Unlink…"))
        .unwrap();
    assert!(unlink.occurrence.locator().contains("manage-unlink:0"));

    let target = delete.occurrence;
    assert!(view.accessibility_action(&cx, target, accesskit::Action::Focus));
    assert_eq!(view.accessible_focus(&cx), target);
    assert!(view.accessibility_action(&cx, target, accesskit::Action::Click));
    assert!(view.dialog().is_some(), "the delete dialog");
    let tree = view.semantics(&cx);
    assert!(
        tree.children
            .iter()
            .any(|node| node.role == accesskit::Role::Dialog)
    );
    // Nothing under a question is pressable.
    assert!(!view.accessibility_action(&cx, target, accesskit::Action::Click));
}
