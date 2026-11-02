// SPDX-License-Identifier: MPL-2.0

//! The container's half of a canvas, driven the way the shell drives it: a
//! listing pushed, a document announced, a gesture routed.

use super::*;

use crate::mainview::context_menu::{self, MenuAction, MenuSubject};
use crate::mainview::harness::*;
use crate::mainview::tab_rules::KoruTabs;
use crate::panes::Outcome;
use koru_core::WorkspaceId;
use koru_core::explore::{ExploreEntry, KIND_DOCUMENT};
use koru_core::schema::Parent;
use koru_workspace_host::settings::PinnedTabRef;
use koru_workspace_host::{NavigationTarget, Settings, TabRef};
use winit::dpi::PhysicalPosition;
use winit::keyboard::{Key, ModifiersState, NamedKey};

mod at_work;

/// A canvas as `Workspace::create_canvas` leaves it: its own `LoroDoc`, a
/// titled root, and nothing under it.
fn a_canvas(title: &str) -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let root = koru_workspace::seed_document(&mut ws, title, KIND_CANVAS).expect("seed");
    (ws, root)
}

/// …and a page, the same way.
fn a_page(title: &str) -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create");
    let root = koru_workspace::seed_document(&mut ws, title, KIND_DOCUMENT).expect("seed");
    (ws, root)
}

fn listed(id: DocumentId, title: &str, kind: &str) -> ExploreEntry {
    ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: title.into(),
        kind: kind.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

fn row(id: DocumentId) -> NodeId {
    NodeId::from_uuid(id.as_uuid())
}

/// The same document, again — two windows' worth of one canvas.
fn copy_of(ws: &WorkspaceDoc) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap()
}

/// Everything the container has asked the shell for, in order.
fn requests(v: &mut MainView) -> Vec<PaneRequest> {
    std::iter::from_fn(|| View::take_pane_request(v)).collect()
}

/// A container with a page and a canvas listed, the canvas open in front.
struct OnACanvas {
    v: MainView,
    canvas_ws: WorkspaceDoc,
    canvas: DocumentId,
    root: NodeId,
    page: DocumentId,
}

fn on_a_canvas() -> OnACanvas {
    let (mut v, _) = container();
    let (canvas_ws, root) = a_canvas("Board");
    let canvas = DocumentId::new();
    let page = DocumentId::new();
    View::set_entries(
        &mut v,
        vec![
            listed(page, "Brief", KIND_DOCUMENT),
            listed(canvas, "Board", KIND_CANVAS),
        ],
        Some(canvas),
    );
    assert!(View::show_active_document(&mut v, &canvas_ws, Some(canvas)));
    let _ = requests(&mut v);
    OnACanvas {
        v,
        canvas_ws,
        canvas,
        root,
        page,
    }
}

fn active_canvas(v: &MainView) -> &CanvasView {
    match &v.tabs().active_tab().view {
        TabView::Canvas(canvas) => canvas,
        _ => panic!("the tab in front is not a canvas"),
    }
}

