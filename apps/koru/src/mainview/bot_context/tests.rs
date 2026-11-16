// SPDX-License-Identifier: MPL-2.0

use super::*;
mod controls;
mod routines;
use crate::bots::view::context_library::{BODY, MANAGE, RELOAD, SURFACE};
use crate::bots::{
    BotView,
    link::{Answer, BotsLink, Command},
};
use crate::mainview::{harness::on_document, tab::Tab};
use koru_ai_core::bots::library::BotLibraryView;
use koru_ai_core::bots::{Bot, BotId, BotPage};
use koru_store_core::ids::WorkspaceId;
use koru_workspace_host::View;
use koru_workspace_host::accessibility::{ControlPart, Occurrence, ViewSlot, WindowSlot};
use std::sync::mpsc::{Receiver, SyncSender};
use winit::keyboard::NamedKey;

fn bot() -> Bot {
    crate::bots::tests::bot(BotId::mint(), WorkspaceId::new(), "Scout", false)
}
fn page(bot: &Bot) -> BotPage {
    BotPage {
        workspace: bot.home_workspace,
        document: koru_ai_core::DocumentId::new(),
    }
}

fn loaded(
    bot: &Bot,
    pages: Vec<BotPage>,
    memory: Option<BotPage>,
) -> (BotView, Receiver<Command>, SyncSender<Answer>) {
    let (link, commands, answers) = BotsLink::test_pair();
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    view.shown_pass(None);
    let seq = commands
        .try_iter()
        .find_map(|cmd| match cmd {
            Command::Read { seq, .. } => Some(seq),
            _ => None,
        })
        .unwrap();
    answers
        .send(Answer::Read {
            seq,
            bot: bot.id,
            result: Ok(bot.clone()),
        })
        .unwrap();
    view.shown_pass(None);
    view.show_context_library(true);
    view.shown_pass(None);
    let seq = commands
        .try_iter()
        .find_map(|cmd| match cmd {
            Command::ReadLibrary { seq, .. } => Some(seq),
            _ => None,
        })
        .unwrap();
    answers
        .send(Answer::Library {
            seq,
            bot: bot.id,
            result: Ok(BotLibraryView {
                bot_id: bot.id,
                revision: bot.revision,
                role: bot.config.role.clone(),
                knowledge: pages,
                memory,
            }),
        })
        .unwrap();
    view.shown_pass(None);
    (view, commands, answers)
}
fn install(
    v: &mut MainView,
    bot: &Bot,
    pages: Vec<BotPage>,
    memory: Option<BotPage>,
) -> (Receiver<Command>, SyncSender<Answer>) {
    let (view, commands, answers) = loaded(bot, pages, memory);
    v.tabs_mut().push(Tab::bot(view));
    v.context.set_immediately(true);
    v.relayout();
    v.sync_bot_context();
    (commands, answers)
}
fn rect(v: &MainView, key: Key) -> Rect {
    v.bot_context
        .with_frame(|frame| frame.node(key).unwrap().rect)
        .unwrap()
}
fn click(v: &mut MainView, ws: &mut koru_core::schema::WorkspaceDoc, key: Key) {
    let r = rect(v, key);
    assert!(v.mouse_down(ws, r.x + r.w / 2.0, r.y + r.h / 2.0, false));
    assert!(v.mouse_up(ws));
}
fn press(v: &mut MainView, ws: &mut koru_core::schema::WorkspaceDoc, key: NamedKey) -> bool {
    v.key(ws, &InputKey::Named(key), None, ModifiersState::empty())
}

#[test]
fn context_library_subject_excludes_creation_trash_and_stale_document() {
    let (mut v, mut w) = on_document();
    let document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(document);
    v.active_document = Some(document);
    assert!(v.history_target().is_some());
    let first = bot();
    let _first_link = install(&mut v, &first, vec![page(&first)], None);
    assert_eq!(v.context_sections(), [Section::Library, Section::Routines]);
    assert!(
        v.history_target().is_none(),
        "last document is not Bot authority"
    );
    assert!(v.bot_context_current());
    let old = v.bot_context.occurrence(MANAGE).unwrap();
    let second = bot();
    let _second_link = install(&mut v, &second, vec![], None);
    assert_ne!(v.bot_context.occurrence(MANAGE), Some(old));
    let target = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        old,
        ControlPart::ROOT,
    );
    assert!(!v.accessibility_action(&mut w.ws, target, accesskit::Action::Click, None));
    v.tabs_mut().select(1);
    v.sync_bot_context();
    assert!(v.history_target().is_some());
    assert!(!v.context_sections().contains(&Section::Library));
    assert_eq!(v.context_view.section(), Section::History);
    let (link, _, _) = BotsLink::test_pair();
    v.tabs_mut().push(Tab::bot(BotView::creating_with_link(
        first.home_workspace,
        link,
    )));
    v.sync_bot_context();
    assert!(!v.context_has_sections());
    assert!(!v.bot_context_current());
}

