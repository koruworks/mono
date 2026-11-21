// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed_surface::tests::surface;

fn item() -> wire::ReviewItem {
    wire::ReviewItem {
        occurrence: Uuid::new_v4(),
        routine: Uuid::new_v4(),
        name: "Pricing".into(),
        card: Uuid::new_v4(),
        reply: Uuid::new_v4(),
        published_at_ms: 1,
        review: Some("pending".into()),
        preview: "A price changed".into(),
        truncated: false,
    }
}

#[test]
fn hiding_preserves_uncertain_skip_and_reopening_uses_its_same_identity() {
    let (mut view, commands, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let id = Uuid::new_v4();
    assert!(view.request_review(wire::Command::SkipReview {
        id,
        bot,
        occurrence: Uuid::new_v4()
    }));
    let _ = commands.try_recv().unwrap();
    view.deactivate();
    assert!(view.review.failed);
    assert!(commands.try_recv().is_err());
    let (reader, commands, _updates) = crate::reader::Reader::test_pair();
    view.reader = Some(reader);
    view.resume_reviews();
    assert!(
        matches!(commands.try_recv().unwrap(),Command::RoutineReview { command:wire::Command::SkipReview { id: retry,.. },.. } if id == retry)
    );
    assert!(!view.review.failed);
}

#[test]
fn review_panel_clips_long_results_and_keeps_primitives_nested() {
    let (mut view, _, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    view.review.open = true;
    view.review.selected = Some(wire::ReviewDetail {
        bot: bot.as_uuid(),
        item: item(),
        text: "Retained result.\n".repeat(100),
        offset: 0,
        next: None,
    });
    view.viewport = Rect::new(0.0, 0.0, 420.0, 700.0);
    view.composer_hit_rect = Rect::new(12.0, 600.0, 396.0, 80.0);
    let capture = koru_draw::inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 420.0, 700.0),
    );
    view.draw_review(&mut Scene::new(), Affine::IDENTITY);
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let panel = records
        .iter()
        .position(|r| r.name == "FloatingPanel")
        .unwrap();
    for (i, r) in records.iter().enumerate() {
        if i == panel {
            continue;
        }
        let mut parent = r.parent;
        while let Some(index) = parent {
            if index == panel {
                break;
            }
            parent = records[index].parent;
        }
        assert_eq!(parent, Some(panel), "{} must nest in its panel", r.name);
    }
    assert!(view.review.scroll_max > 0.0);
    let body = view.review.body.unwrap();
    let footer = view.review.footer.unwrap();
    assert!(body.y + body.h <= footer.y);
    assert!(footer.y + footer.h < view.composer_hit_rect.y);
    view.focus = Focus::RoutineReview;
    assert_eq!(
        view.review_key(&Key::Named(NamedKey::PageDown), ModifiersState::empty()),
        Some(true)
    );
    assert!(view.review.scroll > 0.0);
    assert_eq!(
        view.review_key(&Key::Named(NamedKey::Escape), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(view.focus, Focus::Composer);
    assert!(view.review.selected.is_none());
}

#[test]
fn refresh_and_stale_responses_never_replace_the_selected_context() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let first = item();
    assert!(view.open_routine_result(bot, first.occurrence));
    let Command::RoutineReview { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    let detail = wire::ReviewDetail {
        bot: bot.as_uuid(),
        item: first.clone(),
        text: first.preview.clone(),
        offset: 0,
        next: None,
    };
    assert!(!view.review_response(
        ChatId::new(),
        request,
        Ok(wire::Response::Review {
            detail: detail.clone()
        })
    ));
    assert!(!view.review_response(
        thread,
        Uuid::new_v4(),
        Ok(wire::Response::Review {
            detail: detail.clone()
        })
    ));
    assert!(view.review_response(thread, request, Ok(wire::Response::Review { detail })));
    view.review.stamp = None;
    view.refresh_reviews();
    let Command::RoutineReview { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::Reviews {
            page: wire::ReviewPage {
                bot: bot.as_uuid(),
                count: 2,
                items: vec![item(), first.clone()],
                next: None,
            }
        })
    ));
    assert_eq!(
        view.review.selected.as_ref().unwrap().item.occurrence,
        first.occurrence
    );
    assert!(view.review_action(2));
    assert!(view.review.selected.is_none());
    assert_eq!(view.review.count, 2);
    assert!(
        commands.try_recv().is_err(),
        "opening and closing never consumes a result"
    );
}

#[test]
fn failed_skip_retries_the_same_identity_and_keeps_the_draft() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    view.review.open = true;
    view.review.selected = Some(wire::ReviewDetail {
        bot: bot.as_uuid(),
        item: item(),
        text: "result".into(),
        offset: 0,
        next: None,
    });
    view.composer.set_text("use this context");
    assert!(view.review_action(3));
    let Command::RoutineReview {
        request,
        command: wire::Command::SkipReview { id, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    let error = koru_ai_wire::Failure {
        code: "unavailable".into(),
        message: "Retry".into(),
        retryable: true,
        details: Default::default(),
    };
    assert!(view.review_response(thread, request, Err(error)));
    assert!(view.review.selected.is_some());
    let retry = view.review_labels().len() - 1;
    assert!(view.review_action(retry));
    let Command::RoutineReview {
        command: wire::Command::SkipReview { id: retried, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(id, retried);
    assert_eq!(view.composer.text(), "use this context");
}
