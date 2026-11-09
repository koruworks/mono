// SPDX-License-Identifier: MPL-2.0

//! What the container does with the Manage tab: the switcher's *New profile*
//! lands there, it is one tab, it never shares the window with Manage
//! Profiles, its controller hears the account and drains through the same
//! requests, it survives a restart, and a screen reader finds its nav.
use super::*;
use crate::mainview::harness::*;
use crate::manage::Page;
use koru_workspace_host::accessibility::{Scope, SemanticNode, ViewSlot, WindowSlot};

fn profile(name: &str) -> ProfileSummary {
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

/// A container whose window has one Profile open.
fn with_profile() -> (MainView, Workspace, ProfileSummary) {
    let (mut v, w) = container();
    let personal = profile("Personal");
    View::set_pane_context(
        &mut v,
        vec![personal.clone()],
        personal.id,
        Vec::new(),
        None,
    );
    (v, w, personal)
}

fn manage_tabs(v: &MainView) -> usize {
    v.panes
        .iter_groups()
        .flat_map(|(_, group)| group.tabs().iter())
        .filter(|tab| matches!(tab.view, TabView::Manage(_)))
        .count()
}

fn active_manage(v: &mut MainView) -> &mut crate::manage::ManageView {
    match &mut v.tabs_mut().active_mut().view {
        TabView::Manage(view) => view,
        _ => panic!("the Manage tab is in front"),
    }
}

#[test]
fn the_switchers_new_profile_opens_manage_on_its_new_profile_page() {
    let (mut v, w, _) = with_profile();
    assert!(v.apply(&w.ws, crate::panes::Outcome::NewProfile));
    assert_eq!(v.tabs().active_tab().label, "Manage");
    let manage = active_manage(&mut v);
    assert_eq!(manage.page(), Some(Page::NewProfile));
    assert!(
        manage.controller().creating(),
        "on the embedded create form"
    );
}

#[test]
fn manage_is_one_tab_brought_forward_on_the_asked_page() {
    let (mut v, w, personal) = with_profile();
    assert!(v.open_manage(Page::Devices));
    let at = v.tabs().active();
    let count = v.tabs().len();
    v.select(0);
    assert!(v.apply(&w.ws, crate::panes::Outcome::NewProfile));
    assert_eq!(v.tabs().len(), count, "no second Manage tab");
    assert_eq!(v.tabs().active(), at, "the existing one came forward");
    assert_eq!(manage_tabs(&v), 1);
    assert!(v.apply(&w.ws, crate::panes::Outcome::ManageCloud));
    assert_eq!(active_manage(&mut v).page(), Some(Page::Cloud));
    assert_eq!(v.tabs().active(), at);
    assert_eq!(manage_tabs(&v), 1, "Cloud reuses the existing Manage tab");
    assert!(v.open_manage(Page::Profile(personal.id)));
    assert_eq!(
        active_manage(&mut v).page(),
        Some(Page::Profile(personal.id))
    );
}

/// A Profile row's gear on the who/where popover opens Manage on that
/// Profile's page.
#[test]
fn a_profiles_gear_opens_manage_on_its_page() {
    let (mut v, w, personal) = with_profile();
    assert!(v.apply(&w.ws, crate::panes::Outcome::ManageProfile(personal.id)));
    assert_eq!(manage_tabs(&v), 1);
    assert_eq!(
        active_manage(&mut v).page(),
        Some(Page::Profile(personal.id))
    );
    // Again from another page: the same tab comes back to the Profile.
    assert!(v.open_manage(Page::Cloud));
    assert!(v.apply(&w.ws, crate::panes::Outcome::ManageProfile(personal.id)));
    assert_eq!(manage_tabs(&v), 1);
    assert_eq!(
        active_manage(&mut v).page(),
        Some(Page::Profile(personal.id))
    );
}

/// Cloud is read only while a page that shows it is the tab in front: Koru
/// Cloud asks for the subscription; *Linked devices* does not.
#[test]
fn koru_cloud_reads_the_subscription_only_while_it_is_in_front() {
    let (mut v, _, _) = with_profile();
    View::set_account_state(
        &mut v,
        koru_agent_wire::AccountState {
            active_scope: 7,
            active_account_id: Some("70000000-0000-4000-8000-000000000007".into()),
            generation: 3,
            stage: koru_agent_wire::AccountStage::SignedIn,
            ..Default::default()
        },
    );
    let inspects = |v: &mut MainView| {
        let mut asked = false;
        while let Some(request) = View::take_pane_request(v) {
            asked |= matches!(
                request,
                PaneRequest::AccountTracked {
                    command: koru_agent_wire::AccountCommand::InspectCloudSubscription { .. },
                    ..
                }
            );
        }
        asked
    };
    assert!(v.open_manage(Page::Devices));
    assert!(!inspects(&mut v), "Linked devices reads no Cloud");
    assert!(v.open_manage(Page::Cloud));
    assert!(inspects(&mut v), "Koru Cloud asks for the subscription");
}

/// The fan-out reaches the Manage tab, and what its controller asks for
/// leaves through the same `PaneRequest`s Manage Profiles' does.
#[test]
fn manage_hears_the_account_and_drains_through_the_shared_requests() {
    let (mut v, _, _) = with_profile();
    assert!(v.open_manage(Page::Cloud));
    // Opening asks the Agent for the device-wide session, as Manage
    // Profiles always has.
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::AccountOwned {
            command: koru_agent_wire::AccountCommand::Status,
            ..
        })
    ));
    while View::take_pane_request(&mut v).is_some() {}
    View::set_account_state(
        &mut v,
        koru_agent_wire::AccountState {
            active_scope: 41,
            generation: 9,
            stage: koru_agent_wire::AccountStage::SignedOut,
            ..Default::default()
        },
    );
    assert_eq!(active_manage(&mut v).controller().account_scope(), 41);
}

