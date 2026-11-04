// SPDX-License-Identifier: MPL-2.0

//! The click rule for references, and the place chooser Alt+click opens:
//! every gesture's destination, for a document reference and a chat link.

use koru_editor::{ChatTarget, Editing, OpenWhere, Reference, SplitSide};
use koru_workspace_host::View;
use koru_workspace_host::routing::{Blockers, ModalKind};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::{Place, PlaceChooser};
use crate::mainview::MainView;
use crate::mainview::harness::*;
use crate::mainview::panes::{PaneNode, PanePath, SplitAxis};
use crate::mainview::tab::TabView;

/// The window's split, as the axis and which side the focused pane is on —
/// `None` while it is one pane.
fn split_shape(v: &MainView) -> Option<(SplitAxis, Vec<u8>)> {
    match v.panes.root() {
        PaneNode::Split { axis, .. } => Some((*axis, v.panes.focused_path().as_bytes().to_vec())),
        PaneNode::Group(_) => None,
    }
}

/// What each arm must make: the split's axis and the new, focused pane's
/// side of it (`0` the first — above or left — and `1` the second).
fn expected(place: Place) -> Option<(SplitAxis, Vec<u8>)> {
    match place {
        Place::Up => Some((SplitAxis::Horizontal, vec![0])),
        Place::Down => Some((SplitAxis::Horizontal, vec![1])),
        Place::Left => Some((SplitAxis::Vertical, vec![0])),
        Place::Right => Some((SplitAxis::Vertical, vec![1])),
        Place::Middle => None,
    }
}

/// The document the pane the click came from shows, once a split has made
/// the new pane the focused one.
fn other_pane_root(v: &MainView) -> Option<koru_core::NodeId> {
    let focused = v.panes.focused_path().as_bytes().to_vec();
    let other = PanePath::root().child(1 - focused[0] as usize);
    v.panes.group(&other)?.active_tab().view.root()
}

/// Put an Alt+click's ask where the editor leaves it, and let the container
/// drain it, the way a press does.
fn alt_click(v: &mut MainView, reference: Reference, at: (f32, f32)) {
    v.pointer = at;
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("a document in front");
    };
    leaf.view_mut().wants_place_chooser = Some(reference);
    assert!(v.drain_place_chooser(), "the ask opened the chooser");
}

fn a_chat() -> ChatTarget {
    ChatTarget {
        chat: uuid::Uuid::new_v4(),
        block: None,
    }
}

fn chat_tab_on(v: &MainView, chat: ChatTarget) -> bool {
    matches!(
        &v.tabs().active_tab().view,
        TabView::AiChat(view) if view.chat_id() == Some(koru_ai_ui::ChatId::from_uuid(chat.chat))
    )
}

// ------------------------------------------------------------ the chooser

#[test]
fn each_target_names_where_it_opens() {
    assert_eq!(Place::Up.where_(), OpenWhere::Split(SplitSide::Up));
    assert_eq!(Place::Right.where_(), OpenWhere::Split(SplitSide::Right));
    assert_eq!(Place::Down.where_(), OpenWhere::Split(SplitSide::Down));
    assert_eq!(Place::Left.where_(), OpenWhere::Split(SplitSide::Left));
    assert_eq!(
        Place::Middle.where_(),
        OpenWhere::NewTab,
        "the middle is a new tab, and no split"
    );
    for place in Place::ALL {
        assert!(!place.label(koru_i18n::Lang::En).is_empty(), "{place:?}");
    }
}

/// The arrows move across the cross it draws: out from the middle, back to
/// it from the arm opposite, round to a neighbour perpendicular — and never
/// off the edge.
#[test]
fn the_arrows_walk_the_cross() {
    use NamedKey::{ArrowDown as D, ArrowLeft as L, ArrowRight as R, ArrowUp as U};
    for (from, key, to) in [
        (Place::Middle, U, Place::Up),
        (Place::Middle, R, Place::Right),
        (Place::Middle, D, Place::Down),
        (Place::Middle, L, Place::Left),
        (Place::Up, D, Place::Middle),
        (Place::Up, U, Place::Up),
        (Place::Up, L, Place::Left),
        (Place::Up, R, Place::Right),
        (Place::Right, L, Place::Middle),
        (Place::Right, R, Place::Right),
        (Place::Right, D, Place::Down),
        (Place::Down, U, Place::Middle),
        (Place::Left, R, Place::Middle),
        (Place::Left, U, Place::Up),
    ] {
        assert_eq!(from.step(key), to, "{from:?} {key:?}");
    }
}

