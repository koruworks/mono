// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::CapabilityName;
use std::collections::BTreeSet;
use std::str::FromStr;

#[test]
fn registrations_have_unique_valid_kinds_and_host_capabilities() {
    let mut kinds = BTreeSet::new();
    for registration in MANAGED_PROVIDER_REGISTRATIONS {
        let kind = ProviderKind::from_str(registration.kind).unwrap();
        assert!(kinds.insert(kind.clone()));
        assert_eq!(managed_provider(&kind), Some(registration));
        assert!(!registration.executable.is_empty());
        CapabilityName::from_str(registration.minimum_host_capability).unwrap();
        assert!(capability::ALL.contains(&registration.minimum_host_capability));
    }
    assert!(managed_provider(&ProviderKind::from_str("unregistered").unwrap()).is_none());
    assert!(managed_provider(&ProviderKind::from_str("gemini").unwrap()).is_none());
}

#[test]
fn registry_keeps_codex_launch_and_raw_profiles_unchanged() {
    assert_eq!(
        MANAGED_PROVIDER_REGISTRATIONS,
        &[
            CODEX_REGISTRATION,
            CLAUDE_REGISTRATION,
            OPENCODE_REGISTRATION,
            KIMI_REGISTRATION,
            GROK_REGISTRATION,
            ANTIGRAVITY_REGISTRATION,
            DEVIN_REGISTRATION
        ]
    );
    assert_eq!(CODEX_REGISTRATION.arguments, ["app-server", "--stdio"]);
    assert_eq!(
        CODEX_REGISTRATION.launch_policy,
        ManagedLaunchPolicy::PreferManaged
    );
    for profile in crate::built_in_profiles() {
        if profile
            .provider
            .as_ref()
            .is_some_and(|kind| kind.as_str() == "codex")
        {
            assert_eq!(profile.argv.first().unwrap(), CODEX_REGISTRATION.executable);
        }
    }
}

#[test]
fn profiles_has_only_the_protocol_dependency() {
    let manifest = include_str!("../../Cargo.toml");
    let dependencies = manifest.split_once("[dependencies]").unwrap().1;
    let entries: Vec<_> = dependencies
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].starts_with("koru-session-protocol ="));
}

#[test]
fn claude_wrapper_registration_does_not_replace_raw_cli_or_profile_policy() {
    assert_eq!(CLAUDE_REGISTRATION.executable, "claude-agent-acp");
    assert!(CLAUDE_REGISTRATION.arguments.is_empty());
    assert_eq!(CLAUDE_REGISTRATION.launch_policy, ManagedLaunchPolicy::Raw);
    let profiles = crate::built_in_profiles();
    let raw = profiles
        .iter()
        .find(|p| {
            p.provider
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "claude")
        })
        .unwrap();
    assert_eq!(raw.argv.first().unwrap(), "claude");
}

#[test]
fn antigravity_server_registration_does_not_replace_the_raw_agy_profile() {
    assert_eq!(ANTIGRAVITY_REGISTRATION.kind, "antigravity");
    assert_eq!(ANTIGRAVITY_REGISTRATION.executable, "agy_acp_server");
    assert!(ANTIGRAVITY_REGISTRATION.arguments.is_empty());
    assert_eq!(
        ANTIGRAVITY_REGISTRATION.launch_policy,
        ManagedLaunchPolicy::Raw
    );
    let raw = crate::built_in_profiles()
        .into_iter()
        .find(|p| {
            p.provider
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "antigravity")
        })
        .unwrap();
    assert_eq!(raw.argv, vec!["agy".to_owned()]);
    // An ordinary tab keeps launching the raw CLI.
    assert_eq!(
        managed_launch_mode(
            &raw,
            LaunchIntent::Ordinary,
            MANAGED_PROVIDER_REGISTRATIONS,
            None
        ),
        Ok(SessionMode::RawPty)
    );
}

/// **A Devin tab stays the raw CLI**; a chat runs the same program as
/// `devin acp`, and an ordinary launch never prefers it.
#[test]
fn devin_registration_runs_the_raw_programs_acp_subcommand() {
    assert_eq!(DEVIN_REGISTRATION.kind, "devin");
    assert_eq!(DEVIN_REGISTRATION.executable, "devin");
    assert_eq!(DEVIN_REGISTRATION.arguments, ["acp"]);
    assert_eq!(DEVIN_REGISTRATION.launch_policy, ManagedLaunchPolicy::Raw);
    let raw = crate::built_in_profiles()
        .into_iter()
        .find(|p| {
            p.provider
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "devin")
        })
        .unwrap();
    assert_eq!(raw.label, crate::ProfileLabel::Devin);
    assert_eq!(raw.argv, vec!["devin".to_owned()]);
    assert_eq!(
        crate::resume_arguments(raw.label),
        Some(&["--continue"][..])
    );
    assert_eq!(
        managed_launch_mode(
            &raw,
            LaunchIntent::Ordinary,
            MANAGED_PROVIDER_REGISTRATIONS,
            None
        ),
        Ok(SessionMode::RawPty)
    );
}

fn fixture_profile() -> crate::LaunchProfile {
    let mut profile = crate::built_in_profiles()
        .into_iter()
        .find(|profile| profile.provider.is_some())
        .unwrap();
    profile.provider = Some("fixture".parse().unwrap());
    profile
}

