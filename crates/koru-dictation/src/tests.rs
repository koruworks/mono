// SPDX-License-Identifier: MPL-2.0

//! The controller's own suite. **Nothing here starts a worker** — see
//! [`ENGINE_OFF`] and the `no-engine` feature: `open_session` mints an id and a
//! generation, and the three dialling methods return without touching the
//! socket, so every path below is exercised with no microphone anywhere near
//! it.

use super::*;

/// **A controller somebody has agreed with.**
///
/// Every test below starts from one, because a controller with no consent
/// refuses before it looks at anything else — which is the point of the gate
/// and is asserted on its own in
/// `a_controller_with_no_consent_opens_nothing`. `Granted::recorded` is the
/// in-memory grant the consent step itself holds between the button and the
/// write, so this is the product's path rather than a test seam.
fn consenting() -> Dictation {
    let mut d = Dictation::new();
    d.set_consent(Some(koru_voice_consent::Granted::recorded(
        koru_voice_consent::Terms {
            model: "parakeet-tdt-v3".into(),
            version: "1.2.0".into(),
            licence: "Apache-2.0".into(),
            attribution: "k2-fsa, Apache-2.0".into(),
            download_bytes: 648_291_840,
        },
        1,
    )));
    d
}

/// A controller with a session open and a surface that accepted it.
fn listening() -> (Dictation, Instant, Live) {
    let mut d = consenting();
    let now = Instant::now();
    let action = d.press(now).expect("a press opens a session");
    assert!(matches!(action, DictationAction::Begin { .. }));
    d.began(true);
    let live = Live {
        session: d.session().expect("a session id"),
        generation: d.generation().expect("a generation"),
    };
    (d, now, live)
}

fn partial(live: Live, text: &str) -> Event {
    Event::Partial {
        session: live.session,
        generation: live.generation,
        text: Transcript::new(text),
    }
}

fn final_of(live: Live, text: &str) -> Event {
    Event::Final {
        session: live.session,
        generation: live.generation,
        text: Transcript::new(text),
    }
}

fn ended(live: Live, reason: EndReason) -> Event {
    Event::Ended {
        session: live.session,
        generation: live.generation,
        reason,
    }
}

#[test]
fn a_controller_built_under_test_reaches_no_worker() {
    assert!(
        ENGINE_OFF,
        "the suite must not be able to dial a worker; \
         `no-engine` is off, or this crate's own `cfg(test)` guard has been removed"
    );
}

/// **The four, taken from the authority rather than parsed.** A real device
/// name has a space and brackets in it, and the snapshot has to carry that
/// verbatim rather than as a tidy token.
#[test]
fn independent_hosts_take_the_dictation_choices_from_the_section() {
    let section = koru_settings::GlobalSettings {
        dictation_gesture: koru_settings::DictationGesture::Toggle,
        dictation_language: koru_settings::DictationLanguage::Tag("en".into()),
        dictation_tier: koru_settings::DictationTier::HighAccuracy,
        dictation_input: koru_settings::DictationInput::Named("Microphone (Yeti X)".into()),
        ..koru_settings::GlobalSettings::default()
    };
    let preferences = Preferences::from_section(&section);
    assert_eq!(preferences.mode, Mode::Toggle);
    assert_eq!(preferences.language, Requested::Fixed(VoiceLanguage::En));
    assert_eq!(preferences.tier, ModelTier::HighAccuracy);
    assert_eq!(preferences.input, InputChoice::named("Microphone (Yeti X)"));
}

/// **A tag the authority accepts and this build does not know is automatic
/// detection**, never a refusal. The authority validates the *shape* of a
/// language tag deliberately and leaves the closed set to the consumer, so
/// this is where an unknown one has to land softly.
#[test]
fn a_section_this_build_cannot_resolve_falls_back_safely() {
    let section = koru_settings::GlobalSettings {
        dictation_language: koru_settings::DictationLanguage::Tag("kli-Ngon".into()),
        ..koru_settings::GlobalSettings::default()
    };
    assert_eq!(Preferences::from_section(&section), Preferences::default());
}

