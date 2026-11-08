// SPDX-License-Identifier: MPL-2.0

//! A Profile page's Workspaces: what it lists for which Profile, what each
//! press asks of the controller or the window, the archived and trash lists,
//! the controller's questions drawn over the tab, what the inspector sees and
//! what a screen reader is offered. The tests ported from the retired
//! Workspaces tab's own suite are marked as such.
use super::*;
use koru_core::schema::WorkspaceDoc;
use koru_workspace_host::accessibility::{ViewSlot, WindowSlot};
use koru_workspaces_ui::Authorization;

fn profile(name: &str) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: Some("teal".into()),
        account: None,
        folder: String::new(),
    }
}

fn row(name: &str, owner: &ProfileSummary, position: i64) -> Catalog {
    Catalog {
        id: Wid::new(),
        owner: catalog_profile(owner.id),
        name: name.into(),
        position,
        authority: koru_workspaces_ui::Authority::Verified,
        ..Catalog::default()
    }
}

/// A Manage tab over *Personal* (open) and *Work*, on Personal's page, its
/// controller holding `rows` with both permissions granted.
fn manage(rows: &[Catalog]) -> (ManageView, ProfileSummary, ProfileSummary) {
    let mut view = ManageView::new();
    let personal = profile("Personal");
    let work = profile("Work");
    let profiles = vec![personal.clone(), work.clone()];
    view.delegate(|c| {
        c.take_request();
        c.set_profiles(profiles.clone(), personal.id, &[])
    });
    view.set_workspace_profiles(&profiles);
    view.with_workspaces(|c| {
        c.adopt_rows(rows);
        c.set_authorization(Authorization::Vouched);
        c.set_migration_authorization(Authorization::Vouched);
    });
    View::set_viewport(&mut view, 1000.0, 1400.0);
    (view, personal, work)
}

/// The fixtures need the Profiles before the rows; this is `manage` for rows
/// that name its Profiles.
fn manage_with(
    rows: impl FnOnce(&ProfileSummary, &ProfileSummary) -> Vec<Catalog>,
) -> (ManageView, ProfileSummary, ProfileSummary, Vec<Catalog>) {
    let (mut view, personal, work) = manage(&[]);
    let rows = rows(&personal, &work);
    view.with_workspaces(|c| c.adopt_rows(&rows));
    (view, personal, work, rows)
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

fn section(view: &ManageView) -> &WorkspacesSection {
    view.laid
        .workspaces
        .as_ref()
        .expect("a Profile page has a Workspaces section")
}

fn listed(view: &ManageView, kind: ListKind) -> Vec<String> {
    section(view)
        .lists
        .iter()
        .filter(|list| list.kind == kind)
        .flat_map(|list| list.rows.iter().map(|row| row.full_name.clone()))
        .collect()
}

fn rect_of(view: &ManageView, control: Control) -> Rect {
    view.laid
        .placed(control)
        .unwrap_or_else(|| panic!("{control:?} is on the page"))
        .rect
}

fn press(view: &mut ManageView, control: Control) -> bool {
    let r = rect_of(view, control);
    let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5 - view.page_scroll);
    View::mouse_down(view, &mut doc(), x, y, false)
}

fn requests(view: &mut ManageView) -> Vec<Request> {
    std::iter::from_fn(|| view.take_request()).collect()
}

fn capture(view: &mut ManageView) -> inspect::Snapshot {
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, w, h));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    frame.finish()
}

// ------------------------------------------------------------- what it lists

#[test]
fn a_profile_page_lists_only_its_own_workspaces_pinned_first() {
    let (mut view, _, work, rows) = manage_with(|personal, work| {
        let mut pinned = row("Roadmap", personal, 1);
        pinned.pinned = true;
        vec![row("Notes", personal, 0), pinned, row("Theirs", work, 0)]
    });
    assert_eq!(listed(&view, ListKind::Active), ["Roadmap", "Notes"]);
    assert_eq!(section(&view).count.1, "2", "the heading counts them");
    let roadmap = section(&view).row(rows[1].id).expect("listed").clone();
    assert_eq!(
        roadmap.tags.iter().map(|(_, tag)| *tag).collect::<Vec<_>>(),
        ["Pinned"]
    );

    view.show(Page::Profile(work.id));
    assert_eq!(listed(&view, ListKind::Active), ["Theirs"]);
}

