// SPDX-License-Identifier: MPL-2.0

//! What the window does with a Profile page's Workspaces: the routes into
//! them (the switcher's *Manage workspaces…*, `Ctrl+Shift+O`, a restored
//! `workspaces` token), the one workspace menu and what each of its rows
//! does or refuses, and the requests the page raises. Ports of the
//! Workspaces tab's container tests, which now act on the Manage tab.
use super::*;
use crate::mainview::harness::*;
use crate::manage::Page;
use crate::manage::workspaces::{Wid, app_workspace};
use koru_workspaces_ui::{Authorization, Row};
use winit::keyboard::NamedKey;

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

#[test]
fn only_verified_retirement_asks_the_host_to_leave_and_pending_rows_stay_out_of_navigation() {
    let mut f = fixture();
    let owner = profile("Owner");
    let current = WorkspaceId::new();
    let mut entry = row(current, &owner, "Current", 0);
    for authority in [
        koru_workspaces_ui::Authority::Unknown,
        koru_workspaces_ui::Authority::Pending,
        koru_workspaces_ui::Authority::Uncertain {
            cause: "storage_failed".into(),
            recovery: None,
        },
    ] {
        entry.authority = authority;
        entry.deleted = true;
        f.v.workspace_controller
            .borrow_mut()
            .adopt_rows(&[entry.clone()]);
        assert!(!View::workspace_retired(&f.v, owner.id, current));
    }
    entry.authority = koru_workspaces_ui::Authority::Verified;
    entry.listing_differs = true;
    f.v.workspace_controller
        .borrow_mut()
        .adopt_rows(&[entry.clone()]);
    assert!(View::workspace_retired(&f.v, owner.id, current));
    entry.deleted = false;
    entry.archived = true;
    f.v.workspace_controller
        .borrow_mut()
        .adopt_rows(&[entry.clone()]);
    assert!(View::workspace_retired(&f.v, owner.id, current));
    entry.archived = false;
    f.v.workspace_controller
        .borrow_mut()
        .adopt_rows(&[entry.clone()]);
    assert!(
        !View::workspace_retired(&f.v, owner.id, current),
        "repair alone is not removal"
    );
    assert!(
        View::profile_has_live_content(&f.v, owner.id),
        "repairable local content precedes Cloud"
    );
    entry.authority = koru_workspaces_ui::Authority::Pending;
    f.v.workspace_controller
        .borrow_mut()
        .adopt_rows(&[entry.clone()]);
    assert!(!View::profile_has_live_content(&f.v, owner.id));
    entry.authority = koru_workspaces_ui::Authority::Verified;
    entry.owner = koru_store_core::ProfileId::new();
    f.v.workspace_controller.borrow_mut().adopt_rows(&[entry]);
    assert!(View::workspace_retired(&f.v, owner.id, current));
}

#[test]
fn availability_runs_on_the_window_link_with_manage_closed() {
    use std::sync::{Arc, mpsc};
    let mut f = fixture();
    let (links, worker) = koru_workspaces_ui::WorkspaceLinks::fixture(Arc::new(|| {}));
    f.v.workspace_controller = std::rc::Rc::new(std::cell::RefCell::new(
        WorkspacesController::with_links(links),
    ));
    let owner = koru_store_core::ProfileId::new();
    let (reply, answer) = mpsc::channel();
    assert!(View::ensure_live_workspace(
        &mut f.v,
        owner,
        "First".into(),
        7,
        reply
    ));
    assert!(f.v.manage_view().is_none());
    assert!(f.v.workspaces_poll_due.is_some());
    assert!(answer.try_recv().is_err());
    assert_eq!(
        worker.settle_availability(Ok(koru_api_client::EnsuredWorkspace::Pending {
            workspaces: vec![],
        })),
        (owner, "First".into(), 7)
    );
    View::maintain(&mut f.v, &mut f.w.ws, Instant::now());
    assert!(matches!(
        answer.try_recv().unwrap().unwrap(),
        koru_api_client::EnsuredWorkspace::Pending { .. }
    ));
    assert!(!f.v.workspace_controller.borrow().has_pending());
    assert!(f.v.manage_view().is_none());
}

