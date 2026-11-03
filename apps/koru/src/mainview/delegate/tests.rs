// SPDX-License-Identifier: MPL-2.0

//! The surface, driven the way a person drives it.
//!
//! Nothing here opens a window, dials a gateway or touches a file: the modal
//! is a value over a [`World`] of values, which is the whole reason it was
//! written that way.

use koru_core::{DocumentId, NodeId};
use koru_delegate_context::ContextSource;
use koru_delegate_core::{BlockNodeId, DocumentId as StoreDocumentId, WorkspaceId, WorkspaceScope};
use koru_draw::text::TextEngine;
use koru_i18n::Key as I18nKey;
use koru_session_protocol::{ProjectId, ProjectSummary};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::modal::{Delegation, Hit, Outcome, Phase};
mod models;
use super::world::{AgentRow, ContextRow, ScopeRow, World, agents_for, scopes_for};

/// The one profile this build has a managed adapter for, resolved from the
/// real catalogue rather than spelled out — a fixture that named a profile the
/// product does not ship would prove nothing about the filter.
fn codex_profile() -> koru_delegate_core::SessionProfileKey {
    koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            profile
                .provider
                .as_ref()
                .and_then(koru_session_profiles::managed_provider)
                .is_some()
        })
        .expect("a delegatable built-in")
        .id
}

fn workspace() -> WorkspaceId {
    WorkspaceId::from_uuid(uuid::Uuid::new_v4())
}

fn a_document() -> DocumentId {
    DocumentId::new()
}

fn projects(names: &[(&str, Option<&str>)]) -> Vec<ProjectSummary> {
    names
        .iter()
        .map(|(name, root)| ProjectSummary {
            id: ProjectId::mint(),
            name: (*name).to_owned(),
            root_directory: root.map(str::to_owned),
            workspace_scope: None,
            created_at_ms: 0,
        })
        .collect()
}

fn a_world(workspace: WorkspaceId) -> World {
    World {
        workspace,
        workspace_name: "Work".into(),
        profile: koru_store_core::ProfileId::from_uuid(uuid::Uuid::new_v4()),
        agents: vec![AgentRow {
            key: codex_profile(),
            name: "Codex (Embedded)".into(),
            provider: koru_session_profiles::CODEX_REGISTRATION
                .kind
                .parse()
                .unwrap(),
            readiness: ready_provider(koru_session_profiles::CODEX_REGISTRATION.kind, true),
        }],
        scopes: scopes_for(
            &projects(&[("koru", Some("D:/koru"))]),
            "A folder Koru makes",
            A_BASE,
        ),
        context: Vec::new(),
        attachable: None,
        objective: String::new(),
        pill_anchor: None,
        temporary_base: A_BASE.into(),
        base_problem: None,
        dropped_context: false,
        chosen_agent: None,
        chosen_model: None,
        chosen_scope: None,
        grant: true,
    }
}

/// A base that is nowhere near this machine's real temporary directory, so a
/// test asserting on the row cannot pass by accident.
const A_BASE: &str = "D:/koru-runs-base";

fn open(engine: &mut TextEngine, world: World) -> Delegation {
    Delegation::open(engine, world)
}

fn context_row(workspace: WorkspaceId, document: DocumentId, node: Option<NodeId>) -> ContextRow {
    let store_document = StoreDocumentId::from_uuid(document.as_uuid());
    ContextRow {
        source: match node {
            Some(node) => ContextSource::Block {
                workspace,
                document: store_document,
                node: BlockNodeId::from_uuid(node.as_uuid()),
            },
            None => ContextSource::Document {
                workspace,
                document: store_document,
            },
        },
        label: "Notes".into(),
        block: node.is_some(),
    }
}

fn press(modal: &mut Delegation, engine: &mut TextEngine, key: NamedKey) -> Outcome {
    modal.key(engine, &Key::Named(key), None, ModifiersState::empty())
}

fn type_str(modal: &mut Delegation, engine: &mut TextEngine, text: &str) {
    for ch in text.chars() {
        let value = ch.to_string();
        modal.key(
            engine,
            &Key::Character(value.clone().into()),
            Some(&value),
            ModifiersState::empty(),
        );
    }
}

/// **One code path, four gestures** (D7, P1.14's first verification).
///
/// Every gesture reaches the same constructor with a [`World`], so the four
/// surfaces may differ only in what the gesture *knew* — the objective's first
/// line and the context it arrived with. Everything else about the modal is
/// identical, and this asserts that rather than asserting that four call sites
/// all spell `begin_delegation`.
#[test]
fn every_gesture_produces_the_same_modal_but_for_its_prefill() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let document = a_document();
    let node = NodeId::new();
    let attached = || vec![context_row(ws, document, Some(node))];

    // What each gesture hands the one constructor.
    let plus = a_world(ws);

    let mut slash = a_world(ws);
    slash.context = attached();

    let mut task = a_world(ws);
    task.objective = "Write the release notes".into();
    task.context = attached();

    let mut selection = a_world(ws);
    selection.context = attached();

    let plus = open(&mut engine, plus);
    let slash = open(&mut engine, slash);
    let task = open(&mut engine, task);
    let selection = open(&mut engine, selection);

    for modal in [&plus, &slash, &task, &selection] {
        // The state a gesture cannot influence, and does not.
        assert_eq!(modal.agent, Some(0), "the same agent is preselected");
        assert_eq!(modal.scope, Some(0), "the same working folder");
        assert!(modal.grant, "D28's grant starts on, whichever gesture");
        assert_eq!(modal.phase, Phase::Editing { problem: None });
        assert_eq!(modal.focused(), Some(Hit::Objective));
    }

    // …and they differ exactly where the gesture knew something.
    assert!(plus.context.is_empty(), "the `+` menu knows no subject");
    assert!(plus.objective.value().is_empty());

    assert_eq!(slash.context.len(), 1);
    assert!(slash.objective.value().is_empty());

    assert_eq!(task.context.len(), 1);
    assert_eq!(task.objective.value(), "Write the release notes");

    assert_eq!(selection.context.len(), 1);
    assert!(selection.objective.value().is_empty());

    // A deterministic state per gesture: the same gesture twice is the same
    // surface, so a test of one opening is a test of every opening of it.
    let again = open(&mut engine, a_world(ws));
    assert_eq!(again.controls(), plus.controls());
}

fn seed_managed_readiness(view: &mut crate::mainview::MainView) {
    view.provider_readiness = Some(
        koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS
            .iter()
            .map(|entry| ready_provider(entry.kind, true))
            .collect(),
    );
}

fn ready_provider(kind: &str, delegatable: bool) -> koru_session_protocol::ProviderReadiness {
    koru_session_protocol::ProviderReadiness {
        kind: kind.parse().unwrap(),
        availability: koru_session_protocol::ExecutableAvailability::Available { version: None },
        capabilities: koru_session_protocol::CapabilitySet::new(if delegatable {
            vec![
                koru_session_protocol::capability::MANAGED_DELEGATION
                    .parse()
                    .unwrap(),
            ]
        } else {
            vec![]
        }),
        failure: None,
    }
}

#[test]
fn registry_candidates_retain_unavailable_rows_and_ignore_raw_presence() {
    let profiles = koru_session_profiles::built_in_profiles();
    let registrations = koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS;
    let ready = registrations
        .iter()
        .map(|entry| ready_provider(entry.kind, true))
        .collect::<Vec<_>>();
    let offered = agents_for(
        &profiles,
        registrations,
        Some(&ready),
        "Platform shell",
        "Custom",
    );
    assert_eq!(offered.len(), registrations.len());
    assert!(offered.iter().all(AgentRow::choosable));
    let pending = agents_for(&profiles, registrations, None, "Platform shell", "Custom");
    assert_eq!(pending.len(), offered.len());
    assert!(pending.iter().all(|row| matches!(
        row.readiness.availability,
        koru_session_protocol::ExecutableAvailability::Checking
    ) && !row.choosable()));
    let empty = agents_for(
        &profiles,
        registrations,
        Some(&[]),
        "Platform shell",
        "Custom",
    );
    assert!(empty.iter().all(|row| matches!(
        row.readiness.availability,
        koru_session_protocol::ExecutableAvailability::Incompatible { .. }
    )));
    for row in offered {
        let profile = profiles
            .iter()
            .find(|profile| profile.id == row.key)
            .unwrap();
        assert_eq!(profile.kind, koru_session_protocol::ProfileKind::Agent);
        assert!(profile.provider.is_some());
    }
}

#[test]
fn a_registered_nondefault_provider_is_a_candidate_but_delegation_requires_its_own_hint() {
    let mut profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| profile.provider.is_some())
        .unwrap();
    profile.provider = Some("fixture".parse().unwrap());
    let registration = koru_session_profiles::ManagedProviderRegistration {
        kind: "fixture",
        executable: "fixture-only",
        arguments: &[],
        launch_policy: koru_session_profiles::ManagedLaunchPolicy::Raw,
        minimum_host_capability: koru_session_protocol::capability::MANAGED_SESSIONS,
        update_command: None,
        sign_in_command: None,
        desktop_app: None,
    };
    let rows = agents_for(
        &[profile.clone()],
        &[registration],
        Some(&[ready_provider("fixture", false)]),
        "Shell",
        "Custom",
    );
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].choosable());
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "fixture objective".into();
    world.agents = rows;
    let mut modal = open(&mut engine, world);
    assert!(modal.agent.is_none());
    modal.activate(Hit::Agent(0));
    assert!(modal.agent.is_none());
    assert!(!modal.ready());
    let rows = agents_for(
        &[profile],
        &[registration],
        Some(&[ready_provider("fixture", true)]),
        "Shell",
        "Custom",
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].choosable());
}

/// A project with no root is **shown** with its reason and can never be chosen
/// — not hidden, because a project you cannot find out about is worse than one
/// you cannot use yet.
#[test]
fn a_project_with_no_folder_is_offered_disabled_and_the_ring_skips_it() {
    let rows = scopes_for(
        &projects(&[("no folder", None), ("koru", Some("D:/koru"))]),
        "A folder Koru makes",
        A_BASE,
    );
    assert_eq!(rows.len(), 3, "two projects and the temporary folder");
    assert_eq!(rows[0].disabled, Some(I18nKey::DelegateScopeNoRoot));
    assert!(rows[1].choosable());
    assert!(
        matches!(rows[2].scope, WorkspaceScope::Temporary),
        "the temporary folder closes the list"
    );

    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.scopes = rows;
    let modal = open(&mut engine, world);
    assert_eq!(
        modal.scope,
        Some(1),
        "the first folder that can be worked in"
    );
    assert!(
        !modal.controls().contains(&Hit::Scope(0)),
        "focus can never land on a folder that is not there"
    );
    assert!(modal.controls().contains(&Hit::Scope(1)));
    assert!(modal.controls().contains(&Hit::Scope(2)));
}

/// **D28's control, and what removing it means.** The grant is the delegating
/// workspace; taking it away leaves an empty set, which denies all workspace
/// access and leaves the run only what is attached.
#[test]
fn removing_the_grant_empties_the_grant_set_in_the_request() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let mut modal = open(&mut engine, a_world(ws));
    type_str(&mut modal, &mut engine, "do the thing");

    assert_eq!(
        modal.grants().into_iter().collect::<Vec<_>>(),
        vec![ws],
        "the delegating surface's workspace is the initial grant"
    );
    assert!(modal.controls().contains(&Hit::GrantRemove));

    let outcome = modal.activate(Hit::GrantRemove);
    assert_eq!(outcome, Outcome::Keep);
    assert!(modal.grants().is_empty(), "an empty set denies everything");
    assert!(
        !modal.controls().contains(&Hit::GrantRemove),
        "there is nothing left to remove"
    );

    let Outcome::Delegate(request) = modal.activate(Hit::Submit) else {
        panic!("the request");
    };
    assert!(
        request.grants.is_empty(),
        "what the run is created with, not just what the surface says"
    );

    // …and with the grant left alone, the workspace travels.
    let mut kept = open(&mut engine, a_world(ws));
    type_str(&mut kept, &mut engine, "do the thing");
    let Outcome::Delegate(request) = kept.activate(Hit::Submit) else {
        panic!("the request");
    };
    assert_eq!(request.grants.into_iter().collect::<Vec<_>>(), vec![ws]);
}