/// The registry's defaults are this crate's defaults. Two spellings of what a
/// host shows before the authority has answered, pinned together — a drift
/// would mean dictation behaving one way for two seconds and another after.
#[test]
fn the_sections_defaults_are_the_snapshots_defaults() {
    assert_eq!(
        Preferences::from_section(&koru_settings::GlobalSettings::default()),
        Preferences::default()
    );
}

#[test]
fn a_whole_dictation_is_begin_update_commit() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "create a new"));
    d.observe(partial(live, "create a new project"));
    assert!(d.release(now));
    d.observe(final_of(live, "Create a new project plan."));
    d.observe(ended(live, EndReason::Completed));
    let actions = d.pump(now);
    assert_eq!(
        actions,
        vec![
            DictationAction::Update {
                generation: live.generation,
                text: Transcript::new("create a new project"),
            },
            DictationAction::Commit {
                generation: live.generation,
                text: Transcript::new("Create a new project plan."),
            },
        ]
    );
    assert_eq!(d.stage(), Stage::Idle);
    assert_eq!(d.stats().commits, 1);
}

#[test]
fn every_action_carries_the_generation() {
    let (mut d, now, live) = listening();
    let begin = d.pump(now);
    // `Begin` came back from `press`, not from the queue.
    assert!(begin.is_empty());
    d.observe(partial(live, "one"));
    let update = d.pump(now);
    d.release(now);
    d.observe(final_of(live, "One."));
    d.observe(ended(live, EndReason::Completed));
    let commit = d.pump(now);
    for action in update.iter().chain(commit.iter()) {
        assert_eq!(action.generation(), live.generation);
    }
}

#[test]
fn ten_partials_coalesce_into_one_update() {
    let (mut d, now, live) = listening();
    for i in 0..10 {
        d.observe(partial(live, &format!("word {i}")));
    }
    let actions = d.pump(now);
    assert_eq!(
        actions,
        vec![DictationAction::Update {
            generation: live.generation,
            text: Transcript::new("word 9"),
        }]
    );
    // One `Update` counted, not ten: the count is what reached a surface.
    assert_eq!(d.stats().updates, 1);
}

#[test]
fn a_release_with_no_matching_press_is_inert() {
    let mut d = consenting();
    let now = Instant::now();
    assert!(!d.release(now));
    assert!(d.pump(now).is_empty());
    assert_eq!(d.stage(), Stage::Idle);
    assert_eq!(d.stats().sessions, 0);
}

#[test]
fn a_key_held_indefinitely_is_bounded() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "still going"));
    // The key never comes up.
    let much_later = now + HOLD_CAP + Duration::from_secs(1);
    let actions = d.pump(much_later);
    assert_eq!(
        d.stage(),
        Stage::Finalising,
        "the cap finishes, not cancels"
    );
    assert_eq!(d.stats().capped, 1);
    // What was heard is still on screen; nothing has been committed.
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], DictationAction::Update { .. }));
    // And the finalisation window is bounded too.
    let later_still = much_later + FINALISE_TIMEOUT + Duration::from_secs(1);
    let actions = d.pump(later_still);
    assert_eq!(
        actions,
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(d.stats().finalise_timeouts, 1);
    assert_eq!(
        d.take_refusal().map(|r| r.code),
        Some(ErrorCode::FinishFailed)
    );
}

#[test]
fn a_local_event_in_the_finalising_window_discards() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "half a sentence"));
    let _ = d.pump(now);
    d.release(now);
    assert_eq!(d.stage(), Stage::Finalising);
    d.interrupt(Interruption::LocalEvent);
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(d.stage(), Stage::Idle);
    // …and the `Final` that was already in flight commits nothing.
    d.observe(final_of(live, "Half a sentence, finished."));
    d.observe(ended(live, EndReason::Completed));
    assert!(d.pump(now).is_empty());
    assert_eq!(d.stats().commits, 0);
}

