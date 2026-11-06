// SPDX-License-Identifier: MPL-2.0

use super::super::modal::readiness_tests::{row, world};
use super::*;
use koru_session_protocol::ExecutableAvailability;

#[test]
fn six_brand_rows_and_recovery_guidance_fit_all_eight_languages_without_column_overlap() {
    let mut engine = TextEngine::new();
    let mut agents: Vec<_> = ["Codex", "Claude", "Gemini", "OpenCode", "Kimi", "Grok"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut row = row(
                index,
                ExecutableAvailability::Failed {
                    code: koru_session_protocol::ErrorCode::ProviderFailed,
                },
                false,
            );
            row.name = (*name).into();
            row
        })
        .collect();
    agents[0].readiness.availability = ExecutableAvailability::incompatible(
        koru_session_protocol::ErrorCode::CapabilityNotNegotiated,
    );
    let mut modal = Delegation::open(&mut engine, world(agents));
    modal.activate(Hit::Agent(0));
    for lang in koru_i18n::Lang::ALL {
        let pieces =
            pieces_for_language(&mut engine, &modal, Rect::new(0.0, 0.0, 480.0, 600.0), lang);
        let mut count = 0;
        for piece in pieces {
            match piece {
                Piece::Agent {
                    rect,
                    text,
                    status: Some(key),
                    ..
                } => {
                    count += 1;
                    let name_w = engine.simple_layout(&text, theme::role::row()).width();
                    let status_w = engine
                        .simple_layout(t(lang, key), theme::role::hint())
                        .width();
                    assert!(name_w + status_w + GAP + 12.0 <= rect.w, "{lang:?}: {text}");
                }
                Piece::Guidance { rect, text } => {
                    let layout = engine.simple_layout_wrapped(&text, theme::role::hint(), rect.w);
                    assert!(rect.h >= layout.height());
                }
                _ => {}
            }
        }
        assert_eq!(count, 6);
    }
}

#[test]
fn repeating_controls_share_draw_hit_geometry_and_stay_inside_the_panel() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    modal.activate_with_engine(&mut engine, Hit::ScheduleWeekly);
    let viewport = (640.0, 3000.0);
    let panel = panel(&mut engine, &modal, viewport);
    let controls = layout(&mut engine, &modal, viewport);
    for control in modal.controls() {
        let (_, rect) = controls
            .iter()
            .find(|(hit, _)| *hit == control)
            .unwrap_or_else(|| panic!("missing geometry for {control:?}"));
        assert!(rect.x >= panel.x && rect.x + rect.w <= panel.x + panel.w);
        assert_eq!(
            hit_at(
                &mut engine,
                &modal,
                viewport,
                rect.x + rect.w * 0.5,
                rect.y + rect.h * 0.5,
            ),
            Some(control)
        );
    }
}

#[test]
fn advanced_only_minute_rules_do_not_claim_a_simple_picker_chip() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    modal.repeat.advanced.set_text(&mut engine, "*/5 * * * *");
    modal.repeat.advanced_changed(&mut engine);

    let frequency_hits = [
        Hit::ScheduleHourly,
        Hit::ScheduleDaily,
        Hit::ScheduleWeekly,
        Hit::ScheduleMonthly,
    ];
    let selected = pieces_for_language(
        &mut engine,
        &modal,
        Rect::new(0.0, 0.0, 480.0, 3000.0),
        koru_i18n::Lang::En,
    )
    .into_iter()
    .filter(|piece| {
        matches!(
            piece,
            Piece::Chip {
                hit,
                selected: true,
                ..
            } if frequency_hits.contains(hit)
        )
    })
    .count();
    assert_eq!(selected, 0);
}

#[test]
fn schedule_copy_wraps_without_expanding_past_the_panel_in_every_locale() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    modal.activate_with_engine(&mut engine, Hit::ScheduleWeekly);
    for lang in koru_i18n::Lang::ALL {
        let panel = Rect::new(0.0, 0.0, 360.0, 3000.0);
        for piece in pieces_for_language(&mut engine, &modal, panel, lang) {
            match piece {
                Piece::Guidance { rect, text } => {
                    let layout = engine.simple_layout_wrapped(&text, theme::role::hint(), rect.w);
                    assert!(rect.x >= PAD && rect.x + rect.w <= panel.w - PAD + 0.01);
                    assert!(rect.h >= layout.height(), "{lang:?}: {text}");
                }
                Piece::Field { rect, .. } | Piece::Chip { rect, .. } => {
                    assert!(rect.x >= PAD && rect.x + rect.w <= panel.w - PAD + 0.01);
                }
                _ => {}
            }
        }
    }
}