/// **Tab reaches every control and wraps**, and Shift+Tab goes back.
#[test]
fn tab_reaches_every_control_and_wraps() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let mut world = a_world(ws);
    world.context = vec![context_row(ws, a_document(), None)];
    // A second page open in the active tab, so *Attach the open document* is
    // in the ring too — this is the widest the surface ever gets.
    world.attachable = Some(context_row(ws, a_document(), None));
    let mut modal = open(&mut engine, world);
    type_str(&mut modal, &mut engine, "do the thing");

    let ring = modal.controls();
    assert!(ring.contains(&Hit::Objective));
    assert!(ring.contains(&Hit::Agent(0)));
    assert!(ring.contains(&Hit::Scope(0)));
    assert!(ring.contains(&Hit::ContextRemove(0)));
    assert!(ring.contains(&Hit::ContextAdd));
    assert!(ring.contains(&Hit::GrantRemove));
    assert!(ring.contains(&Hit::Submit), "ready, so it is offered");
    assert!(ring.contains(&Hit::Cancel));

    // Tab out of the objective and all the way round.
    let mut seen = vec![modal.focused().expect("something focused")];
    for _ in 1..ring.len() {
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        );
        seen.push(modal.focused().expect("something focused"));
    }
    let objective = ring
        .iter()
        .position(|hit| *hit == Hit::Objective)
        .expect("the objective is in the ring");
    let expected = ring[objective..]
        .iter()
        .chain(ring[..objective].iter())
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(seen, expected, "every control, in the order it is drawn");
    modal.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(modal.focused(), Some(Hit::Objective), "and it wraps");
    modal.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::SHIFT,
    );
    assert_eq!(modal.focused(), expected.last().copied(), "backwards too");
}

/// **Enter writes a newline; Ctrl+Enter delegates.** The objective is a
/// multi-line field, so the chord is what commits — and the chord is
/// `koru_keys::ShortcutId::Delegate`, which the registry carries.
#[test]
fn enter_writes_a_line_and_the_chord_hands_the_work_over() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    type_str(&mut modal, &mut engine, "first");
    assert_eq!(
        press(&mut modal, &mut engine, NamedKey::Enter),
        Outcome::Keep
    );
    type_str(&mut modal, &mut engine, "second");
    assert_eq!(
        modal.objective.value(),
        "first\nsecond",
        "Enter is a newline, not a submission"
    );
    assert_eq!(modal.phase, Phase::Editing { problem: None });

    let outcome = modal.key(
        &mut engine,
        &Key::Named(NamedKey::Enter),
        None,
        koru_keys::PRIMARY,
    );
    assert!(matches!(outcome, Outcome::Delegate(_)));
    assert_eq!(modal.phase, Phase::Sending);

    // The registry is where that chord is written down, so the hint a surface
    // prints and the key that fires cannot disagree.
    assert_eq!(
        koru_keys::REGISTRY
            .iter()
            .filter(|entry| entry.id == koru_keys::ShortcutId::Delegate)
            .count(),
        1
    );
}

/// **Escape cancels, from every phase.** A request already sent is durable
/// before the gateway answers, so closing the surface never unmakes a run.
#[test]
fn escape_always_cancels() {
    let mut engine = TextEngine::new();
    for phase in [
        Phase::Editing { problem: None },
        Phase::Sending,
        Phase::Refused {
            code: "delegate_unavailable".into(),
        },
    ] {
        let mut modal = open(&mut engine, a_world(workspace()));
        modal.answered(phase.clone());
        assert_eq!(
            press(&mut modal, &mut engine, NamedKey::Escape),
            Outcome::Close,
            "{phase:?}"
        );
    }
}

/// A *Delegate* with nothing to do says so, and puts the caret where the
/// missing thing goes.
#[test]
fn an_empty_objective_is_refused_by_the_surface_and_says_why() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    assert!(!modal.ready(), "nothing to do yet");
    assert!(
        !modal.controls().contains(&Hit::Submit),
        "the button is not offered before it could work"
    );

    // Reached anyway — a chord does not go through the ring.
    let outcome = modal.key(
        &mut engine,
        &Key::Named(NamedKey::Enter),
        None,
        koru_keys::PRIMARY,
    );
    assert_eq!(outcome, Outcome::Keep);
    assert_eq!(
        modal.phase,
        Phase::Editing {
            problem: Some(I18nKey::DelegateObjectiveNeeded)
        }
    );
    assert_eq!(modal.focused(), Some(Hit::Objective));

    // Typing takes the complaint away: it was about a moment that has passed.
    type_str(&mut modal, &mut engine, "x");
    assert_eq!(modal.phase, Phase::Editing { problem: None });
    assert!(modal.ready());
}

/// Removing a context row leaves the rest, and leaves focus somewhere real.
#[test]
fn removing_context_keeps_the_rest_and_the_focus() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let mut world = a_world(ws);
    world.context = vec![
        context_row(ws, a_document(), None),
        context_row(ws, a_document(), Some(NodeId::new())),
    ];
    let mut modal = open(&mut engine, world);
    assert_eq!(modal.context.len(), 2);

    modal.focus = modal.controls().len() - 1;
    modal.activate(Hit::ContextRemove(0));
    assert_eq!(modal.context.len(), 1);
    assert!(modal.context[0].block, "the one that was kept");
    assert!(
        modal.focused().is_some(),
        "focus never points past the end of the ring"
    );
}

/// **A finished surface closes on Enter.** A Run-once refusal is Close-only
/// and has no run to look at.
#[test]
fn a_finished_surface_offers_one_way_out() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    modal.answered(Phase::Done {
        run: koru_delegate_core::RunId::mint(),
        document: koru_core::DocumentId::new(),
        busy: None,
    });
    assert_eq!(
        modal.focused(),
        Some(Hit::Close),
        "a finished surface opens on the answer that only shuts the window"
    );
    assert_eq!(
        press(&mut modal, &mut engine, NamedKey::Enter),
        Outcome::Close
    );

    let mut modal = open(&mut engine, a_world(workspace()));
    modal.answered(Phase::Scheduled {
        schedule: koru_delegate_core::ScheduleId::mint(),
    });
    assert_eq!(modal.controls(), vec![Hit::Close]);
    assert!(!modal.controls().contains(&Hit::ViewRun));
    for lang in koru_i18n::Lang::ALL {
        assert_eq!(
            super::paint::finished_sentences(lang, &modal.phase),
            vec![koru_i18n::t(lang, I18nKey::DelegateScheduleCreated).to_owned()]
        );
    }

    let mut modal = open(&mut engine, a_world(workspace()));
    modal.answered(Phase::Refused {
        code: "delegate_unavailable".into(),
    });
    assert_eq!(modal.controls(), vec![Hit::Close]);
    assert!(!modal.controls().contains(&Hit::ViewRun));
    assert_eq!(modal.activate(Hit::Close), Outcome::Close);
}

/// **The confirmation names the run and offers a way to go and look.**
///
/// The maintainers' acceptance testing: *"Don't show the UUID, show the name of the
/// run, also provide a way to View run"*. The sentence read *Delegated. The
/// run is 27e24511-6d05-4d53-bc99-f5c40b5468c9.* — the id was what was to
/// hand before runs had names.
#[test]
fn a_delegated_run_is_named_and_can_be_opened() {
    let mut engine = TextEngine::new();
    let run = koru_delegate_core::RunId::mint();
    let mut modal = open(&mut engine, a_world(workspace()));
    modal.answered(Phase::Done {
        run,
        document: koru_core::DocumentId::new(),
        busy: None,
    });

    let name = koru_delegate_core::RunName::of(run).to_string();
    for lang in koru_i18n::Lang::ALL {
        let said = super::paint::finished_sentences(lang, &modal.phase).join(" ");
        assert!(said.contains(&name), "{lang:?}: {said}");
        assert!(
            !said.contains(&run.to_storage()),
            "{lang:?}: the id is not something a person recognises"
        );
    }

    // The control is in the ring, is drawn where it can be pressed, does not
    // sit on top of *OK*, and asks the window for the run it was told about.
    assert!(modal.controls().contains(&Hit::ViewRun));
    let laid = super::paint::layout(&mut engine, &modal, (1200.0, 900.0));
    let rect = |wanted: Hit| {
        laid.iter()
            .find(|(hit, _)| *hit == wanted)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("{wanted:?} is on the panel"))
    };
    let (view, ok) = (rect(Hit::ViewRun), rect(Hit::Close));
    assert!(view.w > 0.0 && view.h > 0.0);
    assert!(
        view.x + view.w <= ok.x,
        "View run sits beside OK, which keeps the corner it has always had"
    );
    assert_eq!(modal.activate(Hit::ViewRun), Outcome::ViewRun(run));

    // And it has a word in every language, distinct from the button it sits
    // beside — two buttons reading alike is a row you have to try.
    for lang in koru_i18n::Lang::ALL {
        let view = koru_i18n::t(lang, I18nKey::DelegateViewRun);
        assert!(!view.trim().is_empty(), "{lang:?}");
        assert_ne!(view, koru_i18n::t(lang, I18nKey::DelegateOk), "{lang:?}");
        assert_ne!(view, koru_i18n::t(lang, I18nKey::DelegateClose), "{lang:?}");
    }
}

/// ...and it is offered only where there is a run. A refusal started nothing.
#[test]
fn a_refused_delegation_offers_nothing_to_view() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    modal.answered(Phase::Refused {
        code: "delegate_unavailable".into(),
    });
    assert!(!modal.controls().contains(&Hit::ViewRun));
    assert!(
        !super::paint::layout(&mut engine, &modal, (1200.0, 900.0))
            .iter()
            .any(|(hit, _)| *hit == Hit::ViewRun)
    );
    assert_eq!(modal.activate(Hit::ViewRun), Outcome::Ignored);
}

/// **The open and typing paths cannot reach the world.**
///
/// A structural assertion rather than a counter: the three modules a gesture
/// and a keystroke run through do not *name* a client, a filesystem or a
/// thread, so there is no path by which one could. Everything that reaches the
/// gateway is in `job.rs`, which only `Outcome::Delegate` starts, and which
/// runs on a thread of its own.
#[test]
fn opening_and_typing_name_nothing_that_could_block() {
    for (name, module) in [
        ("modal.rs", include_str!("modal.rs")),
        ("world.rs", include_str!("world.rs")),
        ("paint.rs", include_str!("paint.rs")),
    ] {
        let production = module
            .split("#[cfg(test)]")
            .next()
            .expect("the module's production half");
        for forbidden in [
            "koru_api_client",
            "std::fs",
            "std::thread",
            "reqwest",
            "koru_delegate_api",
        ] {
            assert!(
                !production.contains(forbidden),
                "{name} names `{forbidden}`; the open and typing paths must reach nothing"
            );
        }
    }
}

