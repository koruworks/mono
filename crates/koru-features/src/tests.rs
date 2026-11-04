// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn the_truthiness_rule_is_koru_envs() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("No"),
        Some(" OFF "),
        Some("\toff\n"),
        Some("1"),
        Some("true"),
        Some("yes"),
        Some("On"),
        Some("anything"),
        Some(" 0 1 "),
    ] {
        assert_eq!(
            switch_value(value),
            koru_env::switch_value(value),
            "value {value:?}"
        );
    }
}

#[test]
fn a_set_variable_wins_in_both_directions_and_an_unset_one_keeps_the_baked_default() {
    let lookup = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    };
    assert_eq!(
        Features::resolve(Features::ALL_OFF, lookup(&[])),
        Features::ALL_OFF,
        "nothing set, nothing baked: the simplified product"
    );
    assert_eq!(
        Features::resolve(Features::ALL_ON, lookup(&[])),
        Features::ALL_ON
    );
    let on = Features::resolve(
        Features::ALL_OFF,
        lookup(&[
            ("KORU_ENABLE_AI", "1"),
            ("KORU_ENABLE_TASKS", "yes"),
            ("KORU_ENABLE_CANVAS", "on"),
        ]),
    );
    assert_eq!(
        on,
        Features {
            ai: true,
            tasks: true,
            canvas: true,
            ..Features::ALL_OFF
        }
    );
    let off = Features::resolve(
        Features::ALL_ON,
        lookup(&[
            ("KORU_ENABLE_CLOUD_SYNC", "0"),
            ("KORU_ENABLE_REMINDERS", ""),
            ("KORU_ENABLE_SOCIALS", "off"),
        ]),
    );
    assert_eq!(
        off,
        Features {
            cloud_sync: false,
            reminders: false,
            socials: false,
            ..Features::ALL_ON
        }
    );
}

#[test]
fn agent_views_need_agents_and_are_off_by_default() {
    // Off when unset, unlike agents: the tabs ship, the views do not.
    assert!(!switch_value(None));
    let tabs_only = Features {
        agents: true,
        ..Features::ALL_OFF
    };
    testing::with(tabs_only, || assert!(agents() && !agent_views()));
    let views_alone = Features {
        agent_views: true,
        ..Features::ALL_OFF
    };
    testing::with(views_alone, || {
        assert!(!agent_views(), "views of agents need agents on")
    });
    let both = Features {
        agents: true,
        agent_views: true,
        ..Features::ALL_OFF
    };
    testing::with(both, || assert!(agent_views()));
}

/// Starting Delegate follows the agent views: Koru AI alone shows chat and
/// the runs already made, not a way to start one.
#[test]
fn delegate_needs_ai_and_the_agent_views() {
    let ai_and_agents = Features {
        ai: true,
        agents: true,
        ..Features::ALL_OFF
    };
    testing::with(ai_and_agents, || {
        assert!(ai() && !delegate(), "the agent views are off")
    });
    testing::with(
        Features {
            agent_views: true,
            ..ai_and_agents
        },
        || assert!(delegate()),
    );
    testing::with(
        Features {
            ai: false,
            ..Features::ALL_ON
        },
        || assert!(agent_views() && !delegate(), "Koru AI is off"),
    );
    testing::with(
        Features {
            agents: false,
            ..Features::ALL_ON
        },
        || assert!(ai() && !delegate(), "agent views need agents"),
    );
}

#[test]
fn an_unset_agents_flag_is_on_and_a_set_one_follows_the_rule() {
    assert!(switch_value_or(None, true), "unset: agents ship");
    assert!(!switch_value_or(None, false));
    for off in ["", "0", "false", "no", " OFF "] {
        assert!(
            !switch_value_or(Some(off), true),
            "{off:?} turns agents off"
        );
    }
    for on in ["1", "true", "yes", "on"] {
        assert!(switch_value_or(Some(on), false), "{on:?}");
    }
}

#[test]
fn the_names_are_the_ones_the_registry_declares() {
    for name in env::ALL {
        let var = koru_env::get(name).unwrap_or_else(|| panic!("{name} is not in koru-env VARS"));
        assert_eq!(var.kind, koru_env::Kind::Switch, "{name}");
    }
}

#[test]
fn a_test_build_shows_everything_and_an_override_is_per_thread() {
    assert_eq!(*features(), Features::ALL_ON);
    testing::with(Features::ALL_OFF, || {
        assert!(
            !ai()
                && !tasks()
                && !agents()
                && !reminders()
                && !cloud_sync()
                && !canvas()
                && !socials()
        );
        // Nested overrides restore the outer one.
        testing::with(
            Features {
                tasks: true,
                ..Features::ALL_OFF
            },
            || assert!(tasks() && !ai()),
        );
        assert!(!tasks());
        let other = std::thread::spawn(features).join().expect("thread");
        assert_eq!(*other, Features::ALL_ON, "another thread is not overridden");
    });
    assert_eq!(*features(), Features::ALL_ON);
}

#[test]
fn the_summary_names_every_flag() {
    assert_eq!(
        Features::ALL_OFF.to_string(),
        "cloud_sync=off agents=off agent_views=off tasks=off ai=off reminders=off canvas=off socials=off"
    );
}