#[test]
fn embedded_profile_publication_runs_on_the_window_link_with_manage_closed() {
    use std::sync::{Arc, mpsc};
    let mut f = fixture();
    let (links, worker) = koru_workspaces_ui::WorkspaceLinks::fixture(Arc::new(|| {}));
    f.v.workspace_controller = std::rc::Rc::new(std::cell::RefCell::new(
        WorkspacesController::with_links(links),
    ));
    let id = koru_store_core::ProfileId::new();
    let (reply, answer) = mpsc::channel();
    assert!(View::create_profile_with_workspace(
        &mut f.v,
        koru_api_client::CreateProfileWithWorkspace {
            id,
            name: "Owner".into(),
            locale: None,
            purpose: None,
            avatar: None,
            icon: None,
            color: None,
            workspace: koru_api_client::FirstWorkspace {
                name: "First".into(),
                icon: None,
                color: None
            },
        },
        reply
    ));
    assert!(f.v.manage_view().is_none());
    assert!(f.v.workspaces_poll_due.is_some());
    assert!(answer.try_recv().is_err());
    let sent = worker.settle_profile(Err(koru_store_core::Error::bare(
        koru_store_core::Code::ProfileExists,
    )));
    assert_eq!(sent.id, id);
    View::maintain(&mut f.v, &mut f.w.ws, Instant::now());
    assert_eq!(
        answer.try_recv().unwrap().unwrap_err().code(),
        koru_store_core::Code::ProfileExists
    );
    assert!(!f.v.workspace_controller.borrow().has_pending());
    assert!(f.v.manage_view().is_none());
}

/// A window with *Personal* open on `current` and *Work* beside it.
struct Fixture {
    v: MainView,
    w: Workspace,
    personal: ProfileSummary,
    work: ProfileSummary,
    current: WorkspaceId,
}

fn fixture() -> Fixture {
    let (mut v, w) = container();
    let personal = profile("Personal");
    let work = profile("Work");
    let current = w.ws.id().expect("workspace identity");
    View::set_pane_context(
        &mut v,
        vec![personal.clone(), work.clone()],
        personal.id,
        vec![WorkspaceEntry::new(current, "Current", 0)],
        Some(current),
    );
    Fixture {
        v,
        w,
        personal,
        work,
        current,
    }
}

fn row(id: WorkspaceId, owner: &ProfileSummary, name: &str, position: i64) -> Row {
    Row {
        id: crate::manage::workspaces::catalog_workspace(id),
        owner: crate::manage::workspaces::catalog_profile(owner.id),
        name: name.to_owned(),
        position,
        authority: koru_workspaces_ui::Authority::Verified,
        ..Row::default()
    }
}

impl Fixture {
    /// Manage open on `page_of`'s page, its controller holding `rows` with
    /// both permissions.
    fn manage(&mut self, page_of: ProfileId, rows: &[Row]) {
        assert!(self.v.open_manage(Page::Profile(page_of)));
        let manage = self.manage_view();
        manage.with_workspaces(|c| {
            c.adopt_rows(rows);
            c.set_authorization(Authorization::Vouched);
            c.set_migration_authorization(Authorization::Vouched);
        });
        while View::take_pane_request(&mut self.v).is_some() {}
    }

    fn manage_view(&mut self) -> &mut crate::manage::ManageView {
        match &mut self.v.tabs_mut().active_mut().view {
            TabView::Manage(view) => view,
            _ => panic!("the Manage tab is in front"),
        }
    }

    /// Where a workspace's row is, in the window.
    fn row_point(&mut self, id: WorkspaceId) -> (f32, f32) {
        let local = self
            .manage_view()
            .workspace_name_rect(id)
            .expect("the row is on the page");
        let view = self.v.focused_view_rect();
        (view.x + local.x + 4.0, view.y + local.y + local.h * 0.5)
    }