fn type_text(v: &mut MainView, ws: &mut WorkspaceDoc, text: &str) {
    for c in text.chars() {
        let s = c.to_string();
        v.key(
            ws,
            &Key::Character(s.as_str().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
}

fn enter(v: &mut MainView, ws: &mut WorkspaceDoc) {
    v.key(
        ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
}

// ------------------------------------------------------------------ creating

/// **New canvas asks the shell for a canvas, from both doors** — the `+`
/// menu's Canvases flyout and the empty section's button — with an empty title,
/// as a new page's is. What the shell makes and announces lands in a canvas
/// tab over a document holding only its root.
#[test]
fn new_canvas_makes_a_canvas_and_lands_in_a_canvas_tab() {
    let (mut v, workspace) = container();
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::NewCanvas));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::CreateCanvas(String::new()))
    );
    assert!(v.apply(&workspace.ws, Outcome::NewCanvas));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::CreateCanvas(String::new()))
    );

    // The shell's half: register, seed the root, list it (as a canvas, even
    // before the manifest's echo), then announce it.
    let (ws, root) = a_canvas("");
    let id = DocumentId::new();
    View::set_entries(&mut v, vec![listed(id, "", KIND_CANVAS)], Some(id));
    let tabs = v.tabs().len();
    assert!(View::show_active_document(&mut v, &ws, Some(id)));

    assert_eq!(v.tabs().len(), tabs + 1, "a tab of its own");
    let tab = v.tabs().active_tab();
    assert_eq!(tab.document, Some(id));
    assert_eq!(tab.view.root(), Some(root));
    assert_eq!(
        tab.view.icon(),
        Some(koru_draw::icons::CANVAS),
        "wears its kind"
    );
    assert!(tab.strip_shows_icon(), "and the strip makes room for it");
    let canvas = active_canvas(&v);
    assert_eq!(canvas.document(), id);
    assert_eq!(canvas.root(), root);
    assert!(
        ws.children(Parent::Node(root)).unwrap().is_empty(),
        "a canvas holds only its root"
    );
    assert_eq!(v.recents.first(), Some(&id), "recorded as a page would be");
}

// ------------------------------------------------------------------- opening

/// **From Explore**: a click on a canvas row asks for the document, and the
/// arrival is a canvas tab — the kind decided in `document_view`, not at the
/// door.
#[test]
fn a_canvas_opens_as_a_canvas_from_explore() {
    let (mut v, page_workspace) = container();
    let (canvas_ws, root) = a_canvas("Board");
    let canvas = DocumentId::new();
    View::set_entries(&mut v, vec![listed(canvas, "Board", KIND_CANVAS)], None);
    assert!(v.apply(&page_workspace.ws, Outcome::OpenNode(row(canvas))));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::Navigate {
            target: NavigationTarget::Document { document, .. },
            ..
        }) if document == canvas
    ));
    assert!(View::show_active_document(&mut v, &canvas_ws, Some(canvas)));
    assert_eq!(active_canvas(&v).root(), root);
    assert_eq!(v.tabs().active_tab().document, Some(canvas));
}

/// A page stays a page: the same door, the other kind.
#[test]
fn a_page_still_opens_as_a_page() {
    let (mut v, _) = container();
    let (page_ws, _) = a_page("Brief");
    let page = DocumentId::new();
    View::set_entries(&mut v, vec![listed(page, "Brief", KIND_DOCUMENT)], None);
    assert!(View::show_active_document(&mut v, &page_ws, Some(page)));
    assert!(matches!(v.tabs().active_tab().view, TabView::Leaf(_)));
}

/// **From the recents**: the `+` menu's Canvases flyout lists the canvas and
/// the Documents flyout does not, and choosing it after its tab has closed
/// reopens it as a canvas.
#[test]
fn a_recent_canvas_is_under_canvases_and_reopens_as_one() {
    let OnACanvas {
        mut v,
        canvas_ws,
        canvas,
        page,
        ..
    } = on_a_canvas();
    let (page_ws, _) = a_page("Brief");
    assert!(View::show_active_document(&mut v, &page_ws, Some(page)));

    v.snapshot_plus_menu();
    assert!(context_menu::canvas_plus_items().contains(&MenuAction::OpenRecentDocument(canvas)));
    assert!(!context_menu::document_plus_items().contains(&MenuAction::OpenRecentDocument(canvas)));
    assert!(context_menu::document_plus_items().contains(&MenuAction::OpenRecentDocument(page)));

    let i = v.tabs().showing_document(canvas).expect("the canvas tab");
    assert!(v.close(i));
    assert!(v.tabs().showing_document(canvas).is_none());
    let _ = requests(&mut v);

    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenRecentDocument(canvas)));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::Navigate { .. })
    ));
    assert!(View::show_active_document(&mut v, &canvas_ws, Some(canvas)));
    assert_eq!(active_canvas(&v).document(), canvas);
    context_menu::set_plus_canvases(Vec::new());
}

