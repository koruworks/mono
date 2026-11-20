// SPDX-License-Identifier: MPL-2.0
//! Actual Bot link, selected context frame and navigation host, all in memory.
use super::*;
use crate::bots::link::Failure;
use crate::bots::view::{
    context_library::{LIBRARY_SECTION, ROUTINES_SECTION},
    context_routines::{detail::occurrence_key, details_key},
};
use koru_ai_core::bots::routines::*;
use koru_delegate_core::{DeviceId, RunId, ScheduleId};
use koru_delegate_schedule::{ScheduleName, ScheduleState};

pub(super) fn held(v: &mut MainView) -> &mut BotView {
    match v.active_view() {
        TabView::Bot(view) => view,
        _ => panic!("expected Bot tab"),
    }
}
pub(super) fn item() -> BotRoutineListItem {
    BotRoutineListItem {
        schedule_id: ScheduleId::mint(),
        creation: BotRoutineCreationObservation {
            request_id: koru_ai_core::bots::BotRequestId::mint(),
            state: BotRoutineCreationState::Bound,
            refusal: None,
            updated_at_ms: -60_000,
        },
        grant: BotRoutineGrantSummary {
            source_bot_policy_revision: 1,
            grant_revision: 1,
            access: koru_ai_core::bots::BotAccess::Default,
            selected_page_count: 2,
        },
        live: BotRoutineLiveObservation::Current {
            name: ScheduleName::new("Morning reading").unwrap(),
            definition_revision: 2,
            state: ScheduleState::Active {
                owner: DeviceId::new(),
            },
            next: RoutineNextObservation::OwnerLocal {},
        },
        local_conversations: BotRoutineLocalConversations::Available {
            items: vec![BotRoutineLocalConversation {
                occurrence_ms: -60_000,
                run_id: RunId::mint(),
                chat_id: uuid::Uuid::new_v4(),
                attempt_id: uuid::Uuid::new_v4(),
                conversation: BotRoutineConversationVisibility::Available,
            }],
            more: false,
        },
    }
}
pub(super) fn page(bot: &Bot) -> BotRoutineListPage {
    BotRoutineListPage {
        bot_id: bot.id,
        bot_revision: bot.revision,
        effective_model: BotRoutineModelObservation::Configured {
            provider: bot.config.model.provider.clone(),
            requested_model: bot.config.model.requested_model.clone(),
        },
        items: vec![item()],
        next_schedule: None,
    }
}
pub(super) fn request(v: &mut MainView, commands: &Receiver<Command>) -> u64 {
    held(v).shown_pass(None);
    commands
        .try_iter()
        .find_map(|c| match c {
            Command::ReadRoutines { seq, .. } => Some(seq),
            _ => None,
        })
        .expect("routine read")
}
pub(super) fn answer(
    v: &mut MainView,
    tx: &SyncSender<Answer>,
    bot: &Bot,
    seq: u64,
    result: Result<BotRoutineListPage, crate::bots::routines::ReadFailure>,
) {
    tx.send(Answer::Routines {
        seq,
        bot: bot.id,
        result,
    })
    .unwrap();
    held(v).shown_pass(None);
    v.sync_bot_context();
}
pub(super) fn setup(
    v: &mut MainView,
    bot: &Bot,
    page: BotRoutineListPage,
) -> (Receiver<Command>, SyncSender<Answer>) {
    let (commands, tx) = install(v, bot, vec![], None);
    assert!(v.bot_context_action(ROUTINES_SECTION));
    let seq = request(v, &commands);
    answer(v, &tx, bot, seq, Ok(page));
    (commands, tx)
}
pub(super) fn lines(v: &MainView) -> String {
    v.bot_context
        .with_frame(|frame| {
            frame
                .nodes
                .iter()
                .filter_map(|node| match node.label.as_ref()? {
                    koru_draw::el::Name::Text(value) => Some(value.clone()),
                    koru_draw::el::Name::Key(key) => {
                        Some(koru_i18n::t(koru_i18n::Lang::En, *key).into())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap()
}
fn chat(page: &BotRoutineListPage) -> koru_ai_core::ChatId {
    let BotRoutineLocalConversations::Available { items, .. } = &page.items[0].local_conversations
    else {
        panic!()
    };
    koru_ai_core::ChatId::parse(&items[0].chat_id.to_string()).unwrap()
}

pub(super) fn detail_page(
    bot: &Bot,
    page: &BotRoutineListPage,
    id: ScheduleId,
) -> BotRoutineDetail {
    let item = page
        .items
        .iter()
        .find(|item| item.schedule_id == id)
        .unwrap();
    let live = match &item.live {
        BotRoutineLiveObservation::Current {
            name,
            definition_revision,
            state,
            ..
        } => RoutineDetailObservation::Current {
            name: name.clone(),
            definition_revision: *definition_revision,
            effective_at_ms: 0,
            recurrence: koru_recurrence::RecurrenceRule::floating(
                koru_recurrence::CronExpression::parse("0 9 * * *").unwrap(),
            ),
            state: state.clone(),
        }
        .live(0),
        BotRoutineLiveObservation::NotLive {} => RoutineDetailLive::NotLive {},
        BotRoutineLiveObservation::Unavailable {} => RoutineDetailLive::Unavailable {},
    };
    BotRoutineDetail {
        bot_id: bot.id,
        schedule_id: id,
        bot_revision: bot.revision,
        observed_at_ms: 0,
        creation: item.creation.clone(),
        access: item.grant.clone(),
        access_at_setup: item.grant.clone(),
        configured_model: page.effective_model.clone(),
        destination: RoutineDestinationObservation::Unavailable {},
        live,
        readiness: RoutineReadinessObservation::NotAssessed {},
        local_history: match &item.local_conversations {
            BotRoutineLocalConversations::Available { items, more } => {
                RoutineLocalHistoryObservation::Available {
                    items: items.clone(),
                    next_before_occurrence_ms: more.then(|| items.last().unwrap().occurrence_ms),
                }
            }
            _ => RoutineLocalHistoryObservation::Unavailable {},
        },
        occurrences: RoutineOccurrenceHistoryObservation::Unavailable {},
    }
}
pub(super) fn detail_request(
    v: &mut MainView,
    commands: &Receiver<Command>,
) -> (u64, ScheduleId, Option<i64>) {
    held(v).shown_pass(None);
    commands
        .try_iter()
        .find_map(|command| match command {
            Command::ReadRoutineDetail {
                seq,
                schedule,
                native_before,
                ..
            } => Some((seq, schedule, native_before.map(|cursor| cursor.at_ms))),
            _ => None,
        })
        .expect("selected detail read")
}
pub(super) fn detail_answer(
    v: &mut MainView,
    tx: &SyncSender<Answer>,
    bot: &Bot,
    seq: u64,
    schedule: ScheduleId,
    result: Result<BotRoutineDetail, crate::bots::routines::ReadFailure>,
) {
    tx.send(Answer::NativeRoutineDetail {
        seq,
        bot: bot.id,
        schedule,
        result: result.map(|detail| {
            // This fixture carries one native history row per input record.
            // Reuse its UUID as the occurrence identity for stable UI keys;
            // all of these occurrences open the snapshot's single thread.
            let occurrences = match &detail.local_history {
                RoutineLocalHistoryObservation::Available { items, .. } => items
                    .iter()
                    .map(|item| koru_api_client::bot_routines::Occurrence {
                        id: item.chat_id,
                        routine: schedule.as_uuid(),
                        definition_revision: 2,
                        occurrence_ms: item.occurrence_ms,
                        manual: false,
                        state: "answered".into(),
                        reason: None,
                        message: (item.conversation == BotRoutineConversationVisibility::Available)
                            .then(uuid::Uuid::new_v4),
                        reply: None,
                        card: None,
                        recorded_at_ms: item.occurrence_ms,
                        updated_at_ms: item.occurrence_ms,
                        preview: None,
                    })
                    .collect(),
                _ => vec![],
            };
            let thread = match &detail.local_history {
                RoutineLocalHistoryObservation::Available { items, .. } => items
                    .first()
                    .map(|item| koru_ai_core::ChatId::from_uuid(item.chat_id))
                    .unwrap_or_else(koru_ai_core::ChatId::new),
                _ => koru_ai_core::ChatId::new(),
            };
            let selected_pages = (0..detail.access.selected_page_count)
                .map(|_| super::page(bot))
                .collect();
            koru_api_client::bot_routines::Snapshot {
                detail,
                thread,
                selected_pages,
                occurrences,
                next: None,
            }
        }),
    })
    .unwrap();
    held(v).shown_pass(None);
    v.sync_bot_context();
}
pub(super) fn select_detail(
    v: &mut MainView,
    commands: &Receiver<Command>,
    tx: &SyncSender<Answer>,
    bot: &Bot,
    id: ScheduleId,
) {
    assert!(v.bot_context_action(details_key(id)));
    let page = held(v).routines.held.clone().unwrap();
    let (seq, schedule, before) = detail_request(v, commands);
    assert_eq!(schedule, id);
    assert_eq!(before, None);
    detail_answer(v, tx, bot, seq, id, Ok(detail_page(bot, &page, id)));
}

#[test]
fn routine_context_reads_only_selected_visible_section() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let p = page(&bot);
    let (commands, tx) = install(&mut v, &bot, vec![], None);
    held(&mut v).shown_pass(None);
    assert!(
        !commands
            .try_iter()
            .any(|c| matches!(c, Command::ReadRoutines { .. }))
    );
    v.bot_context_action(ROUTINES_SECTION);
    let seq = request(&mut v, &commands);
    answer(&mut v, &tx, &bot, seq, Ok(p));
    held(&mut v).shown_pass(None);
    let pending: Vec<_> = commands.try_iter().collect();
    assert!(matches!(
        pending.as_slice(),
        [Command::RoutineStartup { bot: claimed, .. }] if *claimed == bot.id
    ));
    held(&mut v).shown_pass(None);
    assert!(commands.try_recv().is_err());
    v.bot_context_action(LIBRARY_SECTION);
    held(&mut v).shown_pass(None);
    assert!(
        !commands
            .try_iter()
            .any(|c| matches!(c, Command::ReadRoutines { .. }))
    );
    assert!(!Section::VISIBLE.contains(&Section::Routines));
    v.bot_context_action(ROUTINES_SECTION);
    let _ = request(&mut v, &commands);
    held(&mut v).hidden();
    held(&mut v).shown_pass(None);
    assert!(
        !commands
            .try_iter()
            .any(|c| matches!(c, Command::ReadRoutines { .. }))
    );
}

#[test]
fn routine_context_replaces_pages_and_rejects_superseded_replies() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let mut p = page(&bot);
    p.next_schedule = Some(p.items[0].schedule_id);
    let (commands, tx) = setup(&mut v, &bot, p.clone());
    let next = crate::bots::view::context_routines::NEXT;
    assert!(v.bot_context_action(next));
    let seq = request(&mut v, &commands);
    let mut newer = page(&bot);
    newer.items = (0..16).map(|_| item()).collect();
    answer(&mut v, &tx, &bot, seq.wrapping_sub(1), Ok(page(&bot)));
    assert_eq!(held(&mut v).routines.held.as_ref().unwrap().items.len(), 1);
    answer(&mut v, &tx, &bot, seq, Ok(newer.clone()));
    assert_eq!(held(&mut v).routines.held.as_ref().unwrap().items.len(), 16);
    assert_eq!(
        held(&mut v).routines.held.as_ref().unwrap().items[0].schedule_id,
        newer.items[0].schedule_id
    );
    // Same view, same A identity again: the old A answer must not acquire the
    // newer request's identity after the intermediate B reset.
    let b = koru_ai_core::bots::BotId::mint();
    let state = &mut held(&mut v).routines;
    state.show(Some(b), true);
    let Command::ReadRoutines { seq: b_seq, .. } = state.read().unwrap() else {
        panic!()
    };
    state.show(Some(bot.id), true);
    let Command::ReadRoutines { seq: a_seq, .. } = state.read().unwrap() else {
        panic!()
    };
    assert!(seq < b_seq && b_seq < a_seq);
    assert!(!state.answer(seq, bot.id, Ok(page(&bot))));
    assert!(!state.answer(b_seq, b, Ok(page(&bot))));
    assert!(state.held.is_none());
    assert!(state.answer(a_seq, bot.id, Ok(page(&bot))));
}

#[test]
fn routine_context_partial_states_do_not_invent_names_or_history() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let mut p = page(&bot);
    p.items[0].live = BotRoutineLiveObservation::Unavailable {};
    p.items[0].creation.state = BotRoutineCreationState::Conflict;
    p.items[0].creation.refusal = Some(BotRoutineCreationRefusal::DocumentChanged);
    p.items[0].creation.updated_at_ms = i64::MIN;
    p.items[0].local_conversations = BotRoutineLocalConversations::Unavailable {};
    let id = p.items[0].schedule_id;
    let (commands, tx) = setup(&mut v, &bot, p);
    select_detail(&mut v, &commands, &tx, &bot, id);
    let labels = lines(&v);
    assert!(!labels.contains("Morning reading"));
    assert!(!labels.contains(&id.to_string()));
    assert!(labels.contains("Routine setup"));
    assert!(labels.contains("Date unavailable"));
    assert!(labels.contains("Access at setup"));
    assert!(labels.contains(koru_i18n::t(
        koru_i18n::Lang::En,
        koru_i18n::Key::BotsRoutineNothingRan,
    )));
    assert_ne!(
        crate::bots::view::context_routines::date(koru_i18n::Lang::En, -60_000),
        "Date unavailable"
    );
    let (mut v, _w) = on_document();
    let mut mixed = page(&bot);
    mixed.effective_model = BotRoutineModelObservation::Unavailable {};
    mixed.items = (0..16).map(|_| item()).collect();
    mixed.items[1].live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("Paused").unwrap(),
        definition_revision: 2,
        state: ScheduleState::Paused {
            owner: DeviceId::new(),
            reason: koru_delegate_core::SchedulePauseReason::Manual,
        },
        next: RoutineNextObservation::Inactive {},
    };
    mixed.items[2].live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("Owner transfer").unwrap(),
        definition_revision: 2,
        state: ScheduleState::HandoffPending {
            owner: DeviceId::new(),
            requests: vec![],
        },
        next: RoutineNextObservation::Inactive {},
    };
    mixed.items[3].live = BotRoutineLiveObservation::Current {
        name: ScheduleName::new("Retired").unwrap(),
        definition_revision: 2,
        state: ScheduleState::Retired,
        next: RoutineNextObservation::Inactive {},
    };
    mixed.items[4].live = BotRoutineLiveObservation::NotLive {};
    mixed.items[5].live = BotRoutineLiveObservation::Unavailable {};
    mixed.items[5].creation.state = BotRoutineCreationState::Prepared;
    mixed.items[6].live = BotRoutineLiveObservation::Unavailable {};
    mixed.items[6].creation.state = BotRoutineCreationState::Unconfirmed;
    mixed.items[7].grant.access = koru_ai_core::bots::BotAccess::Workspace;
    mixed.items[8].local_conversations = BotRoutineLocalConversations::Available {
        items: vec![],
        more: false,
    };
    let history = (0..4)
        .map(|i| BotRoutineLocalConversation {
            occurrence_ms: if i == 3 { i64::MAX } else { -60_000 },
            run_id: RunId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            attempt_id: uuid::Uuid::new_v4(),
            conversation: if i == 3 {
                BotRoutineConversationVisibility::Unavailable
            } else {
                BotRoutineConversationVisibility::Available
            },
        })
        .collect::<Vec<_>>();
    mixed.items[0].local_conversations = BotRoutineLocalConversations::Available {
        items: history.clone(),
        more: true,
    };
    let ids = mixed
        .items
        .iter()
        .map(|item| item.schedule_id)
        .collect::<Vec<_>>();
    let (commands, tx) = setup(&mut v, &bot, mixed);
    select_detail(&mut v, &commands, &tx, &bot, ids[0]);
    View::set_viewport(&mut v, 420.0, 500.0);
    v.sync_bot_context();
    for lang in koru_i18n::Lang::ALL {
        v.bot_context.data.as_mut().unwrap().lang = lang;
        v.bot_context
            .with_frame(|frame| {
                for id in &ids {
                    assert!(
                        frame
                            .node(Key::model(id.as_uuid().as_u128(), "routine-group"))
                            .is_some()
                    );
                }
                for reference in &history {
                    assert_eq!(
                        frame.node(occurrence_key(reference.chat_id)).is_some(),
                        reference.conversation == BotRoutineConversationVisibility::Available,
                    );
                }
                for node in frame.nodes.iter().filter(|node| node.focusable) {
                    assert!(
                        node.rect.w > 0.0 && node.rect.h > 0.0,
                        "{} {:?}",
                        lang.tag(),
                        node.rect
                    );
                    assert!(
                        node.rect.x >= v.bot_context.area.x
                            && node.rect.x + node.rect.w
                                <= v.bot_context.area.x + v.bot_context.area.w + 0.5,
                        "{} {:?}",
                        lang.tag(),
                        node.rect
                    );
                    if !node.disabled {
                        if let Some(hit) = node.hit_region().filter(|r| r.w > 0.0 && r.h > 0.0) {
                            assert_eq!(
                                frame.hit_at(hit.x + hit.w / 2.0, hit.y + hit.h / 2.0),
                                node.key,
                                "{} {:?}",
                                lang.tag(),
                                hit
                            );
                        }
                    }
                }
            })
            .unwrap();
    }
}