    fn requests(&mut self) -> Vec<PaneRequest> {
        std::iter::from_fn(|| View::take_pane_request(&mut self.v)).collect()
    }
}

// -------------------------------------------------------------------- routes

/// A workspace row's gear on the who/where popover opens Manage on the open
/// Profile's page, where its workspaces are.
#[test]
fn a_workspaces_gear_opens_manage_on_the_open_profiles_page() {
    let mut f = fixture();
    let personal = f.personal.id;
    assert!(f.v.apply(
        &f.w.ws,
        crate::panes::Outcome::ManageWorkspace(koru_core::WorkspaceId::new())
    ));
    assert_eq!(f.manage_view().page(), Some(Page::Profile(personal)));
}

/// Port of *Ctrl+Shift+O opens the manager once and brings it forward*: it
/// is now Manage, on the open Profile's page.
#[test]
fn ctrl_shift_o_opens_manage_on_the_open_profile_once() {
    let mut f = fixture();
    let ctrl_shift = koru_keys::PRIMARY.union(ModifiersState::SHIFT);
    assert!(f.v.key(&mut f.w.ws, &ch("o"), Some("o"), ctrl_shift));
    let personal = f.personal.id;
    assert_eq!(f.manage_view().page(), Some(Page::Profile(personal)));
    let (count, at) = (f.v.tabs().len(), f.v.tabs().active());

    f.v.select(0);
    assert!(f.v.key(&mut f.w.ws, &ch("o"), Some("o"), ctrl_shift));
    assert_eq!(f.v.tabs().len(), count, "no second Manage tab");
    assert_eq!(f.v.tabs().active(), at, "the existing one came forward");
}

/// Port of *the manager tab comes back after a restart*, and decision 9: a
/// session written with the Workspaces tab's `workspaces` token restores
/// Manage — once, beside a `manage` token.
#[test]
fn a_saved_workspaces_token_restores_manage_once() {
    let f = fixture();
    let mut settings = koru_workspace_host::Settings::default();
    settings.tabs = vec![
        (koru_workspace_host::TabRef::Workspaces, 0.0),
        (koru_workspace_host::TabRef::Manage, 0.0),
    ];
    settings.tab = 0;
    let (mut back, _w) = container();
    back.restore(&f.w, &settings);
    let manage = back
        .tabs()
        .iter()
        .filter(|tab| matches!(tab.view, TabView::Manage(_)))
        .count();
    assert_eq!(manage, 1, "one Manage tab");
    assert!(matches!(back.tabs().active_tab().view, TabView::Manage(_)));
}

/// A pending command keeps its fallback deadline when Manage is hidden.
#[test]
fn the_pending_command_deadline_survives_hiding_manage() {
    let mut f = fixture();
    let personal = f.personal.id;
    let current = row(f.current, &f.personal, "Current", 0);
    f.manage(personal, &[current]);
    let manage = f.v.tabs().active();
    assert!(f.v.workspaces_poll_due.is_none());
    f.v.workspace_controller
        .borrow_mut()
        .perform(Action::TogglePinned(catalog_workspace(f.current)));
    f.v.arm_workspaces_poll();
    let due =
        f.v.workspaces_poll_due
            .expect("armed for the pending command");
    View::maintain(&mut f.v, &mut f.w.ws, due);
    let again = f.v.workspaces_poll_due.expect("and armed again");
    assert!(again > due);

    assert!(f.v.select(0));
    View::maintain(&mut f.v, &mut f.w.ws, again);
    assert!(
        f.v.workspaces_poll_due.is_some(),
        "pending work belongs to the window"
    );
    assert!(f.v.select(manage));
    assert!(f.v.workspaces_poll_due.is_some(), "and armed on selection");
}

// ------------------------------------------------------------------ the menu