/// **As a background tab** — a Ctrl+click, or a document the shell loads
/// behind the one in front: inserted without being selected, and a canvas.
#[test]
fn a_canvas_opens_as_a_canvas_in_a_background_tab() {
    let (mut v, _) = container();
    let (canvas_ws, root) = a_canvas("Board");
    let canvas = DocumentId::new();
    View::set_entries(&mut v, vec![listed(canvas, "Board", KIND_CANVAS)], None);
    let active = v.tabs().active();
    assert!(View::open_background_tab(&mut v, &canvas_ws, canvas, None));
    assert_eq!(v.tabs().active(), active, "not selected");
    let i = v.tabs().showing_document(canvas).expect("inserted");
    let tab = v.tabs().get(i).unwrap();
    assert!(matches!(&tab.view, TabView::Canvas(c) if c.root() == root));
}

// --------------------------------------------------------------- restarting

/// Close the window over `settings` and open a fresh container onto the one
/// document the workspace holds, with the listing already pushed.
fn reopen(settings: &Settings, ws: WorkspaceDoc, id: DocumentId, listing: bool) -> MainView {
    let workspace = koru_workspace::Workspace::holding_for_tests(ws, id);
    let mut v = MainView::new();
    if listing {
        View::set_entries(&mut v, vec![listed(id, "Board", KIND_CANVAS)], None);
    }
    v.restore(&workspace, settings);
    v.set_viewport(1200.0, 700.0);
    v
}

fn find_canvas(v: &MainView, id: DocumentId) -> Option<&crate::mainview::tab::Tab> {
    v.tabs()
        .iter()
        .find(|t| t.document == Some(id) && matches!(t.view, TabView::Canvas(_)))
}

/// **A canvas tab persists as a document and comes back a canvas**, through
/// the pane tree a session writes today and through the legacy tab list an
/// older file carries.
#[test]
fn a_canvas_tab_survives_a_restart() {
    let OnACanvas {
        v,
        canvas_ws,
        canvas,
        ..
    } = on_a_canvas();
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert!(
        settings
            .tabs
            .iter()
            .any(|(t, _)| matches!(t, TabRef::Document { id, .. } if *id == canvas)),
        "saved as a document tab, the format unchanged"
    );

    let back = reopen(&settings, copy_of(&canvas_ws), canvas, true);
    assert!(find_canvas(&back, canvas).is_some(), "the pane tree's path");

    let mut legacy = settings.clone();
    legacy.pane_tree = None;
    let back = reopen(&legacy, canvas_ws, canvas, true);
    assert!(find_canvas(&back, canvas).is_some(), "the legacy path");
}

/// …and **pinned**, the same, through both paths.
#[test]
fn a_pinned_canvas_survives_a_restart() {
    let OnACanvas {
        mut v,
        canvas_ws,
        canvas,
        ..
    } = on_a_canvas();
    let i = v.tabs().active();
    assert!(v.pin_tab(i));
    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert!(
        settings
            .pinned_tabs
            .iter()
            .any(|pin: &PinnedTabRef| pin.document() == Some(canvas))
    );

    let back = reopen(&settings, copy_of(&canvas_ws), canvas, true);
    let tab = find_canvas(&back, canvas).expect("the pinned canvas, pane tree");
    assert!(tab.is_pinned());

    let mut legacy = settings.clone();
    legacy.pane_tree = None;
    let back = reopen(&legacy, canvas_ws, canvas, true);
    let tab = find_canvas(&back, canvas).expect("the pinned canvas, legacy");
    assert!(tab.is_pinned());
}

