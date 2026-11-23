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

fn attach(view: &mut ChatSurface) -> wire::ReviewItem {
    let (bot, _, _) = view.review_subject().unwrap();
    let result = item();
    view.review.pending = None;
    view.restore_review_context(Some(Attachment {
        detail: wire::ReviewDetail {
            bot: bot.as_uuid(),
            item: result.clone(),
            text: result.preview.clone(),
            offset: 0,
            next: None,
        },
        token: Uuid::new_v4(),
    }));
    result
}

fn idle(view: &mut ChatSurface) {
    view.managed.as_mut().unwrap().snapshot.turns.clear();
    view.attempt = None;
    view.waiting = false;
}

#[test]
fn context_send_keeps_exact_anchor_through_retry_and_refusal() {
    use koru_ai_wire::bot_threads::Command as C;
    let (mut view, commands, _) = surface();
    idle(&mut view);
    let result = attach(&mut view);
    view.composer.set_text("Send this quote");
    assert!(view.send_composer());
    let (thread, command) = commands
        .try_iter()
        .find_map(|c| match c {
            Command::Engine {
                thread,
                command: c @ C::Send { .. },
            } => Some((thread, c)),
            _ => None,
        })
        .expect("native contextual send");
    assert!(
        matches!(&command, C::Send { in_reply_to: Some(card), body, .. }
        if *card == result.card && body == "Send this quote")
    );
    let failure = |retryable| koru_ai_wire::Failure {
        code: "test_refusal".into(),
        message: "Refused".into(),
        retryable,
        details: Default::default(),
    };
    assert!(view.managed_failed(thread, command.clone(), failure(true)));
    assert_eq!(
        serde_json::to_value(
            view.managed
                .as_ref()
                .unwrap()
                .pending_send
                .as_ref()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&command).unwrap()
    );
    assert_eq!(
        view.attached_review().unwrap().detail.item.card,
        result.card
    );
    assert!(view.managed_failed(thread, command, failure(false)));
    assert_eq!(view.composer.text(), "Send this quote");
    assert_eq!(
        view.attached_review().unwrap().detail.item.card,
        result.card
    );
}

#[test]
fn accepted_reply_clears_only_its_selection_and_never_a_newer_draft_or_card() {
    use koru_ai_wire::bot_threads::{Command as C, Response as R};
    let (mut view, commands, _) = surface();
    idle(&mut view);
    let result = attach(&mut view);
    view.composer.set_text("Reply");
    view.send_composer();
    let (thread, id) = commands
        .try_iter()
        .find_map(|c| match c {
            Command::Engine {
                thread,
                command: C::Send { id, .. },
            } => Some((thread, id)),
            _ => None,
        })
        .unwrap();
    let original = view.attached_review().unwrap();
    // Even explicitly reopening the same result creates a new attachment.
    view.restore_review_context(Some(original.clone()));
    view.composer.set_text("Another action");
    assert!(view.managed_reply(
        thread,
        R::Accepted {
            request: id,
            message: Uuid::new_v4(),
            thread: thread.as_uuid(),
            sequence: 1,
            replayed: false
        }
    ));
    assert_eq!(view.composer.text(), "Another action");
    assert_eq!(
        view.attached_review().unwrap().detail.item.card,
        result.card
    );
    let latest = view.attached_review().unwrap();
    view.accepted_review_context(&latest);
    assert!(view.attached_review().is_none());
    assert_eq!(view.composer.text(), "Another action");
}

#[test]
fn queued_context_does_not_inherit_later_selection_and_failed_send_keeps_both_drafts() {
    use koru_ai_wire::bot_threads::Command as C;
    let (mut view, commands, _) = surface();
    let first = attach(&mut view);
    view.composer.set_text("First action");
    assert!(view.queue_composer());
    assert!(view.attached_review().is_none());
    assert!(!view.shown_queue().unwrap().items[0].text_only());
    let second = attach(&mut view);
    view.composer.set_text("Second action");
    idle(&mut view);
    assert!(view.send_queue_head());
    let (thread, command) = commands
        .try_iter()
        .find_map(|c| match c {
            Command::Engine {
                thread,
                command: c @ C::Send { .. },
            } => Some((thread, c)),
            _ => None,
        })
        .unwrap();
    assert!(matches!(&command, C::Send { in_reply_to: Some(card), .. } if *card == first.card));
    view.managed_failed(
        thread,
        command,
        koru_ai_wire::Failure {
            code: "refused".into(),
            message: "Refused".into(),
            retryable: false,
            details: Default::default(),
        },
    );
    assert_eq!(view.composer.text(), "Second action");
    assert_eq!(
        view.attached_review().unwrap().detail.item.card,
        second.card
    );
    let queued = &view.shown_queue().unwrap().items[0];
    assert_eq!(queued.markdown, "First action");
    assert_eq!(
        queued.routine_context.as_ref().unwrap().detail.item.card,
        first.card
    );
}

