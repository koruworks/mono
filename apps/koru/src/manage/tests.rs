// SPDX-License-Identifier: MPL-2.0

//! The Manage tab: where the nav puts things and what a click or key there
//! means, which page shows, the embedded form's hosting, what the inspector
//! sees and what a screen reader is offered. Behaviour that is only about the
//! controller's state is tested beside it (`manage/profiles/tests.rs`).
use super::*;
use koru_agent_wire::{AccountOperationResult, AccountProfileRow, AccountStage, AccountState};
use koru_core::ProfileSummary;
use koru_draw::geometry::Rect;
use koru_workspace_host::accessibility::{Occurrence, ViewSlot, WindowSlot};

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

/// A Manage tab over two Profiles, the first of them open.
fn manage() -> (ManageView, ProfileSummary, ProfileSummary) {
    let mut view = ManageView::new();
    let personal = profile("Personal");
    let work = profile("Work");
    view.delegate(|c| {
        c.take_request();
        c.set_profiles(vec![personal.clone(), work.clone()], personal.id, &[])
    });
    View::set_viewport(&mut view, 1000.0, 700.0);
    (view, personal, work)
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create")
}

fn click(view: &mut ManageView, entry: nav::Entry) -> bool {
    let rect = row_of(view, entry);
    press(view, rect)
}

fn press(view: &mut ManageView, rect: Rect) -> bool {
    let (x, y) = rect.centre();
    View::mouse_down(view, &mut doc(), x, y, false)
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

fn row_of(view: &ManageView, wanted: nav::Entry) -> Rect {
    view.nav_layout()
        .rows
        .into_iter()
        .find(|(entry, _)| *entry == wanted)
        .map(|(_, rect)| rect)
        .unwrap_or_else(|| panic!("{wanted:?} is in the nav"))
}

fn capture(view: &mut ManageView) -> inspect::Snapshot {
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, w, h));
    view.paint(&mut Scene::new(), Affine::IDENTITY);
    frame.finish()
}

// ------------------------------------------------------------- the page

#[test]
fn it_opens_on_the_open_profiles_page() {
    let (view, personal, _) = manage();
    assert_eq!(view.page(), Some(Page::Profile(personal.id)));
    assert_eq!(view.controller().selected(), Some(personal.id));
}

#[test]
fn a_removed_profiles_page_falls_back_to_the_open_one() {
    let (mut view, personal, work) = manage();
    view.show(Page::Profile(work.id));
    view.delegate(|c| c.set_profiles(vec![personal.clone()], personal.id, &[]));
    assert_eq!(view.page(), Some(Page::Profile(personal.id)));
}

// ------------------------------------------------------------- the nav

#[test]
fn the_nav_lays_out_in_order_and_every_row_is_pressed_where_it_is_drawn() {
    let a = ProfileId::new();
    let b = ProfileId::new();
    let other = ProfileId::new();
    let area = Rect::new(16.0, 48.0, 168.0, 600.0);
    let laid = nav::layout(area, &[a, b], &[other], true, 0.0);
    let order: Vec<_> = laid.rows.iter().map(|(entry, _)| *entry).collect();
    assert_eq!(
        order,
        vec![
            nav::Entry::Profile(a),
            nav::Entry::Profile(b),
            nav::Entry::Other(other),
            nav::Entry::Hidden,
            nav::Entry::NewProfile,
            nav::Entry::Devices,
            nav::Entry::Cloud,
        ]
    );
    // Top to bottom, inside the nav, none overlapping.
    for pair in laid.rows.windows(2) {
        assert!(pair[0].1.y + pair[0].1.h <= pair[1].1.y, "{pair:?}");
    }
    for (_, rect) in &laid.rows {
        assert!(rect.x >= area.x && rect.x + rect.w <= area.x + area.w);
        assert_eq!(rect.h.round(), if rect.h < 30.0 { 24.0 } else { 30.0 });
    }
    assert_eq!(
        laid.profiles_caption.y, area.y,
        "the first caption is level with a title"
    );
    assert!(laid.profiles_caption.y + laid.profiles_caption.h <= laid.rows[0].1.y);
    assert!(laid.computer_caption.y + laid.computer_caption.h <= row(&laid, nav::Entry::Devices).y);
    assert!(laid.others_heading.is_some());

    // The tab's own hit test answers each row back to itself.
    let (view, personal, work) = manage();
    for entry in [
        nav::Entry::Profile(personal.id),
        nav::Entry::Profile(work.id),
        nav::Entry::NewProfile,
        nav::Entry::Devices,
        nav::Entry::Cloud,
    ] {
        let (x, y) = row_of(&view, entry).centre();
        assert_eq!(view.nav_hit(x, y), Some(entry));
    }
    // A caption is not a row.
    let laid = view.nav_layout();
    let (x, y) = laid.profiles_caption.centre();
    assert_eq!(view.nav_hit(x, y), None);
    // The page is not the nav.
    let nav = view.sidebar();
    assert_eq!(view.nav_hit(nav.x + nav.w + 10.0, y), None);
}