/// **A tab built before the listing knew its kind becomes a canvas tab when it
/// does.** Restore runs before the shell's first listing; a workspace with no
/// manifest to ask — this test's — builds a page over the canvas's root, and
/// the listing that arrives next corrects it in place: same tab, same pin, same
/// place in the strip.
#[test]
fn a_restored_tab_becomes_a_canvas_when_the_listing_arrives() {
    let OnACanvas {
        v,
        canvas_ws,
        canvas,
        root,
        ..
    } = on_a_canvas();
    let mut settings = Settings::default();
    v.capture(&mut settings);

    let mut back = reopen(&settings, canvas_ws, canvas, false);
    let i = back.tabs().showing_document(canvas).expect("reopened");
    assert!(matches!(back.tabs().get(i).unwrap().view, TabView::Leaf(_)));
    let id = back.tabs().get(i).unwrap().id;

    View::set_entries(
        &mut back,
        vec![listed(canvas, "Board", KIND_CANVAS)],
        Some(canvas),
    );
    let i = back.tabs().showing_document(canvas).expect("still there");
    let tab = back.tabs().get(i).unwrap();
    assert_eq!(tab.id, id, "the same tab");
    assert!(matches!(&tab.view, TabView::Canvas(c) if c.root() == root && c.document() == canvas));
}

// ------------------------------------------------------------------ renaming

/// **F2 renames the canvas in front** with the name prompt, which writes its
/// root — and the tab's label follows on the Enter that committed it, as a
/// page's does.
#[test]
fn f2_renames_the_canvas_in_front() {
    let OnACanvas {
        mut v,
        mut canvas_ws,
        root,
        ..
    } = on_a_canvas();
    v.key(
        &mut canvas_ws,
        &Key::Named(NamedKey::F2),
        None,
        ModifiersState::empty(),
    );
    assert!(v.tabs().active_tab().view.is_renaming());
    assert!(
        v.tabs().active_tab().view.is_prompting(),
        "the prompt owns the keys"
    );
    type_text(&mut v, &mut canvas_ws, "Plans");
    enter(&mut v, &mut canvas_ws);
    assert!(!v.tabs().active_tab().view.is_renaming());
    assert_eq!(
        canvas_ws.read(root).unwrap().content.as_text(),
        Some("Plans")
    );
    assert_eq!(
        v.tabs().active_tab().label,
        "Plans",
        "the tab follows at once"
    );
}

/// **The tab menu's Rename…** asks for the document, as it does for a page,
/// and the prompt opens over the canvas when the answer lands.
#[test]
fn the_tab_menus_rename_opens_the_canvas_prompt() {
    let OnACanvas {
        mut v,
        canvas_ws,
        canvas,
        ..
    } = on_a_canvas();
    let tab = v.tabs().active();
    assert!(v.run_menu_action(MenuSubject::Tab(tab), MenuAction::Rename));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::Navigate { .. })
    ));
    assert!(View::open_search_target(
        &mut v,
        &canvas_ws,
        Some(row(canvas)),
        None,
        None,
        None
    ));
    assert!(v.tabs().active_tab().view.is_renaming());
    assert!(v.pending_document_rename.is_none());
}

/// **Explore's Rename…** on the canvas's row, the same prompt.
#[test]
fn explores_rename_opens_the_canvas_prompt() {
    let OnACanvas {
        mut v,
        mut canvas_ws,
        canvas,
        root,
        ..
    } = on_a_canvas();
    assert!(v.run_menu_action(MenuSubject::Nodes(vec![row(canvas)]), MenuAction::Rename));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::Navigate { .. })
    ));
    assert!(View::open_search_target(
        &mut v,
        &canvas_ws,
        Some(row(canvas)),
        None,
        None,
        None
    ));
    assert!(v.tabs().active_tab().view.is_renaming());
    type_text(&mut v, &mut canvas_ws, "Roadmap");
    enter(&mut v, &mut canvas_ws);
    assert_eq!(
        canvas_ws.read(root).unwrap().content.as_text(),
        Some("Roadmap")
    );
}

// ------------------------------------------------------------------ the bin

/// **Trash and restore** travel the way a page's do: by document, through the
/// one queue, to the shell.
#[test]
fn a_canvas_goes_to_the_trash_and_comes_back_as_a_document() {
    let OnACanvas {
        mut v,
        mut canvas_ws,
        canvas,
        ..
    } = on_a_canvas();
    assert!(v.apply(&canvas_ws, Outcome::TrashNodes(vec![row(canvas)])));
    assert!(requests(&mut v).contains(&PaneRequest::TrashDocuments(vec![canvas])));
    v.pending_restore = Some(vec![row(canvas)]);
    v.drain_restore(&mut canvas_ws);
    assert!(requests(&mut v).contains(&PaneRequest::RestoreDocuments(vec![canvas])));
}

