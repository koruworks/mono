// SPDX-License-Identifier: MPL-2.0

//! The New… launcher in the window: the `+` and Ctrl+N open it, a letter
//! makes its tile's kind, Alt hands over to the place chooser, Escape and a
//! press off it make nothing. The grid on its own — geometry, keys,
//! semantics — is pinned beside the component, in `koru_ui::launcher`.

use koru_workspace_host::routing::{Blockers, ModalKind};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::NewKind;
use crate::mainview::harness::*;
use crate::mainview::panes::PaneNode;
use crate::mainview::tab::TabView;

fn letter(
    v: &mut crate::mainview::MainView,
    w: &mut koru_workspace::Workspace,
    c: &str,
    mods: ModifiersState,
) -> bool {
    v.mods = mods;
    v.key(&mut w.ws, &Key::Character(c.into()), Some(c), mods)
}

#[test]
fn the_plus_opens_the_launcher_as_a_modal() {
    let (mut v, _w) = container();
    assert!(v.open_launcher());
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::Launcher));
    let ask = v.launcher.as_ref().unwrap();
    assert_eq!(
        ask.kinds.iter().map(|k| k.letter()).collect::<String>(),
        "dtcasi"
    );
}

/// **D makes a document**: a draft tab in front, nothing else standing.
#[test]
fn d_opens_a_new_document_tab() {
    let (mut v, mut w) = container();
    let tabs = v.tabs().len();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "d", ModifiersState::empty()));
    assert!(v.launcher.is_none());
    assert_eq!(v.tabs().len(), tabs + 1);
    assert!(v.tabs().active_tab().draft.is_some(), "a draft document");
    assert_eq!(v.blockers(), Blockers::None);
}

/// **T opens Today with the caret in Add a task** when no Tasks page was
/// visited.
#[test]
fn t_opens_tasks_with_the_capture_row_open() {
    let (mut v, mut w) = container();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "t", ModifiersState::empty()));
    let TabView::Tasks(view) = &v.tabs().active_tab().view else {
        panic!("a Tasks page in front");
    };
    assert!(view.capturing_for_test(), "the caret is in Add a task");
}

/// **Alt asks where**: the launcher closes and the place chooser opens for
/// the tile; choosing an arm splits with the new thing.
#[test]
fn alt_with_a_letter_opens_the_place_chooser() {
    let (mut v, mut w) = container();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "d", ModifiersState::ALT));
    assert!(v.launcher.is_none());
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::PlaceChooser));
    assert_eq!(
        v.place_chooser.as_ref().map(|ask| ask.target.clone()),
        Some(super::PlaceTarget::New(NewKind::Document))
    );
    v.mods = ModifiersState::empty();
    assert!(v.key(
        &mut w.ws,
        &Key::Named(NamedKey::ArrowRight),
        None,
        ModifiersState::empty()
    ));
    assert!(v.key(
        &mut w.ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert!(matches!(v.panes.root(), PaneNode::Split { .. }), "split");
    assert!(
        v.tabs().active_tab().draft.is_some(),
        "the new pane holds a draft"
    );
}

/// **Shift on a session asks which**: the profile list, not a session.
#[test]
fn shift_on_shell_lists_the_profiles() {
    let (mut v, mut w) = container();
    let tabs = v.tabs().len();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "S", ModifiersState::SHIFT));
    assert!(v.launcher.is_none());
    assert!(v.menu.is_some(), "the shell profiles");
    assert_eq!(v.tabs().len(), tabs, "nothing started yet");
}

/// **Shift+T asks which page**: the queues, then the lists; choosing a
/// list opens its page with the caret in Add a task.
#[test]
fn shift_on_task_lists_the_pages_to_write_it_on() {
    use crate::mainview::context_menu::{MenuAction, MenuSubject};
    use crate::panes::explore::rail::{Bucket, RailTarget};
    let (mut v, mut w) = container();
    let groceries = koru_core::ids::ListId::new();
    v.list_table = vec![koru_core::system::ListEntry {
        id: groceries,
        name: "Groceries".into(),
        color: None,
        order: None,
        created_ms: None,
        deleted_ms: None,
    }];
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "T", ModifiersState::SHIFT));
    let items = v.menu.as_ref().expect("the pages").items().to_vec();
    assert_eq!(
        items.first(),
        Some(&MenuAction::NewTaskIn(RailTarget::Bucket(Bucket::Today)))
    );
    let list = MenuAction::NewTaskIn(RailTarget::List(groceries));
    assert!(items.contains(&list));
    assert_eq!(list.label(), "Groceries");
    v.menu = None;
    assert!(v.run_menu_action(MenuSubject::NewTab, list));
    let TabView::Tasks(view) = &v.tabs().active_tab().view else {
        panic!("the list's page in front");
    };
    assert!(view.capturing_for_test());
}