/// A window with no store client refuses at once rather than starting a thread
/// that answers never — the honest shape of every test container, and of a
/// machine whose gateway is down.
#[test]
fn a_surface_with_no_client_refuses_and_dials_nothing() {
    let ws = workspace();
    let request = super::modal::Request {
        token: super::modal::RequestToken::mint(),
        requested_model: None,
        submission: super::modal::Submission::RunOnce {
            run: koru_delegate_core::RunId::mint(),
        },
        workspace: ws,
        profile: koru_store_core::ProfileId::from_uuid(uuid::Uuid::new_v4()),
        objective: "do the thing".into(),
        agent: codex_profile(),
        scope: WorkspaceScope::Temporary,
        sources: Vec::new(),
        grants: std::collections::BTreeSet::new(),
        pill_anchor: None,
    };
    match super::job::start(None, request, None) {
        Err(Phase::Refused { code }) => assert_eq!(code, "delegate_unavailable"),
        other => panic!(
            "expected an immediate refusal, got {other:?}",
            other = other.is_ok()
        ),
    }
}

/// The scope rows a modal offers are exactly the projects the Agents navigator
/// was handed, plus the temporary folder — one list, so a folder you can
/// launch a session in and a folder you can delegate into cannot disagree.
#[test]
fn the_scope_list_is_the_session_projects_plus_a_temporary_folder() {
    let rows: Vec<ScopeRow> = scopes_for(
        &projects(&[("a", Some("D:/a")), ("b", Some("D:/b"))]),
        "A folder Koru makes",
        A_BASE,
    );
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "A folder Koru makes"]);
}

/// **The gestures reach the one path**, through a real container.
///
/// Driven over a container rather than by calling `begin_delegation` directly,
/// so what is asserted is that the row a person presses opens the surface —
/// the half a unit test of the modal cannot see.
mod through_the_container {
    use super::super::DelegationOrigin;
    use super::Hit;
    use crate::mainview::context_menu::{self, MenuAction, MenuSubject};
    use crate::mainview::harness::container;

    #[test]
    fn the_plus_menu_row_opens_the_surface_and_escape_takes_it_down() {
        let (mut view, _workspace) = container();
        assert!(!view.delegation_open());

        // The `+` menu offers it, and choosing it opens the surface.
        assert!(
            context_menu::new_tab_items().contains(&MenuAction::Delegate),
            "the `+` menu carries the entry"
        );
        assert!(view.run_menu_action(MenuSubject::NewTab, MenuAction::Delegate));
        assert!(view.delegation_open(), "one gesture, one surface");

        // Escape takes it down again, whichever gesture opened it.
        view.key(
            &mut koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
                .expect("a document"),
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            winit::keyboard::ModifiersState::empty(),
        );
        assert!(!view.delegation_open(), "Escape cancels");
    }

    /// **Each origin's pre-fill, through the one path.** The gesture is the
    /// only thing that varies, and what it varies is the objective's first
    /// line and the one context row — which is the whole of the difference
    /// between the four.
    #[test]
    fn each_origin_prefills_what_its_gesture_knew() {
        use crate::mainview::delegate::TaskSubject;

        let document = koru_core::DocumentId::new();
        let node = koru_core::NodeId::new();

        let cases: Vec<(DelegationOrigin, &str, usize)> = vec![
            (DelegationOrigin::PlusMenu, "", 0),
            (
                DelegationOrigin::SlashCommand {
                    document,
                    block: Some(node),
                    objective: "Write the release notes".into(),
                },
                "Write the release notes",
                1,
            ),
            (
                DelegationOrigin::TaskContextMenu {
                    task: TaskSubject {
                        document,
                        node,
                        title: "Write the release notes".into(),
                    },
                },
                "Write the release notes",
                1,
            ),
            (
                DelegationOrigin::SelectionContextMenu {
                    document,
                    selection: Some(node),
                    objective: String::new(),
                },
                "",
                1,
            ),
        ];

        for (origin, objective, context) in cases {
            let (mut view, _workspace) = container();
            assert!(view.begin_delegation(origin.clone()), "{origin:?}");
            let modal = view.delegation.as_ref().expect("the surface");
            assert_eq!(modal.objective.value(), objective, "{origin:?}");
            assert_eq!(modal.context.len(), context, "{origin:?}");
            // …and everything a gesture cannot influence is the same.
            assert!(modal.grant, "{origin:?}");
            assert_eq!(modal.focused(), Some(Hit::Objective), "{origin:?}");
        }
    }

    /// **The pointer over the modal is the modal's answer, never the tab's.**
    ///
    /// The surface draws a scrim over the document, so every shape the pointer
    /// takes on it has to come from the panel. It did not: `cursor_icon` had no
    /// branch for this modal at all, so the pointer fell through to the leaf
    /// underneath and the row behind the *Cancel* / *Delegate* pair made it an
    /// I-beam. Driven over a real document, because a container with nothing
    /// behind the panel could not have shown the bug.
    #[test]
    fn the_pointer_is_the_i_beam_over_the_objective_and_the_arrow_over_the_buttons() {
        use crate::mainview::delegate::{Hit, paint};
        use crate::mainview::harness::on_document;
        use koru_workspace_host::View;
        use winit::window::CursorIcon;

        let (mut view, mut workspace) = on_document();
        super::seed_managed_readiness(&mut view);
        crate::mainview::harness::fill(&view, &mut workspace.ws, 60);
        View::adopt(&mut view, &workspace.ws);
        // An objective, so the panel carries the *Delegate* button the
        // screenshot was of: it is only laid out once the request is ready.
        assert!(view.begin_delegation(DelegationOrigin::SlashCommand {
            document: koru_core::DocumentId::new(),
            block: None,
            objective: "Write the release notes".into(),
        }));

        let viewport = view.viewport;
        let controls = {
            let modal = view.delegation.as_ref().expect("the surface");
            paint::layout(&mut view.engine, modal, viewport)
        };
        let centre =
            |rect: &koru_draw::geometry::Rect| (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        for want in [Hit::Cancel, Hit::Submit, Hit::Objective] {
            assert!(
                controls.iter().any(|(hit, _)| *hit == want),
                "{want:?} is on the panel"
            );
        }

        // Every control on the panel wears the plain arrow — Koru does not use
        // the hand — including the *Cancel* / *Delegate* pair the report was
        // about. The one exception is asserted below.
        for (hit, rect) in &controls {
            if *hit == Hit::Objective {
                continue;
            }
            let (x, y) = centre(rect);
            View::mouse_move(&mut view, x, y);
            assert_eq!(
                View::cursor_icon(&view),
                koru_ui::cursor::PRESS,
                "{hit:?} must not borrow the document's I-beam"
            );
        }

        // The one field a caret goes into is the one place the I-beam belongs.
        let (_, rect) = controls
            .iter()
            .find(|(hit, _)| *hit == Hit::Objective)
            .expect("the objective");
        let (x, y) = centre(rect);
        View::mouse_move(&mut view, x, y);
        assert_eq!(View::cursor_icon(&view), CursorIcon::Text);

        // And the scrim, which is over the document and answers for it.
        View::mouse_move(&mut view, 4.0, viewport.1 - 4.0);
        assert_eq!(View::cursor_icon(&view), koru_ui::cursor::IDLE);

        // The bug itself, pinned: the document under the *Delegate* button is
        // rows of text, so with no branch for the modal the pointer read
        // `Text` there. If this stops being true the test above has stopped
        // proving anything and needs a different point.
        let (x, y) = controls
            .iter()
            .find(|(hit, _)| *hit == Hit::Submit)
            .map(|(_, rect)| centre(rect))
            .expect("the delegate button");
        view.delegation = None;
        View::mouse_move(&mut view, x, y);
        assert_eq!(
            View::cursor_icon(&view),
            CursorIcon::Text,
            "the leaf under the panel is what the pointer used to fall through to"
        );
    }
}

/// **Attaching the open document**, which is the whole of "add" in M1: a
/// picker over every document in the workspace is the artifact surface's, and
/// a control that opened nothing would be worse than one that is not there.
#[test]
fn the_open_document_can_be_attached_once_and_then_the_control_goes() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let open_page = context_row(ws, a_document(), None);

    let mut world = a_world(ws);
    world.attachable = Some(open_page.clone());
    let mut modal = open(&mut engine, world);

    assert!(modal.context.is_empty(), "the `+` menu attached nothing");
    assert!(modal.controls().contains(&Hit::ContextAdd));

    assert_eq!(modal.activate(Hit::ContextAdd), Outcome::Keep);
    assert_eq!(modal.context.len(), 1);
    assert_eq!(modal.context[0].source, open_page.source);
    assert!(
        !modal.controls().contains(&Hit::ContextAdd),
        "there is nothing left to attach, so the row is gone rather than dim"
    );

    // Taking it off again offers it again — the two controls are each other's
    // inverse, which is what makes the list a person's rather than a fact.
    modal.activate(Hit::ContextRemove(0));
    assert!(modal.context.is_empty());
    assert!(modal.controls().contains(&Hit::ContextAdd));
}

/// A gesture that already attached the open page does not offer to attach it
/// twice, and a window showing no document offers nothing at all.
#[test]
fn nothing_is_offered_twice_and_a_tab_with_no_document_offers_nothing() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let document = a_document();
    let page = context_row(ws, document, None);

    let mut already = a_world(ws);
    already.attachable = Some(page.clone());
    already.context = vec![page];
    let modal = open(&mut engine, already);
    assert!(
        !modal.controls().contains(&Hit::ContextAdd),
        "the slash command already brought this page along"
    );

    let modal = open(&mut engine, a_world(ws));
    assert!(
        !modal.controls().contains(&Hit::ContextAdd),
        "a session tab or the Welcome screen has no document to attach"
    );
}

// ---------------------------------------------------------------- P1.15: the pill

use super::{DelegationOrigin, TaskSubject};

/// **Where each gesture's crumb lands** (D7's *"leave a crumb behind"*, D8).
///
/// One table rather than four tests, because the four gestures are supposed to
/// differ only in what they arrive with: reading the four answers side by side
/// is what makes a fifth gesture's omission visible.
#[test]
fn each_gesture_says_where_its_pill_goes() {
    let document = a_document();
    let row = NodeId::new();

    // `/delegate`: the row it was typed on. The typed text is erased before
    // the modal opens, so the pill follows what is left.
    assert_eq!(
        DelegationOrigin::SlashCommand {
            document,
            block: Some(row),
            objective: String::new(),
        }
        .pill_anchor(),
        Some((document, row))
    );
    // A selection's *Delegate…*: the selected row.
    assert_eq!(
        DelegationOrigin::SelectionContextMenu {
            document,
            selection: Some(row),
            objective: String::new(),
        }
        .pill_anchor(),
        Some((document, row))
    );
    // A task's *Delegate…*: the task's own row, which is the line a person
    // keeps looking at.
    assert_eq!(
        DelegationOrigin::TaskContextMenu {
            task: TaskSubject {
                document,
                node: row,
                title: "Tidy the release notes".into(),
            },
        }
        .pill_anchor(),
        Some((document, row))
    );
    // **The `+` menu leaves none.** It names no document, and a durable mark
    // in whatever happened to be on screen would land in a document nobody
    // pointed at. ADR-357 records the choice.
    assert_eq!(DelegationOrigin::PlusMenu.pill_anchor(), None);
    // A gesture that knew a document but no row is the `+` menu's case again.
    assert_eq!(
        DelegationOrigin::SlashCommand {
            document,
            block: None,
            objective: String::new(),
        }
        .pill_anchor(),
        None
    );
}

/// **What a gesture pins and where its crumb goes are different questions.**
///
/// A `/delegate` typed on a document's own title row pins the whole document —
/// the root is what the document *is* — and still leaves its pill on that row.
/// The two answers come from two places, and this is the one that must not
/// borrow the other's filter.
#[test]
fn the_title_row_still_gets_a_pill_even_though_it_pins_the_document() {
    let document = a_document();
    let title = NodeId::new();
    let origin = DelegationOrigin::SlashCommand {
        document,
        block: Some(title),
        objective: String::new(),
    };
    assert_eq!(origin.pill_anchor(), Some((document, title)));
}