/// Decision 8: *Opened {when}* from the catalog's `last_opened_at`, and no
/// sub-line at all for a workspace never opened.
#[test]
fn a_row_says_when_it_was_opened_and_which_one_is_open_here() {
    let three_hours_ago = now_ms() - 3 * 60 * 60 * 1000;
    let (mut view, _, _, rows) = manage_with(|personal, _| {
        let mut opened = row("Roadmap", personal, 0);
        opened.last_opened_at = Some(three_hours_ago);
        vec![opened, row("Notes", personal, 1)]
    });
    assert!(view.set_current_workspace(Some(app_workspace(rows[1].id))));
    let s = section(&view);
    let roadmap = s.row(rows[0].id).expect("listed");
    assert_eq!(
        roadmap.sub.as_ref().map(|(_, text)| text.as_str()),
        Some("Opened 3 hours ago")
    );
    let notes = s.row(rows[1].id).expect("listed");
    assert_eq!(notes.sub, None, "never opened: no sub-line");
    assert_eq!(
        notes.tags.iter().map(|(_, tag)| *tag).collect::<Vec<_>>(),
        ["Open"],
        "the window's open workspace wears the tag"
    );
}

#[test]
fn an_empty_profile_says_so_rather_than_drawing_an_empty_card() {
    let (view, _, _) = manage(&[]);
    assert!(section(&view).lists.is_empty());
    assert_eq!(
        section(&view)
            .quiet
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        [EMPTY]
    );
}

// ----------------------------------------------------------- the heading line

/// Decision 10: *Import…* is offered only on the open Profile's page.
#[test]
fn import_is_only_on_the_open_profiles_page() {
    let (mut view, personal, work) = manage(&[]);
    assert!(view.laid.placed(Control::ImportWorkspaces).is_some());
    assert!(press(&mut view, Control::ImportWorkspaces));
    assert_eq!(requests(&mut view), [Request::ImportContent]);

    view.show(Page::Profile(work.id));
    assert!(
        view.laid.placed(Control::ImportWorkspaces).is_none(),
        "not on another Profile's page"
    );
    assert!(view.laid.placed(Control::NewWorkspace).is_some());
    let _ = personal;
}

/// *New workspace* is made in the page's Profile: the open one here, another
/// one there — the window turns the second into `NewWorkspaceIn`.
#[test]
fn new_workspace_asks_for_the_pages_own_profile() {
    let (mut view, personal, work) = manage(&[]);
    assert!(press(&mut view, Control::NewWorkspace));
    assert_eq!(requests(&mut view), [Request::NewWorkspace(personal.id)]);

    view.show(Page::Profile(work.id));
    assert!(press(&mut view, Control::NewWorkspace));
    assert_eq!(requests(&mut view), [Request::NewWorkspace(work.id)]);
}

// ---------------------------------------------------------------- the rows