// ------------------------------------------------------------ zoom and wheel

/// A point inside the content rect's view area, clear of the tab lane.
fn over_the_view(v: &MainView) -> (f32, f32) {
    let rect = v
        .panes
        .view_rect_for(v.content(), v.panes.focused_path())
        .expect("a pane");
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// **Ctrl+wheel and a pinch over a canvas zoom the canvas**: the container
/// claims it for the canvas under the pointer, handing over the raw delta and
/// the pointer in the canvas's own space. Nowhere else does it claim — over a
/// page, over the explore column, under an open menu — and the display zoom
/// keeps those.
#[test]
fn primary_wheel_over_a_canvas_is_the_canvases() {
    let OnACanvas { mut v, .. } = on_a_canvas();
    let (x, y) = over_the_view(&v);
    let rect = v
        .panes
        .view_rect_for(v.content(), v.panes.focused_path())
        .unwrap();
    let pinch = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.5));
    assert!(View::zoom_wheel(&mut v, pinch, x, y));
    let seen = active_canvas(&v).gestures().zoom_wheel.expect("reached");
    assert_eq!(seen.delta, (0.0, 4.5), "the raw delta, not its sign");
    assert_eq!(seen.at, (x - rect.x, y - rect.y), "pane-local");

    // Over the tab lane — chrome, not the canvas.
    assert!(!View::zoom_wheel(&mut v, pinch, x, rect.y - 4.0));
    // Under a menu.
    v.open_new_tab_menu();
    assert!(v.menu.is_some());
    assert!(!View::zoom_wheel(&mut v, pinch, x, y));
}

#[test]
fn primary_wheel_over_a_page_is_the_apps() {
    let (mut v, workspace) = on_document();
    let _ = workspace;
    let (x, y) = over_the_view(&v);
    let wheel = MouseScrollDelta::LineDelta(0.0, 1.0);
    assert!(!View::zoom_wheel(&mut v, wheel, x, y));
    assert!(!View::zoom_key(&mut v, ZoomIntent::In));
}

/// **Ctrl+= / Ctrl+- / Ctrl+0 on a canvas zoom the canvas**, the keyboard
/// following focus rather than the pointer.
#[test]
fn the_zoom_chords_on_a_canvas_are_the_canvases() {
    let OnACanvas { mut v, .. } = on_a_canvas();
    assert!(View::zoom_key(&mut v, ZoomIntent::Out));
    assert!(View::zoom_key(&mut v, ZoomIntent::ActualSize));
    let seen = active_canvas(&v).gestures();
    assert_eq!(seen.zoom_key, Some(ZoomIntent::ActualSize));
    assert_eq!(seen.zooms_claimed, 2);
}

/// **The plain wheel reaches the canvas with where it happened** — the
/// container knows the pointer and the pane under it, and hands the canvas a
/// pane-local position the position-less `scroll_by` cannot carry.
#[test]
fn the_wheel_reaches_the_canvas_with_a_pane_local_pointer() {
    let OnACanvas { mut v, .. } = on_a_canvas();
    let (x, y) = over_the_view(&v);
    let rect = v
        .panes
        .view_rect_for(v.content(), v.panes.focused_path())
        .unwrap();
    View::mouse_move(&mut v, x, y);
    View::scroll_by(&mut v, MouseScrollDelta::LineDelta(0.0, -3.0));
    let seen = active_canvas(&v).gestures().wheel.expect("reached");
    assert_eq!(seen.delta, (0.0, -3.0));
    assert_eq!(seen.at, (x - rect.x, y - rect.y));
}