/// **The anchor rides through the surface untouched.** The modal never learns
/// which gesture made it — everything the gesture knew is a value in the
/// `World` — and the request the worker is handed carries the anchor so an
/// answer arriving after the surface has closed still knows where the crumb
/// belongs.
#[test]
fn the_request_carries_the_anchor_the_gesture_chose() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let anchor = super::world::PillAnchor {
        document: a_document(),
        node: NodeId::new(),
    };
    let mut world = a_world(ws);
    world.pill_anchor = Some(anchor);

    let mut modal = open(&mut engine, world);
    modal.objective.set_text(&mut engine, "do the thing");
    match modal.activate(Hit::Submit) {
        Outcome::Delegate(request) => assert_eq!(request.pill_anchor, Some(anchor)),
        other => panic!("{other:?}"),
    }
}

/// A `+`-menu delegation asks for no pill, all the way through.
#[test]
fn a_plus_menu_delegation_asks_for_no_pill() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    modal.objective.set_text(&mut engine, "do the thing");
    match modal.activate(Hit::Submit) {
        Outcome::Delegate(request) => assert_eq!(request.pill_anchor, None),
        other => panic!("{other:?}"),
    }
}

/// **The pill points at the Run *document*, not at the run id.** The two are
/// different identities and the gateway answers both; a pill carrying the run
/// id would name a document that does not exist.
#[test]
fn the_answer_names_the_run_document() {
    let run = koru_delegate_core::RunId::mint();
    let document = DocumentId::new();
    let phase = Phase::Done {
        run,
        document,
        busy: None,
    };
    match phase {
        Phase::Done {
            run: named,
            document: named_document,
            ..
        } => {
            assert_eq!(named, run);
            assert_eq!(named_document, document);
            assert_ne!(
                named_document.as_uuid(),
                run.as_uuid(),
                "two identities, minted apart"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// **A handed-over delegation is not cancellable, and its button must not say
/// so.** The maintainers found this in acceptance: the panel that says *Delegated. The
/// run is …* offered one button reading **Cancel**, which asks a person
/// whether pressing it takes the work back. It does not — the run was durable
/// before the gateway answered (§7.1), and this window holds nothing that
/// could stop it.
///
/// Sending can still be cancelled; an accepted run and a Run-once refusal
/// only shut their finished surface. Repeat's stateful retry is covered by
/// the schedule-refusal test because it cannot be derived from `Phase` alone.
#[test]
fn the_button_says_what_pressing_it_would_actually_do() {
    use super::paint::action_for;

    let sending = Phase::Sending;
    let done = Phase::Done {
        run: koru_delegate_core::RunId::from_uuid(uuid::Uuid::from_u128(0x246a)),
        document: DocumentId::from_uuid(uuid::Uuid::from_u128(0x246a)),
        busy: None,
    };
    let refused = Phase::Refused {
        code: "delegate_unavailable".into(),
    };

    for lang in koru_i18n::Lang::ALL {
        let (cancel, cancel_hit) = action_for(lang, &sending);
        let (ok, ok_hit) = action_for(lang, &done);
        let (close, refused_hit) = action_for(lang, &refused);

        assert_eq!(
            cancel_hit,
            Hit::Cancel,
            "{lang:?}: sending is the one phase a press can still stop"
        );
        assert_eq!(ok_hit, Hit::Close, "{lang:?}: the run is already real");
        assert_eq!(refused_hit, Hit::Close, "{lang:?}: nothing was started");

        assert_ne!(
            ok, cancel,
            "{lang:?}: a delegated run's button must not read like the one that cancels"
        );
        for word in [cancel, ok, close] {
            assert!(!word.trim().is_empty(), "{lang:?}: every button has a word");
        }
    }
}

/// **A block you right-click has already said what you want done.** The maintainers
/// found this in acceptance: right-clicking a line reading *"Need to research
/// most common kid story themes I can write too for selling"* and choosing
/// *Delegate* opened a modal with an empty objective, asking them to type
/// what was on the screen in front of them.
///
/// The words travel with the gesture, the way a task's title does — **and
/// `/delegate` carries them too**. The command itself is erased before the
/// modal opens, so what is left on the row is what the person wrote around it;
/// the two document gestures now differ in nothing at all. A row holding only
/// the command still opens the empty box, which is the case that never
/// changed.
#[test]
fn a_block_you_delegate_from_becomes_the_objective() {
    let mut engine = TextEngine::new();
    let workspace = workspace();
    let document = a_document();
    let node = NodeId::new();

    let from_block = super::DelegationOrigin::SelectionContextMenu {
        document,
        selection: Some(node),
        objective: "Research the best selling kids' stories".into(),
    };
    let mut world = a_world(workspace);
    world.objective = from_block.objective();
    let modal = open(&mut engine, world);
    assert_eq!(
        modal.objective.value(),
        "Research the best selling kids' stories",
        "the words on the row are what was asked for"
    );

    let from_slash = super::DelegationOrigin::SlashCommand {
        document,
        block: Some(node),
        objective: "Research the best selling kids' stories".into(),
    };
    assert_eq!(
        from_slash.objective(),
        from_block.objective(),
        "the same row, the same words, whichever of the two gestures asked"
    );
    assert!(
        super::DelegationOrigin::SlashCommand {
            document,
            block: Some(node),
            objective: String::new(),
        }
        .objective()
        .is_empty(),
        "a row holding nothing but the command opens the empty box"
    );
}

// -------------------------------------------- the slash command's own context

/// **`/delegate`, driven the way a person types it** — the reproduction the
/// the maintainers' report asked for, and now the guard that keeps the answer.
///
/// Every other test in this file hands `begin_delegation` an origin, which
/// cannot see the half that matters here: the keystroke, `koru_editor`'s
/// `choose_command`, the view's `wants_delegate`, the container's
/// `drain_delegate_request`, and out the other side as the modal's Context
/// list. The report was that the row did not travel; driven end to end it
/// always did. **What did not travel was the row's words** — the right-click
/// filled the objective and `/delegate` opened it empty, which is what "it
/// passed nothing" looks like from in front of the screen.
///
/// So the two gestures are asserted against *each other*, on the same row of
/// the same document, rather than against a fixture: a future change that
/// moves one and not the other fails here whichever way it moves them.
mod the_two_document_gestures {
    use crate::mainview::MainView;
    use crate::mainview::harness::{NONE, ch, on_document, row_y};
    use koru_editor::Editing;
    use koru_workspace::Workspace;
    use koru_workspace_host::View;
    use winit::keyboard::NamedKey;

    /// A document showing one paragraph with words on it, announced active by
    /// the shell the way a real window is told which document it is over.
    fn a_document_with_a_row() -> (MainView, Workspace) {
        let (mut v, mut w) = on_document();
        let doc = View::document_root(&v).expect("a document tab");
        w.ws.insert(
            koru_core::schema::Parent::Node(doc),
            koru_core::node::Node::paragraph("the row".to_owned()).unwrap(),
        )
        .expect("a row");
        View::adopt(&mut v, &w.ws);
        View::show_active_document(
            &mut v,
            &w.ws,
            Some(koru_core::DocumentId::from_uuid(doc.as_uuid())),
        );
        View::adopt(&mut v, &w.ws);
        (v, w)
    }

    /// Put the caret on body row `n` and answer which row that is.
    fn caret_on(v: &mut MainView, w: &mut Workspace, n: usize) -> koru_core::NodeId {
        let rect = v.focused_view_rect();
        v.mouse_down(&mut w.ws, rect.x + 40.0, rect.y + row_y(n), false);
        match &v.tabs().active_tab().view {
            crate::mainview::TabView::Leaf(leaf) => leaf.body().focused().map(|row| row.id),
            _ => None,
        }
        .expect("a focused row")
    }

    fn type_str(v: &mut MainView, w: &mut Workspace, text: &str) {
        for c in text.chars() {
            let s = c.to_string();
            View::key(v, &mut w.ws, &ch(&s), Some(&s), NONE);
        }
    }

    /// What the modal ended up holding: its context sources and its objective.
    fn prefill(v: &MainView) -> (Vec<koru_delegate_context::ContextSource>, String) {
        let modal = v.delegation.as_ref().expect("the surface opened");
        (
            modal.context.iter().map(|row| row.source.clone()).collect(),
            modal.objective.value(),
        )
    }

    /// `/delegate` typed at the end of a line, then the same line's
    /// *Delegate…*. Same row, same document, same everything.
    #[test]
    fn the_slash_command_and_the_right_click_prefill_the_same_thing() {
        let (mut slash, mut slash_ws) = a_document_with_a_row();
        let row = caret_on(&mut slash, &mut slash_ws, 1);
        type_str(&mut slash, &mut slash_ws, "/delegate");
        View::key(
            &mut slash,
            &mut slash_ws.ws,
            &winit::keyboard::Key::Named(NamedKey::Enter),
            None,
            NONE,
        );
        assert!(
            slash.drain_delegate_request(),
            "the slash command opens the one surface"
        );
        let (slash_context, slash_objective) = prefill(&slash);

        let (mut click, mut click_ws) = a_document_with_a_row();
        let same_row = caret_on(&mut click, &mut click_ws, 1);
        match &mut click.tabs_mut().active_mut().view {
            crate::mainview::TabView::Leaf(leaf) => {
                Editing::request_delegation(leaf, Some(same_row), "the row".into())
            }
            _ => panic!("a document tab is a leaf"),
        }
        assert!(
            click.drain_delegate_request(),
            "and so does the right-click"
        );
        let (click_context, click_objective) = prefill(&click);

        // The two documents are different in-memory workspaces, so the ids
        // cannot be compared directly — what must match is the *shape*: one
        // row each, both naming a block rather than the page, and the block
        // each names is the row the gesture happened on.
        assert!(
            matches!(
                slash_context.as_slice(),
                [koru_delegate_context::ContextSource::Block { node, .. }]
                    if *node == koru_delegate_core::BlockNodeId::from_uuid(row.as_uuid())
            ),
            "the row `/delegate` was typed on is the context: {slash_context:?}"
        );
        assert!(
            matches!(
                click_context.as_slice(),
                [koru_delegate_context::ContextSource::Block { node, .. }]
                    if *node == koru_delegate_core::BlockNodeId::from_uuid(same_row.as_uuid())
            ),
            "and so is the row that was right-clicked: {click_context:?}"
        );
        assert_eq!(
            slash_context.len(),
            click_context.len(),
            "neither gesture attaches more than the other"
        );

        // And the half that was missing: the words on the row.
        assert_eq!(
            slash_objective, "the row",
            "what is left of the line is the objective"
        );
        assert_eq!(
            slash_objective, click_objective,
            "the same row, the same words, whichever gesture asked"
        );
    }

    /// `/delegate` alone on a blank line still attaches that line, and still
    /// opens the empty box — the case the acceptance walkthrough uses, and the
    /// one that must not start inventing an objective out of a command.
    #[test]
    fn typed_alone_on_a_blank_line_it_attaches_the_line_and_asks_for_words() {
        let (mut v, mut w) = a_document_with_a_row();
        let blank = caret_on(&mut v, &mut w, 0);
        type_str(&mut v, &mut w, "/delegate");
        View::key(
            &mut v,
            &mut w.ws,
            &winit::keyboard::Key::Named(NamedKey::Enter),
            None,
            NONE,
        );
        assert!(v.drain_delegate_request());
        let (context, objective) = prefill(&v);
        assert!(
            matches!(
                context.as_slice(),
                [koru_delegate_context::ContextSource::Block { node, .. }]
                    if *node == koru_delegate_core::BlockNodeId::from_uuid(blank.as_uuid())
            ),
            "the blank line is still what the run is given: {context:?}"
        );
        assert!(
            objective.is_empty(),
            "a row holding nothing but the command has nothing to say"
        );
    }
}

// -------------------------------- where the work happens, and who decides it

/// **Every row says where it is** (ADR-365). *"A folder Koru makes for this
/// run"* answers what kind of folder it is and not which one; a person handing
/// work to an agent is entitled to know where it will happen before they press
/// the button.
#[test]
fn every_working_folder_row_shows_the_folder_it_means() {
    let rows = scopes_for(
        &projects(&[("koru", Some("D:/koru")), ("no folder", None)]),
        "A folder Koru makes",
        "D:/base",
    );
    assert_eq!(
        rows[0].path.as_deref(),
        Some("D:/koru"),
        "a project shows its own root"
    );
    assert_eq!(
        rows[1].path, None,
        "a project with no root has no path to show, which is why it is dim"
    );
    assert_eq!(
        rows[2].path.as_deref(),
        Some(
            std::path::Path::new("D:/base")
                .join(koru_settings::RUNS_FOLDER)
                .to_string_lossy()
                .as_ref()
        ),
        "and the temporary row shows the folder the run's own directory is \
         made in — the deepest folder that is the same for every run"
    );
}

/// **The base is a control, and the keyboard reaches it.** It sits with the
/// row it belongs to, so Tab walks the list the way the eye does — and it is
/// on the temporary row only, because a project's root belongs to the project.
#[test]
fn only_the_temporary_row_offers_to_change_its_folder() {
    let mut engine = TextEngine::new();
    let modal = open(&mut engine, a_world(workspace()));
    let controls = modal.controls();
    assert!(controls.contains(&Hit::ScopeBase));
    assert_eq!(
        controls
            .iter()
            .filter(|hit| **hit == Hit::ScopeBase)
            .count(),
        1,
        "one folder is Koru's to place, so one control moves it"
    );
    let base = controls
        .iter()
        .position(|hit| *hit == Hit::ScopeBase)
        .expect("the control is in the ring");
    let temporary = modal
        .world
        .scopes
        .iter()
        .position(|row| matches!(row.scope, WorkspaceScope::Temporary))
        .expect("the temporary row");
    assert_eq!(
        controls[base - 1],
        Hit::Scope(temporary),
        "it follows the row it changes"
    );
}

/// Pressing it asks the window for a picker rather than opening one: a native
/// dialog blocks, and nothing on this surface may.
#[test]
fn changing_the_folder_asks_the_window_and_opens_nothing() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    assert_eq!(modal.activate(Hit::ScopeBase), Outcome::ChooseFolder);
}

/// **A chosen folder shows at once**, on the row it changed — and the folder
/// shown is the one the worker will write into, not the base alone.
#[test]
fn a_chosen_folder_lands_on_the_temporary_row() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    modal.chose_temporary_base(Ok("D:/somewhere-else".into()));

    let temporary = modal
        .world
        .scopes
        .iter()
        .find(|row| matches!(row.scope, WorkspaceScope::Temporary))
        .expect("the temporary row");
    assert_eq!(
        temporary.path.as_deref(),
        Some(
            std::path::Path::new("D:/somewhere-else")
                .join(koru_settings::RUNS_FOLDER)
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(modal.world.base_problem.is_none());
    assert!(
        modal
            .world
            .scopes
            .iter()
            .filter(|row| !matches!(row.scope, WorkspaceScope::Temporary))
            .all(|row| row.path.as_deref() == Some("D:/koru")),
        "a project's root is not Koru's to move"
    );
}

/// **A folder Koru cannot write into is refused where it is chosen**, with a
/// sentence — not minutes later when a run will not start in a place nobody
/// remembers choosing. The row keeps the folder it had.
#[test]
fn an_unwritable_folder_is_refused_with_a_sentence_and_changes_nothing() {
    let mut engine = TextEngine::new();
    let mut modal = open(&mut engine, a_world(workspace()));
    let before = modal.world.temporary_base.clone();

    modal.chose_temporary_base(Err(I18nKey::DelegateScopeBaseRefused));

    assert_eq!(
        modal.world.base_problem,
        Some(I18nKey::DelegateScopeBaseRefused)
    );
    assert_eq!(
        modal.world.temporary_base, before,
        "nothing was taken, so nothing moved"
    );
    // …and asking again clears the complaint, which was about the last folder.
    assert_eq!(modal.activate(Hit::ScopeBase), Outcome::ChooseFolder);
    assert!(modal.world.base_problem.is_none());
}

/// **Nothing chosen is this machine's temporary directory** — `%TMP%` on
/// Windows, `/tmp` on Unix — resolved by `koru-settings` so the window and the
/// worker that makes the folder cannot disagree about where the work went.
#[test]
fn with_nothing_chosen_the_base_is_the_platforms_temporary_directory() {
    let (view, _workspace) = crate::mainview::harness::container();
    assert_eq!(
        view.delegate_temporary_base(),
        std::env::temp_dir().to_string_lossy(),
        "an untouched device follows the platform"
    );
}

/// **The picker's answer has to survive the shell's own view** (ADR-367).
///
/// The shell holds a `ManagerView`, never a `MainView`, and calls
/// `View::delegate_folder_chosen` on it — which `ManagerView` did not forward,
/// so the answer met the trait's default `false` and the choice was dropped
/// between the picker closing and the row it was about. Everything below the
/// enum was already right, which is why every test that called the container
/// directly passed while *Change…* did nothing at all.
///
/// So this drives the enum: choose a folder, and the row, the setting the next
/// dialog reads, and the redraw the shell asks for all have to follow.
#[test]
fn a_folder_chosen_in_the_picker_reaches_the_container_through_the_shells_view() {
    use koru_workspace_host::{DelegateFolder, View};

    let (mut view, _workspace) = crate::mainview::harness::container();
    assert!(view.begin_delegation(super::DelegationOrigin::PlusMenu));
    let mut pane = crate::manager_view::ManagerView::Container(view);

    let chosen = std::path::PathBuf::from("D:/work/koru-runs");
    assert!(
        View::delegate_folder_chosen(&mut pane, DelegateFolder::Chosen(chosen.clone())),
        "the answer has to reach the container, and the row it moved wants redrawing"
    );

    let crate::manager_view::ManagerView::Container(view) = &mut pane else {
        unreachable!("the pane this test built")
    };
    assert_eq!(
        view.delegate_temporary_base(),
        chosen.to_string_lossy(),
        "the next dialog, and the next launch, start from what was chosen"
    );
    assert_eq!(
        temporary_row(view).path.as_deref(),
        Some(
            chosen
                .join(koru_settings::RUNS_FOLDER)
                .to_string_lossy()
                .as_ref()
        ),
        "and the row says so at once, without waiting for a round trip"
    );

    // A refusal crosses the same seam, and must not read as a press that did
    // nothing: the row keeps its folder *and* the surface says why.
    assert!(View::delegate_folder_chosen(
        &mut pane,
        DelegateFolder::NotWritable
    ));
    let crate::manager_view::ManagerView::Container(view) = &mut pane else {
        unreachable!("the pane this test built")
    };
    assert_eq!(
        view.delegation
            .as_ref()
            .expect("the surface")
            .world
            .base_problem,
        Some(I18nKey::DelegateScopeBaseRefused)
    );
    assert_eq!(
        view.delegate_temporary_base(),
        chosen.to_string_lossy(),
        "nothing was taken, so nothing moved"
    );
}

/// The row a temporary run's folder is shown on.
fn temporary_row(view: &crate::mainview::MainView) -> &ScopeRow {
    view.delegation
        .as_ref()
        .expect("the surface")
        .world
        .scopes
        .iter()
        .find(|row| matches!(row.scope, WorkspaceScope::Temporary))
        .expect("the temporary row")
}

// ------------------------------------------ ADR-366: the fifth gesture

/// **A finished run, filled back into the surface it came from.**
///
/// The seed is what `koru-runs-ui` hands over; everything here is about what
/// the modal makes of it. Driven through `begin_delegation`, because a re-run
/// is the fifth gesture through the one path and not a second surface.
mod asking_again {
    use super::*;
    use crate::mainview::delegate::DelegationOrigin;
    use crate::mainview::harness::container;
    use koru_delegate_core::{
        BlobHash, BlobKind, DocumentVersionId, Objective, RunContextItem, RunId, SessionProfileKey,
    };

    fn an_agent() -> SessionProfileKey {
        codex_profile()
    }

    fn a_seed(workspace: WorkspaceId) -> koru_runs_ui::ReRun {
        koru_runs_ui::ReRun {
            requested_model: None,
            run: RunId::mint(),
            workspace,
            objective: Objective::new("Write the quarterly summary").expect("an objective"),
            agent: an_agent(),
            scope: WorkspaceScope::Temporary,
            context: vec![
                RunContextItem::Document {
                    workspace,
                    document: StoreDocumentId::from_uuid(uuid::Uuid::new_v4()),
                    version: DocumentVersionId::from_uuid(uuid::Uuid::new_v4()),
                },
                RunContextItem::Block {
                    workspace,
                    document: StoreDocumentId::from_uuid(uuid::Uuid::new_v4()),
                    version: DocumentVersionId::from_uuid(uuid::Uuid::new_v4()),
                    node: BlockNodeId::from_uuid(uuid::Uuid::new_v4()),
                },
                RunContextItem::Blob {
                    workspace,
                    blob: BlobHash::parse(&format!("b3:{}", "a".repeat(64))).expect("a hash"),
                    blob_kind: BlobKind::Pdf,
                },
            ],
            dropped: false,
            grants: [workspace].into_iter().collect(),
        }
    }

    /// The window's workspace, in the store's spelling — what the modal grants
    /// and what a seed has to name for its grant to be recognised.
    fn window_workspace(view: &crate::mainview::MainView) -> WorkspaceId {
        let (_, _, _, workspace) = view.pane_context.clone().expect("a pane context");
        WorkspaceId::from_uuid(workspace.expect("a workspace").as_uuid())
    }

    /// **Every field the run was asked for comes back**, and every one of them
    /// is still editable — the difference between asking again and repeating.
    #[test]
    fn a_re_run_prefills_the_whole_request() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let seed = a_seed(window_workspace(&view));

        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed.clone()),
        }));
        let modal = view.delegation.as_ref().expect("the surface");

        assert_eq!(modal.objective.value(), "Write the quarterly summary");
        assert_eq!(
            modal.world.agents[modal.agent.expect("an agent")].key,
            seed.agent,
            "the agent the run used, not the first installed one"
        );
        assert!(
            matches!(
                modal.chosen_scope().expect("a folder").scope,
                WorkspaceScope::Temporary
            ),
            "the folder the run worked in"
        );
        assert!(modal.grant, "and what it was allowed to reach");
        assert_eq!(modal.context.len(), 3, "everything it was given");
        assert!(
            !modal.world.dropped_context,
            "nothing was left out, so nothing is claimed to have been"
        );

        // **Editable, all of it**: a starting point, not a replay. Typed
        // through the container the way a person types, so what is asserted
        // is that the pre-filled field takes keys rather than that a struct
        // has a public member.
        for ch in [" ", "a", "g", "a", "i", "n"] {
            view.delegation_key(
                &Key::Character(ch.into()),
                Some(ch),
                ModifiersState::empty(),
            );
        }
        let modal = view.delegation.as_ref().expect("the surface");
        assert_eq!(modal.objective.value(), "Write the quarterly summary again");
        assert!(modal.ready(), "and it can be sent");
    }

    /// **A pin becomes a source again.** A new run pins the material as it
    /// stands now rather than inheriting a version somebody has moved past;
    /// asking for the same work on the same page means the page as it is.
    #[test]
    fn a_pinned_version_becomes_the_page_as_it_is_now() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let workspace = window_workspace(&view);
        let seed = a_seed(workspace);
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed.clone()),
        }));
        let modal = view.delegation.as_ref().expect("the surface");

        let RunContextItem::Document { document, .. } = seed.context[0] else {
            panic!("the fixture's first pin is a document");
        };
        assert_eq!(
            modal.context[0].source,
            ContextSource::Document {
                workspace,
                document
            },
            "the document, at no version"
        );
        assert!(matches!(
            modal.context[1].source,
            ContextSource::Block { .. }
        ));
        assert!(modal.context[1].block, "and it reads as one block");
        assert!(matches!(
            modal.context[2].source,
            ContextSource::Blob { .. }
        ));
        // A pin carries ids and no title, deliberately. Every row still says
        // what it is, because a row with no words reads as nothing attached.
        for row in &modal.context {
            assert!(!row.label.trim().is_empty(), "{row:?}");
        }
    }

    /// **A dropped source is said, not silently swallowed** (D48).
    #[test]
    fn a_source_left_behind_is_said_on_the_surface() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let mut seed = a_seed(window_workspace(&view));
        seed.context.truncate(1);
        seed.dropped = true;
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed),
        }));
        let modal = view.delegation.as_ref().expect("the surface");
        assert_eq!(modal.context.len(), 1);
        assert!(modal.world.dropped_context);
    }

    /// **A run that was given no reach comes back with none.** D28's grant is
    /// a choice somebody made, and a re-run that quietly turned it back on
    /// would hand an agent a workspace its first run was deliberately kept out
    /// of.
    #[test]
    fn a_run_with_no_grant_is_asked_for_again_with_no_grant() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let mut seed = a_seed(window_workspace(&view));
        seed.grants = Default::default();
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed),
        }));
        let modal = view.delegation.as_ref().expect("the surface");
        assert!(!modal.grant);
        assert!(modal.grants().is_empty());
    }

    /// **What this machine no longer has, it does not pretend to have.** An
    /// agent that has been uninstalled and a project that has lost its folder
    /// are simply absent from the lists, and the surface starts where it would
    /// have started anyway rather than on a row that is not there.
    #[test]
    fn a_request_naming_what_is_gone_falls_back_rather_than_pointing_at_nothing() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let mut seed = a_seed(window_workspace(&view));
        seed.agent = SessionProfileKey::BuiltIn("no-such-agent".parse().expect("a key"));
        seed.scope = WorkspaceScope::ProjectRoot {
            project: koru_delegate_core::ProjectId::mint(),
        };
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed),
        }));
        let modal = view.delegation.as_ref().expect("the surface");
        assert_eq!(modal.agent, Some(0), "the first agent this machine has");
        assert!(
            modal.chosen_scope().is_some_and(|row| row.choosable()),
            "and a folder that can actually be worked in"
        );
    }

    /// **Pressing Delegate makes a new run**, carrying what was filled in and
    /// nothing of the old run's identity.
    #[test]
    fn delegating_a_re_run_asks_for_a_new_run_and_names_the_old_one_nowhere() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let workspace = window_workspace(&view);
        let seed = a_seed(workspace);
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed.clone()),
        }));

        let modal = view.delegation.as_mut().expect("the surface");
        let Outcome::Delegate(request) = modal.activate(Hit::Submit) else {
            panic!("a filled-in surface can be sent as it stands");
        };

        let crate::mainview::delegate::modal::Submission::RunOnce { run } = request.submission
        else {
            panic!("re-run keeps the run-once path")
        };
        assert_ne!(
            run, seed.run,
            "a new run, not the one it was filled in from"
        );
        assert_eq!(request.workspace, workspace);
        assert_eq!(request.objective, "Write the quarterly summary");
        assert_eq!(request.agent, seed.agent);
        assert_eq!(request.scope, seed.scope);
        assert_eq!(request.sources.len(), seed.context.len());
        assert_eq!(request.grants, seed.grants);
        assert!(
            request.pill_anchor.is_none(),
            "and it marks no document on the way"
        );
    }

    /// **It leaves no crumb.** The gesture happened in a run's own view, which
    /// names no document; writing a mark into whatever page was open would put
    /// it somewhere nobody pointed at — the `+` menu's answer, and ADR-357's
    /// reason.
    #[test]
    fn asking_again_marks_no_document() {
        let (mut view, _workspace) = container();
        seed_managed_readiness(&mut view);
        let seed = a_seed(window_workspace(&view));
        assert!(view.begin_delegation(DelegationOrigin::ReRun {
            seed: Box::new(seed),
        }));
        assert!(
            view.delegation
                .as_ref()
                .expect("the surface")
                .world
                .pill_anchor
                .is_none()
        );
    }
}