/// **Never clipped at the window's edge.** An Alt+click in a corner keeps
/// the whole cross inside the window; in the open, the middle is under the
/// press.
#[test]
fn the_chooser_stays_inside_the_window() {
    let viewport = (1200.0, 700.0);
    let reference = Reference::Node(koru_core::NodeId::new());
    let inset = koru_draw::theme::overlay::popover().viewport_inset;
    for at in [(0.0, 0.0), (1200.0, 700.0), (1199.0, 3.0), (2.0, 698.0)] {
        let chooser = PlaceChooser::new(reference, PanePath::root(), at);
        let panel = chooser.panel(viewport);
        assert!(panel.x >= inset && panel.y >= inset, "{at:?}: {panel:?}");
        assert!(
            panel.x + panel.w <= viewport.0 - inset && panel.y + panel.h <= viewport.1 - inset,
            "{at:?}: {panel:?}"
        );
        for place in Place::ALL {
            let cell = chooser.cell_rect(viewport, place);
            assert!(
                cell.x >= panel.x && cell.x + cell.w <= panel.x + panel.w,
                "{place:?} outside the panel"
            );
        }
    }
    let chooser = PlaceChooser::new(reference, PanePath::root(), (600.0, 350.0));
    assert_eq!(
        chooser.place_at(viewport, 600.0, 350.0),
        Some(Place::Middle),
        "a second press where the first was opens a new tab"
    );
}

#[test]
fn an_alt_click_opens_the_chooser_at_the_press_on_the_middle() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    let tabs = v.tabs().len();

    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));

    let chooser = v.place_chooser.as_ref().expect("the chooser is up");
    assert_eq!(chooser.highlight, Place::Middle);
    assert_eq!(chooser.at, (400.0, 300.0));
    assert_eq!(v.blockers(), Blockers::Modal(ModalKind::PlaceChooser));
    assert_eq!(
        v.tabs().len(),
        tabs,
        "nothing opens until a place is chosen"
    );
    assert!(split_shape(&v).is_none());
}

// ------------------------------------------- a document reference, everywhere

/// Every target, for a document the window holds: the four arms split the
/// pane on their side and show the reference in the new one — the page it
/// was followed from stays in the other — and the middle opens a new tab
/// with no split.
#[test]
fn a_document_reference_opens_on_every_side_and_in_a_new_tab() {
    for place in Place::ALL {
        let (mut v, mut w) = on_document();
        let notes = v.document_root().expect("notes");
        let brief = a_document(&mut w.ws, "Brief");
        let tabs = v.tabs().len();
        alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));

        assert!(v.choose_place(&w.ws, place), "{place:?}");

        assert!(v.place_chooser.is_none(), "{place:?}: the chooser closed");
        assert_eq!(split_shape(&v), expected(place), "{place:?}");
        assert_eq!(v.document_root(), Some(brief), "{place:?}");
        match place {
            Place::Middle => assert_eq!(v.tabs().len(), tabs + 1, "a new tab"),
            _ => assert_eq!(other_pane_root(&v), Some(notes), "{place:?}"),
        }
    }
}

/// A document the Store routes — an `@` pill, a `koru://doc/…` link — splits
/// when it lands, and the open is pinned to this window, whose pane it is.
#[test]
fn a_routed_document_splits_when_it_lands() {
    let (mut v, w) = on_document();
    let root = v.document_root().expect("notes");
    let document = koru_core::DocumentId::from_uuid(root.as_uuid());
    let target = koru_editor::trigger::DocumentTarget {
        document,
        node: None,
    };
    alt_click(&mut v, Reference::Document(target), (400.0, 300.0));

    assert!(v.choose_place(&w.ws, Place::Right));
    assert_eq!(
        v.pending_lands.back().copied(),
        Some((document, OpenWhere::Split(SplitSide::Right)))
    );
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            disposition: koru_workspace_host::SearchTargetDisposition::SourceWindow,
            ..
        })
    ));
    assert!(split_shape(&v).is_none(), "nothing moves before it lands");

    v.active_document = Some(document);
    assert!(v.land_on(&w.ws, root, false, None));
    assert_eq!(
        split_shape(&v),
        Some((SplitAxis::Vertical, vec![1])),
        "landed to the right"
    );
    assert!(v.pending_lands.is_empty(), "the disposition was spent");
}