fn row(laid: &nav::Laid, wanted: nav::Entry) -> Rect {
    laid.rows
        .iter()
        .find(|(entry, _)| *entry == wanted)
        .map(|(_, rect)| *rect)
        .unwrap()
}

#[test]
fn without_other_or_hidden_profiles_the_nav_has_no_room_for_them() {
    let laid = nav::layout(
        Rect::new(16.0, 48.0, 168.0, 600.0),
        &[ProfileId::new()],
        &[],
        false,
        0.0,
    );
    assert!(laid.others_heading.is_none());
    assert!(laid.hidden.is_none());
    assert_eq!(laid.rows.len(), 4);
}

#[test]
fn clicking_a_row_shows_its_page() {
    let (mut view, _, work) = manage();
    let rect = row_of(&view, nav::Entry::Profile(work.id));
    assert!(press(&mut view, rect));
    assert_eq!(view.page(), Some(Page::Profile(work.id)));
    assert_eq!(view.controller().selected(), Some(work.id));

    assert!(click(&mut view, nav::Entry::Devices));
    assert_eq!(view.page(), Some(Page::Devices));
    assert!(click(&mut view, nav::Entry::Cloud));
    assert_eq!(view.page(), Some(Page::Cloud));

    assert!(click(&mut view, nav::Entry::NewProfile));
    assert_eq!(view.page(), Some(Page::NewProfile));
    assert!(view.controller().creating(), "the create form opened");
}

#[test]
fn the_hidden_line_opens_koru_cloud() {
    let (mut view, _, _) = manage();
    view.delegate(|c| c.set_hidden_profile_count(2));
    assert!(click(&mut view, nav::Entry::Hidden));
    assert_eq!(view.page(), Some(Page::Cloud));
}

#[test]
fn arrows_walk_the_nav_and_enter_shows_the_row() {
    let (mut view, personal, work) = manage();
    // The first press lands on the current page's row.
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert_eq!(view.focus(), Some(nav::Entry::Profile(personal.id)));
    assert!(key(&mut view, NamedKey::ArrowDown));
    assert_eq!(view.focus(), Some(nav::Entry::Profile(work.id)));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.page(), Some(Page::Profile(work.id)));
    // Up from the first row wraps to the last.
    assert!(key(&mut view, NamedKey::ArrowUp));
    assert!(key(&mut view, NamedKey::ArrowUp));
    assert_eq!(view.focus(), Some(nav::Entry::Cloud));
    assert!(key(&mut view, NamedKey::Enter));
    assert_eq!(view.page(), Some(Page::Cloud));
}

/// The account's Profiles that are not here sit under the list, and a row's
/// *Connect* is the controller's inline connect.
#[test]
fn an_other_profile_connects_from_the_nav() {
    let (mut view, _, _) = manage();
    let missing = ProfileId::new();
    view.delegate(|c| {
        c.set_account(AccountState {
            active_scope: 1,
            generation: 10,
            stage: AccountStage::SignedIn,
            ..Default::default()
        });
        c.take_request();
        c.complete(AccountOperationResult::Profiles {
            profiles: vec![AccountProfileRow {
                profile_id: missing.to_storage(),
                koru_identity_id: "10000000-0000-4000-8000-000000000001".into(),
                name: Some("Family".into()),
                locale: None,
                auth_identities: vec![],
            }],
        });
        c.take_request();
    });
    assert_eq!(view.controller().connect_label(missing), Some("Connect"));
    let rect = row_of(&view, nav::Entry::Other(missing));
    assert!(press(&mut view, rect));
    assert_eq!(view.controller().inline_connect(), Some((missing, false)));
    // In flight, the row has nothing to press.
    let (x, y) = rect.centre();
    assert_eq!(view.nav_hit(x, y), None);
    assert!(!view.focus_order().contains(&nav::Entry::Other(missing)));
}

#[test]
fn the_nav_marks_the_open_profile_and_one_that_needs_attention() {
    let (mut view, personal, work) = manage();
    view.c
        .start_hosted_delete(work.id, "Work", profiles::HostedDeleteStage::Failed);
    assert!(view.controller().needs_attention(work.id));
    assert!(!view.controller().needs_attention(personal.id));
    let snapshot = capture(&mut view);
    let marks = snapshot
        .records
        .iter()
        .filter(|r| r.name == "Nav attention")
        .count();
    let dots = snapshot
        .records
        .iter()
        .filter(|r| r.name == "Nav dot")
        .count();
    assert_eq!(marks, 1, "one Profile needs attention");
    // The open Profile's dot; Koru Cloud's is unlit while its state is unknown.
    assert_eq!(dots, 1);
}