#[test]
fn context_library_actions_revalidate_subject() {
    let (mut v, mut w) = on_document();
    let first = bot();
    let _link = install(&mut v, &first, vec![], None);
    let r = rect(&v, MANAGE);
    assert!(v.mouse_down(&mut w.ws, r.x + 2.0, r.y + 2.0, false));
    let second = bot();
    let _other = install(&mut v, &second, vec![], None);
    assert!(v.mouse_up(&mut w.ws));
    assert!(matches!(v.active_view(),TabView::Bot(view) if !view.settings_shown()));
    click(&mut v, &mut w.ws, MANAGE);
    assert!(matches!(v.active_view(),TabView::Bot(view) if view.settings_shown()));
    assert!(v.bot_context.focus.is_none());
    let role = v.bot_context.data.as_ref().unwrap().pages[0].clone();
    v.context.set_immediately(true);
    v.sync_bot_context();
    v.bot_context.focus = Some(role.key);
    v.bot_context.reveal();
    click(&mut v, &mut w.ws, role.key);
    let opened = match v.active_view() {
        TabView::Bot(view) => view.take_role_request(),
        _ => None,
    };
    assert_eq!(opened, Some(role.page));
}

#[test]
fn context_library_keyboard_and_narrow_disclosure() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let pages = (0..16).map(|_| page(&bot)).collect();
    let _link = install(&mut v, &bot, pages, None);
    View::set_viewport(&mut v, 420.0, 400.0);
    v.context.set_immediately(true);
    v.relayout();
    v.sync_bot_context();
    assert!(v.bot_context.floating);
    for _ in 0..10 {
        press(&mut v, &mut w.ws, NamedKey::F6);
        if v.bot_context.focus.is_some() {
            break;
        }
    }
    assert!(v.bot_context.focus.is_some());
    for _ in 0..5 {
        press(&mut v, &mut w.ws, NamedKey::F6);
        assert!(v.bot_context.focus.is_some());
    }
    let outside = (1.0, 200.0);
    v.pointer = outside;
    let scroll = v.bot_context.scroll;
    View::scroll_by(&mut v, winit::event::MouseScrollDelta::LineDelta(0.0, -3.0));
    assert_eq!(v.bot_context.scroll, scroll, "outside wheel is consumed");
    assert!(View::context_menu(&mut v, &mut w.ws, outside.0, outside.1));
    assert!(
        v.menu.is_none(),
        "no chat/sidebar context menu beneath disclosure"
    );
    assert!(v.key(
        &mut w.ws,
        &InputKey::Character("x".into()),
        Some("x"),
        ModifiersState::empty()
    ));
    let before = v.bot_context.focus;
    v.key(
        &mut w.ws,
        &InputKey::Named(NamedKey::Tab),
        None,
        ModifiersState::CONTROL,
    );
    assert_eq!(
        v.bot_context.focus, before,
        "Ctrl+I-as-Tab must not traverse or activate"
    );
    for _ in 0..18 {
        press(&mut v, &mut w.ws, NamedKey::Tab);
    }
    assert!(v.bot_context.scroll > 0.0);
    let focused = rect(&v, v.bot_context.focus.unwrap());
    assert!(
        focused.y >= v.bot_context.area.y
            && focused.y + focused.h <= v.bot_context.area.y + v.bot_context.area.h + 1.0
    );
    assert!(press(&mut v, &mut w.ws, NamedKey::Escape));
    assert!(!v.context.is_open());
    v.next_context_section();
    assert!(v.context.is_open());
    assert!(v.mouse_down(&mut w.ws, 1.0, 200.0, false));
    assert!(!v.context.is_open());
    assert!(
        v.mouse_up(&mut w.ws),
        "outside dismissal consumes release too"
    );
}

#[test]
fn context_library_same_tab_replacement_retires_actions_and_focus_return() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let _old = install(&mut v, &bot, vec![], None);
    v.focus_bot_context();
    let old_instance = v.bot_context.data.as_ref().unwrap().instance;
    let old_occurrence = v.bot_context.occurrence(MANAGE).unwrap();
    let r = rect(&v, MANAGE);
    v.mouse_down(&mut w.ws, r.x + 2.0, r.y + 2.0, false);
    let (replacement, _commands, _answers) = loaded(&bot, vec![], None);
    v.tabs_mut().active_mut().view = TabView::Bot(replacement);
    assert!(v.mouse_up(&mut w.ws));
    assert_ne!(v.bot_context.data.as_ref().unwrap().instance, old_instance);
    assert!(v.context_return_tab.is_none());
    let target = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        old_occurrence,
        ControlPart::ROOT,
    );
    assert!(!v.accessibility_action(&mut w.ws, target, accesskit::Action::Click, None));
    assert!(matches!(v.active_view(),TabView::Bot(view) if !view.settings_shown()));
}

