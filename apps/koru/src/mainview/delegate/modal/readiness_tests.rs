// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_i18n::{Lang, t};
use koru_session_protocol::{
    CapabilitySet, ErrorCode, ExecutableAvailability as Availability, ProviderKind,
    ProviderReadiness, SessionProfileKey, capability,
};

pub(crate) fn row(index: usize, availability: Availability, delegatable: bool) -> AgentRow {
    let provider: ProviderKind = format!("fixture-{index}").parse().unwrap();
    AgentRow {
        key: SessionProfileKey::Custom(koru_session_protocol::SessionProfileId::mint()),
        provider: provider.clone(),
        name: format!("Agent {index}"),
        readiness: ProviderReadiness {
            kind: provider,
            availability,
            capabilities: if delegatable {
                CapabilitySet::new([capability::MANAGED_DELEGATION.parse().unwrap()])
            } else {
                CapabilitySet::default()
            },
            failure: None,
        },
    }
}

pub(crate) fn world(agents: Vec<AgentRow>) -> World {
    World {
        workspace: WorkspaceId::from_uuid(uuid::Uuid::new_v4()),
        workspace_name: "Workspace".into(),
        profile: koru_store_core::ProfileId::from_uuid(uuid::Uuid::new_v4()),
        agents,
        scopes: vec![ScopeRow {
            scope: WorkspaceScope::Temporary,
            name: "Temporary".into(),
            path: Some("fixture/folder".into()),
            disabled: None,
        }],
        context: vec![],
        attachable: None,
        objective: "Do the task".into(),
        pill_anchor: None,
        temporary_base: "fixture".into(),
        base_problem: None,
        dropped_context: false,
        chosen_agent: None,
        chosen_model: None,
        chosen_scope: None,
        grant: true,
    }
}

#[test]
fn each_readiness_state_has_a_distinct_localized_reason() {
    let cases = [
        (
            Availability::Checking,
            I18nKey::TerminalChecking,
            I18nKey::TerminalChecking,
        ),
        (
            Availability::Missing,
            I18nKey::TerminalNotInstalled,
            I18nKey::TerminalManagedInstallRequired,
        ),
        (
            Availability::SignedOut {
                code: ErrorCode::ProviderUnavailable,
            },
            I18nKey::TerminalManagedSignedOut,
            I18nKey::TerminalManagedSignInRequired,
        ),
        (
            Availability::incompatible(ErrorCode::ProviderIncompatible),
            I18nKey::TerminalManagedUnsupported,
            I18nKey::TerminalManagedUpdateRequired,
        ),
        (
            Availability::Failed {
                code: ErrorCode::ProviderFailed,
            },
            I18nKey::TerminalManagedCheckFailed,
            I18nKey::TerminalManagedRetryRequired,
        ),
        (
            Availability::incompatible(ErrorCode::CapabilityNotNegotiated),
            I18nKey::TerminalManagedUnsupported,
            I18nKey::TerminalManagedHostUpdateRequired,
        ),
    ];
    for (availability, status, guidance) in cases {
        let row = row(0, availability, false);
        assert_eq!(readiness::status(&row.readiness), Some(status));
        assert_eq!(readiness::guidance(&row.readiness), Some(guidance));
        for lang in Lang::ALL {
            assert!(!t(lang, status).is_empty());
            assert!(!t(lang, guidance).is_empty());
        }
    }
    let ready = row(0, Availability::Available { version: None }, true);
    assert_eq!(readiness::guidance(&ready.readiness), None);
    let managed_only = row(0, Availability::Available { version: None }, false);
    assert_eq!(
        readiness::guidance(&managed_only.readiness),
        Some(I18nKey::TerminalManagedChooseAnother)
    );
}

#[test]
fn keyboard_reaches_unavailable_reason_and_retry_without_selecting_or_submitting_it() {
    let mut engine = TextEngine::new();
    let unavailable = row(
        0,
        Availability::SignedOut {
            code: ErrorCode::ProviderUnavailable,
        },
        false,
    );
    let kind = unavailable.provider.clone();
    let mut modal = Delegation::open(&mut engine, world(vec![unavailable]));
    assert_eq!(modal.agent, None);
    assert!(!modal.ready());
    modal.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(modal.focused(), Some(Hit::Agent(0)));
    assert_eq!(modal.inspected_agent(), Some(0));
    assert_eq!(modal.activate(Hit::Agent(0)), Outcome::Keep);
    assert_eq!(modal.agent, None);
    assert_eq!(modal.activate(Hit::Submit), Outcome::Ignored);
    modal.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(modal.focused(), Some(Hit::AgentRefresh(0)));
    assert_eq!(
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ),
        Outcome::RefreshProvider(kind)
    );
    assert_eq!(modal.focused(), Some(Hit::Agent(0)));
    assert!(!modal.controls().contains(&Hit::AgentRefresh(0)));
    assert_eq!(modal.activate(Hit::AgentRefresh(0)), Outcome::Ignored);
}

#[test]
fn readiness_updates_preserve_draft_and_choice_by_key_and_remove_stale_launchability() {
    let mut engine = TextEngine::new();
    let first = row(0, Availability::Available { version: None }, true);
    let second = row(1, Availability::Available { version: None }, true);
    let mut modal = Delegation::open(&mut engine, world(vec![first.clone(), second.clone()]));
    modal.activate(Hit::Agent(1));
    modal.replace_agents(vec![second.clone(), first]);
    assert_eq!(modal.agent, Some(0));
    assert_eq!(modal.focused(), Some(Hit::Agent(0)));
    assert_eq!(modal.objective.value(), "Do the task");
    let mut unavailable = second;
    unavailable.readiness.availability = Availability::Missing;
    modal.replace_agents(vec![unavailable]);
    assert_eq!(
        modal.agent,
        Some(0),
        "keep the chosen provider identity while unavailable"
    );
    assert!(!modal.ready());
    assert_eq!(modal.activate(Hit::Submit), Outcome::Ignored);
}

