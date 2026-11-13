// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn mini_app_refinement_moves_open_search_popup_without_replacing_focus_or_query() {
    let (mut app, mut doc) = super::super::tests::app();
    app.chrome.search_focus = true;
    app.chrome.header_focus = Some("search");
    let owner = View::ime_target(&app).unwrap().id;
    View::ime(
        &mut app,
        &mut doc,
        owner,
        &winit::event::Ime::Preedit("に".into(), Some((3, 3))),
    );
    app.chrome
        .show_search_choices(vec![Choice::Document(DocumentId::new(), "Result".into())]);
    app.chrome.menu.as_mut().unwrap().hover = Some(0);
    let old_panel = app.chrome.menu.as_ref().unwrap().panel(app.viewport);
    let instance = app.chrome.menu_instance;
    let revision = app.refinement_target();
    let mut root = app
        .effective_definition()
        .envelope
        .surface
        .components
        .remove(0);
    root.arrangement = Some("document-first".into());
    app.refine(
        revision,
        &koru_ai_core::mini_apps::UpdateEnvelope {
            version: "1.0".into(),
            update: koru_ai_core::mini_apps::ComponentUpdate {
                surface_id: revision.surface_name(),
                components: vec![root],
            },
        },
    )
    .unwrap();
    let menu = app.chrome.menu.as_ref().unwrap();
    assert!(menu.panel(app.viewport).x < old_panel.x);
    assert_eq!(menu.hover, Some(0));
    assert_eq!(app.chrome.menu_instance, instance);
    assert_eq!(View::ime_target(&app).unwrap().id, owner);
    assert_eq!(app.chrome.input.raw_text(), "");
    assert!(View::ime(
        &mut app,
        &mut doc,
        owner,
        &winit::event::Ime::Commit("日本".into())
    ));
    assert_eq!(app.chrome.input.raw_text(), "日本");
}

fn result(seq: u64) -> koru_search_ui::EntitySearchUpdate {
    koru_search_ui::EntitySearchUpdate {
        seq,
        documents: vec![],
        workspaces: vec![],
        unavailable: false,
    }
}

#[test]
fn every_selector_and_result_remains_reachable_in_short_windows() {
    let mut chrome = Chrome::new();
    chrome.layout((500.0, 180.0), false, Region::Tasks);
    for selector in [true, false] {
        chrome.selector_menu = selector;
        chrome.menu_choices = (0..300)
            .map(|i| Choice::Document(DocumentId::new(), format!("Choice {i}")))
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut identities = std::collections::HashSet::new();
        chrome.show_page(0);
        loop {
            let menu = chrome.menu.as_ref().unwrap();
            assert!(menu.panel(chrome.viewport).y + menu.height() <= chrome.viewport.1);
            let mut next = None;
            for (i, choice) in menu.items().iter().enumerate() {
                assert!(identities.insert(chrome.menu_instance + i as u64 + 1));
                match choice {
                    Choice::Document(id, _) => {
                        seen.insert(*id);
                    }
                    Choice::Page(page, _) if *page > chrome.list_page => next = Some(*page),
                    _ => {}
                }
            }
            let Some(next) = next else {
                break;
            };
            chrome.show_page(next);
        }
        assert_eq!(seen.len(), 300);
    }
}

#[test]
fn dismissed_or_stale_search_cannot_reopen_results() {
    let workspace = WorkspaceId::new();
    let mut chrome = Chrome::new();
    chrome.search_focus = true;
    chrome.query = "query".into();
    chrome.seq = 4;
    assert!(!chrome.accept_result(workspace, result(3)));
    chrome.dismissed = true;
    assert!(!chrome.accept_result(workspace, result(4)));
    assert!(chrome.menu.is_none());
    chrome.dismissed = false;
    assert!(chrome.accept_result(workspace, result(4)));
    assert!(matches!(
        chrome.menu.as_ref().unwrap().items(),
        [Choice::Status(_)]
    ));
}

#[test]
fn header_tab_returns_to_child_and_status_is_not_an_action() {
    let (mut app, _) = super::super::tests::app();
    app.focus_header();
    assert_eq!(app.chrome.header_focus, Some("selector"));
    assert!(app.chrome_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty()));
    assert!(app.chrome.search_focus);
    app.chrome
        .show_search_choices(vec![Choice::Status("Waiting".into())]);
    app.chrome_key(
        &Key::Named(NamedKey::ArrowDown),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(app.chrome.menu.as_ref().unwrap().hover, None);
    let window = WindowSlot::from_raw(1);
    let view = ViewSlot::from_raw(1);
    let rows = app.chrome_semantics(window, view, Rect::new(0.0, 0.0, 1000.0, 600.0));
    let status = &rows.last().unwrap().children[0];
    assert_eq!(status.role, Role::Label);
    assert!(!app.chrome_accessibility_action(status.occurrence, Action::Click));
    app.chrome_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
    assert_eq!(app.chrome.header_focus, Some("save"));
    app.chrome_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
    assert_eq!(app.chrome.header_focus, Some("pin"));
    assert!(
        app.chrome_semantics(window, view, Rect::new(0.0, 0.0, 1000.0, 600.0))
            .iter()
            .any(|n| n.occurrence.part == ControlPart::new("pin") && n.role == Role::Button)
    );
    app.chrome_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty());
    assert!(!app.header_focused());
    assert!(app.document_focused());
}