#[test]
fn routine_context_ambiguous_failure_clears_prior_content() {
    for code in [
        "bot_unavailable",
        "storage_error",
        "forbidden",
        "not_found",
        "ai_worker_timeout",
        "invalid_response",
        "gateway_unavailable",
        "bot_lifecycle_pending",
    ] {
        let (mut v, _w) = on_document();
        let bot = bot();
        let (commands, tx) = setup(&mut v, &bot, page(&bot));
        v.bot_context_action(crate::bots::view::context_routines::REFRESH);
        let seq = request(&mut v, &commands);
        let error = crate::bots::routines::listed(Err(crate::bots::link::Said::new(code, true)))
            .unwrap_err();
        answer(&mut v, &tx, &bot, seq, Err(error));
        assert!(held(&mut v).routines.held.is_none(), "{code}");
        assert!(!lines(&v).contains("Morning reading"));
    }
}

#[test]
fn routine_context_transport_stale_disables_navigation_until_refresh() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let p = page(&bot);
    let id = p.items[0].schedule_id;
    let chat = chat(&p);
    let (commands, tx) = setup(&mut v, &bot, p.clone());
    select_detail(&mut v, &commands, &tx, &bot, id);
    assert!(
        v.bot_context
            .action(occurrence_key(chat.as_uuid()))
            .is_some()
    );
    v.bot_context_action(crate::bots::view::context_routines::REFRESH);
    let seq = request(&mut v, &commands);
    let error = crate::bots::routines::connection_failure(koru_store_core::Code::StoreUnreachable);
    answer(&mut v, &tx, &bot, seq, Err(error));
    v.bot_context_action(details_key(id));
    assert!(held(&mut v).routines.held.is_some());
    assert!(
        v.bot_context
            .action(occurrence_key(chat.as_uuid()))
            .is_none()
    );
    assert!(lines(&v).contains("may be old"));
    v.bot_context_action(crate::bots::view::context_routines::REFRESH);
    let seq = request(&mut v, &commands);
    answer(&mut v, &tx, &bot, seq, Ok(p));
    select_detail(&mut v, &commands, &tx, &bot, id);
    assert!(
        v.bot_context
            .action(occurrence_key(chat.as_uuid()))
            .is_some()
    );
}