#[test]
fn escape_after_release_discards() {
    let (mut d, now, live) = listening();
    d.release(now);
    d.interrupt(Interruption::Escape);
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(d.stage(), Stage::Idle);
}

#[test]
fn losing_focus_mid_session_discards() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "mid sentence"));
    d.interrupt(Interruption::FocusLost);
    // The queued `Update` is gone with it: telling a surface to lay out text
    // it is about to throw away in the same pass is a frame nobody sees.
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(d.stats().discards, 1);
}

#[test]
fn a_late_final_from_a_cancelled_session_commits_nothing() {
    let (mut d, now, live) = listening();
    d.interrupt(Interruption::WindowClosed);
    let _ = d.pump(now);
    // The next dictation opens while the old one's result is still in flight.
    let begin = d.press(now).expect("a second session");
    d.began(true);
    let second = d.generation().expect("a second generation");
    assert_ne!(second, live.generation);
    assert_eq!(begin.generation(), second);
    d.observe(final_of(live, "the cancelled sentence"));
    d.observe(ended(live, EndReason::Completed));
    assert!(d.pump(now).is_empty());
    assert!(d.stats().stale >= 2);
    assert_eq!(d.stage(), Stage::Live, "the live session is undisturbed");
}

#[test]
fn a_final_volunteered_while_listening_is_counted_under_its_own_name() {
    let (mut d, now, live) = listening();
    d.observe(final_of(live, "not asked for"));
    assert!(d.pump(now).is_empty());
    assert_eq!(d.stats().volunteered_finals, 1);
    assert_eq!(d.stats().stale, 0, "this is a contract breach, not a race");
    assert_eq!(d.stage(), Stage::Live);
}

#[test]
fn a_worker_that_exits_mid_finalise_discards() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "and then"));
    let _ = d.pump(now);
    d.release(now);
    // What the client's reader delivers when the socket closes with no
    // `Ended` of the worker's own.
    d.observe(ended(
        live,
        EndReason::Failed {
            code: ErrorCode::EngineFailed,
            message: "the voice worker's connection closed".into(),
        },
    ));
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(
        d.take_refusal().map(|r| r.code),
        Some(ErrorCode::EngineFailed)
    );
}

#[test]
fn a_completed_session_with_no_words_records_nothing() {
    let (mut d, now, live) = listening();
    d.release(now);
    d.observe(final_of(live, ""));
    d.observe(ended(live, EndReason::Completed));
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }],
        "saying nothing over a selection must not delete it"
    );
    assert_eq!(d.stats().commits, 0);
}

#[test]
fn a_surface_that_refuses_ends_the_session_before_anything_is_heard() {
    let mut d = consenting();
    let now = Instant::now();
    let begin = d.press(now).expect("a press opens a session");
    let generation = begin.generation();
    d.began(false);
    assert_eq!(d.stage(), Stage::Idle);
    assert!(d.pump(now).is_empty(), "there is no composition to close");
    let refusal = d.take_refusal().expect("the host said no");
    assert!(
        refusal.surface,
        "the refusal came from the surface, not the worker"
    );
    // A partial for that session reaches nothing.
    d.observe(Event::Partial {
        session: SessionId::new(),
        generation,
        text: Transcript::new("into the void"),
    });
    assert!(d.pump(now).is_empty());
}

#[test]
fn nothing_is_emitted_before_a_surface_has_accepted() {
    let mut d = consenting();
    let now = Instant::now();
    let begin = d.press(now).expect("a press opens a session");
    let live = Live {
        session: d.session().unwrap(),
        generation: begin.generation(),
    };
    // The host has not answered `began` yet.
    d.observe(partial(live, "too early"));
    assert!(d.pump(now).is_empty());
    d.began(true);
    d.observe(partial(live, "now"));
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Update {
            generation: live.generation,
            text: Transcript::new("now"),
        }]
    );
}

