// SPDX-License-Identifier: MPL-2.0
use super::*;

fn surface() -> ChatSurface {
    let mut surface = ChatSurface::new();
    surface.set_viewport(Rect::new(0.0, 0.0, 360.0, 600.0));
    surface.status = Some(
        "The mini-app request was interrupted. Check the current app before using Retry.".into(),
    );
    surface.composer.set_text("Keep this draft");
    surface.focus = Focus::Composer;
    surface
}

#[test]
fn notice_dismiss_click_hides_repeated_status_without_changing_draft_or_request() {
    let mut surface = surface();
    let attempt = InferenceAttemptId::new();
    surface.attempt = Some(attempt);
    let composer = surface.composer_rect();
    surface.draw_footer(&mut Scene::new(), Affine::IDENTITY, composer);
    let rect = surface.footer_dismiss_rect().unwrap();
    assert!(rect.y + rect.h < composer.y);
    assert!(rect.x + rect.w <= surface.content_rect().x + surface.content_rect().w);
    let status = surface.status.clone();
    assert!(surface.mouse_down(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0));
    assert!(surface.footer_line().is_none());
    assert!(surface.footer_dismiss_rect().is_none());
    assert_eq!(surface.status, status);
    assert_eq!(surface.attempt, Some(attempt));
    assert_eq!(surface.composer.text(), "Keep this draft");
    surface.status = status;
    assert!(
        surface.footer_line().is_none(),
        "a state refresh must not restore the dismissed notice"
    );
    surface.status = Some("A different problem".into());
    assert_eq!(surface.footer_line(), Some("A different problem"));
}

#[test]
fn notice_dismiss_is_keyboard_reachable_and_preserves_stop_hint() {
    let mut surface = surface();
    assert!(surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT));
    assert_eq!(surface.focus, Focus::DismissNotice);
    assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
    assert_eq!(surface.focus, Focus::Composer);
    assert!(surface.footer_line().is_none());
    surface.attempt = Some(InferenceAttemptId::new());
    surface.stop_armed_at = Some(Instant::now());
    assert!(surface.footer_line().is_some());
    assert!(
        !surface.dismiss_footer(),
        "the temporary stop hint is not a notice"
    );
    assert_eq!(surface.composer.text(), "Keep this draft");
}

#[test]
fn notice_dismiss_does_not_overlap_the_stored_document_link() {
    let mut surface = surface();
    let line = "Stored a document with a long name in Koru.".to_owned();
    surface.status = Some(line.clone());
    surface.stored_document = Some((line, DocumentId::new()));
    let composer = surface.composer_rect();
    surface.draw_footer(&mut Scene::new(), Affine::IDENTITY, composer);
    let open = surface.stored_open_rect().unwrap();
    let close = surface.footer_dismiss_rect().unwrap();
    assert!(open.x + open.w <= close.x);
    assert!(surface.dismiss_footer());
    assert!(surface.stored_open_rect().is_none());
}

#[test]
fn notice_can_reappear_for_a_new_retry() {
    let mut surface = surface();
    let original = surface.status.clone();
    assert!(surface.dismiss_footer());
    let (reader, _commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.queue_command(Command::Retry(
        ChatId::new(),
        koru_ai_wire::RetryTurnRequest {
            operation_id: uuid::Uuid::new_v4(),
            message_id: MessageId::new(),
            surface: SURFACE,
            explicit_refs: vec![],
            reasoning_override: None,
            web_search: false,
            replace_pinned_documents: false,
            destination_workspace: None,
            mini_app_host: None,
            mini_app_refinement: None,
        },
    ));
    surface.status = original;
    assert!(surface.footer_line().is_some());
}
