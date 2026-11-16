// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::harness::container;
use koru_workspace_host::View;

#[test]
fn copied_files_wait_for_destination_and_cancel_does_nothing() {
    let (mut view, _) = container();
    while View::take_pane_request(&mut view).is_some() {}
    let paths = vec![PathBuf::from("picture.png"), PathBuf::from("scan.pdf")];
    assert!(view.offer_copied_files(paths.clone()));
    assert!(View::take_pane_request(&mut view).is_none());
    view.answer_file_paste(ChoiceOutcome::Dismiss);
    assert!(View::take_pane_request(&mut view).is_none());

    view.offer_copied_files(paths.clone());
    let index = usize::from(view.file_paste.as_ref().unwrap().document.is_some());
    view.answer_file_paste(ChoiceOutcome::Chosen(index));
    assert!(matches!(View::take_pane_request(&mut view),
        Some(PaneRequest::ImportFiles(ImportSourceSelection::Files(actual))) if actual == paths));
    assert!(view.file_paste.is_none());
}

#[test]
fn copied_files_attach_to_the_document_captured_when_asked() {
    let (mut view, _) = crate::mainview::harness::on_document();
    while View::take_pane_request(&mut view).is_some() {}
    let document = koru_core::DocumentId::new();
    let paths = vec![PathBuf::from("picture.png")];
    view.tabs_mut().active_mut().document = Some(document);
    view.offer_copied_files(paths.clone());
    assert_eq!(view.file_paste.as_ref().unwrap().dialog.labels().len(), 3);
    view.active_document = Some(koru_core::DocumentId::new());
    view.answer_file_paste(ChoiceOutcome::Chosen(0));
    assert!(matches!(View::take_pane_request(&mut view),
        Some(PaneRequest::ImportFileAttachments { paths: actual, document: target })
        if actual == paths && target == document));
}

#[test]
fn copied_files_create_a_draft_document_only_after_attach_is_chosen() {
    let (mut view, mut workspace) = container();
    view.new_tab();
    while View::take_pane_request(&mut view).is_some() {}
    view.offer_copied_files(vec![PathBuf::from("picture.png")]);
    assert_eq!(view.file_paste.as_ref().unwrap().dialog.labels().len(), 3);
    view.answer_file_paste(ChoiceOutcome::Dismiss);
    assert!(View::take_pane_request(&mut view).is_none());
    assert!(view.tabs().active_tab().draft.is_some());
    view.offer_copied_files(vec![PathBuf::from("picture.png")]);
    view.answer_file_paste(ChoiceOutcome::Chosen(0));
    assert!(matches!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::CreateDocument(_))
    ));
    assert!(view.pending_file_paste_draft.is_some());
    let document = koru_core::DocumentId::new();
    let root = crate::mainview::harness::a_document(&mut workspace.ws, "");
    view.active_document = Some(document);
    view.pending_draft_text = Some((root, String::new(), vec![String::new()], None));
    view.flush_promoted_draft(&mut workspace.ws);
    assert!(view.tabs().active_tab().draft.is_none());
    assert!(matches!(View::take_pane_request(&mut view),
        Some(PaneRequest::ImportFileAttachments { document: target, .. }) if target == document));
}