#[test]
fn keyboard_inspection_scrolls_later_registered_rows_into_shared_hit_geometry() {
    let mut engine = TextEngine::new();
    let agents = (0..9)
        .map(|index| row(index, Availability::Missing, false))
        .collect();
    let mut modal = Delegation::open(&mut engine, world(agents));
    modal.activate(Hit::Agent(8));
    assert!(super::super::paint::visible_agents(&modal).contains(&8));
    let layout = super::super::paint::layout(&mut engine, &modal, (900.0, 1000.0));
    for target in [Hit::Agent(8), Hit::AgentRefresh(8)] {
        let (_, rect) = layout.iter().find(|(hit, _)| *hit == target).unwrap();
        assert_eq!(
            super::super::paint::hit_at(
                &mut engine,
                &modal,
                (900.0, 1000.0),
                rect.x + rect.w * 0.5,
                rect.y + rect.h * 0.5
            ),
            Some(target)
        );
    }
}

#[test]
fn six_agents_and_wrapped_guidance_keep_every_control_reachable_on_small_laptop() {
    use super::super::paint;
    let mut engine = TextEngine::new();
    let agents = (0..6)
        .map(|index| {
            row(
                index,
                if index == 5 {
                    Availability::Available { version: None }
                } else {
                    Availability::Missing
                },
                index == 5,
            )
        })
        .collect();
    let mut modal = Delegation::open(&mut engine, world(agents));
    for viewport in [(1024.0, 600.0), (1280.0, 720.0)] {
        for hit in modal.controls() {
            modal.focus = modal
                .controls()
                .iter()
                .position(|item| *item == hit)
                .unwrap();
            paint::reveal_focus(&mut engine, &mut modal, viewport);
            let panel = paint::panel(&mut engine, &modal, viewport);
            let layout = paint::layout(&mut engine, &modal, viewport);
            let (_, rect) = layout.iter().find(|(item, _)| *item == hit).unwrap();
            assert!(
                rect.y >= panel.y && rect.y + rect.h <= panel.y + panel.h,
                "{hit:?} must be entirely inside the clipped viewport"
            );
        }
        paint::scroll_by(&mut engine, &mut modal, viewport, 100_000.0);
        assert!(
            paint::layout(&mut engine, &modal, viewport)
                .iter()
                .any(|(hit, _)| *hit == Hit::Submit)
        );
        assert_eq!(paint::hit_at(&mut engine, &modal, viewport, 0.0, 0.0), None);
        paint::scroll_by(&mut engine, &mut modal, viewport, -100_000.0);
        assert_eq!(modal.scroll, 0.0);
        assert!(
            paint::layout(&mut engine, &modal, viewport)
                .iter()
                .any(|(hit, _)| *hit == Hit::Agent(0))
        );
    }
}

#[test]
fn a_readiness_change_never_switches_provider_or_moves_focus_to_an_inserted_retry() {
    let mut engine = TextEngine::new();
    let first = row(0, Availability::Available { version: None }, true);
    let second = row(1, Availability::Available { version: None }, true);
    let mut modal = Delegation::open(&mut engine, world(vec![first.clone(), second.clone()]));
    modal.focus = modal
        .controls()
        .iter()
        .position(|hit| *hit == Hit::Scope(0))
        .unwrap();
    let mut unavailable = first;
    unavailable.readiness.availability = Availability::Missing;
    modal.replace_agents(vec![unavailable, second]);
    assert_eq!(modal.agent, Some(0));
    assert_eq!(modal.focused(), Some(Hit::Scope(0)));
    assert!(
        !modal.ready(),
        "a different ready provider needs a deliberate selection"
    );
    assert_eq!(modal.activate(Hit::Submit), Outcome::Ignored);
}

#[test]
fn claude_wrapper_runtime_and_auth_have_separate_eight_locale_recovery_sentences() {
    use koru_session_protocol::{ProviderDiagnostic, ProviderReadinessFailure, ProviderStage};
    let cases = [
        (
            Availability::Missing,
            None,
            I18nKey::TerminalManagedClaudeWrapperMissing,
        ),
        (
            Availability::SignedOut {
                code: ErrorCode::ProviderUnavailable,
            },
            None,
            I18nKey::TerminalManagedClaudeSignInRequired,
        ),
        (
            Availability::Failed {
                code: ErrorCode::ProviderFailed,
            },
            Some(ProviderDiagnostic::RuntimeUnavailable),
            I18nKey::TerminalManagedClaudeRuntimeUnavailable,
        ),
        (
            Availability::Failed {
                code: ErrorCode::ProviderFailed,
            },
            Some(ProviderDiagnostic::Timeout),
            I18nKey::TerminalManagedRetryRequired,
        ),
    ];
    for (availability, diagnostic, expected) in cases {
        let mut row = row(0, availability, false);
        row.readiness.kind = "claude".parse().unwrap();
        row.readiness.failure = diagnostic.map(|diagnostic| ProviderReadinessFailure {
            stage: ProviderStage::Initialize,
            diagnostic,
        });
        assert_eq!(readiness::guidance(&row.readiness), Some(expected));
        for lang in Lang::ALL {
            assert!(!t(lang, expected).is_empty());
        }
    }
}
