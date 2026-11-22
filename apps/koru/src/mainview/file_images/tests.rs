// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::{LiftedViewer, MenuAction, MenuSubject};
use koru_core::NodeId;
use koru_workspace_host::View;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn picture() -> Arc<ViewerImage> {
    Arc::new(ViewerImage::from_rgba(image::RgbaImage::new(16, 8), (16, 8)).unwrap())
}

#[test]
fn workspace_image_viewer_search_selects_opens_and_closes_without_losing_selection() {
    let (mut view, mut workspace) = crate::mainview::harness::container();
    let file = DocumentId::new();
    let node = NodeId::from_uuid(file.as_uuid());
    View::set_entries(
        &mut view,
        vec![koru_core::explore::ExploreEntry {
            id: file,
            parent: None,
            ordinal: 0,
            title: "Picture.png".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("image/png".into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );
    while View::take_pane_request(&mut view).is_some() {}
    assert!(View::open_file_search_target(&mut view, node));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::OpenFile(file))
    );
    // The shipping host sees ManagerView, whose defaults must not swallow
    // the asynchronous image delivery before it reaches this container.
    let mut hosted = crate::manager_view::ManagerView::Container(view);
    assert!(View::begin_file_image_open(&mut hosted, file));
    assert!(View::show_file_image(
        &mut hosted,
        file,
        "Picture.png".into(),
        picture()
    ));
    let crate::manager_view::ManagerView::Container(mut view) = hosted else {
        panic!("container")
    };
    view.refresh_explore(&workspace.ws, true);
    assert_eq!(
        view.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Files
    );
    assert_eq!(view.explore.selection_nodes(), vec![node]);
    assert_eq!(view.lifted_viewer(), Some(LiftedViewer::FileImage));
    assert!(view.lifted_viewer_semantics(koru_i18n::Lang::En).is_some());
    assert!(view.lifted_viewer_act(Action::ZoomIn));
    assert!(view.lifted_viewer_act(Action::SaveAs));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::SaveFile(file))
    );
    assert!(view.lifted_viewer_act(Action::OpenExternally));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::OpenFileExternally(file))
    );
    assert!(View::key(
        &mut view,
        &mut workspace.ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(view.lifted_viewer().is_none());
    assert_eq!(view.explore.selection_nodes(), vec![node]);
}

#[test]
fn workspace_image_viewer_ignores_a_superseded_or_other_workspace_delivery() {
    let (mut view, _) = crate::mainview::harness::container();
    let first = DocumentId::new();
    let second = DocumentId::new();
    view.begin_file_image(first);
    view.begin_file_image(second);
    assert!(!view.receive_file_image(first, "first".into(), picture()));
    view.set_pane_context(
        Vec::new(),
        koru_core::ProfileId::new(),
        Vec::new(),
        Some(WorkspaceId::new()),
    );
    assert!(!view.receive_file_image(second, "second".into(), picture()));
    assert!(view.file_images.viewer.is_none());
    // A Files row's ordinary open still reaches the same host operation.
    while View::take_pane_request(&mut view).is_some() {}
    view.run_menu_action(
        MenuSubject::Files(vec![NodeId::from_uuid(first.as_uuid())]),
        MenuAction::OpenFileInApp,
    );
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::OpenFile(first))
    );
}