#[test]
fn loading_context_blocks_send_and_late_chunk_cannot_replace_another_attachment() {
    let (mut view, commands, _) = surface();
    idle(&mut view);
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    view.open_routine_result(bot, Uuid::new_v4());
    let Command::RoutineReview {
        request,
        command: wire::Command::Review { occurrence, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    view.composer.set_text("Keep this");
    view.send_composer();
    assert_eq!(view.composer.text(), "Keep this");
    assert!(commands.try_recv().is_err());
    let pending = view.review.pending.clone();
    let next = attach(&mut view);
    view.review.pending = pending;
    let mut old = item();
    old.occurrence = occurrence;
    view.review_response(
        thread,
        request,
        Ok(wire::Response::Review {
            detail: wire::ReviewDetail {
                bot: bot.as_uuid(),
                item: old,
                text: "old".into(),
                offset: 0,
                next: None,
            },
        }),
    );
    assert_eq!(view.attached_review().unwrap().detail.item.card, next.card);
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
        view.review_key(
            &Key::Named(NamedKey::PageDown),
            None,
            ModifiersState::empty()
        ),
        Some(true)
    );
    assert!(view.review.scroll > 0.0);
    assert_eq!(
        view.review_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()),
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
    let _ = view.take_activity();
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
    assert!(
        view.take_activity(),
        "a changed count invalidates the roster summary"
    );
    assert_eq!(
        view.review.selected.as_ref().unwrap().item.occurrence,
        first.occurrence
    );
    assert!(view.review_action(Part::Close));
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
    assert!(view.review_action(Part::Skip));
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
    assert!(view.review_action(Part::Retry));
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

fn detail(bot: BotId, item: &wire::ReviewItem) -> wire::ReviewDetail {
    wire::ReviewDetail {
        bot: bot.as_uuid(),
        item: item.clone(),
        text: item.preview.clone(),
        offset: 0,
        next: None,
    }
}

#[test]
fn header_counts_the_queue_and_skip_moves_on_to_the_next_result() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let (first, second) = (item(), item());
    view.review.items = vec![first.clone(), second.clone()];
    view.review.count = 2;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &first));
    assert_eq!(view.review_position_text().as_deref(), Some("1 of 2"));
    assert_eq!(
        view.review_parts(),
        vec![Part::Next, Part::Close, Part::Skip],
        "nothing before the first result; Skip ends the footer"
    );

    assert!(view.review_action(Part::Skip));
    let Command::RoutineReview {
        request,
        command: wire::Command::SkipReview { id, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    let mut skipped = first.clone();
    skipped.review = Some("skipped".into());
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::SkippedReview {
            request: id,
            item: skipped,
            replayed: false,
        })
    ));
    assert!(view.review.open, "the card stays to show what is next");
    let _ = view.take_activity();
    let Command::RoutineReview {
        request,
        command: wire::Command::Reviews { before: None, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::Reviews {
            page: wire::ReviewPage {
                bot: bot.as_uuid(),
                count: 1,
                items: vec![second.clone()],
                next: None,
            }
        })
    ));
    let Command::RoutineReview {
        command: wire::Command::Review { occurrence, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(
        occurrence, second.occurrence,
        "the next result took its place"
    );
}

#[test]
fn a_card_closed_during_skip_stays_closed() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let first = item();
    view.review.items = vec![first.clone(), item()];
    view.review.count = 2;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &first));
    assert!(view.review_action(Part::Skip));
    let Command::RoutineReview {
        request,
        command: wire::Command::SkipReview { id, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert!(view.review_action(Part::Close));
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::SkippedReview {
            request: id,
            item: first,
            replayed: false,
        })
    ));
    assert!(!view.review.open);
}