#[test]
fn a_second_press_while_listening_does_nothing_in_push_to_talk() {
    let (mut d, now, _live) = listening();
    let sessions = d.stats().sessions;
    assert!(d.press(now).is_none(), "a key repeat starts nothing");
    assert_eq!(d.stats().sessions, sessions);
    assert_eq!(d.stage(), Stage::Live);
}

#[test]
fn a_toggle_ends_on_the_next_press_and_ignores_the_release() {
    let mut d = consenting();
    d.set_mode(Mode::Toggle);
    let now = Instant::now();
    let begin = d.press(now).expect("a press opens a session");
    d.began(true);
    let live = Live {
        session: d.session().unwrap(),
        generation: begin.generation(),
    };
    assert!(
        !d.release(now),
        "the key coming up is not the end of a toggle"
    );
    assert_eq!(d.stage(), Stage::Live);
    assert!(d.press(now).is_none(), "the second press ends it");
    assert_eq!(d.stage(), Stage::Finalising);
    d.observe(final_of(live, "Toggled."));
    d.observe(ended(live, EndReason::Completed));
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Commit {
            generation: live.generation,
            text: Transcript::new("Toggled."),
        }]
    );
}

#[test]
fn a_refusal_from_the_worker_discards_and_is_said_once() {
    let (mut d, now, live) = listening();
    d.observe(Event::Refused {
        session: live.session,
        generation: live.generation,
        code: ErrorCode::Busy,
        message: "another dictation is running".into(),
    });
    assert_eq!(
        d.pump(now),
        vec![DictationAction::Cancel {
            generation: live.generation
        }]
    );
    assert_eq!(d.take_refusal().map(|r| r.code), Some(ErrorCode::Busy));
    assert!(d.take_refusal().is_none(), "taken by the asking");
}

#[test]
fn an_idle_controller_asks_for_no_wake() {
    let mut d = consenting();
    assert!(d.deadline().is_none());
    let now = Instant::now();
    let _ = d.press(now);
    d.began(true);
    assert_eq!(d.deadline(), Some(now + HOLD_CAP));
    d.release(now);
    assert_eq!(d.deadline(), Some(now + FINALISE_TIMEOUT));
    d.interrupt(Interruption::Suspended);
    assert!(
        d.deadline().is_none(),
        "nothing running, nothing to wake for"
    );
}

#[test]
fn the_worker_state_is_reported_without_touching_the_composition() {
    let (mut d, now, live) = listening();
    assert_eq!(d.worker_state(), State::Loading);
    d.observe(Event::State {
        session: live.session,
        generation: live.generation,
        state: State::Listening,
    });
    d.observe(Event::Level {
        session: live.session,
        generation: live.generation,
        bucket: LevelBucket::new(5),
    });
    d.observe(Event::Language {
        session: live.session,
        generation: live.generation,
        language: VoiceLanguage::En,
    });
    assert_eq!(d.worker_state(), State::Listening);
    assert_eq!(d.level().step(), 5);
    assert_eq!(d.detected_language(), Some(VoiceLanguage::En));
    assert!(d.pump(now).is_empty(), "a meter is not a composition");
}

#[test]
fn a_transcript_never_prints_itself() {
    let action = DictationAction::Commit {
        generation: 7,
        text: Transcript::new("a private sentence"),
    };
    let printed = format!("{action:?}");
    assert!(!printed.contains("private"), "{printed}");
    assert!(printed.contains("REDACTED"), "{printed}");
}

