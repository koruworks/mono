// SPDX-License-Identifier: MPL-2.0

//! The Koru Cloud page: each subscription state, the switches, *Sign in…*, the progress disclosure and its visible-only poll,
//! the account switcher, and the two account-wide attention cards. What the
//! controller decides is tested beside it (`manage/profiles/cloud/tests.rs`,
//! `progress/tests.rs`); this is where it lands on the page.
use super::*;
use crate::manage::profiles::progress_fixtures::{
    ACCOUNT, controller_with, held, profile, row, synced,
};
use crate::manage::profiles::{AccountCommand, Effect, Waiting};
use koru_agent_wire::AccountState;
use koru_workspace_host::accessibility::{SemanticNode, ViewSlot, WindowSlot};
use std::time::Instant;

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

/// A Manage tab in front on Koru Cloud over `c`. Coming into view asks for
/// the subscription again, as it always has; the answer `c` already had is
/// put back, as the Agent's would be.
fn on_cloud(c: ProfilesController) -> ManageView {
    let status = c.cloud_status();
    let mut view = ManageView::new();
    view.c = c;
    View::set_viewport(&mut view, 1000.0, 2400.0);
    view.show(Page::Cloud);
    view.set_in_front(true);
    answered(&mut view, status);
    view
}

/// The subscription answer arrived: `status`.
fn answered(view: &mut ManageView, status: CloudStatus) {
    view.c.set_cloud_status(status);
    view.relayout();
}

/// Subscribed, signed in: Work syncs, Home does not, Scratch is local-only.
fn subscribed() -> ManageView {
    on_cloud(controller_with(true))
}

/// Signed in to `ACCOUNT` with nothing asked yet.
fn signed_in() -> ProfilesController {
    let mut c = ProfilesController::new();
    c.clear_pending();
    *c.account_mut() = AccountState {
        active_scope: 1,
        active_account_id: Some(ACCOUNT.into()),
        stage: AccountStage::SignedIn,
        capability: true,
        ..Default::default()
    };
    *c.profiles_mut() = vec![profile("Work", Some(ACCOUNT)), profile("Scratch", None)];
    c
}

fn id(view: &ManageView, name: &str) -> ProfileId {
    view.c
        .profiles()
        .iter()
        .find(|row| row.name == name)
        .map(|row| row.id)
        .unwrap_or_else(|| panic!("{name} is a Profile"))
}

