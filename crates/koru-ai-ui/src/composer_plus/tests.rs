// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_recent_document_becomes_a_sendable_pill_at_the_caret() {
    let mut surface = ChatSurface::new();
    let document = DocumentId::new();
    surface.set_recent_documents(vec![(document, "Plan".into())]);
    surface.open_plus_menu();
    assert!(matches!(
        surface.plus_menu.as_ref().unwrap().items()[0],
        PlusItem::Recent(_)
    ));
    assert!(surface.choose_plus_item(0));
    assert!(matches!(
        surface.plus_menu.as_ref().unwrap().items()[1],
        PlusItem::Document(_, _)
    ));
    assert!(surface.choose_plus_item(1));
    assert!(surface.plus_menu.is_none());
    assert_eq!(
        surface.composer.message().documents,
        vec![(document, "Plan".into())]
    );
    assert!(surface.composer.message().markdown.contains("koru://doc/"));
}

#[test]
fn an_open_recent_menu_keeps_its_snapshot_when_the_window_changes() {
    let mut surface = ChatSurface::new();
    let first = DocumentId::new();
    let second = DocumentId::new();
    surface.set_recent_documents(vec![(first, "First".into())]);
    surface.open_recent_plus_menu();
    surface.set_recent_documents(vec![(second, "Second".into())]);
    assert!(
        matches!(surface.plus_menu.as_ref().unwrap().items()[1], PlusItem::Document(id, _) if id == first)
    );
    surface.open_recent_plus_menu();
    assert!(
        matches!(surface.plus_menu.as_ref().unwrap().items()[1], PlusItem::Document(id, _) if id == second)
    );
}

#[test]
fn koru_context_door_opens_the_existing_entity_picker() {
    let mut surface = ChatSurface::new();
    surface.open_plus_menu();
    assert!(surface.choose_plus_item(1));
    assert!(surface.plus_menu.is_none());
    assert!(surface.composer.picker_open());
}

#[test]
fn plus_menu_shows_trigger_hints_on_the_matching_rows() {
    let mut surface = ChatSurface::new();
    surface.open_plus_menu();
    let items = surface.plus_menu.as_ref().unwrap().items();
    assert_eq!(
        items.len(),
        4,
        "file and folder actions sit beside the trigger doors"
    );
    assert_eq!(items[1].shortcut().as_deref(), Some("@"));
    assert!(matches!(items[2], PlusItem::UploadFiles(_)));
    assert!(matches!(items[3], PlusItem::AddFolder(_)));
    assert!(!items[3].has_submenu());
    assert_eq!(
        PlusItem::Commands("Commands").shortcut().as_deref(),
        Some("/")
    );
    assert_eq!(PlusItem::Skills("Skills").shortcut().as_deref(), Some("$"));
}