/// **Every word this surface gained says something in every language.**
///
/// The compiler already refuses a missing row; what it cannot see is a row
/// that reads the same as another one, which is how a control that changes a
/// folder ends up indistinguishable from the row it sits on.
#[test]
fn the_new_words_read_in_every_language_and_none_of_them_read_alike() {
    use koru_i18n::{Lang, t};
    for lang in Lang::ALL {
        let words = [
            t(lang, I18nKey::DelegateScopeChange),
            t(lang, I18nKey::DelegateScopeBaseRefused),
            t(lang, I18nKey::DelegateFolderPickerTitle),
            t(lang, I18nKey::DelegateContextDropped),
            t(lang, I18nKey::DelegateScopeTemporary),
            t(lang, I18nKey::DelegateScopeNoRoot),
        ];
        for word in words {
            assert!(!word.trim().is_empty(), "{lang:?}");
        }
        for (i, a) in words.iter().enumerate() {
            for b in words.iter().skip(i + 1) {
                assert_ne!(a, b, "{lang:?}: two of this surface's words read alike");
            }
        }
    }
}

#[test]
fn run_once_keeps_the_existing_run_identity_and_crumb_path() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let mut world = a_world(ws);
    world.objective = "Review the notes".into();
    let anchor = super::world::PillAnchor {
        document: a_document(),
        node: NodeId::new(),
    };
    world.pill_anchor = Some(anchor);
    let mut modal = open(&mut engine, world);
    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("the valid run-once draft submits")
    };
    assert!(matches!(
        request.submission,
        super::modal::Submission::RunOnce { .. }
    ));
    assert_eq!(request.pill_anchor, Some(anchor));
}