#[test]
fn invalid_schedule_fields_show_a_readable_reason_in_every_locale() {
    fn assert_reason(engine: &mut TextEngine, modal: &Delegation, key: I18nKey, hit: Hit) {
        for lang in koru_i18n::Lang::ALL {
            let pieces =
                pieces_for_language(engine, modal, Rect::new(0.0, 0.0, 480.0, 3000.0), lang);
            assert!(
                pieces.iter().any(|piece| matches!(
                        piece,
                        Piece::Field { hit: field, invalid: true, .. } if *field == hit
                )),
                "{lang:?} {hit:?} is not visibly invalid"
            );
            let expected = t(lang, key);
            let rect = pieces.iter().find_map(|piece| match piece {
                Piece::Guidance { rect, text } if text == expected => Some(*rect),
                _ => None,
            });
            let rect = rect.unwrap_or_else(|| panic!("{lang:?} {hit:?} has no readable reason"));
            let layout = engine.simple_layout_wrapped(expected, theme::role::hint(), rect.w);
            assert!(rect.h >= layout.height(), "{lang:?} {hit:?} is clipped");
        }
    }

    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);

    modal.repeat.time.set_text(&mut engine, "not a time");
    modal.repeat.accept_picker(&mut engine);
    assert_reason(
        &mut engine,
        &modal,
        I18nKey::DelegateScheduleTimeInvalid,
        Hit::ScheduleTime,
    );

    modal.repeat.time.set_text(&mut engine, "9:00 AM");
    modal.repeat.accept_picker(&mut engine);
    modal.activate_with_engine(&mut engine, Hit::ScheduleMonthly);
    modal.repeat.day.set_text(&mut engine, "32");
    modal.repeat.accept_picker(&mut engine);
    assert_reason(
        &mut engine,
        &modal,
        I18nKey::DelegateScheduleDayInvalid,
        Hit::ScheduleDay,
    );

    modal.repeat.day.set_text(&mut engine, "31");
    modal.repeat.accept_picker(&mut engine);
    modal.activate_with_engine(&mut engine, Hit::ScheduleZoned);
    modal.repeat.zone.set_text(&mut engine, "not/a-zone");
    assert_reason(
        &mut engine,
        &modal,
        I18nKey::DelegateScheduleZoneInvalid,
        Hit::ScheduleZone,
    );
}

#[test]
fn every_repeating_control_is_focus_revealed_inside_a_short_panel() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    modal.activate_with_engine(&mut engine, Hit::ScheduleWeekly);
    modal.activate_with_engine(&mut engine, Hit::ScheduleZoned);
    let viewport = (560.0, 420.0);

    for index in 0..modal.controls().len() {
        modal.focus = index;
        reveal_focus(&mut engine, &mut modal, viewport);
        let panel = panel(&mut engine, &modal, viewport);
        let focused = modal.focused().expect("a control remains focused");
        let visible = layout(&mut engine, &modal, viewport);
        let (_, rect) = visible
            .into_iter()
            .find(|(hit, _)| *hit == focused)
            .unwrap_or_else(|| {
                let content_panel = scrolled_panel(&mut engine, &modal, panel);
                let raw = pieces(&mut engine, &modal, content_panel)
                    .into_iter()
                    .filter_map(|piece| piece.hit())
                    .find(|(hit, _)| *hit == focused);
                panic!(
                    "focused {focused:?} has no geometry; scroll {}, panel {panel:?}, raw {raw:?}",
                    modal.scroll
                )
            });
        assert!(rect.y >= panel.y + 3.9, "{focused:?} above clip: {rect:?}");
        assert!(
            rect.y + rect.h <= panel.y + panel.h - 3.9,
            "{focused:?} below clip: {rect:?}"
        );
    }
}