#[test]
fn an_attached_card_names_the_bot_in_the_composer_hint() {
    let (mut view, _, _) = surface();
    assert_eq!(view.review_placeholder(), None);
    attach(&mut view);
    let name = view
        .managed
        .as_ref()
        .unwrap()
        .snapshot
        .conversation
        .label
        .bot_name
        .clone();
    let hint = view.review_placeholder().unwrap();
    assert!(
        hint.starts_with(&format!(
            "Ask {name} about this, or tell it what to do instead"
        )),
        "{hint}"
    );
    // The card's keys follow, from the registry, where hints are shown.
    if koru_keys::hint::shown(koru_keys::Section::Guide) {
        assert!(hint.ends_with("to move · Alt+S to skip"), "{hint}");
    }
}

#[test]
fn escape_lets_a_selection_go_then_closes_the_card_from_the_composer() {
    let (mut view, _, _) = surface();
    attach(&mut view);
    view.focus = Focus::Composer;
    view.composer.set_text("a draft");
    view.review.selection = Some((0, 3));
    let escape = Key::Named(NamedKey::Escape);
    assert_eq!(
        view.review_key(&escape, None, ModifiersState::empty()),
        Some(true)
    );
    assert!(view.review.open, "the first Escape takes the selection");
    assert!(view.review.selection.is_none());
    assert_eq!(
        view.review_key(&escape, None, ModifiersState::empty()),
        Some(true)
    );
    assert!(!view.review.open);
    assert_eq!(view.composer.text(), "a draft", "the draft stays");
}

#[test]
fn a_selection_in_the_result_offers_add_to_chat_over_the_card() {
    let (mut view, _, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    view.review.open = true;
    view.review.selected = Some(wire::ReviewDetail {
        text: "Rates held steady this morning.".into(),
        ..detail(bot, &item())
    });
    view.viewport = Rect::new(0.0, 0.0, 420.0, 700.0);
    view.composer_hit_rect = Rect::new(12.0, 600.0, 396.0, 80.0);
    view.draw_review(&mut Scene::new(), Affine::IDENTITY);
    let words = view.review.words_at.unwrap();
    let (y, x0, x1) = (words.y + 4.0, words.x + 1.0, words.x + 60.0);
    assert_eq!(view.review_press(x0, y), Some(true));
    assert!(view.review_pointer((x0, y), x1, y));
    assert!(view.review_release());
    assert!(view.review_selected_text().is_some());
    view.draw_add_pill(&mut Scene::new(), Affine::IDENTITY, false);
    assert!(view.add_pill.rect().is_none(), "not over the transcript");
    view.draw_add_pill(&mut Scene::new(), Affine::IDENTITY, true);
    let pill = view.add_pill.rect().expect("offered over the card");
    assert!(pill.y + pill.h <= words.y, "just above the selected line");
    assert!(view.add_selection_to_chat());
    assert!(view.review_selected_text().is_none());
    assert!(
        view.composer.text().contains("Rates"),
        "quoted into the message"
    );
}

#[test]
fn a_result_reopened_from_a_reply_has_no_queue_position_and_next_starts_the_queue() {
    let (mut view, _, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let mut handled = item();
    handled.review = Some("handled".into());
    view.review.items = vec![item()];
    view.review.count = 1;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &handled));
    assert_eq!(view.review_position_text(), None);
    assert_eq!(view.review_parts(), vec![Part::Next, Part::Close]);
}

#[test]
fn skip_keeps_the_card_showing_its_result_until_the_next_one_arrives() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let (first, second) = (item(), item());
    view.review.items = vec![first.clone(), second.clone()];
    view.review.count = 2;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &first));
    view.review.shown = view.review.selected.clone();
    assert!(view.review_action(Part::Skip));
    let Command::RoutineReview {
        request,
        command: wire::Command::SkipReview { id, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!()
    };
    let mut skipped = first.clone();
    skipped.review = Some("skipped".into());
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::SkippedReview {
            request: id,
            item: skipped,
            replayed: false,
        })
    ));
    // Between the skip and the next result the card keeps its words: it
    // never empties to a loading line, so it does not flicker.
    assert!(
        view.review.selected.is_none(),
        "nothing is attached meanwhile"
    );
    assert!(!view.review_send_ready());
    let (_, words) = view.review_words();
    assert_eq!(words, first.preview);
    let _ = view.take_activity();
    let Command::RoutineReview { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::Reviews {
            page: wire::ReviewPage {
                bot: bot.as_uuid(),
                count: 1,
                items: vec![second.clone()],
                next: None,
            }
        })
    ));
    assert_eq!(view.review_words().1, first.preview, "still the old words");
    let Command::RoutineReview { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::Review {
            detail: detail(bot, &second)
        })
    ));
    assert_eq!(view.review_words().1, second.preview);
}