#[test]
fn repeat_seeds_name_once_and_never_tracks_later_objective_edits() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "  First line  \nSecond line".into();
    let mut modal = open(&mut engine, world);
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    assert_eq!(modal.schedule_name.value(), "First line");
    modal
        .objective
        .set_text(&mut engine, "A changed objective after the seed");
    modal.activate_with_engine(&mut engine, Hit::RunOnce);
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    assert_eq!(modal.schedule_name.value(), "First line");

    let mut empty_world = a_world(workspace());
    empty_world.objective.clear();
    let mut empty = open(&mut engine, empty_world);
    empty.activate_with_engine(&mut engine, Hit::Repeat);
    assert!(empty.schedule_name.value().is_empty());
    empty.objective.set_text(&mut engine, "Written later");
    assert!(empty.schedule_name.value().is_empty());
    assert!(!empty.ready());
}

#[test]
fn edit_form_is_fixed_to_repeat_prefilled_and_submits_the_observed_snapshot() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let other = workspace();
    let mut world = a_world(ws);
    world.objective = "Research launches".into();
    let context = context_row(ws, a_document(), None);
    world.context = vec![context.clone()];
    world.chosen_agent = Some(codex_profile());
    world.chosen_scope = Some(WorkspaceScope::Temporary);
    world.grant = true;
    let original = koru_delegate_schedule::ScheduleName::new("Launch research").unwrap();
    let recurrence =
        koru_recurrence::RecurrenceRule::zoned("*/5 * * * *".parse().unwrap(), "Europe/Paris")
            .unwrap();
    let requested_by = koru_delegate_core::ActorRef::Orchestrator {
        run: koru_delegate_core::RunId::mint(),
    };
    let schedule = koru_delegate_core::ScheduleId::mint();
    let mut modal = Delegation::open_edit_with_environment(
        &mut engine,
        world,
        koru_i18n::time::LocaleFormat::default(),
        "America/New_York",
        super::modal::EditSeed {
            schedule,
            expected_revision: 7,
            expected_name: original.clone(),
            requested_by,
            grants: std::collections::BTreeSet::from([ws, other]),
        },
        &original,
        &recurrence,
    );

    assert_eq!(modal.kind, super::modal::repeat::Kind::Repeat);
    assert!(!modal.controls().contains(&Hit::RunOnce));
    assert!(!modal.controls().contains(&Hit::Repeat));
    assert_eq!(modal.schedule_name.value(), "Launch research");
    assert_eq!(modal.context, vec![context.clone()]);
    assert_eq!(modal.repeat.rule(), Some(recurrence.clone()));
    assert_eq!(modal.repeat.advanced.value(), "*/5 * * * *");
    assert!(modal.ready());
    assert!(modal.controls().contains(&Hit::Submit));
    assert_eq!(
        modal.grants(),
        std::collections::BTreeSet::from([ws, other])
    );
    assert_eq!(
        modal.activate_with_engine(&mut engine, Hit::RunOnce),
        Outcome::Ignored
    );

    modal.schedule_name.set_text(&mut engine, "Launch brief");
    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("a valid edit submits")
    };
    let super::modal::Submission::Edit {
        schedule: submitted,
        expected_revision,
        expected_name,
        requested_by: submitted_by,
        name,
        recurrence: submitted_rule,
    } = request.submission
    else {
        panic!("edit mode must not mint a new schedule")
    };
    assert_eq!(submitted, schedule);
    assert_eq!(expected_revision, 7);
    assert_eq!(expected_name, original);
    assert_eq!(submitted_by, requested_by);
    assert_eq!(name.as_str(), "Launch brief");
    assert_eq!(submitted_rule, recurrence);
    assert_eq!(
        request.grants,
        std::collections::BTreeSet::from([ws, other])
    );
    assert_eq!(request.sources, vec![context.source]);
}

#[test]
fn edit_remains_saveable_while_its_existing_execution_choices_are_unavailable() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let mut world = a_world(ws);
    world.objective = "Research launches".into();
    world.chosen_model = Some(koru_delegate_core::ModelName::new("provider/retained").unwrap());
    world.agents[0].readiness.availability =
        koru_session_protocol::ExecutableAvailability::Checking;
    world.scopes[0].disabled = Some(koru_i18n::Key::DelegateScopeNoRoot);
    world.chosen_scope = Some(world.scopes[0].scope.clone());
    world.chosen_agent = Some(world.agents[0].key.clone());
    let original = koru_delegate_schedule::ScheduleName::new("Launch research").unwrap();
    let recurrence = koru_recurrence::RecurrenceRule::floating("*/5 * * * *".parse().unwrap());
    let schedule = koru_delegate_core::ScheduleId::mint();
    let requested_by = koru_delegate_core::ActorRef::Orchestrator {
        run: koru_delegate_core::RunId::mint(),
    };
    let agent = world.agents[0].key.clone();
    let mut modal = Delegation::open_edit_with_environment(
        &mut engine,
        world,
        koru_i18n::time::LocaleFormat::default(),
        "America/New_York",
        super::modal::EditSeed {
            schedule,
            expected_revision: 7,
            expected_name: original.clone(),
            requested_by,
            grants: std::collections::BTreeSet::from([ws]),
        },
        &original,
        &recurrence,
    );
    assert!(!modal.models.selection_valid());
    assert!(!modal.world.agents[0].choosable());
    assert!(!modal.world.scopes[0].choosable());
    assert!(modal.ready());
    assert!(modal.controls().contains(&Hit::Submit));
    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("an existing schedule remains editable while execution is unavailable")
    };
    assert_eq!(request.agent, agent);
    assert_eq!(
        request.requested_model.unwrap().as_str(),
        "provider/retained"
    );
    assert_eq!(request.scope, modal.world.scopes[0].scope);
}