#[test]
fn routine_context_retires_old_press_and_accessibility_occurrence() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let p = page(&bot);
    let id = p.items[0].schedule_id;
    let chat = chat(&p);
    let (commands, tx) = setup(&mut v, &bot, p.clone());
    select_detail(&mut v, &commands, &tx, &bot, id);
    let key = occurrence_key(chat.as_uuid());
    let old = v.bot_context.occurrence(key).unwrap();
    v.bot_context.focus = Some(key);
    v.bot_context.reveal();
    let r = rect(&v, key);
    assert!(
        r.w > 0.0 && r.h > 0.0,
        "focused action has physical geometry"
    );
    assert!(v.mouse_down(&mut w.ws, r.x + r.w / 2.0, r.y + r.h / 2.0, false));
    v.bot_context_action(crate::bots::view::context_routines::REFRESH);
    assert!(v.bot_context.pressed.is_none());
    let seq = request(&mut v, &commands);
    answer(&mut v, &tx, &bot, seq, Ok(p));
    select_detail(&mut v, &commands, &tx, &bot, id);
    assert_ne!(v.bot_context.occurrence(key), Some(old));
    assert!(v.bot_context.key_for_occurrence(old).is_none());
    let target = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        old,
        ControlPart::ROOT,
    );
    assert!(!v.accessibility_action(&mut w.ws, target, accesskit::Action::Click, None));
    assert!(v.mouse_up(&mut w.ws));
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::RoutineLabel { .. }))
    );
}