// ------------------------------------------------------- a chat, everywhere

/// **A Koru chat link opens inside Koru**, on every target of the chooser:
/// a chat tab in a new pane on each side, or a new tab in the middle.
#[test]
fn a_chat_link_opens_as_a_koru_chat_on_every_side_and_in_a_new_tab() {
    if !koru_features::ai() {
        return;
    }
    for place in Place::ALL {
        let (mut v, w) = on_document();
        let notes = v.document_root().expect("notes");
        let tabs = v.tabs().len();
        let chat = a_chat();
        alt_click(&mut v, Reference::Chat(chat), (400.0, 300.0));

        assert!(v.choose_place(&w.ws, place), "{place:?}");

        assert_eq!(split_shape(&v), expected(place), "{place:?}");
        assert!(chat_tab_on(&v, chat), "{place:?}: the chat is in front");
        match place {
            Place::Middle => assert_eq!(v.tabs().len(), tabs + 1),
            _ => assert_eq!(other_pane_root(&v), Some(notes), "{place:?}"),
        }
    }
}

/// One chat, one view of it: an arm moves a tab already on the chat into
/// the new pane rather than opening a second.
#[test]
fn a_split_moves_a_chat_tab_that_is_already_open() {
    if !koru_features::ai() {
        return;
    }
    let (mut v, w) = on_document();
    let chat = a_chat();
    assert!(v.open_chat_where(chat, OpenWhere::BackgroundTab));
    let tabs = v.tabs().len();

    assert!(v.open_reference(
        &w.ws,
        Reference::Chat(chat),
        OpenWhere::Split(SplitSide::Down)
    ));

    assert_eq!(split_shape(&v), Some((SplitAxis::Horizontal, vec![1])));
    assert!(chat_tab_on(&v, chat));
    let chats = v
        .panes
        .iter_groups()
        .flat_map(|(_, group)| group.iter_tabs())
        .filter(|tab| matches!(tab.view, TabView::AiChat(_)))
        .count();
    assert_eq!(chats, 1, "moved, not doubled");
    let left = v
        .panes
        .group(&PanePath::root().child(0))
        .map(|group| group.len());
    assert_eq!(left, Some(tabs - 1), "it left the pane it was in");
}

/// The click rule, for a chat link: Ctrl+click a background tab, Shift+click
/// in front here (a chat has no window of its own), and a click on a chat
/// this window does not list yet opens it in front rather than replacing the
/// page — the route a chat of another profile is handed on from.
#[test]
fn a_chat_link_follows_the_click_rule() {
    if !koru_features::ai() {
        return;
    }
    let (mut v, w) = on_document();
    let notes = v.document_root().expect("notes");
    let active = v.tabs().active();
    let tabs = v.tabs().len();

    let background = a_chat();
    assert!(v.open_reference(&w.ws, Reference::Chat(background), OpenWhere::BackgroundTab));
    assert_eq!(v.tabs().len(), tabs + 1, "a tab was inserted");
    assert_eq!(v.tabs().active(), active, "and not switched to");
    assert_eq!(v.document_root(), Some(notes));

    let shifted = a_chat();
    assert!(v.open_reference(&w.ws, Reference::Chat(shifted), OpenWhere::NewWindow));
    assert!(chat_tab_on(&v, shifted), "opened here, in front");

    let (mut v, w) = on_document();
    let notes = v.document_root().expect("notes");
    let tabs = v.tabs().len();
    let unlisted = a_chat();
    assert!(v.open_reference(&w.ws, Reference::Chat(unlisted), OpenWhere::SameTab));
    assert!(chat_tab_on(&v, unlisted));
    assert_eq!(v.tabs().len(), tabs + 1, "the page was not replaced");
    assert!(
        v.tabs().iter().any(|tab| tab.view.root() == Some(notes)),
        "the page is still open"
    );
}