#[test]
fn context_library_resize_and_normal_toggle_restore_the_same_view_focus() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let _link = install(&mut v, &bot, vec![], None);
    let prior = match v.active_view() {
        TabView::Bot(view) => {
            view.focus_header();
            view.focus()
        }
        _ => None,
    };
    assert!(prior.is_some());
    View::set_viewport(&mut v, 420.0, 400.0);
    assert!(
        v.bot_context.floating && v.bot_context.focus.is_some(),
        "resize acquires disclosure focus"
    );
    assert!(matches!(v.active_view(),TabView::Bot(view) if view.focus().is_none()));
    assert!(v.key(
        &mut w.ws,
        &InputKey::Character("j".into()),
        Some("j"),
        koru_keys::PRIMARY
    ));
    assert!(!v.context.is_open());
    assert!(matches!(v.active_view(),TabView::Bot(view) if view.focus() == prior));
}

#[test]
fn context_library_new_canonical_revision_retires_old_press_and_action() {
    let (mut v, mut w) = on_document();
    let mut bot = bot();
    let selected = page(&bot);
    let (commands, answers) = install(&mut v, &bot, vec![selected.clone()], None);
    let page_key = v.bot_context.data.as_ref().unwrap().pages[1].key;
    let old_occurrence = v.bot_context.occurrence(page_key).unwrap();
    let r = rect(&v, page_key);
    v.mouse_down(&mut w.ws, r.x + 2.0, r.y + 2.0, false);
    // A remove/re-add may be observed only at its later canonical revision.
    commands.try_iter().for_each(drop);
    if let TabView::Bot(view) = v.active_view() {
        view.perform_context_library(Action::Reload);
        view.shown_pass(None);
    }
    let sent: Vec<_> = commands.try_iter().collect();
    let read = sent
        .iter()
        .find_map(|cmd| match cmd {
            Command::Read { seq, .. } => Some(*seq),
            _ => None,
        })
        .unwrap();
    let library = sent
        .iter()
        .find_map(|cmd| match cmd {
            Command::ReadLibrary { seq, .. } => Some(*seq),
            _ => None,
        })
        .unwrap();
    bot.revision += 2;
    answers
        .send(Answer::Read {
            seq: read,
            bot: bot.id,
            result: Ok(bot.clone()),
        })
        .unwrap();
    answers
        .send(Answer::Library {
            seq: library,
            bot: bot.id,
            result: Ok(BotLibraryView {
                bot_id: bot.id,
                revision: bot.revision,
                role: bot.config.role.clone(),
                knowledge: vec![selected],
                memory: None,
            }),
        })
        .unwrap();
    if let TabView::Bot(view) = v.active_view() {
        view.shown_pass(None);
    }
    assert!(v.mouse_up(&mut w.ws));
    let opened = match v.active_view() {
        TabView::Bot(view) => view.take_role_request(),
        _ => None,
    };
    assert!(opened.is_none());
    let target = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        old_occurrence,
        ControlPart::ROOT,
    );
    assert!(!v.accessibility_action(&mut w.ws, target, accesskit::Action::Click, None));
}

#[test]
fn context_library_reload_uses_the_existing_reader() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let (commands, _answers) = install(&mut v, &bot, vec![], None);
    commands.try_iter().for_each(drop);
    click(&mut v, &mut w.ws, RELOAD);
    if let TabView::Bot(view) = v.active_view() {
        view.shown_pass(None);
    }
    assert_eq!(
        commands
            .try_iter()
            .filter(|cmd| matches!(cmd, Command::ReadLibrary { .. }))
            .count(),
        1
    );
    if let TabView::Bot(view) = v.active_view() {
        view.set_settings_open(true);
        view.shown_pass(None);
    }
    assert_eq!(
        commands
            .try_iter()
            .filter(|cmd| matches!(cmd, Command::ReadLibrary { .. }))
            .count(),
        0,
        "Settings shares the in-flight read"
    );
    v.context.set_immediately(false);
    v.sync_bot_context();
    if let TabView::Bot(view) = v.active_view() {
        view.set_settings_open(false);
    }
    assert!(!v.bot_context_current());
}