#[test]
fn a_listening_surface_is_alive_exactly_as_long_as_the_session() {
    let mut d = consenting();
    assert!(
        d.status().is_none(),
        "nothing to draw before anything is running"
    );
    let (mut d2, now, live) = listening();
    let open = d2.status().expect("a session is running");
    assert_eq!(open.generation, live.generation);
    assert_eq!(open.stage, Stage::Live);
    assert_eq!(open.worker, State::Loading);
    assert_eq!(open.level, LevelBucket::SILENT);
    d2.observe(Event::State {
        session: live.session,
        generation: live.generation,
        state: State::Listening,
    });
    d2.observe(Event::Level {
        session: live.session,
        generation: live.generation,
        bucket: LevelBucket::new(6),
    });
    let heard = d2.status().expect("still running");
    assert_eq!(heard.worker, State::Listening);
    assert_eq!(heard.level.step(), 6);
    d2.release(now);
    assert_eq!(
        d2.status().map(|s| s.stage),
        Some(Stage::Finalising),
        "the composition is still provisional, so the surface is still there"
    );
    d2.observe(final_of(live, "done"));
    d2.observe(ended(live, EndReason::Completed));
    let _ = d2.pump(now);
    assert!(
        d2.status().is_none(),
        "the session is over, so nothing asks for another frame"
    );
    // The idle controller is also silent about its own name.
    let _ = &mut d;
}

#[test]
fn the_microphone_button_finishes_rather_than_discarding() {
    let (mut d, now, live) = listening();
    d.observe(partial(live, "keep these words"));
    let _ = d.pump(now);
    assert!(d.stop(now), "a press on the surface ends the hold");
    assert_eq!(d.stage(), Stage::Finalising);
    d.observe(final_of(live, "keep these words"));
    d.observe(ended(live, EndReason::Completed));
    let actions = d.pump(now);
    assert!(
        matches!(
            actions.as_slice(),
            [DictationAction::Commit { text, .. }] if text.as_str() == "keep these words"
        ),
        "stop is a finish, not an interruption: {actions:?}"
    );
    assert_eq!(d.stats().commits, 1);
    assert_eq!(d.stats().discards, 0);
}

#[test]
fn the_microphone_button_stops_a_toggled_dictation_too() {
    let mut d = consenting();
    d.set_mode(Mode::Toggle);
    let now = Instant::now();
    let _ = d.press(now).expect("a toggle press opens a session");
    d.began(true);
    assert!(d.stop(now), "the pointer has no key to let go of");
    assert_eq!(d.stage(), Stage::Finalising);
    assert!(
        !d.stop(now),
        "a second press while finalising asks for nothing new"
    );
}

#[test]
fn stopping_nothing_is_inert() {
    let mut d = consenting();
    assert!(!d.stop(Instant::now()));
    assert_eq!(d.stage(), Stage::Idle);
    assert!(d.status().is_none());
}

// ------------------------------------------------------------------ consent

/// **No path reaches a microphone without consent**, and this is the half a
/// signature cannot express: the controller is the only thing in a window that
/// opens a session, and one nobody has agreed with refuses before it reaches
/// the stage, the mode or the client.
#[test]
fn a_controller_with_no_consent_opens_nothing() {
    let mut d = Dictation::new();
    assert!(!d.consented());

    assert!(d.press(Instant::now()).is_none());

    assert_eq!(d.stage(), Stage::Idle);
    assert!(!d.is_live());
    assert!(d.session().is_none());
    let refusal = d.take_refusal().expect("a refusal was recorded");
    assert_eq!(refusal.code, ErrorCode::ConsentRequired);
    assert!(
        !refusal.surface,
        "the surface said nothing; this is about the machine"
    );
}

/// Taking consent back closes the door again, and it does so without ending a
/// dictation somebody is in the middle of — the next press is what meets it.
#[test]
fn revoking_consent_refuses_the_next_press() {
    let mut d = consenting();
    let now = Instant::now();
    d.press(now).expect("a press opens a session");
    d.began(true);
    d.interrupt(Interruption::FocusLost);
    let _ = d.pump(now);
    let _ = d.take_refusal();

    d.set_consent(None);

    assert!(d.press(now).is_none());
    assert_eq!(
        d.take_refusal().map(|r| r.code),
        Some(ErrorCode::ConsentRequired)
    );
}