// ------------------------------------------------------------- the shell

/// Manage is laid out as Settings is: the nav and the page's column one
/// centred group, the nav's first line level with the page's title.
#[test]
fn the_nav_and_the_page_are_settings_centred_group() {
    let (mut view, personal, _) = manage();
    View::set_viewport(&mut view, 1600.0, 900.0);
    let shell = page::SHELL;
    let nav = view.sidebar();
    let column = view.column();
    assert_eq!(nav.x, shell.group_x(1600.0));
    assert_eq!(column, shell.column((1600.0, 900.0), 0.0));
    assert_eq!(column.x, nav.x + nav.w + koru_ui::PageShell::PAGE_MARGIN);
    let laid = view.nav_layout();
    assert_eq!(laid.profiles_caption.y, column.y);
    // The title is the Profile's name, the tile before it on its line.
    let header = &view.laid().header;
    assert_eq!(header.name_text, personal.name);
    assert_eq!(header.name.y, column.y);
    assert!(header.tile.x + header.tile.w < header.name.x);
    assert!(
        header.tile.y >= header.name.y && header.tile.y + header.tile.h <= header.name.y + 32.0
    );
    // A press left of the group is neither the nav's nor the page's.
    assert_eq!(
        view.nav_hit(
            nav.x - 4.0,
            row_of(&view, nav::Entry::Profile(personal.id)).centre().1
        ),
        None
    );
}

/// Narrower than Settings' compact width the nav is chips across the top —
/// every Profile, *New profile*, *Linked devices* and *Koru Cloud* — and the
/// page starts under them.
#[test]
fn a_narrow_tab_puts_the_nav_in_chips_above_the_page() {
    let (mut view, personal, work) = manage();
    View::set_viewport(&mut view, 420.0, 700.0);
    assert!(view.compact());
    let laid = view.nav_layout();
    assert!(laid.chips);
    let order: Vec<_> = laid.rows.iter().map(|(entry, _)| *entry).collect();
    assert_eq!(
        order,
        vec![
            nav::Entry::Profile(personal.id),
            nav::Entry::Profile(work.id),
            nav::Entry::NewProfile,
            nav::Entry::Devices,
            nav::Entry::Cloud,
        ]
    );
    let band = view.sidebar();
    assert_eq!(band.w, 420.0);
    for (_, chip) in &laid.rows {
        assert!(chip.y + chip.h <= band.h);
    }
    assert!(view.column().y >= band.h, "the page starts under the chips");
    // A chip is pressed where it is drawn, and Enter walks them.
    assert!(click(&mut view, nav::Entry::Devices));
    assert_eq!(view.page(), Some(Page::Devices));
    assert!(click(&mut view, nav::Entry::Profile(work.id)));
    assert_eq!(view.page(), Some(Page::Profile(work.id)));
    let snapshot = capture(&mut view);
    assert_eq!(
        snapshot
            .records
            .iter()
            .filter(|r| r.name == "KoruChipNav")
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .records
            .iter()
            .filter(|r| r.name == "KoruNavList")
            .count(),
        0
    );
    // Wide again, the column comes back.
    View::set_viewport(&mut view, 1000.0, 700.0);
    assert!(!view.nav_layout().chips);
}

// ------------------------------------------------------------- New profile

#[test]
fn the_create_form_is_hosted_beside_the_nav_and_finishing_lands_on_the_profile() {
    let (mut view, _, work) = manage();
    view.show(Page::NewProfile);
    assert!(view.controller().creating());
    let form = view.form_rect();
    assert_eq!(view.form_point(form.x + 5.0, form.y + 7.0), (5.0, 7.0));
    let head = view.laid().head.clone().expect("the page's head");
    assert_eq!(head.title_text, "New profile");
    assert!(
        form.y >= head.lead.y + head.lead.h,
        "the form is under the lead"
    );
    // The nav still works over an open form, and the form is still there
    // when the page is shown again.
    assert!(click(&mut view, nav::Entry::Devices));
    assert_eq!(view.page(), Some(Page::Devices));
    assert!(view.controller().creating());
    view.show(Page::NewProfile);
    // A Profile connected through the form closes it: the page follows.
    let scope = view.controller().account_scope();
    view.delegate(|c| c.connection_ready(scope, work.id));
    assert!(!view.controller().creating());
    assert_eq!(view.page(), Some(Page::Profile(work.id)));
}