fn press(view: &mut ManageView, control: Control) -> bool {
    let rect = view
        .laid
        .placed(control)
        .unwrap_or_else(|| panic!("{control:?} is on the page"))
        .rect;
    let (x, y) = rect.centre();
    View::mouse_down(view, &mut doc(), x, y - view.page_scroll, false)
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

fn cloud(view: &ManageView) -> &CloudLaid {
    view.laid
        .cloud
        .as_ref()
        .expect("the Koru Cloud page is laid")
}

fn text(view: &ManageView) -> String {
    cloud(view)
        .subscription
        .text
        .as_ref()
        .map(|(_, text)| text.clone())
        .unwrap_or_default()
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

// ------------------------------------------------------------- subscription

#[test]
fn not_subscribed_leads_with_the_sentence_lists_every_feature_and_subscribes() {
    let mut view = on_cloud(signed_in());
    view.c.set_cloud_status(CloudStatus::Subscribe);
    view.relayout();
    assert_eq!(text(&view), LEAD);
    let laid = cloud(&view);
    assert!(laid.subscription.card.is_some());
    assert_eq!(laid.subscription.bullets.len(), FEATURES.len());
    assert!(laid.dot.is_none(), "no dot until subscribed");
    let primary = view.laid.placed(Control::CloudPrimary).unwrap();
    assert_eq!(primary.label, "Subscribe to Koru Cloud");
    assert_eq!(primary.look, Look::Button(ButtonRole::Primary));
    let card = laid.subscription.card.unwrap();
    let last = laid.subscription.bullets.last().unwrap().0;
    assert!(
        last.y + last.h <= primary.rect.y,
        "the button is under the list"
    );
    assert!(primary.rect.y + primary.rect.h <= card.y + card.h);
    assert!(view.laid.placed(Control::CloudLearnMore).is_some());

    // Subscribe and Learn more both open Pricing.
    assert!(press(&mut view, Control::CloudPrimary));
    assert!(
        matches!(view.take_effect(), Some(Effect::OpenWebsite(url)) if url.contains("pricing"))
    );
    assert!(press(&mut view, Control::CloudLearnMore));
    assert!(
        matches!(view.take_effect(), Some(Effect::OpenWebsite(url)) if url.contains("pricing"))
    );
}

#[test]
fn subscribed_says_todays_line_with_its_dot_and_links_to_the_dashboard() {
    let mut view = subscribed();
    assert_eq!(text(&view), "Subscribed · syncing 1 of 2 profiles");
    let laid = cloud(&view);
    assert!(laid.dot.is_some(), "a current subscription shows its dot");
    assert!(laid.subscription.bullets.is_empty());
    assert!(
        laid.subscription.card.is_none(),
        "no storage bar (decision 7)"
    );
    let primary = view.laid.placed(Control::CloudPrimary).unwrap();
    assert_eq!(primary.label, "Manage subscription");
    assert_eq!(primary.look, Look::Link);
    assert!(view.laid.placed(Control::CloudLearnMore).is_none());
    assert!(press(&mut view, Control::CloudPrimary));
    assert!(
        matches!(view.take_effect(), Some(Effect::OpenWebsite(url)) if url.contains("dashboard"))
    );
}

#[test]
fn unknown_loading_unavailable_and_signed_out_use_todays_words() {
    let mut view = on_cloud(signed_in());
    view.c.set_cloud_status(CloudStatus::Loading);
    view.relayout();
    assert_eq!(text(&view), "Checking your subscription…");
    assert!(view.laid.placed(Control::CloudPrimary).is_none());

    view.c.set_cloud_status(CloudStatus::Unavailable);
    view.relayout();
    assert_eq!(text(&view), "Could not check your subscription.");
    let retry = view.laid.placed(Control::CloudPrimary).unwrap();
    assert_eq!(retry.look, Look::Button(ButtonRole::Secondary));
    assert!(press(&mut view, Control::CloudPrimary));
    assert_eq!(view.c.cloud_status(), CloudStatus::Unknown, "asked again");

    view.c.account_mut().stage = AccountStage::SignedOut;
    view.relayout();
    assert_eq!(text(&view), "Sign in to check your subscription.");
    assert_eq!(
        view.laid.placed(Control::CloudPrimary).unwrap().look,
        Look::Google
    );
    assert_eq!(cloud(&view).account.label, "Not signed in");
    assert!(press(&mut view, Control::CloudPrimary));
    assert!(matches!(
        view.c.take_request(),
        Some(AccountCommand::StartGoogle { .. })
    ));
}

/// The Account card says when Koru Cloud last accepted the sign-in, once the
/// Agent has said so; until then it says only that this computer is signed in.
#[test]
fn the_account_card_says_when_the_sign_in_was_last_checked() {
    let mut view = subscribed();
    assert_eq!(cloud(&view).account.hint, "Signed in on this computer");
    view.c.account_mut().session_checked_at_millis = Some(1_794_411_832_787);
    view.relayout();
    let hint = &cloud(&view).account.hint;
    let lead = "Signed in on this computer \u{b7} checked ";
    assert!(hint.starts_with(lead), "{hint}");
    assert!(hint.len() > lead.len(), "a time follows: {hint}");
}

// ------------------------------------------------------------- which profiles sync

#[test]
fn every_profile_has_a_row_with_a_switch_or_sign_in() {
    let view = subscribed();
    let (work, home, scratch) = (id(&view, "Work"), id(&view, "Home"), id(&view, "Scratch"));
    let rows = &cloud(&view).rows;
    assert_eq!(rows.len(), 3);
    assert!(
        rows[0].sub_text.ends_with("Syncing via Koru Cloud")
            || rows[0].sub_text.contains("Koru Cloud")
    );
    assert!(
        rows[1].sub_text.ends_with("Not syncing"),
        "{}",
        rows[1].sub_text
    );
    assert_eq!(rows[2].sub_text, "Not signed in");
    assert!(view.laid.placed(Control::ProfileSync(work)).is_some());
    assert!(view.laid.placed(Control::ProfileSync(home)).is_some());
    assert!(view.laid.placed(Control::ProfileSync(scratch)).is_none());
    assert_eq!(
        view.laid
            .placed(Control::ProfileSignIn(scratch))
            .unwrap()
            .label,
        "Sign in…"
    );
    // Tab walks each row's switch after the one above it.
    let order = view.page_focus_order();
    let at = |control| order.iter().position(|c| *c == control).unwrap();
    assert!(at(Control::ProfileSync(work)) < at(Control::ProfileSync(home)));
    assert!(at(Control::ProfileSync(home)) < at(Control::ProfileSignIn(scratch)));
}

#[test]
fn another_accounts_profile_shows_a_dimmed_switch_that_is_no_control() {
    let mut c = controller_with(false);
    c.profiles_mut().push(profile(
        "Theirs",
        Some("20000000-0000-4000-8000-000000000002"),
    ));
    let view = on_cloud(c);
    let theirs = id(&view, "Theirs");
    let row = cloud(&view)
        .rows
        .iter()
        .find(|row| row.profile == theirs)
        .unwrap();
    assert!(matches!(row.toggle, Some((_, _, false))));
    assert!(view.laid.placed(Control::ProfileSync(theirs)).is_none());
    assert!(
        !view
            .page_focus_order()
            .contains(&Control::ProfileSync(theirs))
    );
}

/// The switch is the whole act: on or off, it sends its one command and
/// raises no question (2026-11-11; it used to ask D258's about Workspaces).
#[test]
fn a_switch_asks_nothing_either_way() {
    let mut view = subscribed();
    let home = id(&view, "Home");
    assert!(press(&mut view, Control::ProfileSync(home)));
    assert_eq!(view.c.cloud_sync_pending_for(home), Some(true));
    assert_eq!(view.c.prompts().count(), 0);
    assert_eq!(named(&capture(&mut view), "Manage prompt"), 0);

    let mut view = subscribed();
    let work = id(&view, "Work");
    assert!(press(&mut view, Control::ProfileSync(work)));
    assert_eq!(view.c.cloud_sync_pending_for(work), Some(false));
    assert_eq!(view.c.prompts().count(), 0, "off is D227's immediate stop");
}

#[test]
fn sign_in_opens_that_profiles_page() {
    let mut view = subscribed();
    let scratch = id(&view, "Scratch");
    assert!(press(&mut view, Control::ProfileSignIn(scratch)));
    assert_eq!(view.page(), Some(Page::Profile(scratch)));
    assert_eq!(view.controller().selected(), Some(scratch));
    assert!(view.laid.sign_in.is_some(), "on its Sign-in and sync");
}

// ------------------------------------------- the account's Profiles not here

/// Subscribed, with one account Profile — *Family* — that is not on this
/// computer.
fn with_family_elsewhere() -> (ManageView, ProfileId) {
    let mut view = subscribed();
    let family = ProfileId::new();
    view.c.take_request();
    view.c
        .complete(koru_agent_wire::AccountOperationResult::Profiles {
            profiles: [
                (family, "Family"),
                (id(&view, "Work"), "Work"),
                (id(&view, "Home"), "Home"),
            ]
            .into_iter()
            .map(|(profile, name)| koru_agent_wire::AccountProfileRow {
                profile_id: profile.to_storage(),
                koru_identity_id: ACCOUNT.into(),
                name: Some(name.into()),
                locale: None,
                auth_identities: vec![],
            })
            .collect(),
        });
    view.c.take_request();
    // Draining the queue asked for the subscription again; put its answer
    // back, as the Agent's would be.
    answered(&mut view, CloudStatus::Manage);
    (view, family)
}

/// **A Profile this sign-in is not linked to says so** (2026-11-11). After a
/// Google Account is unlinked from a Profile, the Profile is still on this
/// computer and still the account's, and Koru Cloud refuses it. Its row used
/// to keep the signed-in email, a live switch left on and *Koru Cloud
/// unavailable*; it reads *Not linked to this sign-in*, has *Connect…* where
/// the switch was, and is not counted among the Profiles that sync.
/// *Connect…* opens its page, whose *Sign-in and sync* connects it again.
#[test]
fn a_profile_this_sign_in_is_not_linked_to_says_so_and_offers_connect() {
    let mut view = subscribed();
    let (work, home) = (id(&view, "Work"), id(&view, "Home"));
    // The account's list of what this sign-in may reach: Home, and not Work.
    view.c.take_request();
    view.c
        .complete(koru_agent_wire::AccountOperationResult::Profiles {
            profiles: vec![koru_agent_wire::AccountProfileRow {
                profile_id: home.to_storage(),
                koru_identity_id: ACCOUNT.into(),
                name: Some("Home".into()),
                locale: None,
                auth_identities: vec![],
            }],
        });
    view.c.take_request();
    // Draining the queue asked for the subscription again; put its answer
    // back, as the Agent's would be.
    answered(&mut view, CloudStatus::Manage);

    let laid = cloud(&view);
    let row = laid.rows.iter().find(|row| row.profile == work).unwrap();
    assert_eq!(row.sub_text, "Not linked to this sign-in");
    assert!(row.toggle.is_none(), "no switch that cannot work");
    assert_eq!(
        view.laid
            .placed(Control::ProfileSignIn(work))
            .unwrap()
            .label,
        "Connect\u{2026}"
    );
    assert!(row.progress.is_none());
    assert!(view.laid.placed(Control::ProfileSync(work)).is_none());
    assert!(view.laid.placed(Control::Progress(work)).is_none());
    assert!(!view.c.cloud_sync_on(work), "whatever was remembered");
    assert_eq!(view.c.cloud_profile_counts(), Some((0, 1)), "Home alone");
    // Home, which the sign-in is linked to, is untouched.
    assert!(view.laid.placed(Control::ProfileSync(home)).is_some());

    // *Connect…* lands on the Profile's page, which offers the connection
    // the account's attach makes — and asks before sending anything.
    assert!(press(&mut view, Control::ProfileSignIn(work)));
    assert_eq!(view.page(), Some(Page::Profile(work)));
    assert!(view.controller().selected_profile_is_unlinked());
    assert!(press(&mut view, Control::Attach));
    assert_eq!(view.c.prompts().count(), 1);
}

/// **The account's Profiles that are not on this computer are rows of *Which
/// profiles sync*, after the ones that are** (2026-11-11) — not a group in
/// the nav. Each has a switch, off, and says where it stands.
#[test]
fn an_account_profile_that_is_not_here_is_a_row_with_a_switch() {
    let (view, family) = with_family_elsewhere();
    let laid = cloud(&view);
    assert_eq!(laid.rows.len(), 3, "the Profiles on this computer");
    assert_eq!(laid.elsewhere.len(), 1);
    let row = &laid.elsewhere[0];
    assert_eq!(
        (row.profile, row.name_text.as_str(), row.sub_text.as_str()),
        (family, "Family", "Not on this computer")
    );
    assert!(row.rect.y >= laid.rows[2].rect.y + laid.rows[2].rect.h);
    assert!(row.rect.y + row.rect.h <= laid.sync_card.y + laid.sync_card.h + 0.5);
    let placed = view.laid.placed(Control::ProfileBring(family)).unwrap();
    assert!(matches!(placed.look, Look::Toggle(_)));
    assert_eq!(placed.label, "Sync Family to this computer");
    // The nav has no row for it.
    assert!(view.focus_order().iter().all(
        |entry| matches!(entry, nav::Entry::Profile(id) if *id != family)
            || !matches!(entry, nav::Entry::Profile(_))
    ));
    // Tab reaches it after the local Profiles' controls.
    let order = view.page_focus_order();
    let at = |control| order.iter().position(|c| *c == control).unwrap();
    assert!(at(Control::ProfileSignIn(id(&view, "Scratch"))) < at(Control::ProfileBring(family)));
}

/// Its switch is the controller's inline connect — what the nav's *Connect*
/// was. While that is out the switch shows where it is going and is not a
/// control.
#[test]
fn its_switch_brings_the_profile_to_this_computer() {
    let (mut view, family) = with_family_elsewhere();
    assert!(press(&mut view, Control::ProfileBring(family)));
    assert_eq!(view.controller().inline_connect(), Some((family, false)));
    view.relayout();
    assert!(view.laid.placed(Control::ProfileBring(family)).is_none());
    let row = &cloud(&view).elsewhere[0];
    assert!(matches!(row.toggle, Some((_, _, false))));
    assert!(
        !view
            .page_focus_order()
            .contains(&Control::ProfileBring(family))
    );
}

// ------------------------------------------------------------- progress

/// Decision 20: a syncing Profile's progress opens under its row, one at a
/// time, and the poll runs only while Koru Cloud is the tab in front.
#[test]
fn the_progress_disclosure_opens_under_its_row_and_polls_only_while_in_front() {
    let mut view = subscribed();
    let (work, home) = (id(&view, "Work"), id(&view, "Home"));
    assert!(
        view.laid.placed(Control::Progress(home)).is_none(),
        "Home does not sync"
    );
    let show = view.laid.placed(Control::Progress(work)).unwrap();
    assert_eq!(show.label, SHOW_PROGRESS);
    assert_eq!(
        view.c.cloud_progress_profile(),
        None,
        "closed: nothing polls"
    );
    View::maintain(&mut view, &mut doc(), Instant::now());
    assert_eq!(view.maintenance_deadline(), None);

    assert!(press(&mut view, Control::Progress(work)));
    assert_eq!(view.disclosed(), Some(work));
    assert_eq!(view.c.cloud_progress_profile(), Some(work));
    assert_eq!(
        view.laid.placed(Control::Progress(work)).unwrap().label,
        HIDE_PROGRESS
    );
    View::maintain(&mut view, &mut doc(), Instant::now());
    assert!(
        view.maintenance_deadline().is_some(),
        "polled while showing"
    );
    let laid = cloud(&view).rows[0]
        .progress
        .clone()
        .expect("open under Work");
    assert!(laid.rect.y >= cloud(&view).rows[0].sub.y);

    // Behind another tab, or on another page, the poll stops.
    view.set_in_front(false);
    View::maintain(&mut view, &mut doc(), Instant::now());
    assert_eq!(view.maintenance_deadline(), None);
    view.set_in_front(true);
    view.show(Page::Devices);
    View::maintain(&mut view, &mut doc(), Instant::now());
    assert_eq!(view.c.cloud_progress_profile(), None);
    view.show(Page::Cloud);
    answered(&mut view, CloudStatus::Manage);
    assert_eq!(
        view.c.cloud_progress_profile(),
        Some(work),
        "still open on return"
    );

    // Pressed again, it closes.
    assert!(press(&mut view, Control::Progress(work)));
    assert_eq!(view.disclosed(), None);
    assert!(cloud(&view).rows[0].progress.is_none());
}

/// Only a held row opens, onto the documents it waits for.
#[test]
fn a_held_progress_row_opens_onto_its_waiting_documents() {
    let mut view = subscribed();
    let work = id(&view, "Work");
    assert!(press(&mut view, Control::Progress(work)));
    let notes = row("Notes", synced());
    let photos = row("Photos", held("object_missing"));
    let (notes_id, photos_id) = (notes.id, photos.id);
    view.c.progress_mut().show(work, vec![notes, photos]);
    view.relayout();
    assert!(view.laid.placed(Control::ProgressRow(notes_id)).is_none());
    assert!(view.laid.placed(Control::ProgressRow(photos_id)).is_some());
    let laid = cloud(&view).rows[0].progress.clone().unwrap();
    assert_eq!(laid.rows.len(), 2);
    assert!(
        laid.summary_text.starts_with("1 of 2"),
        "{}",
        laid.summary_text
    );

    assert!(press(&mut view, Control::ProgressRow(photos_id)));
    let lines = view.c.cloud_progress_lines().unwrap();
    assert_eq!(lines.rows[1].waiting, Some(Waiting::Reading));
    let laid = cloud(&view).rows[0].progress.clone().unwrap();
    assert!(laid.rows[1].open);
    assert_eq!(laid.rows[1].lines.len(), 1, "reading…");
    assert_eq!(named(&capture(&mut view), "Manage cloud progress"), 1);
}

// ------------------------------------------------------------- account switcher

#[test]
fn accounts_opens_the_switcher_whose_keys_and_wheel_are_its_own() {
    let mut view = on_cloud(signed_in());
    assert!(press(&mut view, Control::Accounts));
    assert!(view.switcher_open());
    assert_eq!(named(&capture(&mut view), "Manage account switcher"), 1);
    // Escape closes it, as it always has.
    assert!(key(&mut view, NamedKey::Escape));
    assert!(!view.switcher_open());

    // Enter on its first row, with no other account held, is Add Account.
    assert!(press(&mut view, Control::Accounts));
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert!(view.switcher_open(), "the wheel is the switcher's");
    assert!(key(&mut view, NamedKey::Enter));
    assert!(!view.switcher_open());
    assert!(matches!(
        view.c.take_request(),
        Some(AccountCommand::AddAccount { .. })
    ));

    // A press in the nav closes it and does what it landed on.
    assert!(press(&mut view, Control::Accounts));
    let devices = view
        .nav_layout()
        .rows
        .into_iter()
        .find(|(entry, _)| *entry == nav::Entry::Devices)
        .unwrap()
        .1;
    let (x, y) = devices.centre();
    assert!(View::mouse_down(&mut view, &mut doc(), x, y, false));
    assert!(!view.switcher_open());
    assert_eq!(view.page(), Some(Page::Devices));
}

/// maintainers' decision 16: *{n} Profiles hidden* opens the switcher, on Koru Cloud.
#[test]
fn the_hidden_line_opens_the_switcher() {
    let mut view = on_cloud(signed_in());
    view.show(Page::Devices);
    view.delegate(|c| c.set_hidden_profile_count(2));
    assert!(view.activate(nav::Entry::Hidden));
    assert_eq!(view.page(), Some(Page::Cloud));
    assert!(view.switcher_open());
}

// ------------------------------------------------- a build without Koru Cloud

/// **A build with no Koru Cloud says so, and nothing on the page signs in.**
/// No attention card (it is not an outage), no *Try again*, no *Sign in…*,
/// no *Accounts…* — and nothing is ever put on the wire to the Agent.
#[test]
fn a_build_without_koru_cloud_says_so_and_offers_no_sign_in() {
    use koru_i18n::{Key, t};
    let lang = crate::format::lang();
    let mut c = ProfilesController::new();
    c.clear_pending();
    *c.profiles_mut() = vec![profile("Work", Some(ACCOUNT)), profile("Scratch", None)];
    *c.account_mut() = AccountState::not_configured();
    let mut view = on_cloud(c);

    let laid = cloud(&view).clone();
    assert!(laid.unreachable.is_none(), "not an outage");
    assert_eq!(
        laid.subscription
            .text
            .as_ref()
            .map(|(_, text)| text.as_str()),
        Some(t(lang, Key::CloudNotConfiguredBody))
    );
    assert_eq!(laid.account.hint, t(lang, Key::NotSetUpInThisBuild));
    assert_eq!(laid.rows.len(), 2);
    for row in &laid.rows {
        assert_eq!(row.sub_text, t(lang, Key::CloudNotConfigured));
        assert!(row.toggle.is_none());
        assert!(
            view.laid
                .placed(Control::ProfileSignIn(row.profile))
                .is_none()
        );
    }
    for control in [Control::Accounts, Control::CloudPrimary] {
        assert!(view.laid.placed(control).is_none(), "{control:?}");
    }
    assert!(!view.c.cloud_actionable());

    // Anything that would start a sign-in is dropped before the wire.
    for intent in [
        Intent::Session,
        Intent::SignInGoogle,
        Intent::CloudPrimary,
        Intent::SignInAnotherAccount,
    ] {
        view.perform(intent);
    }
    view.open_account_switcher();
    assert!(!view.switcher_open());
    assert_eq!(view.c.take_request(), None);
}

// ------------------------------------------------------------- attention

fn unreachable() -> ManageView {
    let mut c = ProfilesController::new();
    c.clear_pending();
    *c.profiles_mut() = vec![profile("Work", Some(ACCOUNT))];
    *c.account_mut() = AccountState {
        active_scope: 1,
        active_account_id: Some(ACCOUNT.into()),
        stage: AccountStage::Unavailable,
        ..Default::default()
    };
    on_cloud(c)
}

fn unreachable_answer(
    view: &ManageView,
    wanted: koru_workspace_host::account_ui::Action,
) -> Control {
    let surface = view
        .c
        .account_wide_surface()
        .expect("the can't-connect card");
    Control::Unreachable(surface.actions.iter().position(|a| *a == wanted).unwrap())
}

#[test]
fn cant_connect_is_an_attention_card_with_retry_and_continue_offline() {
    use koru_workspace_host::account_ui::Action;
    let mut view = unreachable();
    let words = cloud(&view).unreachable.clone().expect("at the top");
    assert!(words.rect.y < cloud(&view).account_card.y);
    assert_eq!(named(&capture(&mut view), "Manage can't connect"), 1);

    let retry = unreachable_answer(&view, Action::Retry);
    assert!(press(&mut view, retry));
    assert_eq!(view.c.take_request(), Some(AccountCommand::Retry));

    let mut view = unreachable();
    let offline = unreachable_answer(&view, Action::ContinueOffline);
    assert!(press(&mut view, offline));
    assert!(cloud(&view).unreachable.is_none(), "put aside");
    assert_eq!(view.c.take_request(), None);
}

fn another_account() -> ManageView {
    let mut c = signed_in();
    *c.profiles_mut() = vec![profile(
        "Theirs",
        Some("20000000-0000-4000-8000-000000000002"),
    )];
    c.set_signed_in_identity("google", "me@example.com");
    c.set_account_profiles_loaded(true);
    on_cloud(c)
}

#[test]
fn another_accounts_profiles_are_an_attention_card_beside_the_switcher() {
    let mut view = another_account();
    let laid = cloud(&view);
    let words = laid.different.clone().expect("decision 4");
    assert!(words.rect.y >= laid.account_card.y + laid.account_card.h);
    assert_eq!(
        view.laid.placed(Control::DifferentCreate).unwrap().label,
        "Create a new Profile with me@example.com"
    );
    assert_eq!(named(&capture(&mut view), "Manage different account"), 1);

    assert!(press(&mut view, Control::DifferentSignIn));
    assert!(matches!(
        view.c.take_request(),
        Some(AccountCommand::StartGoogle { .. })
    ));
    assert!(press(&mut view, Control::DifferentOffline));
    assert!(cloud(&view).different.is_none(), "put aside");

    let mut view = another_account();
    assert!(press(&mut view, Control::DifferentCreate));
    assert_eq!(view.page(), Some(Page::NewProfile));
    assert!(view.controller().creating());
}

// ------------------------------------------------------------- inspection

#[test]
fn the_inspector_names_every_part_of_the_page() {
    let mut view = subscribed();
    let work = id(&view, "Work");
    assert!(press(&mut view, Control::Progress(work)));
    let snapshot = capture(&mut view);
    for name in [
        "Manage Koru Cloud",
        "Manage accounts",
        "Manage subscription",
        "Manage profiles sync",
        "Manage cloud progress",
        "KoruToggle",
        "KoruCard",
    ] {
        assert!(named(&snapshot, name) >= 1, "{name}");
    }
    assert_eq!(named(&snapshot, "Manage sync row"), 3);
}

// ------------------------------------------------------------- accessibility

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

#[test]
fn a_screen_reader_hears_switches_buttons_and_the_disclosure() {
    let mut view = subscribed();
    let (work, scratch) = (id(&view, "Work"), id(&view, "Scratch"));
    let cx = accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(0.0, 0.0, 1000.0, 2400.0),
    };
    let mut nodes = Vec::new();
    flatten(&view.semantics(&cx), &mut nodes);
    let find = |label: &str| {
        nodes
            .iter()
            .find(|node| node.label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("{label} is published"))
            .clone()
    };
    let switch = find("Cloud sync for Work");
    assert_eq!(switch.role, accesskit::Role::Switch);
    assert_eq!(switch.toggled, Some(accesskit::Toggled::True));
    assert!(switch.occurrence.locator().ends_with("#manage-sync"));
    let sign_in = find("Sign in to Scratch\u{2026}");
    assert_eq!(sign_in.role, accesskit::Role::Button);
    assert!(sign_in.occurrence.locator().ends_with("#manage-sign-in"));
    let disclosure = find("Show sync progress, Work");
    assert_eq!(disclosure.role, accesskit::Role::Button);
    assert!(
        disclosure
            .occurrence
            .locator()
            .ends_with("#manage-progress")
    );
    assert!(
        find("Accounts\u{2026}")
            .occurrence
            .locator()
            .ends_with("s:manage#accounts")
    );
    let heading = nodes
        .iter()
        .find(|node| node.role == accesskit::Role::Heading)
        .unwrap();
    assert_eq!(heading.label.as_deref(), Some("Koru Cloud"));
    // Each Profile's controls are about that Profile, wherever its row is.
    assert_ne!(switch.occurrence, find("Cloud sync for Home").occurrence);
    assert!(scratch != work);

    // Pressing the disclosure is the click.
    assert!(view.accessibility_action(&cx, disclosure.occurrence, accesskit::Action::Click));
    assert_eq!(view.disclosed(), Some(work));
    // The open switcher is a dialog, and has the focus.
    assert!(press(&mut view, Control::Accounts));
    assert_eq!(view.accessible_focus(&cx), cx_dialog(&view, &cx));
}

fn cx_dialog(
    view: &ManageView,
    cx: &accessibility::Context,
) -> koru_workspace_host::accessibility::Occurrence {
    let mut nodes = Vec::new();
    flatten(&view.semantics(cx), &mut nodes);
    nodes
        .iter()
        .find(|node| node.role == accesskit::Role::Dialog)
        .expect("the switcher is a dialog")
        .occurrence
}