/// Modifier changes reach the canvas, and a drag it owns keeps the pointer
/// past the pane's edge — the two container hooks a pan and a resize need.
#[test]
fn modifiers_and_drags_reach_the_canvas_arm() {
    let OnACanvas {
        mut v,
        mut canvas_ws,
        ..
    } = on_a_canvas();
    View::set_mods(&mut v, ModifiersState::SHIFT);
    assert_eq!(active_canvas(&v).mods(), ModifiersState::SHIFT);

    assert!(!v.tabs().active_tab().view.is_dragging());
    v.key(
        &mut canvas_ws,
        &Key::Named(NamedKey::F2),
        None,
        ModifiersState::empty(),
    );
    let field = {
        let rect = v
            .panes
            .view_rect_for(v.content(), v.panes.focused_path())
            .unwrap();
        let panel = koru_ui::KoruDialogFrame::name_prompt((rect.w, rect.h)).panel();
        let f = koru_editor::components::name_prompt::field_rect(panel.w);
        (
            rect.x + panel.x + f.x + 4.0,
            rect.y + panel.y + f.y + f.h * 0.5,
        )
    };
    v.set_viewport(1200.0, 700.0);
    View::mouse_move(&mut v, field.0, field.1);
    View::mouse_down(&mut v, &mut canvas_ws, field.0, field.1, false);
    assert!(v.tabs().active_tab().view.is_dragging());
    View::mouse_up(&mut v, &mut canvas_ws);
    assert!(!v.tabs().active_tab().view.is_dragging());
}

/// **A canvas does not pop out** — the document window a popped-out tab
/// becomes is the page editor, which would show a canvas as a title over
/// nothing. A page still does.
#[test]
fn a_canvas_tab_stays_in_the_window() {
    let OnACanvas { v, .. } = on_a_canvas();
    assert!(!v.is_poppable(v.tabs().active()));
    let (page, _) = on_document();
    assert!(page.is_poppable(page.tabs().active()));
}

/// **Reveal in Explore** shows a canvas in the Canvases section, where its row
/// is, and a page back in Documents.
#[test]
fn revealing_a_canvas_shows_the_canvases_section() {
    let OnACanvas { mut v, .. } = on_a_canvas();
    let i = v.tabs().active();
    assert!(v.reveal_tab_in_explore(i));
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Canvases
    );
}

// ------------------------------------------------------------------ Explore

/// **A canvas row dragged out of the Canvases section onto a pane opens there,
/// as a canvas** — the document is asked for, pinned to this window, and the
/// arrival lands in a canvas tab. The row stays listed: a drop on a pane is an
/// opening, never a move.
#[test]
fn a_canvas_row_dragged_onto_a_pane_opens_as_a_canvas() {
    let (mut v, mut workspace) = container();
    let canvas = DocumentId::new();
    View::set_entries(&mut v, vec![listed(canvas, "Board", KIND_CANVAS)], None);
    v.toggle_sidebar();
    v.explore.show_canvases();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    let (_, r) = v
        .explore
        .row_rects(v.regions.sidebar)
        .into_iter()
        .find(|(i, _)| v.explore.row_node(*i) == Some(row(canvas)))
        .expect("Board has a row under Canvases");

    let (x, y) = r.centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    v.mouse_move(x, y + 5.0);
    assert!(v.explore.drag_armed());
    let content = v.content();
    v.mouse_move(content.x + content.w * 0.5, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);
    assert!(
        requests(&mut v)
            .iter()
            .any(|r| matches!(r, PaneRequest::Navigate { .. })),
        "the canvas was asked for"
    );

    let (canvas_ws, root) = a_canvas("Board");
    assert!(View::show_active_document(&mut v, &canvas_ws, Some(canvas)));
    assert_eq!(active_canvas(&v).root(), root);

    v.refresh_explore(&canvas_ws, true);
    assert!(
        v.explore
            .row_rects(v.regions.sidebar)
            .into_iter()
            .any(|(i, _)| v.explore.row_node(i) == Some(row(canvas))),
        "still listed"
    );
}