#[test]
fn context_library_bound_memory_and_geometry_are_host_fed() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let memory = page(&bot);
    let _link = install(
        &mut v,
        &bot,
        vec![bot.config.role.clone()],
        Some(memory.clone()),
    );
    let data = v.bot_context.data.as_ref().unwrap();
    assert_eq!(data.pages.len(), 3);
    assert_ne!(
        data.pages[0].key, data.pages[1].key,
        "same page has distinct purposes"
    );
    assert_eq!(data.pages[2].page, memory);
    let window = WindowSlot::from_raw(7);
    v.seat_accessibility(window);
    let cx = super::super::accessibility::Shell {
        window,
        view: ViewSlot::from_raw(3),
        viewport: v.viewport,
        lang: koru_i18n::Lang::En,
    };
    let semantic = v.bot_context_semantics(&cx).unwrap();
    assert_eq!(
        semantic.children.len(),
        7,
        "two selectors, three links, refresh and manage"
    );
    for (index, key, selected) in [
        (0, crate::bots::view::context_library::LIBRARY_SECTION, true),
        (
            1,
            crate::bots::view::context_library::ROUTINES_SECTION,
            false,
        ),
    ] {
        assert_eq!(
            semantic.children[index].occurrence,
            v.context_occurrence(&cx, key).unwrap()
        );
        assert_eq!(semantic.children[index].selected, Some(selected));
    }
    let manage = semantic.children.last().unwrap().occurrence;
    assert!(v.accessibility_action(&mut w.ws, manage, accesskit::Action::Focus, None));
    assert_eq!(v.bot_context.focus, Some(MANAGE));
    assert!(
        v.bot_context
            .with_frame(|frame| frame.node(BODY).unwrap().viewport.is_some())
            .unwrap()
    );
}

#[test]
fn context_library_yields_focus_and_actions_to_a_higher_modal() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let _link = install(&mut v, &bot, vec![], None);
    View::set_viewport(&mut v, 420.0, 400.0);
    let occurrence = v.bot_context.occurrence(MANAGE).unwrap();
    assert!(View::report_error(
        &mut v,
        koru_workspace_host::ErrorReport::new("Test refusal", "Nothing changed.")
    ));
    v.blur_bot_context();
    View::set_viewport(&mut v, 419.0, 400.0);
    assert!(
        v.bot_context.focus.is_none(),
        "geometry maintenance cannot take modal focus"
    );
    let target = Occurrence::instance(
        WindowSlot::from_raw(1),
        ViewSlot::from_raw(1),
        SURFACE,
        occurrence,
        ControlPart::ROOT,
    );
    assert!(!v.accessibility_action(&mut w.ws, target, accesskit::Action::Click, None));
    assert!(press(&mut v, &mut w.ws, NamedKey::Escape));
    assert!(v.error.is_none());
    assert!(
        v.context.is_open(),
        "Escape answered the error, not the Library underneath"
    );
}

#[test]
fn context_library_accessible_bounds_follow_clipping_and_focus_reveals() {
    let (mut v, mut w) = on_document();
    let bot = bot();
    let _link = install(&mut v, &bot, (0..16).map(|_| page(&bot)).collect(), None);
    View::set_viewport(&mut v, 420.0, 350.0);
    let window = WindowSlot::from_raw(7);
    v.seat_accessibility(window);
    let cx = super::super::accessibility::Shell {
        window,
        view: ViewSlot::from_raw(3),
        viewport: v.viewport,
        lang: koru_i18n::Lang::En,
    };
    let first_key = v.bot_context.data.as_ref().unwrap().pages[0].key;
    let first_rect = rect(&v, first_key);
    let offset = first_rect.y - v.bot_context.area.y + first_rect.h / 2.0;
    v.bot_context.scroll_by(offset);
    let semantic = v.bot_context_semantics(&cx).unwrap();
    let first_occurrence = v.context_occurrence(&cx, first_key).unwrap();
    assert!(
        semantic
            .children
            .iter()
            .find(|child| child.occurrence == first_occurrence)
            .unwrap()
            .bounds
            .unwrap()
            .h
            < first_rect.h,
        "partially clipped action uses visible bounds"
    );
    assert!(
        semantic.children.iter().any(|child| child.bounds.is_none()),
        "fully clipped rows retain identity without false bounds"
    );
    for child in &semantic.children {
        if let Some(bounds) = child.bounds {
            let panel = v.bot_context.area;
            assert!(bounds.x >= panel.x && bounds.y >= panel.y);
            assert!(
                bounds.x + bounds.w <= panel.x + panel.w + 0.1
                    && bounds.y + bounds.h <= panel.y + panel.h + 0.1
            );
        }
    }
    let target = semantic.children.last().unwrap().occurrence;
    assert!(v.accessibility_action(&mut w.ws, target, accesskit::Action::Focus, None));
    let revealed = v.bot_context_semantics(&cx).unwrap();
    let bounds = revealed.children.last().unwrap().bounds.unwrap();
    assert!(bounds.h > 0.0);
    assert!(v.bot_context.scroll > offset);
}