/// Port of *right-clicking a manager row opens the shared workspace menu*:
/// on a Profile page, with the row selected.
#[test]
fn right_clicking_a_profile_pages_row_opens_the_workspace_menu() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    let mut pinned = row(notes, &personal, "Notes", 1);
    pinned.pinned = true;
    f.manage(
        personal.id,
        &[row(f.current, &personal, "Current", 0), pinned],
    );
    let (x, y) = f.row_point(notes);
    assert!(f.v.context_menu(&mut f.w.ws, x, y));
    let menu = f.v.menu.as_ref().expect("the workspace menu opened");
    assert_eq!(menu.subject, MenuSubject::Workspace(notes));
    assert_eq!(
        menu.items(),
        context_menu::workspace_items(true, false, false),
        "one menu definition"
    );
    let selected = f.manage_view().workspaces().selected();
    assert_eq!(selected.map(app_workspace), Some(notes));
}

/// The row's `•••` opens the same menu, its corner under the button.
#[test]
fn the_more_button_opens_the_menu_under_itself() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    f.manage(personal.id, &[row(notes, &personal, "Notes", 0)]);
    let wid: Wid = crate::manage::workspaces::catalog_workspace(notes);
    let more = f
        .manage_view()
        .laid()
        .placed(crate::manage::Control::WorkspaceMore(wid))
        .expect("the row's •••")
        .rect;
    let view = f.v.focused_view_rect();
    let (x, y) = (
        view.x + more.x + more.w * 0.5,
        view.y + more.y + more.h * 0.5,
    );
    assert!(View::mouse_down(&mut f.v, &mut f.w.ws, x, y, false));
    let menu = f.v.menu.as_ref().expect("the workspace menu opened");
    assert_eq!(menu.subject, MenuSubject::Workspace(notes));
}

/// Decision 11: another Profile's workspace cannot open in a new window from
/// here, and the row says why; *Open* switches Profile to get there.
#[test]
fn another_profiles_workspace_switches_profile_and_cannot_open_a_new_window() {
    let mut f = fixture();
    let theirs = WorkspaceId::new();
    let work = f.work.clone();
    f.manage(work.id, &[row(theirs, &work, "Theirs", 0)]);
    assert_eq!(
        f.v.workspace_refusal(theirs, MenuAction::OpenInNewWindow)
            .as_deref(),
        Some(SWITCH_FIRST)
    );
    assert!(f.v.open_workspace_menu(theirs, (10.0, 10.0), false));
    let menu = f.v.menu.clone().expect("open");
    let enabled = f.v.enabled_items(&menu);
    let at = menu
        .items()
        .iter()
        .position(|a| *a == MenuAction::OpenInNewWindow)
        .expect("listed, not hidden");
    assert!(!enabled[at], "dimmed");
    assert_eq!(
        koru_ui::MenuItem::disabled_reason(&MenuAction::OpenInNewWindow).as_deref(),
        Some(SWITCH_FIRST),
        "with the reason beside it"
    );

    assert!(f.v.run_menu_action(MenuSubject::Workspace(theirs), MenuAction::Open));
    assert_eq!(
        f.requests(),
        [PaneRequest::SwitchToWorkspaceIn(work.id, theirs)]
    );
}

/// A double-click on a row of the open Profile opens it here; one of
/// another Profile switches there.
#[test]
fn double_clicking_a_row_opens_it_through_the_window() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    f.manage(personal.id, &[row(notes, &personal, "Notes", 0)]);
    let (x, y) = f.row_point(notes);
    assert!(View::mouse_double_click(&mut f.v, &f.w.ws, x, y));
    assert_eq!(f.requests(), [PaneRequest::Switch(notes)]);
}