#[test]
fn the_manage_tab_comes_back_after_a_restart_on_the_open_profile() {
    let (mut v, workspace, personal) = with_profile();
    assert!(v.open_manage(Page::Devices));
    let mut settings = koru_workspace_host::Settings::default();
    v.capture(&mut settings);
    assert!(
        settings
            .tabs
            .iter()
            .any(|(which, _)| *which == koru_workspace_host::TabRef::Manage),
        "it was written down as `manage`"
    );

    let (mut back, _w) = container();
    back.restore(&workspace, &settings);
    View::set_pane_context(
        &mut back,
        vec![personal.clone()],
        personal.id,
        Vec::new(),
        None,
    );
    let page = back
        .panes
        .iter_groups()
        .flat_map(|(_, group)| group.tabs().iter())
        .find_map(|tab| match &tab.view {
            TabView::Manage(view) => Some(view.page()),
            _ => None,
        })
        .expect("it came back");
    assert_eq!(page, Some(Page::Profile(personal.id)));
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

#[test]
fn a_screen_reader_finds_the_nav_under_the_content_region() {
    let (mut v, mut workspace, _) = with_profile();
    v.seat_accessibility(WindowSlot::from_raw(1));
    assert!(v.open_manage(Page::Profile(
        active_profile(&v).expect("a Profile is open")
    )));
    let view = ViewSlot::from_raw(1);
    let mut nodes = Vec::new();
    for node in v.semantics(view, Scope::Whole) {
        flatten(&node, &mut nodes);
    }
    let nav = nodes
        .iter()
        .find(|node| node.occurrence.locator().ends_with("s:manage#nav"))
        .expect("the nav is published");
    assert_eq!(nav.role, accesskit::Role::ListBox);
    assert!(
        nav.children
            .iter()
            .any(|option| option.label.as_deref() == Some("Personal, open"))
    );
    let cloud = nav
        .children
        .iter()
        .find(|option| option.occurrence.locator().ends_with("s:manage#cloud"))
        .expect("Koru Cloud is an option")
        .occurrence;
    assert!(v.accessibility_action(&mut workspace.ws, cloud, accesskit::Action::Click, None));
    assert_eq!(active_manage(&mut v).page(), Some(Page::Cloud));
}

fn active_profile(v: &MainView) -> Option<ProfileId> {
    v.pane_context.as_ref().map(|(_, current, _, _)| *current)
}

/// The Profile header's name opens the window's own rename field over it,
/// and its tile the icon and color panel: no second editor in the tab.
#[test]
fn the_profile_header_opens_the_windows_rename_field_and_icon_panel() {
    let (mut v, _w, personal) = with_profile();
    assert!(v.open_manage(Page::Profile(personal.id)));
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
    let manage = active_manage(&mut v);
    let name = manage
        .profile_name_rect(personal.id)
        .expect("the header shows");
    let tile = manage.laid().header.tile;
    let (x, y) = manage.laid().header.name.centre();
    assert!(View::mouse_down(manage, &mut doc, x, y, false));
    assert!(v.drain_manage_profiles_request());
    assert!(matches!(
        v.workspace_rename.as_ref().map(|field| field.target),
        Some(RenameTarget::Profile(id)) if id == personal.id
    ));
    let view = v.focused_view_rect();
    let row = v.workspace_rename_row().expect("the field has a row");
    assert_eq!((row.x, row.y), (view.x + name.x, view.y + name.y));
    v.workspace_rename = None;

    let (x, y) = tile.centre();
    assert!(View::mouse_down(
        active_manage(&mut v),
        &mut doc,
        x,
        y,
        false
    ));
    assert!(v.drain_manage_profiles_request());
    assert!(v.workspace_icon.is_some(), "the icon and color panel");
}

/// *Switch to this profile* leaves through the same request the switcher's
/// rows do.
#[test]
fn switch_to_this_profile_asks_the_window_to_open_it() {
    let (mut v, _w) = container();
    let personal = profile("Personal");
    let work = profile("Work");
    View::set_pane_context(
        &mut v,
        vec![personal.clone(), work.clone()],
        personal.id,
        Vec::new(),
        None,
    );
    assert!(v.open_manage(Page::Profile(work.id)));
    while View::take_pane_request(&mut v).is_some() {}
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
    let manage = active_manage(&mut v);
    let (x, y) = manage
        .laid()
        .placed(crate::manage::Control::Switch)
        .expect("another Profile's page offers it")
        .rect
        .centre();
    assert!(View::mouse_down(manage, &mut doc, x, y, false));
    assert!(v.drain_manage_profiles_request());
    let mut asked = Vec::new();
    while let Some(request) = View::take_pane_request(&mut v) {
        asked.push(request);
    }
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, PaneRequest::SwitchProfile(id) if *id == work.id)),
        "{asked:?}"
    );
}