fn fixture_registration() -> ManagedProviderRegistration {
    ManagedProviderRegistration {
        kind: "fixture",
        executable: "fixture-only",
        arguments: &[],
        launch_policy: ManagedLaunchPolicy::Raw,
        minimum_host_capability: capability::MANAGED_SESSIONS,
        update_command: None,
        sign_in_command: None,
        desktop_app: None,
    }
}

fn fixture_readiness(delegatable: bool) -> ProviderReadiness {
    ProviderReadiness {
        kind: "fixture".parse().unwrap(),
        availability: ExecutableAvailability::Available { version: None },
        capabilities: koru_session_protocol::CapabilitySet::new(if delegatable {
            vec![capability::MANAGED_DELEGATION.parse().unwrap()]
        } else {
            vec![]
        }),
        failure: None,
    }
}

#[test]
fn raw_policy_is_preserved_after_registration_and_explicit_managed_is_separate() {
    let profile = fixture_profile();
    let registrations = [fixture_registration()];
    let rows = [fixture_readiness(false)];
    assert_eq!(
        managed_launch_mode(
            &profile,
            LaunchIntent::Ordinary,
            &registrations,
            Some(&rows)
        ),
        Ok(SessionMode::RawPty)
    );
    assert_eq!(
        managed_launch_mode(&profile, LaunchIntent::Raw, &registrations, Some(&rows)),
        Ok(SessionMode::RawPty)
    );
    assert!(matches!(
        managed_launch_mode(&profile, LaunchIntent::Managed, &registrations, Some(&rows)),
        Ok(SessionMode::Managed { .. })
    ));
    assert_eq!(
        managed_launch_mode(
            &profile,
            LaunchIntent::Delegation,
            &registrations,
            Some(&rows)
        ),
        Err(ErrorCode::CapabilityNotNegotiated)
    );
    assert!(matches!(
        managed_launch_mode(
            &profile,
            LaunchIntent::Delegation,
            &registrations,
            Some(&[fixture_readiness(true)])
        ),
        Ok(SessionMode::Managed { .. })
    ));
}

#[test]
fn raw_only_profiles_never_become_managed_and_unavailable_host_answers_refuse_explicit_launch() {
    let mut profile = fixture_profile();
    let registrations = [fixture_registration()];
    for availability in [
        ExecutableAvailability::Checking,
        ExecutableAvailability::Missing,
        ExecutableAvailability::SignedOut {
            code: ErrorCode::ProviderUnavailable,
        },
        ExecutableAvailability::Failed {
            code: ErrorCode::ProviderFailed,
        },
        ExecutableAvailability::Incompatible {
            code: ErrorCode::ProviderIncompatible,
        },
    ] {
        let row = ProviderReadiness {
            availability,
            ..fixture_readiness(true)
        };
        for intent in [LaunchIntent::Managed, LaunchIntent::Delegation] {
            assert!(
                managed_launch_mode(
                    &profile,
                    intent,
                    &registrations,
                    Some(std::slice::from_ref(&row))
                )
                .is_err()
            );
        }
        assert_eq!(
            managed_launch_mode(&profile, LaunchIntent::Raw, &registrations, Some(&[row])),
            Ok(SessionMode::RawPty)
        );
    }
    assert!(managed_launch_mode(&profile, LaunchIntent::Managed, &registrations, None).is_err());
    assert!(
        managed_launch_mode(&profile, LaunchIntent::Managed, &registrations, Some(&[])).is_err()
    );
    profile.provider = None;
    assert_eq!(
        managed_launch_mode(&profile, LaunchIntent::Ordinary, &registrations, None),
        Ok(SessionMode::RawPty)
    );
    assert_eq!(
        managed_launch_mode(
            &profile,
            LaunchIntent::Delegation,
            &registrations,
            Some(&[fixture_readiness(true)])
        ),
        Err(ErrorCode::ProfileIncompatible)
    );
}

#[test]
fn ordinary_prefer_managed_attempts_live_start_when_cache_is_cold_or_checking() {
    let registration = CODEX_REGISTRATION;
    let profile = crate::built_in_profiles()
        .into_iter()
        .find(|profile| {
            profile
                .provider
                .as_ref()
                .is_some_and(|kind| kind.as_str() == registration.kind)
        })
        .unwrap();
    let checking = ProviderReadiness {
        kind: profile.provider.clone().unwrap(),
        availability: ExecutableAvailability::Checking,
        capabilities: Default::default(),
        failure: None,
    };
    for snapshot in [None, Some(std::slice::from_ref(&checking))] {
        assert!(matches!(
            managed_launch_mode(&profile, LaunchIntent::Ordinary, &[registration], snapshot),
            Ok(SessionMode::Managed { .. })
        ));
        for intent in [LaunchIntent::Managed, LaunchIntent::Delegation] {
            assert_eq!(
                managed_launch_mode(&profile, intent, &[registration], snapshot),
                Err(ErrorCode::ProviderUnavailable)
            );
        }
    }
}

#[test]
fn new_acp_registrations_preserve_each_existing_raw_profile_and_preference() {
    let profiles = crate::built_in_profiles();
    for registration in [OPENCODE_REGISTRATION, KIMI_REGISTRATION, GROK_REGISTRATION] {
        assert_eq!(registration.launch_policy, ManagedLaunchPolicy::Raw);
        let raw = profiles
            .iter()
            .find(|profile| {
                profile
                    .provider
                    .as_ref()
                    .is_some_and(|kind| kind.as_str() == registration.kind)
            })
            .unwrap();
        assert_eq!(raw.argv, vec![registration.executable.to_owned()]);
    }
}