#[test]
fn routine_context_open_requires_expected_bot_including_existing_tab() {
    for outcome in 0..5 {
        let (mut v, _w) = on_document();
        let bot = bot();
        let p = page(&bot);
        let id = p.items[0].schedule_id;
        let chat = chat(&p);
        let (commands, tx) = setup(&mut v, &bot, p);
        select_detail(&mut v, &commands, &tx, &bot, id);
        assert!(v.bot_context_action(occurrence_key(chat.as_uuid())));
        let (seq, expected) = commands
            .try_iter()
            .find_map(|c| match c {
                Command::RoutineLabel {
                    seq,
                    bot,
                    chat: found,
                } if found == chat => Some((seq, bot)),
                _ => None,
            })
            .unwrap();
        let label = koru_ai_core::bots::BotChatLabel {
            bot_id: if outcome == 1 {
                koru_ai_core::bots::BotId::mint()
            } else {
                expected
            },
            bot_name: "Scout".into(),
            purpose: koru_ai_core::bots::BotChatPurpose::Additional,
        };
        tx.send(Answer::RoutineLabel {
            seq,
            bot: expected,
            chat,
            result: match outcome {
                0 | 1 => Ok(Some(label)),
                2 => Ok(None),
                3 => Err(Failure::Refused("forbidden".into())),
                _ => Err(Failure::Unanswered),
            },
        })
        .unwrap();
        held(&mut v).shown_pass(None);
        if outcome != 0 {
            assert!(!v.drain_bot_routine_open());
            assert_eq!(held(&mut v).chat_id(), bot.main_chat);
            assert!(held(&mut v).routines.held.is_none());
            assert!(!lines(&v).contains("Morning reading"));
        } else {
            assert!(v.drain_bot_routine_open());
            assert_eq!(held(&mut v).chat_id(), chat);
        }
    }
    {
        let (mut v, _w) = on_document();
        let bot = bot();
        let p = page(&bot);
        let id = p.items[0].schedule_id;
        let chat = chat(&p);
        let (commands, tx) = setup(&mut v, &bot, p);
        select_detail(&mut v, &commands, &tx, &bot, id);
        v.bot_context_action(occurrence_key(chat.as_uuid()));
        let seq = commands
            .try_iter()
            .find_map(|c| match c {
                Command::RoutineLabel { seq, .. } => Some(seq),
                _ => None,
            })
            .unwrap();
        // A disclosure change supersedes the pending label while retaining the
        // same page. Even a refusal must not change its newer notice/content.
        v.bot_context_action(details_key(id));
        tx.send(Answer::RoutineLabel {
            seq,
            bot: bot.id,
            chat,
            result: Err(Failure::Refused("forbidden".into())),
        })
        .unwrap();
        held(&mut v).shown_pass(None);
        v.sync_bot_context();
        assert!(held(&mut v).routines.held.is_some());
        assert!(held(&mut v).routines.notice.is_none());
        assert!(!v.drain_bot_routine_open());
        select_detail(&mut v, &commands, &tx, &bot, id);
        v.bot_context_action(occurrence_key(chat.as_uuid()));
        let old = commands
            .try_iter()
            .find_map(|c| match c {
                Command::RoutineLabel { seq, .. } => Some(seq),
                _ => None,
            })
            .unwrap();
        v.bot_context_action(crate::bots::view::context_routines::REFRESH);
        let read = request(&mut v, &commands);
        let fresh = page(&bot);
        let fresh_id = fresh.items[0].schedule_id;
        answer(&mut v, &tx, &bot, read, Ok(fresh));
        tx.send(Answer::RoutineLabel {
            seq: old,
            bot: bot.id,
            chat,
            result: Err(Failure::Unanswered),
        })
        .unwrap();
        held(&mut v).shown_pass(None);
        v.sync_bot_context();
        assert_eq!(
            held(&mut v).routines.held.as_ref().unwrap().items[0].schedule_id,
            fresh_id
        );
        assert!(held(&mut v).routines.notice.is_none());
        assert!(!v.drain_bot_routine_open());
    }
    let (mut v, _w) = on_document();
    let bot = bot();
    let target = koru_ai_core::ChatId::new();
    let _links = install(&mut v, &bot, vec![], None);
    v.tabs_mut().push(Tab::ai_chat_existing_at(target, None));
    assert!(!v.open_routine_conversation(bot.id, target));
    let foreign = crate::bots::tests::bot(
        koru_ai_core::bots::BotId::mint(),
        bot.home_workspace,
        "Other",
        false,
    );
    let (link, _rx, _tx) = BotsLink::test_pair();
    v.tabs_mut()
        .push(Tab::bot(BotView::with_link(foreign.id, target, link)));
    assert!(!v.open_routine_conversation(bot.id, target));
    let unresolved = koru_ai_core::ChatId::new();
    v.tabs_mut()
        .push(Tab::bot(BotView::resolving(unresolved, None)));
    assert!(!v.open_routine_conversation(bot.id, unresolved));
    let owned = koru_ai_core::ChatId::new();
    let (link, _rx, _tx) = BotsLink::test_pair();
    v.tabs_mut()
        .push(Tab::bot(BotView::with_link(bot.id, owned, link)));
    assert!(v.open_routine_conversation(bot.id, owned));
}