/// **The list sits where the choice came from**: Shift+S from the keyboard
/// centres it in the window; a Shift+press on the tile hangs it from the
/// pointer.
#[test]
fn the_follow_up_list_is_centred_from_keys_and_anchored_from_a_press() {
    let (mut v, mut w) = container();
    let viewport = v.viewport;
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "S", ModifiersState::SHIFT));
    let panel = v.menu.as_ref().expect("the profiles").panel(viewport);
    assert!((panel.x + panel.w * 0.5 - viewport.0 * 0.5).abs() < 1.0);
    assert!((panel.y + panel.h * 0.5 - viewport.1 * 0.5).abs() < 1.0);

    v.menu = None;
    v.open_launcher();
    let shell = v
        .launcher
        .as_ref()
        .unwrap()
        .kinds
        .iter()
        .position(|k| *k == NewKind::Shell)
        .unwrap();
    let tile = v
        .launcher
        .as_ref()
        .unwrap()
        .launcher
        .tile_rect(viewport, shell);
    let at = (tile.x + 10.0, tile.y + 10.0);
    v.pointer = at;
    v.mods = ModifiersState::SHIFT;
    assert_eq!(v.launcher_mouse_down(at.0, at.1), Some(true));
    let panel = v.menu.as_ref().expect("the profiles").panel(viewport);
    assert_eq!((panel.x, panel.y), at, "hung from the pointer");
}

/// **I always asks**: files first, a rule, then the content formats — and
/// nothing is imported until a row is chosen.
#[test]
fn i_lists_files_then_the_content_formats() {
    use crate::mainview::context_menu::MenuAction;
    let (mut v, mut w) = container();
    let tabs = v.tabs().len();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "i", ModifiersState::empty()));
    assert!(v.launcher.is_none());
    let items = v.menu.as_ref().expect("the import list").items().to_vec();
    assert_eq!(&items[..2], [MenuAction::ImportFiles, MenuAction::Divider]);
    assert!(
        items[2..]
            .iter()
            .all(|item| matches!(item, MenuAction::ImportFormat(_))),
        "{items:?}"
    );
    assert!(items.len() > 2, "the formats are listed");
    assert_eq!(v.tabs().len(), tabs, "nothing imported yet");
}

#[test]
fn escape_and_a_press_off_it_make_nothing() {
    let (mut v, mut w) = container();
    let tabs = v.tabs().len();
    v.open_launcher();
    assert!(v.key(
        &mut w.ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(v.launcher.is_none());
    v.open_launcher();
    assert_eq!(v.launcher_mouse_down(1.0, 1.0), Some(true));
    assert!(v.launcher.is_none());
    assert_eq!(v.tabs().len(), tabs);
    assert_eq!(v.blockers(), Blockers::None);
}

/// **C opens a new Koru AI chat** in a tab of its own.
#[test]
fn c_opens_a_new_chat() {
    let (mut v, mut w) = container();
    let tabs = v.tabs().len();
    v.open_launcher();
    assert!(letter(&mut v, &mut w, "c", ModifiersState::empty()));
    assert!(v.launcher.is_none());
    assert_eq!(v.tabs().len(), tabs + 1);
    assert!(matches!(v.tabs().active_tab().view, TabView::AiChat(_)));
}

/// **Each tile wears its own word**, and the footer its own sentence — the
/// string table is read by position, and a row out of place hands a tile
/// its neighbour's word.
#[test]
fn every_tile_and_the_footer_read_their_own_words() {
    let (mut v, _w) = container();
    v.open_launcher();
    let launcher = &v.launcher.as_ref().unwrap().launcher;
    assert_eq!(
        launcher
            .tiles()
            .iter()
            .map(|t| t.label.as_str())
            .collect::<Vec<_>>(),
        ["Document", "Task", "Chat", "Agent", "Shell", "Import"]
    );
    assert!(
        launcher
            .hint()
            .is_some_and(|hint| hint.starts_with("Hold Alt"))
    );
}