/// A click selects (and so does a right-click, for the menu it opens); a
/// double-click opens. Port of the tab's *right-click selects the row its
/// menu is about*.
#[test]
fn a_click_selects_and_a_double_click_opens() {
    let (mut view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    let id = rows[0].id;
    assert!(press(&mut view, Control::WorkspaceRow(id)));
    assert_eq!(view.workspaces().selected(), Some(id));
    assert!(requests(&mut view).is_empty(), "a click opens nothing");

    let r = rect_of(&view, Control::WorkspaceRow(id));
    let (x, y) = (r.x + 120.0, r.y + r.h * 0.5);
    assert!(View::mouse_double_click(&mut view, &doc(), x, y));
    assert_eq!(
        requests(&mut view),
        [Request::OpenWorkspace(app_workspace(id))]
    );

    view.w.borrow_mut().set_selected(None);
    assert_eq!(
        view.workspace_menu_target_at(x, y),
        Some(app_workspace(id)),
        "a right-click names the row"
    );
    assert_eq!(view.workspaces().selected(), Some(id), "and selects it");
    assert_eq!(view.selected_workspace(), Some(app_workspace(id)));
}

/// Enter on a row is the double-click.
#[test]
fn enter_on_a_row_opens_it() {
    let (mut view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    let id = rows[0].id;
    view.page_focus = Some(Control::WorkspaceRow(id));
    assert!(View::key(
        &mut view,
        &mut doc(),
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    ));
    assert_eq!(
        requests(&mut view),
        [Request::OpenWorkspace(app_workspace(id))]
    );
}

/// A row in the trash is not opened: the controller says to restore it first.
#[test]
fn a_trashed_row_does_not_open() {
    let (mut view, _, _, rows) = manage_with(|personal, _| {
        let mut gone = row("Gone", personal, 0);
        gone.deleted = true;
        vec![row("Notes", personal, 1), gone]
    });
    view.with_workspaces(|c| c.set_show_trash(true));
    assert!(view.open_row(rows[1].id));
    assert!(requests(&mut view).is_empty());
}

/// The `•••` asks for the window's workspace menu with its corner under the
/// button, and answers before the row beneath it.
#[test]
fn the_more_button_asks_for_the_menu_under_itself() {
    let (mut view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    let id = rows[0].id;
    let more = rect_of(&view, Control::WorkspaceMore(id));
    assert!(more.w > 0.0 && more.x > rect_of(&view, Control::WorkspaceRow(id)).x);
    assert!(press(&mut view, Control::WorkspaceMore(id)));
    assert_eq!(
        requests(&mut view),
        [Request::WorkspaceMenu {
            workspace: app_workspace(id),
            at: (more.x + more.w, more.y + more.h),
        }]
    );
    assert_eq!(view.workspaces().selected(), Some(id));
}

/// The window's rename field goes over the row's name.
#[test]
fn the_rename_field_goes_over_the_rows_name() {
    let (view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    let field = view
        .workspace_name_rect(app_workspace(rows[0].id))
        .expect("the row is on the page");
    let row = section(&view).row(rows[0].id).expect("listed");
    assert!(row.rect.contains(field.x + 1.0, field.y + 1.0));
    assert!(field.x >= row.tile.x + row.tile.w, "right of the tile");
}

// ------------------------------------------------------ archived and trash

#[test]
fn archived_and_trash_open_inline_from_their_links() {
    let (mut view, _, _, rows) = manage_with(|personal, _| {
        let mut old = row("Old plans", personal, 1);
        old.archived = true;
        let mut gone = row("Scratch", personal, 2);
        gone.deleted = true;
        vec![row("Notes", personal, 0), old, gone]
    });
    assert_eq!(rect_of(&view, Control::ShowArchived).w > 0.0, true);
    let label = |view: &ManageView, control| {
        view.laid
            .placed(control)
            .map(|placed| placed.label.clone())
            .unwrap_or_default()
    };
    assert_eq!(label(&view, Control::ShowArchived), "Show archived (1)");
    assert_eq!(label(&view, Control::ShowTrash), "Show trash (1)");
    assert!(listed(&view, ListKind::Archived).is_empty());

    assert!(press(&mut view, Control::ShowArchived));
    assert_eq!(listed(&view, ListKind::Archived), ["Old plans"]);
    assert_eq!(label(&view, Control::ShowArchived), "Hide archived (1)");
    assert!(view.laid.placed(Control::Unarchive(rows[1].id)).is_some());

    assert!(press(&mut view, Control::ShowTrash));
    assert_eq!(listed(&view, ListKind::Trash), ["Scratch"]);
    assert!(
        view.laid
            .placed(Control::RestoreWorkspace(rows[2].id))
            .is_some()
    );
    let trash = section(&view)
        .lists
        .iter()
        .find(|list| list.kind == ListKind::Trash)
        .expect("the trash list");
    assert!(
        trash.notes.iter().any(|(_, text)| text == TRASH_NOTE),
        "it says deleted workspaces go by themselves"
    );
    // Decision 5: no purge route, so no purge button.
    assert!(
        view.laid
            .controls
            .iter()
            .all(|placed| !placed.label.to_lowercase().contains("forever")),
        "no Delete forever"
    );
}

/// *Unarchive* and *Restore* need this window's `workspace.destroy`. Without
/// it they are dimmed — never hidden — and the reason is said once under
/// each list; a press on one does nothing.
#[test]
fn unarchive_and_restore_are_dimmed_with_the_reason_when_not_permitted() {
    let (mut view, _, _, rows) = manage_with(|personal, _| {
        let mut old = row("Old plans", personal, 0);
        old.archived = true;
        let mut gone = row("Scratch", personal, 1);
        gone.deleted = true;
        vec![old, gone]
    });
    view.with_workspaces(|c| {
        c.set_authorization(Authorization::Refused);
        c.set_show_archived(true);
        c.set_show_trash(true);
    });
    let unarchive = Control::Unarchive(rows[0].id);
    let restore = Control::RestoreWorkspace(rows[1].id);
    assert!(view.laid.disabled.contains(&unarchive));
    assert!(view.laid.disabled.contains(&restore));
    let reason = koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::WorkspacesNotPermitted);
    for kind in [ListKind::Archived, ListKind::Trash] {
        let list = section(&view)
            .lists
            .iter()
            .find(|list| list.kind == kind)
            .expect("open");
        assert!(
            list.notes.iter().any(|(_, text)| text == reason),
            "{kind:?} says why"
        );
    }
    assert!(press(&mut view, unarchive), "the press is consumed");
    assert!(!view.workspaces().is_confirming());
}

/// Port of the tab's persistence: whether the archive is open is kept in
/// `Settings.show_archived`, as the Workspaces tab kept it; the trash is not.
#[test]
fn whether_the_archive_is_open_survives_a_restart() {
    let (mut view, _, _) = manage(&[]);
    view.with_workspaces(|c| {
        c.set_show_archived(true);
        c.set_show_trash(true);
    });
    let mut settings = koru_workspace_host::Settings::default();
    View::capture(&view, &mut settings);
    assert!(settings.show_archived);

    let (mut back, _, _) = manage(&[]);
    let workspace = koru_workspace::Workspace::new(doc());
    View::restore(&mut back, &workspace, &settings);
    assert!(back.workspaces().show_archived());
    assert!(
        !back.workspaces().show_trash(),
        "the trash is not remembered"
    );
}

// ------------------------------------------ the controller's questions and notice

// Window-owned confirmation coverage lives in mainview/workspace_menu/tests.rs.

/// S24: the controller's notice is a line under the heading with *Retry* and
/// *Dismiss*.
#[test]
fn a_notice_offers_retry_and_dismiss() {
    let (mut view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    view.with_workspaces(|c| c.fixture_partial_failure(Action::Archive(rows[0].id, true)));
    assert!(section(&view).notice.is_some());
    assert!(view.laid.placed(Control::NoticeRetry).is_some());
    assert!(press(&mut view, Control::NoticeDismiss));
    assert!(view.workspaces().notice().is_none());
    assert!(section(&view).notice.is_none());
    assert!(view.laid.placed(Control::NoticeRetry).is_none());
}

// ------------------------------------------------- inspector and accessibility

#[test]
fn the_inspector_sees_the_section_its_rows_and_its_lists() {
    let (mut view, _, _, _) = manage_with(|personal, _| {
        let mut old = row("Old plans", personal, 1);
        old.archived = true;
        let mut gone = row("Scratch", personal, 2);
        gone.deleted = true;
        vec![row("Notes", personal, 0), old, gone]
    });
    view.with_workspaces(|c| {
        c.set_show_archived(true);
        c.set_show_trash(true);
    });
    let shot = capture(&mut view);
    let count = |name: &str| shot.records.iter().filter(|r| r.name == name).count();
    assert_eq!(count("Manage workspaces"), 1);
    assert_eq!(count("Manage workspace row"), 3);
    assert_eq!(count("Manage archived"), 1);
    assert_eq!(count("Manage trash"), 1);
}

#[test]
fn a_reader_finds_the_rows_as_a_list_with_their_buttons() {
    let (mut view, _, _, rows) = manage_with(|personal, _| {
        let mut pinned = row("Roadmap", personal, 0);
        pinned.pinned = true;
        vec![pinned, row("Notes", personal, 1)]
    });
    let cx = crate::manage::accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(0.0, 0.0, 1000.0, 1400.0),
    };
    let tree = view.semantics(&cx);
    let list = tree
        .children
        .iter()
        .find(|node| {
            node.role == accesskit::Role::ListBox && node.label.as_deref() == Some("Workspaces")
        })
        .expect("a Workspaces list");
    assert_eq!(list.children.len(), 2);
    let first = &list.children[0];
    assert_eq!(first.role, accesskit::Role::ListBoxOption);
    assert_eq!(first.label.as_deref(), Some("Roadmap, pinned"));
    assert!(
        first.occurrence.locator().ends_with("#manage-row"),
        "{}",
        first.occurrence.locator()
    );
    assert!(
        first
            .occurrence
            .locator()
            .contains(&rows[0].id.as_uuid().simple().to_string())
    );
    let more = first.children.first().expect("its ••• button");
    assert_eq!(more.role, accesskit::Role::Button);
    assert!(more.occurrence.locator().ends_with("#manage-more"));

    // Pressing the row through the tree opens it, as a double-click does.
    let target = first.occurrence;
    assert!(view.accessibility_action(&cx, target, accesskit::Action::Click));
    assert_eq!(
        requests(&mut view),
        [Request::OpenWorkspace(app_workspace(rows[0].id))]
    );
}

// ------------------------------------------------------- ports of the tab's

/// Port of *the manager never connects under test*: the controller holds a
/// link, so it behaves as it does in the app, and the link is the inert one.
#[test]
fn the_controller_holds_a_link_that_never_connects_under_test() {
    let view = ManageView::new();
    assert!(view.workspaces().links().is_held());
    assert!(
        view.workspaces().availability().is_loading(),
        "nothing ever answers it"
    );
}

/// Port of *F2 renames the selection and does nothing without one*: the
/// window asks which row is selected, and there is none until one is
/// pressed.
#[test]
fn f2_has_a_row_to_rename_only_once_one_is_selected() {
    let (mut view, _, _, rows) = manage_with(|personal, _| vec![row("Notes", personal, 0)]);
    assert_eq!(view.selected_workspace(), None);
    assert!(press(&mut view, Control::WorkspaceRow(rows[0].id)));
    assert_eq!(view.selected_workspace(), Some(app_workspace(rows[0].id)));
}
