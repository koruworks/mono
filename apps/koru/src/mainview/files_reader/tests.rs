// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_wire::reader::InstalledReader;
use koru_workspace_host::View;

#[test]
fn files_ocr_menu_follows_enablement_and_routes_to_settings() {
    let (mut view, _) = crate::mainview::harness::container();
    view.explore.show_files();
    view.open_library_menu((100.0, 100.0));
    assert!(
        view.menu
            .as_ref()
            .unwrap()
            .items()
            .contains(&MenuAction::OcrSettings(None))
    );
    assert!(view.set_files_reader_state(ReaderState::default()));
    let menu = view.menu.as_ref().unwrap();
    assert!(menu.items().contains(&MenuAction::OcrSettings(Some(false))));
    assert!(view.enabled_items(menu).iter().all(|enabled| *enabled));

    let mut state = ReaderState {
        installed: Some(InstalledReader {
            model: "test".into(),
            version: "1".into(),
            download_bytes: 1,
            licence: "test".into(),
            attribution: "test".into(),
        }),
        recognizing: false,
        ..ReaderState::default()
    };
    assert!(view.set_files_reader_state(state.clone()));
    assert!(
        view.menu
            .as_ref()
            .unwrap()
            .items()
            .contains(&MenuAction::OcrSettings(Some(true)))
    );
    state.turned_off = true;
    assert!(view.set_files_reader_state(state.clone()));
    assert!(
        view.menu
            .as_ref()
            .unwrap()
            .items()
            .contains(&MenuAction::OcrSettings(Some(false)))
    );
    assert!(
        !view
            .files_reader
            .menu_items(false)
            .iter()
            .any(|action| matches!(action, MenuAction::OcrSettings(_)))
    );

    // An installed but unusable reader is off, even if its preference is on.
    state.turned_off = false;
    state.unavailable = Some(koru_store_wire::reader::ReaderUnavailable::WorkerMissing);
    view.set_files_reader_state(state);
    let items = view.menu.as_ref().unwrap().items();
    assert!(items.contains(&MenuAction::OcrSettings(Some(false))));
    assert!(!items.contains(&MenuAction::RescanAllFiles));

    while View::take_pane_request(&mut view).is_some() {}
    view.run_menu_action(MenuSubject::NewTab, MenuAction::OcrSettings(Some(false)));
    let TabView::Settings(settings) = &view.tabs().active_tab().view else {
        panic!("settings");
    };
    assert_eq!(settings.page(), koru_settings_ui::PageId::Editor);
    assert!(
        View::take_pane_request(&mut view).is_none(),
        "navigation never installs"
    );
}

#[test]
fn files_ocr_menu_reply_does_not_reopen_a_dismissed_menu() {
    let (mut view, _) = crate::mainview::harness::container();
    view.explore.show_files();
    view.open_library_menu((100.0, 100.0));
    view.menu = None;
    assert!(!view.set_files_reader_state(ReaderState::default()));
    assert!(view.menu.is_none());
    view.open_library_menu((100.0, 100.0));
    assert!(
        view.menu
            .as_ref()
            .unwrap()
            .items()
            .contains(&MenuAction::OcrSettings(Some(false)))
    );
}

#[test]
fn files_ocr_rescan_menu_captures_workspace_and_requires_enabled_reader() {
    let (mut view, _) = crate::mainview::harness::container();
    let workspace = view.current_workspace().unwrap();
    view.explore.show_files();
    view.open_library_menu((100.0, 100.0));
    view.set_files_reader_state(ReaderState {
        installed: Some(InstalledReader {
            model: "test".into(),
            version: "1".into(),
            download_bytes: 1,
            licence: "test".into(),
            attribution: "test".into(),
        }),
        ..ReaderState::default()
    });
    let menu = view.menu.as_ref().unwrap();
    assert!(menu.items().contains(&MenuAction::RescanAllFiles));
    assert!(view.enabled_items(menu).iter().all(|enabled| *enabled));
    assert!(
        !view
            .files_reader
            .menu_items(false)
            .contains(&MenuAction::RescanAllFiles)
    );
    while View::take_pane_request(&mut view).is_some() {}
    assert!(view.run_menu_action(MenuSubject::NewTab, MenuAction::RescanAllFiles));
    let request = View::take_pane_request(&mut view);
    view.set_pane_context(
        Vec::new(),
        koru_core::ProfileId::new(),
        Vec::new(),
        Some(koru_core::WorkspaceId::new()),
    );
    assert_eq!(
        request,
        Some(koru_workspace_host::PaneRequest::RescanWorkspaceFiles(
            workspace
        ))
    );
    view.set_files_reader_state(ReaderState::default());
    assert!(
        !view
            .files_reader
            .menu_items(true)
            .contains(&MenuAction::RescanAllFiles)
    );
}