/// The inspector reads the finished surface as **KoruModal → KoruPrompt →
/// title + KoruButtons**, hoisted ahead of the window it covers — and picking
/// still follows paint order, so a point on a button answers the button.
#[test]
fn a_finished_modal_leads_the_inspector_with_a_prompt_of_real_buttons() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    modal.answered(Phase::Done {
        run: koru_delegate_core::RunId::mint(),
        document: koru_core::DocumentId::new(),
        busy: None,
    });
    let mut scene_out = Scene::new();
    let root = Affine::IDENTITY;
    let viewport_rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 650.0);
    let frame = koru_draw::inspect::Frame::begin(1, root, viewport_rect);
    // What the window painted before the modal opened: it must trail the
    // hoisted modal in the tree yet stay beneath it for picking.
    koru_draw::inspect::leaf("window content", root, viewport_rect, |_| {});
    draw(
        &mut modal,
        &mut scene_out,
        root,
        &mut engine,
        (900.0, 650.0),
    );
    let snapshot = frame.finish();

    assert_eq!(snapshot.records[0].name, "KoruModal");
    assert!(snapshot.records[0].hoisted);
    let prompt = snapshot
        .records
        .iter()
        .position(|r| r.name == "KoruPrompt")
        .expect("a finished phase is a KoruPrompt");
    assert_eq!(snapshot.records[prompt].parent, Some(0));
    let window = snapshot
        .records
        .iter()
        .position(|r| r.name == "window content")
        .unwrap();
    assert!(window > prompt, "the hoisted subtree leads the vec");

    // Nested inside the prompt: one title part, and one KoruButton per action.
    let mut title = 0;
    let mut buttons = Vec::new();
    for (index, record) in snapshot.records.iter().enumerate() {
        let mut ancestor = record.parent;
        let mut inside = false;
        while let Some(at) = ancestor {
            if at == prompt {
                inside = true;
                break;
            }
            ancestor = snapshot.records[at].parent;
        }
        if !inside {
            continue;
        }
        match record.name {
            "Prompt title" => title += 1,
            "KoruButton" => buttons.push(index),
            _ => {}
        }
    }
    assert_eq!(title, 1);
    assert_eq!(buttons.len(), modal.controls().len());

    // Over a button the pick names something the button owns — never the
    // window content that trails in the vec.
    let bounds = snapshot.records[buttons[0]].rect();
    let picked = snapshot
        .pick((bounds.x0 + bounds.x1) * 0.5, (bounds.y0 + bounds.y1) * 0.5)
        .expect("the button covers its own centre");
    assert_eq!(snapshot.owner(picked), Some(buttons[0]));
}

/// The editing surface reads as **KoruModal → KoruDelegateForm → FloatingPanel
/// + KoruButtons**, with the scrim named and the shadow collapsed to one
/// record — not a flat run of anonymous fills.
#[test]
fn the_editing_form_is_a_named_surface_with_real_buttons() {
    let mut engine = TextEngine::new();
    let mut modal = Delegation::open(
        &mut engine,
        world(vec![row(
            0,
            ExecutableAvailability::Available { version: None },
            true,
        )]),
    );
    let mut scene_out = Scene::new();
    let root = Affine::IDENTITY;
    let viewport_rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 900.0);
    let frame = koru_draw::inspect::Frame::begin(1, root, viewport_rect);
    draw(
        &mut modal,
        &mut scene_out,
        root,
        &mut engine,
        (900.0, 900.0),
    );
    let snapshot = frame.finish();
    let records = &snapshot.records;
    let inside = |ancestor: usize, at: usize| -> bool {
        let mut parent = records[at].parent;
        while let Some(p) = parent {
            if p == ancestor {
                return true;
            }
            parent = records[p].parent;
        }
        false
    };

    assert_eq!(records[0].name, "KoruModal");
    let scrim = records
        .iter()
        .position(|r| r.name == "Scrim")
        .expect("the modal's opacity is a named part");
    assert_eq!(records[scrim].parent, Some(0));

    let form = records
        .iter()
        .position(|r| r.name == "KoruDelegateForm")
        .expect("the editing form is a named surface");
    assert!(inside(0, form), "the form answers to the modal");

    let panel = records
        .iter()
        .position(|r| r.name == "FloatingPanel")
        .expect("the panel is named");
    assert!(inside(form, panel));
    assert_eq!(
        records.iter().filter(|r| r.name == "Shadow").count(),
        1,
        "one shadow, not sixteen rings"
    );
    let fills_under_panel = records
        .iter()
        .enumerate()
        .filter(|(i, r)| r.name == "fill" && inside(panel, *i))
        .count();
    assert_eq!(fills_under_panel, 1, "the panel's body is the only fill");

    // The answers are KoruButtons (Cancel always; Delegate once the form is
    // ready), and the When chips name themselves too.
    let buttons = records
        .iter()
        .enumerate()
        .filter(|(i, r)| r.name == "KoruButton" && inside(form, *i))
        .count();
    assert!(buttons >= 1, "Cancel is always drawn");
    assert!(
        records
            .iter()
            .enumerate()
            .any(|(i, r)| r.name == "KoruChip" && inside(form, i)),
        "the When chips are components"
    );

    // Nothing anonymous sits directly on the modal: its children are the
    // scrim and the form.
    let anonymous = records
        .iter()
        .filter(|r| r.parent == Some(0) && (r.name == "fill" || r.name == "Text"))
        .count();
    assert_eq!(anonymous, 0);
}