#[test]
fn repeat_keeps_live_context_model_and_its_schedule_crumb_anchor() {
    let mut engine = TextEngine::new();
    let ws = workspace();
    let document = a_document();
    let node = NodeId::new();
    let sources = vec![
        context_row(ws, document, None),
        context_row(ws, document, Some(node)),
        ContextRow {
            source: ContextSource::Blob {
                workspace: ws,
                blob: koru_delegate_core::BlobHash::parse(&format!("b3:{}", "a".repeat(64)))
                    .unwrap(),
                blob_kind: koru_delegate_core::BlobKind::Pdf,
            },
            label: "Brief.pdf".into(),
            block: false,
        },
    ];
    let expected = sources
        .iter()
        .map(|row| super::job::schedule_pointer(row.source.clone()))
        .collect::<Vec<_>>();
    let model = koru_delegate_core::ModelName::new("openai/gpt-5").unwrap();
    let mut world = a_world(ws);
    world.objective = "Review the notes every morning".into();
    world.context = sources;
    let pill_anchor = Some(super::world::PillAnchor { document, node });
    world.pill_anchor = pill_anchor;
    let mut modal = open(&mut engine, world);
    modal.models.offered = true;
    modal.models.catalog = Some(
        vec![koru_session_protocol::ProviderModel::new(
            "openai/gpt-5".parse().unwrap(),
            "openai".parse().unwrap(),
        )]
        .try_into()
        .unwrap(),
    );
    modal.models.filtered = vec![0];
    modal.models.selected = Some(model.clone());
    modal.activate_with_engine(&mut engine, Hit::Repeat);

    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("the valid repeating draft submits")
    };
    assert_eq!(modal.phase, Phase::Scheduling);
    let super::modal::Submission::Repeat {
        schedule,
        name,
        recurrence,
    } = request.submission.clone()
    else {
        panic!("Repeat creates schedule intent, not a run")
    };
    assert_eq!(recurrence.cron().as_str(), "0 9 * * *");
    assert_eq!(request.requested_model, Some(model.clone()));
    assert_eq!(request.pill_anchor, pill_anchor);

    let body = super::job::schedule_body(*request.clone(), schedule, name, recurrence)
        .expect("a valid modal request becomes a schedule body without I/O");
    assert_eq!(body.schedule, schedule);
    assert_eq!(body.workspace, request.workspace);
    assert_eq!(
        body.template.objective,
        koru_delegate_core::Objective::new(request.objective.clone()).unwrap()
    );
    assert_eq!(body.template.agent, request.agent);
    assert_eq!(body.template.requested_model, Some(model));
    assert_eq!(body.template.scope, request.scope);
    assert_eq!(body.template.grants, request.grants);
    assert_eq!(
        body.template.context, expected,
        "sources remain live pointers"
    );
    assert!(
        matches!(body.template.requested_by, koru_delegate_core::ActorRef::Person { profile } if profile == request.profile)
    );
}

#[test]
fn invalid_advanced_cron_stays_authored_and_removes_submit_from_the_ring() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Review the notes".into();
    let mut modal = open(&mut engine, world);
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    let before = modal.repeat.parts;
    modal.repeat.advanced.set_text(&mut engine, "*/5 9 * * *");
    modal.repeat.advanced_changed(&mut engine);

    assert_eq!(modal.repeat.advanced.value(), "*/5 9 * * *");
    assert_eq!(modal.repeat.parts, before, "the last valid picker survives");
    assert!(!modal.controls().contains(&Hit::Submit));
    assert_eq!(
        modal.activate_with_engine(&mut engine, Hit::Submit),
        Outcome::Keep
    );
    assert!(matches!(modal.phase, Phase::Editing { .. }));
}

#[test]
fn advanced_minute_interval_and_list_submit_as_the_canonical_schedule() {
    for (authored, canonical) in [
        ("*/5 * * * *", "*/5 * * * *"),
        ("45,05,30 * * * *", "5,30,45 * * * *"),
    ] {
        let mut engine = TextEngine::new();
        let mut world = a_world(workspace());
        world.objective = "Review the notes".into();
        let mut modal = open(&mut engine, world);
        modal.activate_with_engine(&mut engine, Hit::Repeat);
        modal.repeat.advanced.set_text(&mut engine, authored);
        modal.repeat.advanced_changed(&mut engine);

        assert!(modal.repeat.advanced_valid);
        assert!(!modal.repeat.picker_active);
        assert!(modal.controls().contains(&Hit::Submit));

        let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit)
        else {
            panic!("{authored} should create a schedule request")
        };
        let super::modal::Submission::Repeat { recurrence, .. } = request.submission else {
            panic!("advanced minute rules remain repeating schedules")
        };
        assert_eq!(recurrence.cron().as_str(), canonical);
    }
}

#[test]
fn every_picker_shape_round_trips_through_the_modal_in_every_locale() {
    use koru_i18n::{Lang, time::LocaleFormat};
    use koru_recurrence::RuleShape;

    let shapes = [
        (Hit::ScheduleHourly, RuleShape::Hourly),
        (Hit::ScheduleDaily, RuleShape::Daily),
        (Hit::ScheduleWeekly, RuleShape::Weekly),
        (Hit::ScheduleMonthly, RuleShape::Monthly),
    ];
    for lang in Lang::ALL {
        for (hit, shape) in shapes {
            let mut engine = TextEngine::new();
            let mut world = a_world(workspace());
            world.objective = "Review the notes".into();
            let mut modal = Delegation::open_with_environment(
                &mut engine,
                world,
                LocaleFormat::for_system(Some(lang.tag()), None),
                "America/New_York",
            );
            modal.activate_with_engine(&mut engine, Hit::Repeat);
            modal.activate_with_engine(&mut engine, hit);
            let before = modal.repeat.parts;
            let canonical = modal.repeat.advanced.value();

            modal.repeat.advanced.set_text(&mut engine, &canonical);
            modal.repeat.advanced_changed(&mut engine);
            modal.repeat.settle_advanced(&mut engine);
            assert_eq!(modal.repeat.parts, before, "{lang:?} {shape:?}");
            assert_eq!(modal.repeat.frequency, shape, "{lang:?} {shape:?}");
            assert_eq!(modal.repeat.advanced.value(), canonical);
            assert!(modal.controls().contains(&Hit::Submit));

            let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit)
            else {
                panic!("{lang:?} {shape:?} should submit")
            };
            let super::modal::Submission::Repeat { recurrence, .. } = request.submission else {
                panic!("the Repeat picker must produce a schedule")
            };
            assert_eq!(recurrence.cron().parts(), before, "{lang:?} {shape:?}");
        }
    }
}

#[test]
fn invalid_picker_drafts_preserve_the_rule_and_disable_submit() {
    use koru_i18n::time::LocaleFormat;

    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Review the notes".into();
    let mut modal = Delegation::open_with_environment(
        &mut engine,
        world,
        LocaleFormat::for_system(Some("ar"), None),
        "America/New_York",
    );
    modal.activate_with_engine(&mut engine, Hit::Repeat);

    let before = modal.repeat.parts;
    modal.repeat.time.set_text(&mut engine, "not a time");
    modal.repeat.accept_picker(&mut engine);
    assert_eq!(modal.repeat.parts, before);
    assert!(!modal.repeat.time_valid());
    assert!(!modal.controls().contains(&Hit::Submit));

    modal.repeat.time.set_text(&mut engine, "٩:٠٠ ص");
    modal.repeat.accept_picker(&mut engine);
    modal.activate_with_engine(&mut engine, Hit::ScheduleMonthly);
    assert_eq!(modal.repeat.day.value(), "١", "Arabic day is localized");
    let before = modal.repeat.parts;
    modal.repeat.day.set_text(&mut engine, "٣٢");
    modal.repeat.accept_picker(&mut engine);
    assert_eq!(modal.repeat.parts, before);
    assert!(!modal.repeat.day_valid());
    assert!(!modal.controls().contains(&Hit::Submit));

    modal.repeat.day.set_text(&mut engine, "٣١");
    modal.repeat.accept_picker(&mut engine);
    assert_eq!(modal.repeat.parts.day_of_month(), Some(31));
    modal.activate_with_engine(&mut engine, Hit::ScheduleZoned);
    let before = modal.repeat.parts;
    modal.repeat.zone.set_text(&mut engine, "not/a-zone");
    assert_eq!(modal.repeat.parts, before);
    assert!(!modal.repeat.zone_valid());
    assert!(!modal.controls().contains(&Hit::Submit));
}

#[test]
fn every_new_schedule_word_exists_in_all_eight_languages() {
    use koru_i18n::{Lang, t};
    let keys = [
        I18nKey::DelegateModeLabel,
        I18nKey::DelegateRunOnce,
        I18nKey::DelegateRepeat,
        I18nKey::DelegateScheduleLabel,
        I18nKey::DelegateScheduleHourly,
        I18nKey::DelegateScheduleDaily,
        I18nKey::DelegateScheduleWeekly,
        I18nKey::DelegateScheduleMonthly,
        I18nKey::DelegateScheduleTime,
        I18nKey::DelegateScheduleTimeInvalid,
        I18nKey::DelegateScheduleDay,
        I18nKey::DelegateScheduleDayInvalid,
        I18nKey::DelegateScheduleAdvanced,
        I18nKey::DelegateScheduleAdvancedInvalid,
        I18nKey::DelegateScheduleTimeBasis,
        I18nKey::DelegateScheduleFloating,
        I18nKey::DelegateScheduleZoned,
        I18nKey::DelegateScheduleZone,
        I18nKey::DelegateScheduleZoneInvalid,
        I18nKey::DelegateScheduleSubmit,
        I18nKey::DelegateScheduleSending,
        I18nKey::DelegateScheduleCreated,
        I18nKey::DelegateRetry,
    ];
    for lang in Lang::ALL {
        for key in keys {
            assert!(!t(lang, key).trim().is_empty(), "{lang:?} {key:?}");
        }
    }
}

#[test]
fn refused_run_once_keeps_the_draft_visible_but_offers_close_only() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Review the notes".into();
    let mut modal = open(&mut engine, world);
    let Outcome::Delegate(first) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("run once submits")
    };
    let first = first.correlation();
    assert!(modal.answered_for(
        first,
        Phase::Refused {
            code: "gateway_busy".into(),
        },
    ));
    assert!(matches!(modal.phase, Phase::Refused { .. }));
    assert_eq!(modal.controls(), vec![Hit::Close]);
    assert_eq!(modal.objective.value(), "Review the notes");
    assert_eq!(
        modal.activate_with_engine(&mut engine, Hit::Retry),
        Outcome::Ignored,
        "run once cannot safely repin live sources under an old RunId"
    );
    assert!(matches!(modal.phase, Phase::Refused { .. }));
}