#[test]
fn routine_context_keyboard_narrow_focus_and_geometry() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let mut p = page(&bot);
    p.items = (0..16).map(|_| item()).collect();
    setup(&mut v, &bot, p);
    View::set_viewport(&mut v, 420.0, 400.0);
    v.relayout();
    v.sync_bot_context();
    assert!(v.bot_context.floating);
    v.focus_bot_context();
    for _ in 0..15 {
        press(&mut v, &mut w.ws, NamedKey::Tab);
    }
    assert!(v.bot_context.scroll > 0.0);
    let key = v.bot_context.focus.unwrap();
    let r = rect(&v, key);
    assert!(
        r.y >= v.bot_context.area.y
            && r.y + r.h <= v.bot_context.area.y + v.bot_context.area.h + 1.0
    );
    let hit = v.bot_context.hit(r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert_eq!(hit, Some(key));
    assert!(press(&mut v, &mut w.ws, NamedKey::Escape));
    assert!(!v.context.is_open());
}

#[test]
fn routine_detail_selection_hidden_aba_retire() {
    let (mut v, _w) = on_document();
    let bot = bot();
    let mut p = page(&bot);
    p.items.push(item());
    let a = p.items[0].schedule_id;
    let b = p.items[1].schedule_id;
    let (commands, tx) = setup(&mut v, &bot, p.clone());
    v.bot_context_action(details_key(a));
    let (old, _, _) = detail_request(&mut v, &commands);
    v.bot_context_action(details_key(b));
    let (other, _, _) = detail_request(&mut v, &commands);
    v.bot_context_action(details_key(a));
    let (current, _, _) = detail_request(&mut v, &commands);
    assert!(old < other && other < current);
    detail_answer(&mut v, &tx, &bot, old, a, Ok(detail_page(&bot, &p, a)));
    detail_answer(&mut v, &tx, &bot, other, b, Ok(detail_page(&bot, &p, b)));
    assert!(held(&mut v).routines.detail.held.is_none());
    detail_answer(&mut v, &tx, &bot, current, a, Ok(detail_page(&bot, &p, a)));
    assert_eq!(
        held(&mut v)
            .routines
            .detail
            .held
            .as_ref()
            .unwrap()
            .schedule_id,
        a
    );
    assert!(
        !commands
            .try_iter()
            .any(|c| matches!(c, Command::ReadRoutineDetail { .. }))
    );
    held(&mut v).routines.detail_refresh(false);
    let (hidden, _, _) = detail_request(&mut v, &commands);
    v.bot_context_action(LIBRARY_SECTION);
    detail_answer(&mut v, &tx, &bot, hidden, a, Ok(detail_page(&bot, &p, a)));
    assert!(held(&mut v).routines.detail.stale);
    assert!(!held(&mut v).routines.detail.data().navigable);
    held(&mut v).shown_pass(None);
    assert!(
        !commands
            .try_iter()
            .any(|c| matches!(c, Command::ReadRoutineDetail { .. }))
    );
}