/// *New workspace* on another Profile's page asks for a name and makes it
/// there — the switcher's `NewWorkspaceIn`.
#[test]
fn new_workspace_on_another_profiles_page_is_made_there() {
    let mut f = fixture();
    let work = f.work.id;
    f.manage(work, &[]);
    let new = f
        .manage_view()
        .laid()
        .placed(crate::manage::Control::NewWorkspace)
        .expect("the button")
        .rect;
    let view = f.v.focused_view_rect();
    assert!(View::mouse_down(
        &mut f.v,
        &mut f.w.ws,
        view.x + new.x + 4.0,
        view.y + new.y + 4.0,
        false
    ));
    assert!(f.v.new_document.is_some(), "the name prompt is up");
    assert_eq!(f.v.new_workspace_owner, Some(work), "for Work");
}

/// Decision 6: *Move Up* / *Move Down*, dimmed at the ends with a reason.
#[test]
fn move_up_and_down_dim_at_the_ends() {
    let mut f = fixture();
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    let personal = f.personal.clone();
    f.manage(
        personal.id,
        &[row(a, &personal, "A", 0), row(b, &personal, "B", 1)],
    );
    assert_eq!(
        f.v.workspace_refusal(a, MenuAction::MoveWorkspaceUp)
            .as_deref(),
        Some(AT_TOP)
    );
    assert_eq!(
        f.v.workspace_refusal(a, MenuAction::MoveWorkspaceDown),
        None
    );
    assert_eq!(
        f.v.workspace_refusal(b, MenuAction::MoveWorkspaceDown)
            .as_deref(),
        Some(AT_BOTTOM)
    );
    assert!(f.v.run_menu_action(MenuSubject::Workspace(a), MenuAction::MoveWorkspaceDown));
    assert_eq!(f.v.workspace_refusal(a, MenuAction::CopyWorkspaceId), None);
}