#[test]
fn alt_s_skips_and_the_arrows_step_only_when_no_caret_needs_them() {
    let (mut view, commands, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let (first, second) = (item(), item());
    view.review.items = vec![first.clone(), second.clone()];
    view.review.count = 2;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &first));
    view.focus = Focus::Composer;
    let right = Key::Named(NamedKey::ArrowRight);

    // A draft keeps its caret's arrow.
    view.composer.set_text("a draft");
    assert_eq!(view.review_key(&right, None, ModifiersState::empty()), None);
    assert!(commands.try_recv().is_err());

    view.composer.set_text("");
    assert_eq!(
        view.review_key(&right, None, ModifiersState::empty()),
        Some(true)
    );
    let Command::RoutineReview {
        command: wire::Command::Review { occurrence, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("next result")
    };
    assert_eq!(occurrence, second.occurrence);

    view.review.pending = None;
    view.review.selected = Some(detail(bot, &second));
    let s = Key::Character("s".into());
    assert_eq!(
        view.review_key(&s, Some("s"), ModifiersState::ALT),
        Some(true)
    );
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::RoutineReview {
            command: wire::Command::SkipReview { occurrence, .. },
            ..
        } if occurrence == second.occurrence
    ));
}

#[test]
fn a_long_result_is_read_to_its_end_before_it_is_copied() {
    let (mut view, commands, _) = surface();
    let (bot, thread, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let first = item();
    view.review.open = true;
    view.review.selected = Some(wire::ReviewDetail {
        next: Some(4),
        text: "Part".into(),
        ..detail(bot, &first)
    });
    assert!(view.review_action(Part::Copy));
    let Command::RoutineReview {
        request,
        command: wire::Command::Review { offset: 0, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("read from the start")
    };
    assert!(view.review_response(
        thread,
        request,
        Ok(wire::Response::Review {
            detail: wire::ReviewDetail {
                next: Some(4),
                text: "Part".into(),
                ..detail(bot, &first)
            }
        })
    ));
    let Command::RoutineReview {
        command: wire::Command::Review { offset: 4, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("then the rest")
    };
    assert_eq!(
        view.review.selected.as_ref().unwrap().text,
        "Part",
        "copying does not move the card"
    );
}

#[test]
fn save_names_the_page_after_the_routine_and_saves_by_occurrence() {
    let (mut view, commands, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    let first = item();
    view.review.open = true;
    view.review.selected = Some(detail(bot, &first));
    assert!(view.review_action(Part::Save));
    let reply = MessageId::from_uuid(first.reply);
    assert_eq!(view.destination_open, Some(reply));
    assert_eq!(view.save_name.value(), "Pricing");
    assert!(view.save_bot_reply(reply));
    let Command::RoutineReview {
        command: wire::Command::SaveReview {
            occurrence, title, ..
        },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("save by occurrence")
    };
    assert_eq!((occurrence, title.as_str()), (first.occurrence, "Pricing"));
    assert!(view.destination_open.is_none());
}

#[test]
fn moving_onto_a_card_control_repaints_at_once_and_skip_has_its_tooltip() {
    let (mut view, _, _) = surface();
    let (bot, _, _) = view.review_subject().unwrap();
    view.review.pending = None;
    view.review.open = true;
    view.review.selected = Some(detail(bot, &item()));
    view.viewport = Rect::new(0.0, 0.0, 420.0, 700.0);
    view.composer_hit_rect = Rect::new(12.0, 600.0, 396.0, 80.0);
    view.draw_review(&mut Scene::new(), Affine::IDENTITY);
    let (_, skip, _) = *view
        .review
        .targets
        .iter()
        .find(|(part, _, _)| *part == Part::Skip)
        .unwrap();
    let outside = (skip.x - 200.0, skip.y - 200.0);
    let inside = (skip.x + skip.w / 2.0, skip.y + skip.h / 2.0);
    assert!(
        view.review_pointer(outside, inside.0, inside.1),
        "entering a control asks for a frame"
    );
    assert!(!view.review_pointer(inside, inside.0 + 1.0, inside.1));
    assert_eq!(
        view.review_tooltip_at(inside.0, inside.1).unwrap().label,
        "Skip"
    );
}
