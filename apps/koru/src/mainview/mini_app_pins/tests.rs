// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::mainview::harness::container;
#[test]
fn save_receipt_to_pin_write_has_no_close_gap_and_failure_retries_exact_version() {
    let (mut view, _) = container();
    let (pin_commands, pin_answers) = fake(&mut view);
    view.open_mini_app_fixture();
    let (io, save_commands, save_answers) = crate::mini_app::artifact::Io::in_memory();
    view.mini_app_artifacts = io;
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_pin();
    assert!(view.mini_app_save_close_veto(None));
    view.maintain_mini_app_artifacts();
    let crate::mini_app::artifact::Command::Save {
        tab,
        profile,
        operation,
    } = save_commands.try_recv().unwrap()
    else {
        panic!()
    };
    let version = koru_artifact_wire::ArtifactVersion {
        id: koru_artifact_wire::ArtifactVersionId::mint(),
        family: operation.family,
        ordinal: koru_artifact_wire::Ordinal::FIRST,
        blob: operation.registration.blob.clone(),
        size_bytes: operation.bytes.len() as u64,
        mime: operation.registration.mime.clone(),
        filename: None,
        produced_by: None,
        registration_id: operation.registration.registration_id,
        derived_from: operation.registration.derived_from,
        created_at_ms: 0,
    };
    save_answers
        .send(crate::mini_app::artifact::Answer::Saved {
            tab,
            profile,
            workspace: operation.snapshot.bindings.workspace,
            registration: operation.registration.registration_id,
            captured_revision: operation.captured_revision,
            result: Ok(version.clone()),
        })
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_artifacts();
    assert!(view.mini_app_save_close_veto(None));
    view.maintain_mini_app_pins();
    assert!(view.mini_app_save_close_veto(None));
    let Command::Change { request, epoch, .. } = pin_commands.try_recv().unwrap() else {
        panic!()
    };
    let Mutation::Add { pin } = &request.mutation else {
        panic!()
    };
    assert_eq!(pin.version, version.id.to_storage());
    pin_answers
        .send(Answer {
            scope: request.scope.clone(),
            epoch,
            tab: Some(tab),
            mutation: Some(request.mutation.clone()),
            result: Err("uncertain".into()),
        })
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert!(!view.mini_app_save_close_veto(None));
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_pin();
    view.maintain_mini_app_pins();
    let Command::Change { request: retry, .. } = pin_commands.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(retry, request);
    assert!(save_commands.try_recv().is_err());
}
fn pin(n: u128) -> Pin {
    Pin {
        id: PinId(n),
        family: "33333333-3333-4333-8333-333333333333".into(),
        version: format!("44444444-4444-4444-8444-{n:012x}"),
        title: format!("App {n}"),
    }
}
fn fake(view: &mut MainView) -> (Receiver<Command>, SyncSender<Answer>) {
    let (send, commands) = mpsc::sync_channel(16);
    let (answers, receive) = mpsc::sync_channel(16);
    view.mini_app_pins.sender = Some(send);
    view.mini_app_pins.answers = Some(receive);
    view.mini_app_pins.scope = view.pin_scope();
    view.mini_app_pins.epoch = 1;
    (commands, answers)
}
fn answer(
    view: &MainView,
    result: std::result::Result<Vec<Pin>, String>,
    mutation: Option<Mutation>,
) -> Answer {
    Answer {
        scope: view.pin_scope().unwrap(),
        epoch: view.mini_app_pins.epoch,
        tab: None,
        mutation,
        result,
    }
}
#[test]
fn stale_activation_and_closed_menu_do_not_reopen_or_replace_projection() {
    let (mut view, _) = container();
    let (_, answers) = fake(&mut view);
    view.explore.replace_mini_app_pins(vec![pin(1)]);
    view.mini_app_pins.highlight = Some(PinId(1));
    view.mini_app_pins.epoch = 3;
    let mut stale = answer(&view, Ok(vec![pin(2)]), None);
    stale.epoch = 1;
    answers.send(stale).unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert_eq!(view.explore.mini_app_pins, vec![pin(1)]);
    answers
        .send(answer(&view, Ok(vec![pin(3)]), None))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert_eq!(view.explore.mini_app_pins, vec![pin(3)]);
    assert!(view.mini_app_pins.menu.is_none());
}
#[test]
fn every_accepted_write_retains_close_protection_and_failed_rail_retry_is_exact() {
    let (mut view, _) = container();
    let (commands, answers) = fake(&mut view);
    view.explore.replace_mini_app_pins(vec![pin(1), pin(2)]);
    view.mini_app_pins.authoritative = view.explore.mini_app_pins.clone();
    let movement = Mutation::Move {
        id: PinId(1),
        to: koru_settings::pins::Placement::Last,
    };
    assert!(view.change_pin(movement.clone(), None));
    assert!(view.change_pin(Mutation::Remove { id: PinId(2) }, None));
    assert_eq!(view.mini_app_pins.pending_writes, 2);
    assert!(view.mini_app_save_close_veto(None));
    answers
        .send(answer(
            &view,
            Err("uncertain".into()),
            Some(movement.clone()),
        ))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert_eq!(view.mini_app_pins.pending_writes, 1);
    assert!(view.mini_app_save_close_veto(None));
    let frozen = view.mini_app_pins.failed_rail.clone().unwrap();
    assert_eq!(frozen.mutation, movement);
    answers
        .send(answer(
            &view,
            Ok(vec![pin(1)]),
            Some(Mutation::Remove { id: PinId(2) }),
        ))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert_eq!(view.mini_app_pins.pending_writes, 0);
    assert!(!view.mini_app_save_close_veto(None));
    while commands.try_recv().is_ok() {}
    view.choose_pin(Choice::Retry);
    let Command::Change { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(request, frozen);
}
#[test]
fn pin_overflow_pages_use_full_capacity_and_reorder_keeps_stable_highlight() {
    let (mut view, _) = container();
    let _ = fake(&mut view);
    view.explore
        .replace_mini_app_pins((1..=17).map(pin).collect());
    assert!(view.open_mini_app_pins(Some(PinId(17))));
    let size = view.pin_page_size();
    let page = view.mini_app_pins.page;
    assert!(page > 0);
    view.choose_pin(Choice::Page(page - 1, String::new()));
    assert_eq!(
        view.mini_app_pins.highlight,
        Some(PinId(((page - 1) * size + 1) as u128))
    );
    view.open_mini_app_pins(Some(PinId(17)));
    let before = view.explore.rail_modes();
    assert!(view.pin_menu_key(
        &Key::Named(NamedKey::ArrowUp),
        None,
        ModifiersState::CONTROL | ModifiersState::SHIFT
    ));
    assert_eq!(view.explore.mini_app_pins[15].id, PinId(17));
    assert_eq!(view.mini_app_pins.highlight, Some(PinId(17)));
    assert!(view.mini_app_pins.menu.is_some());
    assert_eq!(view.explore.rail_modes(), before);
    let cx = accessibility::Shell {
        window: koru_workspace_host::accessibility::WindowSlot::from_raw(1),
        view: koru_workspace_host::accessibility::ViewSlot::from_raw(1),
        viewport: view.viewport,
        lang: crate::format::lang(),
    };
    let tree = view.pin_menu_semantics(&cx).unwrap();
    let row = tree
        .children
        .iter()
        .find(|n| n.occurrence.part == ControlPart::new("pin-menu-open"))
        .unwrap();
    assert_ne!(row.occurrence.part, ControlPart::new("rail-pin"));
    view.pin_menu_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty());
    assert!(view.mini_app_pins.menu.is_none());
}
#[test]
fn scope_switch_discards_uncommitted_reorder_and_drop_stops_worker_lifetime() {
    let (mut view, _) = container();
    let (commands, _) = fake(&mut view);
    view.explore.replace_mini_app_pins(vec![pin(1), pin(2)]);
    view.explore.nudge_pin(view.rail_area(), PinId(1), false);
    view.mini_app_pins.scope = None;
    view.maintain_mini_app_pins();
    assert!(view.explore.mini_app_pins.is_empty());
    assert!(view.explore.take_pin_move().is_none());
    assert!(matches!(commands.try_recv(), Ok(Command::Activate { .. })));
    assert!(commands.try_recv().is_err());
    let state = State::default();
    let stopped = state.stop.clone();
    drop(state);
    assert!(stopped.load(Ordering::Acquire));
}
#[test]
fn pin_save_dispatch_failure_releases_guard_and_preserves_registration_for_retry() {
    let (mut view, _) = container();
    view.open_mini_app_fixture();
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    app.request_pin();
    let registration = app.pin.saving.unwrap();
    assert!(app.pin.pending());
    view.maintain_mini_app_artifacts();
    let TabView::MiniApp(app) = &mut view.tabs_mut().active_mut().view else {
        panic!()
    };
    assert!(app.pin.failed);
    assert!(!app.pin.pending());
    app.request_pin();
    assert_eq!(app.pin.saving, Some(registration));
    assert!(app.artifact.requested);
}
#[test]
fn authoritative_reorder_and_refusal_keep_the_focused_pin_and_menu_action() {
    use crate::panes::sidebar::apps_rail::Hit;
    let (mut view, _) = container();
    let (_, answers) = fake(&mut view);
    view.explore.replace_mini_app_pins(vec![pin(1), pin(2)]);
    view.mini_app_pins.authoritative = view.explore.mini_app_pins.clone();
    view.retain_pin_focus(PinId(1));
    view.explore.nudge_pin(view.rail_area(), PinId(1), false);
    view.retain_pin_focus(PinId(1));
    view.maintain_mini_app_pins();
    answers
        .send(answer(
            &view,
            Err("refused".into()),
            Some(Mutation::Move {
                id: PinId(1),
                to: koru_settings::pins::Placement::Last,
            }),
        ))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert_eq!(view.focused_rail_identity(), Some(Hit::Pin(PinId(1))));
    view.open_mini_app_pins(Some(PinId(1)));
    let menu = view.mini_app_pins.menu.as_mut().unwrap();
    menu.hover = menu
        .items()
        .iter()
        .position(|c| matches!(c,Choice::Unpin(p)if p.id==PinId(1)));
    answers
        .send(answer(&view, Ok(vec![pin(2), pin(1)]), None))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    let menu = view.mini_app_pins.menu.as_ref().unwrap();
    assert!(
        matches!(menu.hover.and_then(|i|menu.items().get(i)),Some(Choice::Unpin(p))if p.id==PinId(1))
    );
    let expected = if view
        .explore
        .rail_controls(view.rail_area())
        .contains(&Hit::Pin(PinId(1)))
    {
        Hit::Pin(PinId(1))
    } else {
        Hit::PinOverflow
    };
    assert_eq!(view.focused_rail_identity(), Some(expected));
    assert_eq!(view.mini_app_pins.highlight, Some(PinId(1)));
}
#[test]
fn normal_height_and_empty_unknown_unpin_keep_keyboard_retry_reachable() {
    use crate::panes::sidebar::apps_rail::Hit;
    let (mut view, mut workspace) = container();
    let (commands, answers) = fake(&mut view);
    view.explore.replace_mini_app_pins(vec![pin(1)]);
    view.mini_app_pins.authoritative = vec![pin(1)];
    let mutation = Mutation::Remove { id: PinId(1) };
    view.change_pin(mutation.clone(), None);
    answers
        .send(answer(
            &view,
            Err("unknown completion".into()),
            Some(mutation.clone()),
        ))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    answers
        .send(answer(&view, Ok(vec![]), None))
        .unwrap_or_else(|_| panic!());
    view.maintain_mini_app_pins();
    assert!(view.explore.mini_app_pins.is_empty());
    let controls = view.explore.rail_controls(view.rail_area());
    let i = controls
        .iter()
        .position(|h| *h == Hit::PinOverflow)
        .unwrap();
    view.chrome_focus = Some(chrome_focus::ChromeStop::Apps(i));
    assert!(view.chrome_focus_key(
        &mut workspace.ws,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    let menu = view.mini_app_pins.menu.as_ref().unwrap();
    assert!(matches!(
        menu.hover.and_then(|i| menu.items().get(i)),
        Some(Choice::Retry)
    ));
    while commands.try_recv().is_ok() {}
    view.pin_menu_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
    let Command::Change { request, .. } = commands.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(request.mutation, mutation);
}