/// **The page's X puts the form away and lands on the first Profile** (the
/// review's note). It sits at the title line's right end, outside the form,
/// where Linked devices keeps its own head button.
#[test]
fn the_new_profile_pages_x_closes_the_form_onto_the_first_profile() {
    let (mut view, personal, _) = manage();
    view.show(Page::NewProfile);
    assert!(view.controller().creating());
    let close = view
        .laid()
        .placed(profile_page::Control::CloseCreation)
        .cloned()
        .expect("the X");
    let head = view.laid().head.clone().expect("the head");
    let column = view.column();
    assert!(
        (close.rect.x + close.rect.w - (column.x + column.w)).abs() < 0.5,
        "at the column's right edge: {:?} vs {column:?}",
        close.rect
    );
    assert!(close.rect.y < head.lead.y, "on the title line");
    assert!(
        head.title.x + head.title.w <= close.rect.x,
        "the title leaves it room"
    );
    assert!(press(&mut view, close.rect));
    assert!(!view.controller().creating(), "the form is put away");
    assert_eq!(
        view.page(),
        Some(Page::Profile(personal.id)),
        "the first Profile"
    );
}

// ------------------------------------------------------------- inspection

#[test]
fn the_inspector_names_the_tab_its_nav_and_each_page() {
    let (mut view, _, _) = manage();
    let named = |snapshot: &inspect::Snapshot, name: &str| {
        snapshot.records.iter().filter(|r| r.name == name).count()
    };
    let snapshot = capture(&mut view);
    assert_eq!(named(&snapshot, "KoruManage"), 1);
    assert_eq!(named(&snapshot, "Manage nav"), 1);
    assert_eq!(named(&snapshot, "Manage profile header"), 1);
    assert!(named(&snapshot, "KoruNavList") >= 3, "the shared rows");
    assert_eq!(named(&snapshot, "Manage nav caption"), 2);
    assert_eq!(named(&snapshot, "Manage page title"), 1);
    // Settings' nav: no panel of its own.
    assert_eq!(named(&snapshot, "Manage nav"), 1);
    // Everything hangs from the tab.
    let root = snapshot
        .records
        .iter()
        .position(|r| r.name == "KoruManage")
        .unwrap();
    for (index, record) in snapshot.records.iter().enumerate() {
        if record.name == "Manage nav" || record.name == "Manage profile header" {
            let mut at = snapshot.records[index].parent;
            while let Some(parent) = at {
                if parent == root {
                    break;
                }
                at = snapshot.records[parent].parent;
            }
            assert_eq!(at, Some(root), "{} is under KoruManage", record.name);
        }
    }

    for (page, part) in [
        (Page::NewProfile, "Manage new profile"),
        (Page::Devices, "Manage linked devices"),
        (Page::Cloud, "Manage Koru Cloud"),
    ] {
        view.show(page);
        let snapshot = capture(&mut view);
        assert_eq!(named(&snapshot, part), 1);
        assert_eq!(
            named(&snapshot, "Manage page title"),
            1,
            "{part} has a head"
        );
    }
}

// ------------------------------------------------------------- accessibility

fn context() -> accessibility::Context {
    accessibility::Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(2),
        area: Rect::new(40.0, 60.0, 1000.0, 700.0),
    }
}

#[test]
fn a_screen_reader_hears_the_nav_as_options_and_the_page_as_a_heading() {
    let (mut view, personal, _) = manage();
    let cx = context();
    let tree = view.semantics(&cx);
    assert_eq!(tree.role, accesskit::Role::Group);
    let nav = &tree.children[0];
    assert_eq!(nav.role, accesskit::Role::ListBox);
    assert_eq!(nav.size_of_set, Some(nav.children.len()));
    assert_eq!(nav.children.len(), 5, "two Profiles, New, Devices, Cloud");
    assert!(
        nav.children
            .iter()
            .all(|option| option.role == accesskit::Role::ListBoxOption)
    );
    assert_eq!(nav.children[0].label.as_deref(), Some("Personal, open"));
    // Bounds are the drawn rows, moved into the window.
    let first = row_of(&view, nav::Entry::Profile(personal.id));
    let bounds = nav.children[0].bounds.unwrap();
    assert_eq!((bounds.x, bounds.y), (first.x + 40.0, first.y + 60.0));
    let heading = &tree.children[1];
    assert_eq!(heading.role, accesskit::Role::Heading);
    assert_eq!(heading.label.as_deref(), Some("Personal"));

    // Pressing an option is the click.
    let devices: Occurrence = nav.children[3].occurrence;
    assert!(view.accessibility_action(&cx, devices, accesskit::Action::Click));
    assert_eq!(view.page(), Some(Page::Devices));
    let tree = view.semantics(&cx);
    assert_eq!(tree.children[1].label.as_deref(), Some("Linked devices"));
    // Focus moves the keyboard's row, which is what focus reports.
    assert!(view.accessibility_action(&cx, devices, accesskit::Action::Focus));
    assert_eq!(view.accessible_focus(&cx), devices);
}