/// *Move to Profile ▸* lists every other Profile; choosing one asks the
/// gateway, its confirmation goes up on the Manage tab, and answering it
/// completes the move — which the Workspaces tab never drew.
#[test]
fn move_to_profile_flyout_and_its_confirmation_complete_the_move() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    let work = f.work.clone();
    f.manage(personal.id, &[row(notes, &personal, "Notes", 0)]);
    assert!(f.v.open_workspace_menu(notes, (10.0, 10.0), false));
    let flyout = context_menu::submenu(MenuAction::MoveToProfile).expect("a flyout");
    assert_eq!(flyout, [MenuAction::MoveWorkspaceTo(work.id)]);
    assert_eq!(MenuAction::MoveWorkspaceTo(work.id).label(), "Work");
    // Choosing a row closes the menu; the verb is what is tested here.
    f.v.menu = None;

    assert!(f.v.run_menu_action(
        MenuSubject::Workspace(notes),
        MenuAction::MoveWorkspaceTo(work.id)
    ));
    let wid = crate::manage::workspaces::catalog_workspace(notes);
    let to = crate::manage::workspaces::catalog_profile(work.id);
    let from = crate::manage::workspaces::catalog_profile(personal.id);
    f.manage_view()
        .with_workspaces(|c| c.fixture_move_consequence(wid, to, "Work", &[]));
    assert!(f.manage_view().workspaces().is_confirming());
    assert_eq!(f.v.blockers(), Blockers::Modal(ModalKind::Confirm));
    assert_workspace_confirmation_is_drawn(&mut f.v);
    assert!(f.v.key(
        &mut f.w.ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert!(!f.manage_view().workspaces().is_confirming(), "accepted");
    f.manage_view()
        .with_workspaces(|c| c.fixture_moved(wid, from, to));
    assert!(f.manage_view().workspaces().notice().is_none());
}

/// Port of *workspace menu delete carries its subject into the shared
/// manager*: the verb reaches the Manage tab's controller, which selects the
/// subject and begins its confirmation flow.
#[test]
fn workspace_menu_delete_carries_its_subject_into_manage() {
    let mut f = fixture();
    let doomed = WorkspaceId::new();
    let personal = f.personal.clone();
    f.manage(
        personal.id,
        &[
            row(f.current, &personal, "Current", 0),
            row(doomed, &personal, "Doomed", 1),
        ],
    );
    assert!(f.v.run_menu_action(MenuSubject::Workspace(doomed), MenuAction::Delete));
    let selected = f.manage_view().workspaces().selected();
    assert_eq!(selected.map(app_workspace), Some(doomed));
    f.manage_view().with_workspaces(|c| {
        c.fixture_consequence(
            koru_workspaces_ui::Action::SoftDelete(crate::manage::workspaces::catalog_workspace(
                doomed,
            )),
            &[],
        )
    });
    assert!(
        f.manage_view().workspaces().is_confirming(),
        "its confirmation is up on the Manage tab"
    );
}

/// With no Manage tab, a lifecycle verb stays on the current page and its
/// confirmation belongs to the window.
#[test]
fn a_lifecycle_verb_with_no_manage_tab_uses_the_window_controller() {
    let mut f = fixture();
    assert!(f.v.manage_view().is_none());
    let wid = catalog_workspace(f.current);
    {
        let mut controller = f.v.workspace_controller.borrow_mut();
        controller.adopt_rows(&[row(f.current, &f.personal, "Current", 0)]);
        controller.set_authorization(Authorization::Vouched);
    }
    assert!(f.v.run_workspace_menu_lifecycle(f.current, MenuAction::Archive));
    assert!(f.v.manage_view().is_none());
    f.v.workspace_controller
        .borrow_mut()
        .fixture_consequence(Action::Archive(wid, true), &[]);
    assert_eq!(f.v.blockers(), Blockers::Modal(ModalKind::Confirm));
    assert_workspace_confirmation_is_drawn(&mut f.v);
    assert!(View::mouse_down(&mut f.v, &mut f.w.ws, 1.0, 1.0, false));
    assert!(
        f.v.workspace_controller.borrow().is_confirming(),
        "a stray press answers nothing"
    );
    assert!(f.v.key(
        &mut f.w.ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(!f.v.workspace_controller.borrow().is_confirming());
    assert_eq!(
        f.v.workspace_controller.borrow().confirmation_default(),
        None
    );
    assert!(f.v.manage_view().is_none());
}

fn assert_workspace_confirmation_is_drawn(view: &mut MainView) {
    let (w, h) = view.viewport;
    let frame = koru_draw::inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, w, h),
    );
    view.draw_workspace_confirmation(&mut Scene::new(), Affine::IDENTITY);
    assert!(
        frame
            .finish()
            .records
            .iter()
            .any(|record| record.name == "Workspace confirmation")
    );
}

#[test]
fn manage_borrows_the_window_controller_and_no_idle_poll_is_armed() {
    let mut f = fixture();
    let controller = f.v.workspace_controller.clone();
    f.v.open_manage(Page::Profile(f.personal.id));
    // The window, Explore menu, Manage tab and this assertion share one controller.
    assert_eq!(std::rc::Rc::strong_count(&controller), 4);
    let current = row(f.current, &f.personal, "Current", 0);
    f.manage_view()
        .with_workspaces(|c| c.adopt_rows(&[current]));
    assert!(
        controller
            .borrow()
            .row(catalog_workspace(f.current))
            .is_some()
    );
    f.v.arm_workspaces_poll();
    assert!(f.v.workspaces_poll_due.is_none());
}

#[test]
fn a_window_without_manage_drains_woken_answers_and_settles_its_deadline() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut f = fixture();
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = wakes.clone();
    let (links, worker) = koru_workspaces_ui::WorkspaceLinks::fixture(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    f.v.workspace_controller = std::rc::Rc::new(std::cell::RefCell::new(
        WorkspacesController::with_links(links),
    ));
    assert_eq!(worker.take_catalog_requests(), 1);
    let current = row(f.current, &f.personal, "Current", 0);
    worker.catalog(vec![current.clone()]);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert!(f.v.poll_manage_workspaces());
    assert!(
        !f.v.poll_manage_workspaces(),
        "one answer causes one state change"
    );
    assert!(f.v.manage_view().is_none());
    worker.invalidate();
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    f.v.poll_manage_workspaces();
    assert_eq!(worker.take_catalog_requests(), 1);
    f.v.poll_manage_workspaces();
    assert_eq!(
        worker.take_catalog_requests(),
        0,
        "one invalidation fetches once"
    );
    f.v.workspace_controller
        .borrow_mut()
        .perform(Action::TogglePinned(current.id));
    f.v.arm_workspaces_poll();
    assert!(f.v.workspaces_poll_due.is_some());
    worker.catalog(vec![current.clone()]);
    f.v.poll_manage_workspaces();
    f.v.arm_workspaces_poll();
    assert!(
        f.v.workspaces_poll_due.is_some(),
        "an unrelated catalog is not a receipt"
    );
    let mut pinned = current;
    pinned.pinned = true;
    worker.settle_catalog(vec![pinned]);
    assert!(f.v.poll_manage_workspaces());
    f.v.arm_workspaces_poll();
    assert!(
        f.v.workspaces_poll_due.is_none(),
        "settlement stops the fallback"
    );
    assert!(f.v.workspace_facts(f.current).unwrap().pinned);
    assert!(f.v.manage_view().is_none());
}

#[test]
fn rail_and_menu_take_the_window_catalog_even_when_host_context_is_older() {
    let mut f = fixture();
    f.v.workspace_controller.borrow_mut().adopt_rows(&[row(
        f.current,
        &f.personal,
        "Renamed in another window",
        0,
    )]);
    f.v.refresh_manage_workspaces();
    assert!(
        f.v.explore.rail_model().workspaces[0]
            .name
            .contains("Renamed in another window")
    );
    f.v.workspace_controller
        .borrow_mut()
        .set_availability(koru_workspaces_ui::Availability::Down);
    View::set_pane_context(
        &mut f.v,
        vec![f.personal.clone(), f.work.clone()],
        f.personal.id,
        vec![WorkspaceEntry::new(f.current, "Older push", 0)],
        Some(f.current),
    );
    assert!(
        f.v.explore.rail_model().workspaces[0]
            .name
            .contains("Renamed in another window")
    );
    assert!(f.v.manage_view().is_none());
    f.v.workspace_controller.borrow_mut().adopt_rows(&[]);
    f.v.refresh_manage_workspaces();
    assert!(
        !f.v.explore.rail_model().workspaces[0]
            .name
            .contains("Older push")
    );
}

/// F2 on a Profile page renames the selected row, its field over the row.
#[test]
fn f2_renames_the_selected_row_over_its_name() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    f.manage(personal.id, &[row(notes, &personal, "Notes", 0)]);
    let (x, y) = f.row_point(notes);
    assert!(View::mouse_down(&mut f.v, &mut f.w.ws, x, y, false));
    assert!(f.v.rename_here(&mut f.w.ws));
    assert!(matches!(
        f.v.workspace_rename.as_ref().map(|rename| rename.target),
        Some(RenameTarget::Workspace(id)) if id == notes
    ));
    let field = f.v.workspace_rename_row().expect("anchored");
    assert!(field.contains(x, y), "over the row's name");
}

/// *Copy ID* needs neither a gateway nor a permission.
#[test]
fn copy_id_is_always_offered() {
    let mut f = fixture();
    let notes = WorkspaceId::new();
    let personal = f.personal.clone();
    f.manage(personal.id, &[row(notes, &personal, "Notes", 0)]);
    f.manage_view()
        .with_workspaces(|c| c.set_authorization(Authorization::Refused));
    assert_eq!(
        f.v.workspace_refusal(notes, MenuAction::CopyWorkspaceId),
        None
    );
    assert!(
        f.v.workspace_refusal(notes, MenuAction::Delete).is_some(),
        "while Delete is refused"
    );
}
