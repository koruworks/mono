// SPDX-License-Identifier: MPL-2.0

//! A chat composer's dictation through the container (koru-ai D103), with the
//! shell's controller played by hand — under test it reaches no worker, so
//! the events a worker would send are fed in directly.

use std::time::Instant;

use koru_dictation::{Dictation, EndReason, Event, State, Transcript};
use koru_draw::geometry::Rect;
use koru_workspace_host::View;

use crate::mainview::MainView;
use crate::mainview::harness::{consenting_dictation, on_document};
use crate::mainview::tab::Tab;

fn frame(v: &mut MainView, w: &koru_workspace::Workspace) {
    v.draw_frame(
        &mut vello::Scene::new(),
        w,
        &koru_draw::ImageCache::new(),
        vello::kurbo::Affine::IDENTITY,
        false,
        (-1.0, -1.0),
    );
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// Route everything the controller has to say, and what it shows, the way
/// the shell's `service_dictation` does.
fn service(v: &mut MainView, w: &mut koru_workspace::Workspace, controller: &mut Dictation) {
    for action in controller.pump(Instant::now()) {
        View::dictate(v, &mut w.ws, &action);
    }
    View::dictation_status(v, controller.status());
}

/// What a worker would say, for the session the controller is running.
fn say(controller: &mut Dictation, event: impl FnOnce(koru_dictation::SessionId, u64) -> Event) {
    let session = controller.session().expect("a session");
    let generation = controller.generation().expect("a generation");
    controller.observe(event(session, generation));
}

/// The shell's half of a press on the microphone: it asked, so begin.
fn begin(v: &mut MainView, w: &mut koru_workspace::Workspace) -> Dictation {
    assert!(
        View::take_wants_dictation(v),
        "the microphone asked the shell"
    );
    assert!(!View::take_wants_dictation(v), "once");
    let mut controller = consenting_dictation();
    let action = controller.press(Instant::now()).expect("a session opens");
    let accepted = View::dictate(v, &mut w.ws, &action);
    controller.began(accepted);
    assert!(accepted, "the composer takes it");
    say(&mut controller, |session, generation| Event::State {
        session,
        generation,
        state: State::Listening,
    });
    service(v, w, &mut controller);
    controller
}

/// **The pane's microphone records into the pane's composer**: the footer
/// becomes the recording row (and no floating chip appears), partial words
/// stay provisional, ■ and the composer stop, ✕ cancels, and the stopped
/// recording's words are text in the composer afterwards.
#[test]
fn the_panes_microphone_records_into_its_composer() {
    let (mut v, mut w) = on_document();
    assert!(v.toggle_ai());
    v.ai_chat.offer_dictation(true);
    frame(&mut v, &w);
    let mic = v
        .ai_chat
        .microphone_for_host_test()
        .expect("the microphone is in the pane's footer");
    let (x, y) = centre(mic);
    assert!(View::mouse_down(&mut v, &mut w.ws, x, y, false));
    View::mouse_up(&mut v, &mut w.ws);
    let mut controller = begin(&mut v, &mut w);

    assert!(
        View::dictation_records(&v),
        "a chat's dictation is a recording"
    );
    assert!(v.ai_chat.dictating(), "the footer shows the recording");
    assert!(v.listening.is_none(), "and no floating chip");
    assert!(v.dictation_deadline().is_none(), "nor its meter's clock");
    frame(&mut v, &w);
    let (cancel, stop) = v
        .ai_chat
        .dictation_buttons_for_host_test()
        .expect("✕ and ■ in the footer");
    let (cx, cy) = centre(cancel);
    assert!(View::dictation_cancel_hit(&v, cx, cy), "✕ cancels");
    assert!(!View::dictation_hit(&v, cx, cy));
    let (sx, sy) = centre(stop);
    assert!(View::dictation_hit(&v, sx, sy), "■ stops");
    assert!(!View::dictation_cancel_hit(&v, sx, sy));

    say(&mut controller, |session, generation| Event::Partial {
        session,
        generation,
        text: Transcript::new("Draft the"),
    });
    service(&mut v, &mut w, &mut controller);
    assert_eq!(
        v.ai_chat.composer_text_for_host_test(),
        "",
        "partial words are provisional, not the message"
    );

    assert!(controller.stop(Instant::now()), "■: the shell stops");
    service(&mut v, &mut w, &mut controller);
    assert!(
        v.ai_chat.dictating(),
        "still showing while the words finish"
    );
    say(&mut controller, |session, generation| Event::Final {
        session,
        generation,
        text: Transcript::new("Draft the reply"),
    });
    say(&mut controller, |session, generation| Event::Ended {
        session,
        generation,
        reason: EndReason::Completed,
    });
    service(&mut v, &mut w, &mut controller);
    assert_eq!(v.ai_chat.composer_text_for_host_test(), "Draft the reply");
    assert!(!v.ai_chat.dictating(), "the row is gone with the session");
    assert!(!View::dictation_records(&v));
}

/// **✕ throws away only what this recording put in.**
#[test]
fn cancel_keeps_what_was_typed_before_the_recording() {
    let (mut v, mut w) = on_document();
    assert!(v.toggle_ai());
    v.ai_chat.offer_dictation(true);
    frame(&mut v, &w);
    let (x, y) = centre(v.ai_chat.microphone_for_host_test().unwrap());
    View::mouse_down(&mut v, &mut w.ws, x, y, false);
    View::mouse_up(&mut v, &mut w.ws);
    // Typed before the recording begins, through the window's own keys.
    for c in "Typed first. ".chars() {
        let s = c.to_string();
        View::key(
            &mut v,
            &mut w.ws,
            &winit::keyboard::Key::Character(s.as_str().into()),
            Some(&s),
            winit::keyboard::ModifiersState::empty(),
        );
    }
    assert_eq!(v.ai_chat.composer_text_for_host_test(), "Typed first. ");
    let mut controller = begin(&mut v, &mut w);
    say(&mut controller, |session, generation| Event::Partial {
        session,
        generation,
        text: Transcript::new("spoken words"),
    });
    service(&mut v, &mut w, &mut controller);

    // The shell's answer to a press on ✕: an interruption, which discards.
    controller.interrupt(koru_dictation::Interruption::LocalEvent);
    service(&mut v, &mut w, &mut controller);
    assert_eq!(v.ai_chat.composer_text_for_host_test(), "Typed first. ");
    assert!(!v.ai_chat.dictating());
}

/// **A chat tab's composer takes a dictation nothing else would**, from its
/// own microphone, and shows the recording in its own footer.
#[test]
fn a_chat_tabs_microphone_records_into_that_tab() {
    let (mut v, mut w) = on_document();
    v.tabs_mut().push(Tab::ai_chat());
    let tab = v.tabs().active();
    if let crate::mainview::tab::TabView::AiChat(view) = &mut v.tabs_mut().active_mut().view {
        view.dictation_surface_mut().offer_dictation(true);
    }
    frame(&mut v, &w);
    let origin = v.focused_view_rect();
    let mic = match &v.tabs().active_tab().view {
        crate::mainview::tab::TabView::AiChat(view) => view
            .dictation_surface()
            .microphone_for_host_test()
            .expect("the tab's footer has a microphone"),
        _ => panic!("a chat tab"),
    };
    let (x, y) = centre(mic);
    assert!(View::mouse_down(
        &mut v,
        &mut w.ws,
        x + origin.x,
        y + origin.y,
        false
    ));
    View::mouse_up(&mut v, &mut w.ws);
    let mut controller = begin(&mut v, &mut w);

    assert_eq!(v.chat_dictation(), Some(super::ChatDictation::Tab(tab)));
    assert!(View::dictation_records(&v));
    say(&mut controller, |session, generation| Event::Partial {
        session,
        generation,
        text: Transcript::new("hello"),
    });
    service(&mut v, &mut w, &mut controller);
    let showing = |v: &MainView| match &v.tabs().active_tab().view {
        crate::mainview::tab::TabView::AiChat(view) => view.dictation_surface().dictating(),
        _ => false,
    };
    assert!(showing(&v), "the tab's footer records");
    assert!(!v.ai_chat.dictating(), "and the pane's does not");
    frame(&mut v, &w);
    let composer_text = |v: &mut MainView| match &mut v.tabs_mut().active_mut().view {
        crate::mainview::tab::TabView::AiChat(view) => {
            view.dictation_surface_mut().dictation_composer().text()
        }
        _ => String::new(),
    };
    assert_eq!(composer_text(&mut v), "", "provisional");

    assert!(controller.stop(Instant::now()));
    say(&mut controller, |session, generation| Event::Final {
        session,
        generation,
        text: Transcript::new("hello world"),
    });
    say(&mut controller, |session, generation| Event::Ended {
        session,
        generation,
        reason: EndReason::Completed,
    });
    service(&mut v, &mut w, &mut controller);
    assert_eq!(composer_text(&mut v), "hello world");
    assert!(!showing(&v));
}

/// A document's dictation is not a recording: it keeps the settled rule that
/// an ordinary local event discards it, and the floating chip.
#[test]
fn a_documents_dictation_is_not_a_recording() {
    let (mut v, mut w) = on_document();
    let mut controller = consenting_dictation();
    let action = controller.press(Instant::now()).unwrap();
    let accepted = View::dictate(&mut v, &mut w.ws, &action);
    controller.began(accepted);
    assert!(accepted);
    View::dictation_status(&mut v, controller.status());
    assert!(!View::dictation_records(&v));
    assert!(v.listening.is_some(), "the chip");
    assert!(!View::dictation_cancel_hit(&v, 10.0, 10.0));
}

/// **The pane's microphone chooser is the Settings page's**: the chevron
/// beside the microphone opens the panel, which asks the shell for the
/// device list on its worker thread (`AskVoiceDevices`) — never here — and
/// a device chosen in it is the *Voice → Microphone* preference.
#[test]
fn the_panes_microphone_chooser_keeps_the_voice_preference() {
    let (mut v, mut w) = on_document();
    assert!(v.toggle_ai());
    v.ai_chat.offer_dictation(true);
    v.ai_chat.offer_microphone_choice(true);
    frame(&mut v, &w);
    let (x, y) = centre(v.ai_chat.microphone_for_host_test().unwrap());
    v.ai_chat.mouse_move(x, y);
    let chevron = v
        .ai_chat
        .microphone_chooser_for_host_test()
        .expect("the chevron shows on the microphone");
    let (x, y) = centre(chevron);
    v.ai_chat.mouse_move(x, y);
    v.pane_requests.clear();
    assert!(View::mouse_down(&mut v, &mut w.ws, x, y, false));
    View::mouse_up(&mut v, &mut w.ws);
    assert!(v.ai_chat.microphone_panel_open());
    assert!(
        v.pane_requests
            .iter()
            .any(|request| matches!(request, koru_workspace_host::PaneRequest::AskVoiceDevices)),
        "the list is the shell's to read"
    );

    // The device the worker listed, chosen by the keyboard.
    let before = koru_workspace_host::voice_prefs::input();
    koru_workspace_host::voice_devices::set(vec![
        koru_workspace_host::voice_prefs::InputDevice::new("Test Microphone", true),
    ]);
    // What the shell does when the worker's answer lands.
    v.push_chat_microphones();
    // *Default* is checked; Down reaches the device, Enter chooses it.
    for key in [
        winit::keyboard::NamedKey::ArrowDown,
        winit::keyboard::NamedKey::Enter,
    ] {
        View::key(
            &mut v,
            &mut w.ws,
            &winit::keyboard::Key::Named(key),
            None,
            winit::keyboard::ModifiersState::empty(),
        );
    }
    let chosen = koru_workspace_host::voice_prefs::input();
    koru_workspace_host::voice_prefs::set_input(before);
    assert_eq!(
        chosen,
        koru_workspace_host::voice_prefs::InputChoice::named("Test Microphone")
    );
    assert!(!v.ai_chat.microphone_panel_open(), "a choice closes it");
}