#[test]
fn refused_repeat_replays_the_same_schedule_id_and_body_after_unknown_outcome() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Review the notes each morning".into();
    let mut modal = open(&mut engine, world);
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    let Outcome::Delegate(first) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("repeat submits")
    };
    let first_request = *first;
    let first = first_request.correlation();
    let super::modal::Submission::Repeat {
        schedule,
        name,
        recurrence,
    } = first_request.submission.clone()
    else {
        panic!("repeat request")
    };
    let first_body = super::job::schedule_body(
        first_request.clone(),
        schedule,
        name.clone(),
        recurrence.clone(),
    )
    .expect("valid initial body");
    assert!(modal.answered_for(
        first,
        Phase::Refused {
            code: "unknown_outcome".into(),
        },
    ));
    assert!(matches!(modal.phase, Phase::Refused { .. }));
    assert_eq!(modal.controls(), vec![Hit::Retry, Hit::Close]);
    assert_eq!(modal.objective.value(), "Review the notes each morning");

    let Outcome::Delegate(retry) = modal.activate_with_engine(&mut engine, Hit::Retry) else {
        panic!("retry resubmits the retained schedule draft")
    };
    let retry_request = *retry;
    let retry = retry_request.correlation();
    let retry_body = super::job::schedule_body(retry_request.clone(), schedule, name, recurrence)
        .expect("valid replay body");
    assert_eq!(modal.phase, Phase::Scheduling);
    assert_eq!(retry_request, first_request);
    assert_eq!(retry, first);
    assert_eq!(retry_body, first_body);
    assert_eq!(retry.mode, super::modal::repeat::Kind::Repeat);
}

#[test]
fn a_late_answer_from_a_closed_surface_cannot_mutate_the_reopened_modal() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Old request".into();
    let mut old = open(&mut engine, world.clone());
    let Outcome::Delegate(request) = old.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("old surface submits")
    };
    let correlation = request.correlation();
    let super::modal::RequestTarget::Run(run) = correlation.target else {
        panic!("run-once identity")
    };

    world.objective = "New draft".into();
    let mut reopened = open(&mut engine, world.clone());
    assert!(!reopened.answered_for(
        correlation,
        Phase::Done {
            run,
            document: a_document(),
            busy: None,
        },
    ));
    assert_eq!(reopened.phase, Phase::Editing { problem: None });
    assert_eq!(reopened.objective.value(), "New draft");

    let mut reopened = open(&mut engine, world);
    assert!(!reopened.answered_for(
        correlation,
        Phase::Refused {
            code: "late_failure".into(),
        },
    ));
    assert_eq!(reopened.phase, Phase::Editing { problem: None });
    assert_eq!(reopened.objective.value(), "New draft");
}

#[test]
fn response_correlation_includes_mode_token_and_target_identity() {
    let mut engine = TextEngine::new();
    let mut world = a_world(workspace());
    world.objective = "Repeat this".into();
    let mut modal = open(&mut engine, world);
    modal.activate_with_engine(&mut engine, Hit::Repeat);
    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("repeat submits")
    };
    let correlation = request.correlation();
    let super::modal::RequestTarget::Schedule(schedule) = correlation.target else {
        panic!("schedule identity")
    };

    modal.kind = super::modal::repeat::Kind::RunOnce;
    assert!(!modal.answered_for(correlation, Phase::Scheduled { schedule }));
    assert_eq!(modal.phase, Phase::Scheduling);

    modal.kind = super::modal::repeat::Kind::Repeat;
    let wrong_token = super::modal::RequestCorrelation {
        token: super::modal::RequestToken::mint(),
        ..correlation
    };
    assert!(!modal.answered_for(wrong_token, Phase::Scheduled { schedule }));
    let wrong_target = super::modal::RequestCorrelation {
        target: super::modal::RequestTarget::Schedule(koru_delegate_core::ScheduleId::mint()),
        ..correlation
    };
    assert!(!modal.answered_for(wrong_target, Phase::Scheduled { schedule }));
    assert!(!modal.answered_for(
        correlation,
        Phase::Scheduled {
            schedule: koru_delegate_core::ScheduleId::mint(),
        },
    ));
    assert!(modal.answered_for(correlation, Phase::Scheduled { schedule }));
    assert_eq!(modal.phase, Phase::Scheduled { schedule });
}

#[test]
fn malformed_success_has_no_modal_pill_or_toast_side_effect() {
    let (mut view, _workspace) = crate::mainview::harness::container();
    let mut engine = TextEngine::new();
    let anchor = super::world::PillAnchor {
        document: a_document(),
        node: NodeId::new(),
    };
    let mut world = a_world(workspace());
    world.objective = "Review the notes".into();
    world.pill_anchor = Some(anchor);
    let mut modal = open(&mut engine, world);
    let Outcome::Delegate(request) = modal.activate_with_engine(&mut engine, Hit::Submit) else {
        panic!("run once submits")
    };
    let correlation = request.correlation();
    let (send, receiver) = std::sync::mpsc::channel();
    view.delegation = Some(modal);
    view.delegate_job = Some(super::job::Job {
        correlation,
        pill_anchor: Some(anchor),
        schedule_name: None,
        receiver,
    });
    send.send(super::job::Answer {
        correlation,
        phase: Phase::Done {
            run: koru_delegate_core::RunId::mint(),
            document: a_document(),
            busy: None,
        },
    })
    .unwrap();

    assert!(!view.maintain_delegation());
    assert!(view.pending_run_pills.is_empty());
    assert!(view.toast.is_none());
    assert!(matches!(
        view.delegation.as_ref().map(|modal| &modal.phase),
        Some(Phase::Sending)
    ));
}

#[test]
fn closing_run_a_and_attempting_b_keeps_a_receiver_and_delivers_its_global_side_effects() {
    let (mut view, _workspace) = crate::mainview::harness::container();
    seed_managed_readiness(&mut view);
    let anchor = super::world::PillAnchor {
        document: a_document(),
        node: NodeId::new(),
    };
    assert!(
        view.begin_delegation(super::DelegationOrigin::SlashCommand {
            document: anchor.document,
            block: Some(anchor.node),
            objective: "Review the notes".into(),
        })
    );
    let Outcome::Delegate(request) = view
        .delegation
        .as_mut()
        .expect("A is open")
        .activate(Hit::Submit)
    else {
        panic!("A submits")
    };
    let correlation = request.correlation();
    let super::modal::RequestTarget::Run(run) = correlation.target else {
        panic!("A is a run")
    };
    let (send, receiver) = std::sync::mpsc::channel();
    view.delegate_job = Some(super::job::Job {
        correlation,
        pill_anchor: Some(anchor),
        schedule_name: None,
        receiver,
    });
    assert!(view.settle_delegation(Outcome::Close));
    assert!(!view.delegation_open());

    assert!(view.begin_delegation(super::DelegationOrigin::PlusMenu));
    assert!(!view.delegation_open(), "B is not opened over hidden A");
    assert_eq!(
        view.delegate_job.as_ref().map(|job| job.correlation),
        Some(correlation),
        "B cannot replace A's receiver"
    );
    assert!(view.toast.is_some(), "the blocked opening is visible");
    let mut defensive_b = (*request).clone();
    defensive_b.token = super::modal::RequestToken::mint();
    defensive_b.submission = super::modal::Submission::RunOnce {
        run: koru_delegate_core::RunId::mint(),
    };
    assert!(view.settle_delegation(Outcome::Delegate(Box::new(defensive_b))));
    assert_eq!(
        view.delegate_job.as_ref().map(|job| job.correlation),
        Some(correlation),
        "even a future route that produces B's outcome cannot overwrite A"
    );
    view.toast = None;

    let run_document = a_document();
    send.send(super::job::Answer {
        correlation,
        phase: Phase::Done {
            run,
            document: run_document,
            busy: None,
        },
    })
    .unwrap();
    assert!(view.maintain_delegation());
    assert!(view.delegate_job.is_none());
    assert!(view.toast.is_some(), "hidden A reports its success");
    assert_eq!(view.pending_run_pills.len(), 1);
    assert_eq!(view.pending_run_pills[0].document, anchor.document);
    assert_eq!(view.pending_run_pills[0].node, anchor.node);
    assert_eq!(view.pending_run_pills[0].run, run_document);
}

#[test]
fn closing_schedule_a_and_attempting_b_keeps_a_success_or_refusal_receiver() {
    for succeeds in [true, false] {
        let (mut view, _workspace) = crate::mainview::harness::container();
        seed_managed_readiness(&mut view);
        assert!(view.begin_delegation(super::DelegationOrigin::PlusMenu));
        let modal = view.delegation.as_mut().expect("A is open");
        modal
            .objective
            .set_text(&mut view.engine, "Review each morning");
        modal.activate_with_engine(&mut view.engine, Hit::Repeat);
        let Outcome::Delegate(request) = modal.activate_with_engine(&mut view.engine, Hit::Submit)
        else {
            panic!("A submits")
        };
        let correlation = request.correlation();
        let super::modal::RequestTarget::Schedule(schedule) = correlation.target else {
            panic!("A is a schedule")
        };
        let (send, receiver) = std::sync::mpsc::channel();
        view.delegate_job = Some(super::job::Job {
            correlation,
            pill_anchor: None,
            schedule_name: Some(
                koru_delegate_schedule::ScheduleName::new("Review each morning").unwrap(),
            ),
            receiver,
        });
        assert!(view.settle_delegation(Outcome::Close));

        assert!(view.begin_delegation(super::DelegationOrigin::PlusMenu));
        assert!(!view.delegation_open(), "B is not opened over hidden A");
        assert_eq!(
            view.delegate_job.as_ref().map(|job| job.correlation),
            Some(correlation)
        );
        view.toast = None;

        let phase = if succeeds {
            Phase::Scheduled { schedule }
        } else {
            Phase::Refused {
                code: "gateway_busy".into(),
            }
        };
        send.send(super::job::Answer { correlation, phase })
            .unwrap();
        assert!(view.maintain_delegation());
        assert!(view.delegate_job.is_none());
        assert!(
            view.toast.is_some(),
            "hidden A reports both success and refusal"
        );
        assert!(view.pending_run_pills.is_empty());
    }
}

#[test]
fn clicking_the_scrim_closes_a_create_or_edit_draft() {
    let (mut view, _workspace) = crate::mainview::harness::container();
    assert!(view.begin_delegation(super::DelegationOrigin::PlusMenu));
    assert!(view.delegation_open());

    assert!(view.delegation_mouse_down(0.0, 0.0));
    assert!(!view.delegation_open());
}

#[test]
fn an_accepted_document_origin_schedule_queues_one_named_schedule_pill() {
    let (mut view, _workspace) = crate::mainview::harness::container();
    let anchor = super::world::PillAnchor {
        document: a_document(),
        node: NodeId::new(),
    };
    let schedule = koru_delegate_core::ScheduleId::mint();
    let name = koru_delegate_schedule::ScheduleName::new("Daily research").unwrap();
    let correlation = super::modal::RequestCorrelation {
        token: super::modal::RequestToken::mint(),
        mode: super::modal::repeat::Kind::Repeat,
        target: super::modal::RequestTarget::Schedule(schedule),
        edit: false,
    };
    let (send, receiver) = std::sync::mpsc::channel();
    view.delegate_job = Some(super::job::Job {
        correlation,
        pill_anchor: Some(anchor),
        schedule_name: Some(name.clone()),
        receiver,
    });
    send.send(super::job::Answer {
        correlation,
        phase: Phase::Scheduled { schedule },
    })
    .unwrap();

    assert!(view.maintain_delegation());
    assert_eq!(view.pending_schedule_pills.len(), 1);
    let pill = &view.pending_schedule_pills[0];
    assert_eq!(pill.document, anchor.document);
    assert_eq!(pill.node, anchor.node);
    assert_eq!(pill.schedule, schedule);
    assert_eq!(pill.label, name.as_str());
}