/// A click on a chat this window lists as its profile's follows it **in
/// this tab**, the way a document reference is followed — and the tab is
/// the same tab, so Back returns to the page.
#[test]
fn a_click_on_a_listed_chat_replaces_this_tab() {
    if !koru_features::ai() {
        return;
    }
    let (mut v, w) = on_document();
    let tabs = v.tabs().len();
    let tab_id = v.tabs().active_tab().id;
    let chat = a_chat();
    v.explore
        .chats_mut()
        .take_listing(Ok(koru_chats_ui::Listing {
            chats: vec![koru_chats_ui::Chat {
                id: koru_ai_ui::ChatId::from_uuid(chat.chat),
                title: "Plan".into(),
                private: false,
                current_model: koru_ai_core::ModelSelection {
                    provider: koru_ai_core::AiProviderId::parse("openai").unwrap(),
                    requested_model: "gpt-test".into(),
                    default_reasoning: koru_ai_core::ReasoningChoice::Default,
                },
                branched_from: None,
                agent_working_folder: None,
                agent_mode: None,
                agent_fast: None,
                pinned_at: None,
                profile_id: None,
                created_at: koru_ai_core::Timestamp::from_millis(1),
                updated_at: koru_ai_core::Timestamp::from_millis(2),
            }],
            agents: Vec::new(),
        }));

    assert!(v.open_reference(&w.ws, Reference::Chat(chat), OpenWhere::SameTab));

    assert!(chat_tab_on(&v, chat));
    assert_eq!(v.tabs().len(), tabs, "in this tab, not a new one");
    assert_eq!(v.tabs().active_tab().id, tab_id, "the same tab");
    assert_eq!(v.history.len(), 1, "the page it left is a stop for Back");
}

// ------------------------------------------------- keys, pointer, a reader

#[test]
fn arrows_move_the_highlight_and_enter_opens_there() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));
    let none = ModifiersState::empty();

    assert!(v.key(&mut w.ws, &Key::Named(NamedKey::ArrowLeft), None, none));
    assert_eq!(
        v.place_chooser.as_ref().map(|c| c.highlight),
        Some(Place::Left)
    );
    assert!(
        v.key(&mut w.ws, &Key::Character("x".into()), Some("x"), none),
        "a key it does not use is still swallowed"
    );
    assert!(v.place_chooser.is_some());

    assert!(v.key(&mut w.ws, &Key::Named(NamedKey::Enter), None, none));
    assert!(v.place_chooser.is_none());
    assert_eq!(split_shape(&v), Some((SplitAxis::Vertical, vec![0])));
    assert_eq!(v.document_root(), Some(brief));
}

#[test]
fn escape_closes_the_chooser_with_nothing_opened() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    let tabs = v.tabs().len();
    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));

    assert!(v.key(
        &mut w.ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));

    assert!(v.place_chooser.is_none());
    assert_eq!(v.tabs().len(), tabs);
    assert!(split_shape(&v).is_none());
    assert_eq!(v.blockers(), Blockers::None);
}

#[test]
fn a_press_outside_closes_it_and_a_press_on_a_target_opens_there() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    let tabs = v.tabs().len();
    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));
    let panel = v.place_chooser.as_ref().unwrap().panel(v.viewport);

    assert!(v.mouse_down(&mut w.ws, panel.x + panel.w + 40.0, panel.y, false));
    assert!(v.place_chooser.is_none(), "closed");
    assert_eq!(v.tabs().len(), tabs, "and nothing opened");

    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));
    let viewport = v.viewport;
    let up = v
        .place_chooser
        .as_ref()
        .unwrap()
        .cell_rect(viewport, Place::Up);
    assert!(v.mouse_down(&mut w.ws, up.x + up.w * 0.5, up.y + up.h * 0.5, false));
    assert_eq!(split_shape(&v), Some((SplitAxis::Horizontal, vec![0])));
}

#[test]
fn hovering_a_target_highlights_it() {
    let (mut v, mut w) = on_document();
    let brief = a_document(&mut w.ws, "Brief");
    alt_click(&mut v, Reference::Node(brief), (400.0, 300.0));
    let viewport = v.viewport;
    let down = v
        .place_chooser
        .as_ref()
        .unwrap()
        .cell_rect(viewport, Place::Down);

    assert_eq!(
        v.place_chooser_mouse_move(down.x + 2.0, down.y + 2.0),
        Some(true)
    );
    assert_eq!(
        v.place_chooser.as_ref().map(|c| c.highlight),
        Some(Place::Down)
    );
    assert_eq!(
        v.place_chooser_cursor(),
        Some(koru_ui::cursor::PRESS),
        "pressable, and never the hand (ADR-215 is the cursor table's)"
    );
}
