// SPDX-License-Identifier: MPL-2.0
use super::*;

fn opened(text: &str) -> OcrView {
    let mut view = OcrView::new(DocumentId::new(), None);
    let mut details = koru_workspace_host::reading_details::format(
        "sample.png".into(),
        None,
        koru_i18n::Lang::En,
    );
    details.extracted_text = text.into();
    details.snapshot = plain_snapshot(text).unwrap();
    details.target = Some((
        koru_store_core::ids::WorkspaceId::new(),
        koru_store_core::model::BlobHash::parse(&format!("b3:{}", "ab".repeat(32))).unwrap(),
    ));
    view.receive(Ok(details));
    view
}

#[test]
fn ocr_editor_preserves_literal_text_and_protects_a_draft() {
    let text = "# literal *text*\n\nMikaël 李\n";
    let mut view = opened(text);
    assert_eq!(
        view.document.as_ref().unwrap().plain_text().as_deref(),
        Some(text)
    );
    assert!(!view.request_close());
    view.activate(Action::Edit);
    assert!(view.editing());
    assert!(view.request_close());
    view.receive(Err("late fetch".into()));
    assert!(view.editing());
    view.activate(Action::Cancel);
    assert!(!view.request_close());
    assert_eq!(
        view.document.as_ref().unwrap().plain_text().as_deref(),
        Some(text)
    );
}

#[test]
fn ocr_save_failure_keeps_the_editable_draft() {
    let mut view = opened("keep this");
    view.activate(Action::Edit);
    let (tx, rx) = mpsc::channel();
    view.receiver = Some(rx);
    view.saving = true;
    tx.send(Err(())).unwrap();
    assert!(view.poll());
    assert!(view.editing());
    assert_eq!(view.notice, Some(Label::OcrSaveFailed));
    assert_eq!(
        view.document.as_ref().unwrap().plain_text().as_deref(),
        Some("keep this")
    );
}

#[test]
fn ocr_toolbar_uses_one_frame_for_controls_and_accessible_actions() {
    let mut view = opened("text");
    let tree = view.tree();
    let frame = tree.layout(
        &mut view.engine,
        &mut view.cache,
        Rect::new(0.0, 0.0, 700.0, 500.0),
        el::FrameCx::root(),
    );
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
    view.frame = Some(frame);
    let controls = view.controls();
    assert_eq!(controls.len(), 3);
    assert!(view.accessible_action("export", true));
    assert_eq!(
        View::take_pane_request(&mut view),
        Some(koru_workspace_host::PaneRequest::SaveReading(
            koru_workspace_host::reading_file::ReadingTarget::File(view.file),
        ))
    );
    assert!(View::take_pane_request(&mut view).is_none());
    let (_, _, rect) = controls
        .iter()
        .find(|(part, _, _)| *part == "edit")
        .unwrap();
    assert_eq!(
        view.frame
            .as_ref()
            .unwrap()
            .hit_at(rect.x + 1.0, rect.y + 1.0),
        Some(Action::Edit.key())
    );
    assert!(view.accessible_action("edit", true));
    assert!(view.editing());
    assert!(
        !view.accessible_action("reset", true),
        "a hidden action is refused"
    );
}

#[test]
fn ocr_refresh_keeps_draft_and_successful_save_exits_editing() {
    let mut view = opened("draft");
    view.activate(Action::Edit);
    let mut latest = opened("remote").details.take().unwrap();
    latest
        .corrections
        .versions
        .push(koru_store_wire::readings::ReadingCorrectionVersion {
            id: "remote".into(),
            text: Some("remote".into()),
        });
    view.install(latest, true);
    assert_eq!(
        view.document.as_ref().unwrap().plain_text().as_deref(),
        Some("draft")
    );
    assert!(view.editing());
    assert_eq!(
        view.details.as_ref().unwrap().corrections.versions[0].id,
        "remote"
    );
    let saved = opened("draft").details.take().unwrap();
    let (tx, rx) = mpsc::channel();
    view.receiver = Some(rx);
    view.saving = true;
    tx.send(Ok(saved)).unwrap();
    assert!(view.poll());
    assert!(!view.request_close());
}
