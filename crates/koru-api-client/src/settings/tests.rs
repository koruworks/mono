// SPDX-License-Identifier: MPL-2.0

//! Pure tests: the classifier, the flags, and the bounded wait's timeout path.
//!
//! Nothing here opens a gateway. The round trip is proved end-to-end against a
//! real gateway process in `apps/koru-agent/tests/gateway_settings.rs`; what
//! belongs here is the shape a caller has to reason about on its own thread.

use super::*;

#[test]
fn every_settings_fact_invalidates_exactly_one_section() {
    assert_eq!(
        section_changed("settings.global_changed"),
        Some(Section::Global)
    );
    assert_eq!(section_changed("settings.ai_changed"), Some(Section::Ai));
    assert_eq!(
        section_changed("settings.terminal_changed"),
        Some(Section::Terminal)
    );
}

#[test]
fn ordinary_traffic_invalidates_nothing() {
    for name in [
        "workspace.catalog.created",
        "profile.created",
        "terminal.session.started",
        "app.installed",
        "settings",
    ] {
        assert_eq!(section_changed(name), None, "{name}");
    }
}

#[test]
fn the_registry_agrees_about_which_rows_are_settings_rows() {
    // The classifier and the registry cannot drift: a settings row added
    // without a case here would be a section that silently stopped
    // invalidating.
    for spec in koru_events::REGISTRY {
        let classified = section_changed(spec.name);
        if spec.name.starts_with("settings.") {
            assert!(
                classified.is_some(),
                "`{}` is a settings row that no window would notice",
                spec.name
            );
        } else {
            assert!(
                classified.is_none(),
                "`{}` is not a settings row",
                spec.name
            );
        }
    }
    assert_eq!(
        koru_events::REGISTRY
            .iter()
            .filter(|spec| spec.name.starts_with("settings."))
            .count(),
        3,
        "one fact per section"
    );
}

#[test]
fn a_flag_is_sticky_until_it_is_taken() {
    let flags = SectionFlags::default();
    assert!(!flags.is_stale(Section::Global));
    flags.invalidate(Section::Global);
    // Two facts before anyone looks is still one refetch.
    flags.invalidate(Section::Global);
    assert!(flags.is_stale(Section::Global));
    assert!(
        !flags.is_stale(Section::Terminal),
        "a Global change does not make a window refetch its terminal settings"
    );
    assert!(flags.take(Section::Global));
    assert!(!flags.take(Section::Global));
    assert!(!flags.is_stale(Section::Global));
}

#[test]
fn a_pending_section_answers_with_defaults_before_anything_arrives() {
    let (sender, receiver) = std::sync::mpsc::channel::<Result<GlobalSettings, Timeout>>();
    let mut pending = Pending {
        defaults: GlobalSettings::default(),
        receiver,
        resolved: false,
    };
    assert_eq!(
        pending.defaults().theme,
        koru_settings::ThemeChoice::System,
        "a window has something to draw before it has asked anything"
    );
    assert!(pending.take().is_none(), "and asking never blocks");
    assert!(!pending.is_resolved());

    let authoritative = GlobalSettings {
        theme: koru_settings::ThemeChoice::Dark,
        ..GlobalSettings::default()
    };
    sender.send(Ok(authoritative)).unwrap();
    let answer = pending.take().expect("the section landed");
    assert_eq!(answer.unwrap().theme, koru_settings::ThemeChoice::Dark);
    assert!(pending.is_resolved());
    assert!(
        pending.take().is_none(),
        "the answer is delivered once, not on every frame"
    );
}

#[test]
fn the_timeout_path_leaves_the_caller_on_defaults() {
    // S9: the window opened, it is usable, and it says so with the registry's
    // defaults rather than hanging or showing an error where a theme should be.
    let (sender, receiver) = std::sync::mpsc::channel::<Result<GlobalSettings, Timeout>>();
    let mut pending = Pending {
        defaults: GlobalSettings::default(),
        receiver,
        resolved: false,
    };
    sender.send(Err(Timeout)).unwrap();
    assert_eq!(pending.take(), Some(Err(Timeout)));
    assert_eq!(pending.defaults().theme, koru_settings::ThemeChoice::System);
}

#[test]
fn a_worker_that_vanishes_is_the_same_as_running_out_of_time() {
    let (sender, receiver) = std::sync::mpsc::channel::<Result<GlobalSettings, Timeout>>();
    let mut pending = Pending {
        defaults: GlobalSettings::default(),
        receiver,
        resolved: false,
    };
    drop(sender);
    assert_eq!(pending.take(), Some(Err(Timeout)));
}

#[test]
fn the_wait_budget_is_the_authoritys_own_constant() {
    // Spelled once, so the client and the authority cannot disagree about how
    // long a window is willing to wait.
    assert_eq!(koru_settings::STARTUP_WAIT_MILLIS, 2_000);
}

#[test]
fn a_section_body_names_itself_on_the_wire() {
    let body = serde_json::to_value(SectionBody::Terminal(TerminalSettings::default())).unwrap();
    assert_eq!(body["section"], "terminal");
    let round_trip: SectionBody = serde_json::from_value(body).unwrap();
    assert!(matches!(round_trip, SectionBody::Terminal(_)));
}

#[test]
fn a_mismatched_section_is_a_local_failure_and_not_a_silent_coercion() {
    let error = mismatched(
        Section::Terminal,
        SectionBody::Global(GlobalSettings::default()),
    );
    assert!(matches!(error, SettingsError::Local(_)));
    assert!(error.to_string().contains("terminal"));
    assert!(error.to_string().contains("global"));
}

#[test]
fn a_permission_refusal_is_distinguishable_from_everything_else() {
    // A window without the write scope shows read-only controls; a window that
    // cannot reach the gateway shows something else entirely.
    let denied = SettingsError::Api(SettingsApiFailure {
        status: 403,
        code: "permission_denied".into(),
        message: "this session does not hold `settings.write`".into(),
    });
    assert!(denied.is_permission_denied());
    let refused = SettingsError::Api(SettingsApiFailure {
        status: 400,
        code: "invalid_request".into(),
        message: "zoom".into(),
    });
    assert!(!refused.is_permission_denied());
    assert!(!SettingsError::Local("x".into()).is_permission_denied());
}

#[test]
fn the_paths_name_the_sections_the_routes_declare() {
    assert_eq!(path(Section::Global), "/v1/settings/global");
    assert_eq!(path(Section::Ai), "/v1/settings/ai");
    assert_eq!(path(Section::Terminal), "/v1/settings/terminal");
}

/// **The client's read path is the tolerant one** (the user's call,
/// 2026-11-03): a Global section answered by a newer service, with a field
/// this build does not know, still decodes, with the values it does know.
#[test]
fn a_section_body_from_a_newer_service_still_decodes() {
    let mut body = serde_json::to_value(SectionBody::Global(GlobalSettings {
        spellcheck_enabled: false,
        ..GlobalSettings::default()
    }))
    .unwrap();
    body["chat_font"] = serde_json::json!("follow");
    let read: SectionBody = serde_json::from_value(body).unwrap();
    match read {
        SectionBody::Global(global) => assert!(!global.spellcheck_enabled),
        other => panic!("expected the Global section, got {other:?}"),
    }
}