fn linked_device(id: &str) -> koru_agent_wire::LinkedDeviceView {
    koru_agent_wire::LinkedDeviceView {
        device_id: id.into(),
        display_name: id.into(),
        linked_at_millis: 0,
        connected: true,
        last_seen_millis: None,
        introduced_by: None,
        build_label: None,
        build_drift: None,
        build_severity: None,
        update_needed_on: None,
        connection: None,
        last_sync_millis: None,
        history: Vec::new(),
    }
}

/// maintainers' decision 12: the dialog says *Device linked*, and *Done* opens
/// Manage on *Linked devices*.
#[test]
fn done_after_a_device_is_linked_opens_manage_on_linked_devices() {
    let (mut v, _w, _) = with_profile();
    assert!(v.open_link_device());
    let dialog = v.link_device.as_mut().expect("the Link dialog is up");
    assert!(dialog.set_pairing(
        &mut v.engine,
        koru_agent_wire::PairingView {
            state: koru_agent_wire::PairingState::Succeeded,
            qr_payload: None,
            expires_in_seconds: 0,
            peer_name: Some("Pixel".into()),
            error_code: None,
        },
    ));
    let outcome = v.link_device.as_mut().unwrap().key(
        &mut v.engine,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        winit::keyboard::ModifiersState::empty(),
    );
    assert!(matches!(outcome, LinkDeviceOutcome::Done));
    assert!(v.settle_link_device(outcome));
    assert!(v.link_device.is_none(), "Done closes the dialog");
    assert_eq!(manage_tabs(&v), 1);
    assert_eq!(active_manage(&mut v).page(), Some(Page::Devices));
}

/// **The window's one drain of Agent's organization status reaches the
/// switcher's icon** (root-catalog Plan 2.2): a pending root unit for the
/// open Profile turns the icon's word to *Pending* on the next maintenance
/// pass, and the same snapshot twice is no change.
#[test]
fn the_switcher_hears_the_windows_organization_status() {
    use koru_agent_wire::{RootSyncState, RootSyncStatus, RootSyncSubject};
    let (mut v, _w, personal) = with_profile();
    assert_eq!(
        v.explore.sync_status(),
        crate::panes::explore::SyncStatus::NotSetUp
    );
    v.agent.answer_root_sync_for_test(RootSyncStatus {
        subjects: vec![RootSyncSubject {
            peer_device_id: "laptop".into(),
            profile_id: Some(personal.id.to_string()),
            workspace_id: None,
            state: RootSyncState::Pending,
        }],
    });
    assert!(
        v.refresh_root_sync(),
        "the snapshot changed what the icon says"
    );
    assert_eq!(
        v.explore.sync_status(),
        crate::panes::explore::SyncStatus::Pending
    );
    assert!(!v.refresh_root_sync(), "drained");
}

/// The window's one drain of Agent's device list reaches the page, and the
/// page's *Link a device…* is the window's dialog.
#[test]
fn the_linked_devices_page_hears_the_windows_snapshot_and_opens_the_dialog() {
    let (mut v, _w, _) = with_profile();
    assert!(v.open_manage(Page::Devices));
    v.agent.answer_linked_devices_for_test(
        Some(linked_device("this")),
        vec![linked_device("phone"), linked_device("tablet")],
    );
    v.refresh_link_device();
    let manage = active_manage(&mut v);
    assert_eq!(manage.linked().devices().len(), 2);
    assert!(manage.linked().this_device().is_some());
    let (x, y) = manage
        .laid()
        .placed(crate::manage::Control::LinkDevice)
        .expect("the page's primary button")
        .rect
        .centre();
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
    assert!(View::mouse_down(manage, &mut doc, x, y, false));
    assert!(v.drain_manage_requests());
    assert!(
        v.link_device.is_some(),
        "the dialog opened on its New device tab"
    );
}