/// **A press that ends a toggled dictation is not a press that opens a
/// microphone.** Consent taken back mid-sentence has to leave somebody a way to
/// stop, so the refusal sits after the stage rather than before it.
#[test]
fn revoking_consent_mid_toggle_still_lets_the_chord_stop_it() {
    let mut d = consenting();
    d.set_mode(Mode::Toggle);
    let now = Instant::now();
    d.press(now).expect("a press opens a session");
    d.began(true);

    d.set_consent(None);

    assert!(d.press(now).is_none(), "the press ends rather than starts");
    assert_eq!(
        d.stage(),
        Stage::Finalising,
        "it asked for the final result"
    );
    assert!(
        d.take_refusal().is_none(),
        "stopping is not a refusal to report"
    );
}

#[test]
fn routed_press_adopts_only_the_exact_native_gesture_and_never_hijacks() {
    use RoutedPress::*;

    assert_eq!(routed_press(None, 7, Stage::Idle, false), Begin);
    assert_eq!(routed_press(None, 7, Stage::Live, true), Adopt);
    assert_eq!(routed_press(None, 7, Stage::Live, false), Refuse);
    assert_eq!(routed_press(None, 7, Stage::Finalising, true), Refuse);
    assert_eq!(routed_press(Some(7), 7, Stage::Live, false), Continue);
    assert_eq!(routed_press(Some(8), 7, Stage::Live, true), Ignore);
}

// ------------------------------------------------------------ the level meter

/// **The meter follows what the host wants, and dials only on a change**:
/// opened on a device, moved to another, left alone while nothing moves, and
/// closed. The level is silent until a worker says otherwise.
#[test]
fn the_meter_opens_moves_and_closes_with_what_the_host_wants() {
    let mut d = consenting();
    assert_eq!(d.metering(), None);
    assert_eq!(d.meter_level(), None);

    assert!(d.meter(Some(InputChoice::SystemDefault)), "it opened");
    assert_eq!(d.metering(), Some(&InputChoice::SystemDefault));
    assert_eq!(d.meter_level(), Some(LevelBucket::SILENT));
    assert!(
        !d.meter(Some(InputChoice::SystemDefault)),
        "the same device asked again on the next pass dials nothing"
    );

    let yeti = InputChoice::named("Yeti Nano");
    assert!(d.meter(Some(yeti.clone())), "another device moves it");
    assert_eq!(d.metering(), Some(&yeti));

    assert!(d.meter(None), "the panel closed");
    assert_eq!(d.metering(), None);
    assert_eq!(d.meter_level(), None);
    assert!(!d.meter(None), "closing nothing dials nothing");
}

/// **A dictation wins the microphone.** The press closes a running meter
/// before it begins, and while the dictation runs no meter opens however
/// often the host asks; once it is over, the next pass may open one again.
#[test]
fn a_dictation_closes_the_meter_and_none_opens_while_it_runs() {
    let mut d = consenting();
    d.meter(Some(InputChoice::SystemDefault));
    let now = Instant::now();
    d.press(now).expect("a press opens a session");
    d.began(true);
    assert_eq!(d.metering(), None, "the meter outlived the press");

    assert!(!d.meter(Some(InputChoice::SystemDefault)));
    assert_eq!(d.metering(), None, "a meter opened beside a dictation");

    d.interrupt(Interruption::Escape);
    assert!(d.meter(Some(InputChoice::SystemDefault)));
    assert_eq!(d.metering(), Some(&InputChoice::SystemDefault));
}

/// **No consent, no meter.** It opens a microphone, which is half of what
/// the record is for — even though it loads no model.
#[test]
fn a_controller_with_no_consent_opens_no_meter() {
    let mut d = Dictation::new();
    assert!(!d.meter(Some(InputChoice::SystemDefault)));
    assert_eq!(d.metering(), None);

    let mut d = consenting();
    d.meter(Some(InputChoice::SystemDefault));
    d.set_consent(None);
    assert!(
        d.meter(Some(InputChoice::SystemDefault)),
        "taken back: closed"
    );
    assert_eq!(d.metering(), None);
}