/// **The section a window was showing comes back with it**: a session saved
/// on Canvases reopens on Canvases, and `Alt+7` reaches it — opening the
/// column when it is shut.
#[test]
fn the_canvases_section_survives_a_restart_and_answers_alt_7() {
    let (mut v, mut workspace) = container();
    assert!(!v.sidebar.is_open());
    v.key(
        &mut workspace.ws,
        &Key::Character("7".into()),
        Some("7"),
        // The view modifier: Alt, or ⌃⌘ on a Mac (M42).
        koru_keys::VIEW,
    );
    assert!(
        v.sidebar.is_open(),
        "the column opened for the view asked for"
    );
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Canvases
    );

    let mut settings = Settings::default();
    v.capture(&mut settings);
    assert_eq!(settings.navigator_mode, "canvases");
    let mut back = MainView::new();
    back.restore(&workspace, &settings);
    assert_eq!(
        back.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Canvases
    );
}

// ------------------------------------------------- ENABLE_CANVAS off

/// **No door opens a canvas when the build hides canvases**
/// (`ENABLE_CANVAS` off, koru-features): *New canvas* and the empty section's
/// button ask the shell for nothing, a landing on a canvas the shell
/// announces builds no tab, a background open inserts none, and Alt+7 does
/// not show the Canvases view.
#[test]
fn with_canvases_hidden_no_door_opens_one() {
    use koru_features::{Features, testing};
    testing::with(Features::ALL_OFF, || {
        let (mut v, mut workspace) = container();
        let (canvas_ws, _) = a_canvas("Board");
        let canvas = DocumentId::new();
        View::set_entries(&mut v, vec![listed(canvas, "Board", KIND_CANVAS)], None);
        let _ = requests(&mut v);
        let before = v.tabs().len();

        assert!(!v.run_menu_action(MenuSubject::NewTab, MenuAction::NewCanvas));
        assert!(!v.apply(&workspace.ws, Outcome::NewCanvas));
        assert!(
            !requests(&mut v)
                .iter()
                .any(|r| matches!(r, PaneRequest::CreateCanvas(_))),
            "no canvas was asked for"
        );

        View::show_active_document(&mut v, &canvas_ws, Some(canvas));
        assert!(!View::open_background_tab(&mut v, &canvas_ws, canvas, None));
        assert!(v.tabs().showing_document(canvas).is_none());
        assert!(
            !v.tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Canvas(_)))
        );
        assert_eq!(v.tabs().len(), before, "no tab opened");

        v.key(
            &mut workspace.ws,
            &Key::Character("7".into()),
            Some("7"),
            koru_keys::VIEW,
        );
        assert_ne!(
            v.explore.navigator_mode(),
            crate::panes::explore::NavigatorMode::Canvases
        );
    });
}

/// **A session saved with canvases shown comes back without them** when the
/// build hides canvases: the canvas tab and the pinned canvas are dropped
/// (the document is untouched), on the pane tree's path and the legacy one,
/// and the Canvases view reopens on Documents.
#[test]
fn a_restored_session_drops_canvas_tabs_when_canvases_are_hidden() {
    use koru_features::{Features, testing};
    let OnACanvas {
        mut v,
        canvas_ws,
        canvas,
        ..
    } = on_a_canvas();
    let mut settings = Settings::default();
    v.capture(&mut settings);
    let i = v.tabs().active();
    assert!(v.pin_tab(i));
    let mut pinned = Settings::default();
    v.capture(&mut pinned);
    settings.navigator_mode = "canvases".into();
    testing::with(Features::ALL_OFF, || {
        for settings in [&settings, &pinned] {
            let mut legacy = settings.clone();
            legacy.pane_tree = None;
            for settings in [settings, &legacy] {
                let back = reopen(settings, copy_of(&canvas_ws), canvas, true);
                assert!(back.tabs().showing_document(canvas).is_none());
                assert!(find_canvas(&back, canvas).is_none());
                assert_eq!(
                    back.explore.navigator_mode(),
                    crate::panes::explore::NavigatorMode::Documents
                );
            }
        }
    });
}
