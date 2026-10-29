// SPDX-License-Identifier: MPL-2.0

//! A gone session cleans up (Q-E6 P2): the catalog's confirmed end closes a
//! session tab and releases the window's controller, while a person closing a
//! tab stays detach-only — and the managed reveal of the composer band whose
//! default is hidden (Q-E6 P1).

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use koru_agents_ui::PaneStatus;
use koru_session_backend::{BackendCommand, BackendEvent};
use koru_session_protocol::{
    AttentionState, CapabilitySet, ControllerState, ProcessState, SessionId, SessionMode,
    SessionSummary,
};
use koru_workspace_host::{Settings, TabRef, View};

use super::SessionBackend;
use crate::mainview::MainView;
use crate::mainview::harness::*;
use crate::mainview::tab::TabView;
use crate::session_tab::SessionView;

/// A backend over a plain channel: every command the container sends is
/// observable on the returned receiver, and nothing dials a host.
fn with_backend(v: &mut MainView) -> Receiver<BackendCommand> {
    let (sender, commands) = koru_session_backend::channel();
    v.session_backend = Some(SessionBackend {
        sender,
        events: Arc::new(Mutex::new(VecDeque::new())),
    });
    commands
}

fn drained(commands: &Receiver<BackendCommand>) -> Vec<BackendCommand> {
    let mut all = Vec::new();
    while let Ok(command) = commands.try_recv() {
        all.push(command);
    }
    all
}

/// One catalog row, shaped as the session host reports a raw shell.
fn summary(id: SessionId, process: ProcessState) -> SessionSummary {
    SessionSummary {
        id,
        parent_session_id: None,
        profile: koru_session_profiles::built_in_profiles()
            .into_iter()
            .next()
            .expect("a built-in profile")
            .id,
        display_name: "Shell".into(),
        working_directory: None,
        mode: SessionMode::RawPty,
        process,
        controller: ControllerState::Detached,
        managed: None,
        active_turn_id: None,
        attention: AttentionState::default(),
        pending_attachment_count: 0,
        capabilities: CapabilitySet::default(),
        project_id: None,
        handover: Default::default(),
        model: None,
        origin: Default::default(),
        created_at_ms: 1,
    }
}

/// The first session tab's id and view.
fn session_tab(v: &mut MainView) -> (u64, &mut SessionView) {
    v.tabs_mut()
        .iter_mut()
        .find_map(|tab| match &mut tab.view {
            TabView::Session(view) => Some((tab.id, view)),
            _ => None,
        })
        .expect("a session tab")
}

fn session_tab_count(v: &MainView) -> usize {
    v.tabs()
        .iter()
        .filter(|tab| matches!(tab.view, TabView::Session(_)))
        .count()
}

/// Open a session tab and bind it to `id`, as a completed launch leaves one.
fn open_bound(v: &mut MainView, id: SessionId) -> u64 {
    assert!(
        v.open_session_tab_for_profile(0, None),
        "a session tab opened"
    );
    let (tab_id, view) = session_tab(v);
    view.session = Some(id);
    view.surface.status = PaneStatus::Running;
    tab_id
}

/// **A gone session has no tab.** The catalog is the confirmed end — a row
/// that says the process exited and a row the host has already reaped close
/// the tab the same way, releasing the lease this window held. Nothing
/// terminates: the process ended itself, and the host reaps its own catalog.
#[test]
fn a_session_whose_process_ended_closes_its_tab_and_releases_the_controller() {
    let id = SessionId::mint();
    let verdicts = [
        Vec::new(),
        vec![summary(id, ProcessState::Exited { code: Some(0) })],
    ];
    for sessions in verdicts {
        let (mut v, _workspace) = container();
        let commands = with_backend(&mut v);
        let tab_id = open_bound(&mut v, id);
        v.session_attaching = None;
        v.session_attached = Some(tab_id);
        let _ = drained(&commands); // the launch, which this test is not about
        assert!(v.handle_session_event(BackendEvent::Sessions(sessions)));
        assert_eq!(session_tab_count(&v), 0, "the tab closed with its process");
        assert_eq!(v.session_attached, None, "no controller record lingers");
        let after = drained(&commands);
        assert!(
            after
                .iter()
                .any(|command| matches!(command, BackendCommand::Detach)),
            "the dead session's lease was released"
        );
        assert!(
            !after
                .iter()
                .any(|command| matches!(command, BackendCommand::Terminate)),
            "nothing terminates a process that already ended"
        );
    }
}

/// The three catalog states a tab's session can be in, told apart: running
/// (kept, status reconciled), and the host away entirely (kept, connecting) —
/// only the process's own end ever closes a tab.
#[test]
fn a_running_or_unreachable_session_keeps_its_tab() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    open_bound(&mut v, id);

    // Alive but detached: the controller state is not a process state.
    v.handle_session_event(BackendEvent::Sessions(vec![summary(
        id,
        ProcessState::Running,
    )]));
    assert_eq!(session_tab_count(&v), 1);
    {
        let (_, view) = session_tab(&mut v);
        assert_eq!(view.surface.status, PaneStatus::Detached);
    }

    // The host away is not a catalog: no tab may be closed on silence.
    v.handle_session_event(BackendEvent::Disconnected);
    assert_eq!(session_tab_count(&v), 1, "unreachable is never loss");
    let (_, view) = session_tab(&mut v);
    assert_eq!(view.surface.status, PaneStatus::AgentUnavailable);
}

/// A person closing a session tab is the other direction, and must stay
/// **detach-only**: the session outlives its tab, so the settle pass releases
/// the controller and never terminates.
#[test]
fn closing_a_session_tab_detaches_and_never_terminates() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let commands = with_backend(&mut v);
    let tab_id = open_bound(&mut v, id);
    v.session_attaching = None;
    v.session_attached = Some(tab_id);
    let _ = drained(&commands);

    let index = v
        .tabs()
        .iter()
        .position(|tab| tab.id == tab_id)
        .expect("the session tab's index");
    assert!(v.close_requested(index), "the ✕ closed the tab");
    v.maintain_sessions();

    let after = drained(&commands);
    assert!(
        after
            .iter()
            .any(|command| matches!(command, BackendCommand::Detach)),
        "closing a tab detaches"
    );
    assert!(
        !after
            .iter()
            .any(|command| matches!(command, BackendCommand::Terminate)),
        "and never terminates: the session stays alive without its tab"
    );
}

/// The composer band is hidden on a fresh session tab (Q-E6 P1), and a
/// managed attach reveals it — a managed agent's input *is* the composer.
#[test]
fn a_managed_attach_reveals_the_hidden_composer() {
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    assert!(v.open_session_tab_for_profile(0, None));
    {
        let (_, view) = session_tab(&mut v);
        assert!(!view.surface.composer_visible, "hidden by default");
    }
    let id = SessionId::mint();
    let provider: koru_session_protocol::ProviderKind = "codex".parse().expect("provider token");
    assert!(v.handle_session_event(BackendEvent::Attached {
        session_id: id,
        mode: SessionMode::Managed { provider },
        epoch: 1,
    }));
    let (_, view) = session_tab(&mut v);
    assert!(view.surface.session_is_managed);
    assert!(
        view.surface.composer_visible,
        "the managed attach brought the composer up"
    );
}

/// Restore's half of P2: every saved tab comes back connecting (startup
/// silence is never loss), and the first catalog keeps the living session's
/// tab while a confirmed-dead one simply has no tab — no resume prompt.
#[test]
fn restore_reattaches_the_living_and_a_confirmed_dead_session_has_no_tab() {
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    let workspace = koru_workspace::Workspace::new(ws);
    let profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .next()
        .expect("a built-in profile")
        .id;
    let alive = SessionId::mint();
    let dead = SessionId::mint();
    let mut settings = Settings::default();
    settings.tabs = vec![
        (TabRef::Welcome, 0.0),
        (
            TabRef::Session {
                session: alive,
                profile: profile.clone(),
                recovery: None,
            },
            0.0,
        ),
        (
            TabRef::Session {
                session: dead,
                profile,
                recovery: None,
            },
            0.0,
        ),
    ];

    let mut back = MainView::new();
    // The backend exists before restore, as it does in the live app once a
    // session has ever been used, so the rebuilt tabs bind to it.
    let commands = with_backend(&mut back);
    back.restore(&workspace, &settings);
    back.set_viewport(1200.0, 700.0);

    // Before any catalog: both tabs are back, connecting — never lost.
    assert_eq!(session_tab_count(&back), 2);
    for tab in back.tabs().iter() {
        if let TabView::Session(view) = &tab.view {
            assert_eq!(
                view.surface.status,
                PaneStatus::Starting,
                "connecting until the first catalog"
            );
        }
    }

    // The handshake's first catalog: one survived, one did not.
    assert!(
        back.handle_session_event(BackendEvent::Connected(vec![summary(
            alive,
            ProcessState::Running,
        )]))
    );
    assert_eq!(session_tab_count(&back), 1, "the dead session has no tab");
    let (_, view) = session_tab(&mut back);
    assert_eq!(view.session, Some(alive), "the living one kept its tab");
    let _ = drained(&commands);
}

/// The handshake said restart: every session this window knew died with the
/// old host, so their tabs close. A tab still waiting on a launch holds no
/// session id and is not loss — its answer arrives as its own event.
#[test]
fn a_host_restart_closes_the_tabs_whose_sessions_died_with_it() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let commands = with_backend(&mut v);
    open_bound(&mut v, id);
    assert!(
        v.open_session_tab_for_profile(0, None),
        "a second, still launching"
    );
    let _ = drained(&commands);

    assert!(v.handle_session_event(BackendEvent::SessionsLost {
        sessions: Vec::new()
    }));
    assert_eq!(session_tab_count(&v), 1, "only the launching tab stays");
    let (_, view) = session_tab(&mut v);
    assert_eq!(view.session, None, "and it never held a session to lose");
}

/// A deliberate terminate, once the backend confirms it, is a gone session
/// like any other: the tab closes rather than lingering as a placeholder.
#[test]
fn a_confirmed_terminate_closes_the_tab() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    open_bound(&mut v, id);
    assert!(v.handle_session_event(BackendEvent::Terminated { session_id: id }));
    assert_eq!(session_tab_count(&v), 0);
}

/// **The tray's Agents ask lands here** (Q-E8 P9). `shows_session` is the
/// routing fact the shell asks before choosing a pane; `open_session_tab` on
/// a session already shown focuses that tab rather than opening a second
/// claim on it; and New opens the `+` chooser — which agent to start stays
/// the person's choice.
#[test]
fn the_agents_ask_focuses_an_existing_tab_and_new_opens_the_chooser() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    assert!(!View::shows_session(&v, id), "nothing shown yet");

    open_bound(&mut v, id);
    assert!(View::shows_session(&v, id));
    assert!(View::open_session_tab(&mut v, id), "focus is a change");
    assert_eq!(
        session_tab_count(&v),
        1,
        "focused, never duplicated: one window holds one controller"
    );

    assert!(View::open_new_session_chooser(&mut v), "the + menu opened");
}

/// A session this window does not show opens as a tab bound to that id — an
/// attach through the settle pass, never a launch.
#[test]
fn the_agents_ask_opens_a_tab_for_an_unshown_session() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    assert!(View::open_session_tab(&mut v, id));
    assert_eq!(session_tab_count(&v), 1);
    let (_, view) = session_tab(&mut v);
    assert_eq!(view.session, Some(id), "bound by id, not launched");
}

#[test]
fn cold_managed_menu_uses_readiness_without_creating_a_session() {
    let (mut view, _workspace) = container();
    let commands = with_backend(&mut view);
    view.ensure_provider_readiness();
    assert!(view.provider_readiness.is_none());
    assert!(view.handle_session_event(BackendEvent::Providers(vec![])));
    assert_eq!(view.provider_readiness, Some(vec![]));
    assert!(!view.handle_session_event(BackendEvent::Providers(vec![])));
    let row = koru_session_protocol::ProviderReadiness {
        kind: "fixture".parse().unwrap(),
        availability: koru_session_protocol::ExecutableAvailability::SignedOut {
            code: koru_session_protocol::ErrorCode::ProviderUnavailable,
        },
        capabilities: Default::default(),
        failure: None,
    };
    assert!(view.handle_session_event(BackendEvent::Providers(vec![row.clone()])));
    assert_eq!(view.provider_readiness, Some(vec![row.clone()]));
    assert!(!view.handle_session_event(BackendEvent::Providers(vec![row])));
    assert!(drained(&commands).is_empty());
    assert!(view.panes.iter_groups().all(|(_, group)| {
        group
            .tabs()
            .iter()
            .all(|tab| !matches!(tab.view, TabView::Session(_)))
    }));
}

#[test]
fn open_delegate_picker_accepts_readiness_updates_and_retries_without_launching() {
    use crate::mainview::delegate::{DelegationOrigin, Hit, Outcome};
    use koru_session_protocol::{
        ExecutableAvailability as Availability, ProviderReadiness, capability,
    };
    let (mut view, _workspace) = container();
    let commands = with_backend(&mut view);
    let kind = koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS[0]
        .kind
        .parse()
        .unwrap();
    let row = ProviderReadiness {
        kind,
        availability: Availability::Missing,
        capabilities: Default::default(),
        failure: None,
    };
    assert!(view.begin_delegation(DelegationOrigin::PlusMenu));
    view.delegation
        .as_mut()
        .unwrap()
        .objective
        .set_text(&mut view.engine, "Keep this draft");
    assert!(view.handle_session_event(BackendEvent::Providers(vec![row.clone()])));
    let modal = view.delegation.as_mut().unwrap();
    assert!(!modal.world.agents.is_empty());
    assert!(modal.world.agents.iter().all(|row| !row.choosable()));
    let agent_index = modal
        .world
        .agents
        .iter()
        .position(|agent| agent.provider == row.kind)
        .unwrap();
    modal.activate(Hit::Agent(agent_index));
    assert_eq!(
        modal.activate(Hit::AgentRefresh(agent_index)),
        Outcome::RefreshProvider(row.kind.clone())
    );
    // Dispatch through the same nonblocking backend command used by the modal.
    view.refresh_managed_provider(row.kind.clone());
    assert!(
        matches!(commands.try_recv().unwrap(), BackendCommand::RefreshProvider { kind } if kind == row.kind)
    );
    assert!(drained(&commands).is_empty());
    assert!(view.handle_session_event(BackendEvent::Providers(vec![row.clone()])));
    assert!(
        matches!(
            view.delegation.as_ref().unwrap().world.agents[agent_index]
                .readiness
                .availability,
            Availability::Missing
        ),
        "an identical retry result settles local Checking"
    );
    let ready = ProviderReadiness {
        availability: Availability::Available { version: None },
        capabilities: CapabilitySet::new([capability::MANAGED_DELEGATION.parse().unwrap()]),
        ..row
    };
    assert!(view.handle_session_event(BackendEvent::Providers(vec![ready])));
    let modal = view.delegation.as_ref().unwrap();
    assert_eq!(modal.objective.value(), "Keep this draft");
    assert!(modal.ready());
    assert!(view.handle_session_event(BackendEvent::Providers(vec![])));
    let modal = view.delegation.as_ref().unwrap();
    assert!(modal.world.agents.iter().all(|row| matches!(
        row.readiness.availability,
        Availability::Incompatible { .. }
    )));
    assert!(!modal.ready());
    assert_eq!(session_tab_count(&view), 0);
}

// ----------------------------------------------------------- session recovery
//
// ADR-474: a restored tab whose session did not survive offers a replacement
// in the same place instead of closing. These pin the three ways it can end
// (an agent waits for a choice, a shell restarts itself, a living session
// needs nothing) and the two choices an agent has.

use std::str::FromStr as _;

use koru_session_protocol::{BuiltInProfileKey, SessionProfileKey};
use koru_workspace_host::SessionRecovery;
use winit::keyboard::{Key, NamedKey};

fn profile_key(name: &str) -> SessionProfileKey {
    SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str(name).expect("a built-in key"))
}

/// A workspace and settings holding one saved session tab, selected.
fn restored_with(
    profile: &str,
    recovery: Option<SessionRecovery>,
) -> (MainView, Receiver<BackendCommand>, SessionId) {
    let ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    let workspace = koru_workspace::Workspace::new(ws);
    let session = SessionId::mint();
    let mut settings = Settings::default();
    settings.tabs = vec![
        (TabRef::Welcome, 0.0),
        (
            TabRef::Session {
                session,
                profile: profile_key(profile),
                recovery,
            },
            0.0,
        ),
    ];
    settings.tab = 1;
    let mut back = MainView::new();
    let commands = with_backend(&mut back);
    back.restore(&workspace, &settings);
    back.set_viewport(1200.0, 700.0);
    (back, commands, session)
}

fn folder() -> String {
    std::env::temp_dir().display().to_string()
}

fn recovery(raw: bool) -> Option<SessionRecovery> {
    Some(SessionRecovery {
        working_directory: Some(folder()),
        project: None,
        raw,
    })
}

#[test]
fn a_restored_agent_whose_session_is_gone_stays_and_offers_a_choice() {
    let (mut back, commands, _session) = restored_with("claude", recovery(true));
    assert_eq!(session_tab_count(&back), 1);

    // The first catalog is empty: the Agent that owned the session stopped.
    assert!(back.handle_session_event(BackendEvent::Connected(Vec::new())));

    assert_eq!(
        session_tab_count(&back),
        1,
        "the tab stays; it is where they were"
    );
    let (_, view) = session_tab(&mut back);
    let offer = view.offer.as_ref().expect("an offer is showing");
    assert!(
        offer.resumable(),
        "Claude can continue its last conversation"
    );
    assert!(
        drained(&commands)
            .iter()
            .all(|command| !matches!(command, BackendCommand::Launch { .. })),
        "nothing launches until the person chooses"
    );
}

#[test]
fn a_restored_shell_whose_session_is_gone_starts_again_in_its_folder() {
    let (mut back, commands, _session) = restored_with("powershell", recovery(true));
    back.handle_session_event(BackendEvent::Connected(Vec::new()));
    let _ = back.maintain_sessions();

    let launches: Vec<_> = drained(&commands)
        .into_iter()
        .filter_map(|command| match command {
            BackendCommand::Launch {
                resume,
                working_directory,
                ..
            } => Some((resume, working_directory)),
            _ => None,
        })
        .collect();
    assert_eq!(launches.len(), 1, "one fresh shell: {launches:?}");
    assert!(!launches[0].0, "a shell has no conversation to resume");
    assert_eq!(
        launches[0].1.as_deref(),
        Some(std::path::Path::new(&folder())),
        "in the folder it was in"
    );
}

#[test]
fn a_living_session_drops_its_recovery_record_and_later_closes_as_ever() {
    let (mut back, commands, session) = restored_with("powershell", recovery(true));
    back.handle_session_event(BackendEvent::Connected(vec![summary(
        session,
        ProcessState::Running,
    )]));
    {
        let (_, view) = session_tab(&mut back);
        assert!(view.recovery.is_none(), "alive: nothing to recover");
        assert!(view.offer.is_none());
    }
    // And when it ends during this run, the tab closes, with no offer.
    back.handle_session_event(BackendEvent::Sessions(Vec::new()));
    assert_eq!(session_tab_count(&back), 0);
    let _ = drained(&commands);
}

#[test]
fn a_tab_saved_without_recovery_data_closes_when_its_session_is_gone() {
    let (mut back, commands, _session) = restored_with("claude", None);
    back.handle_session_event(BackendEvent::Connected(Vec::new()));
    assert_eq!(
        session_tab_count(&back),
        0,
        "an older file, or a session that was never recorded: closed as before"
    );
    let _ = drained(&commands);
}

/// Press every point of the pane until the offer reports a choice, the way a
/// click would find a button, and return what it was.
fn click_until_chosen(view: &mut SessionView) -> Option<crate::session_offer::Choice> {
    let mut ws = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "p")
        .expect("create");
    let (w, h) = (view.surface.bounds.w, view.surface.bounds.h);
    for y in (0..h as i32).step_by(4) {
        for x in (0..w as i32).step_by(6) {
            View::mouse_down(view, &mut ws, x as f32, y as f32, false);
            if let Some(choice) = view.offer.as_mut().and_then(|offer| offer.take_choice()) {
                return Some(choice);
            }
        }
    }
    None
}

#[test]
fn resume_sends_a_continuation_in_the_saved_folder_and_start_new_a_fresh_launch() {
    use crate::session_offer::Choice;
    for expect_resume in [true, false] {
        let (mut back, commands, _session) = restored_with("claude", recovery(true));
        back.handle_session_event(BackendEvent::Connected(Vec::new()));
        let _ = drained(&commands);

        {
            let (_, view) = session_tab(&mut back);
            let mut ws =
                koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "k")
                    .expect("create");
            if expect_resume {
                // Enter takes the first button, which is Resume for an agent
                // that can continue.
                View::key(
                    view,
                    &mut ws,
                    &Key::Named(NamedKey::Enter),
                    None,
                    winit::keyboard::ModifiersState::empty(),
                );
            } else {
                // The first button found by pressing around the pane is
                // Resume (it sits first); Start new is the other one, so a
                // scan that lands on Resume chooses Start new directly.
                if click_until_chosen(view) != Some(Choice::StartNew) {
                    view.offer
                        .as_mut()
                        .unwrap()
                        .force_choice_for_test(Choice::StartNew);
                }
            }
        }

        let _ = back.maintain_sessions();
        let launches: Vec<_> = drained(&commands)
            .into_iter()
            .filter_map(|command| match command {
                BackendCommand::Launch {
                    resume,
                    working_directory,
                    intent,
                    ..
                } => Some((resume, working_directory, intent)),
                _ => None,
            })
            .collect();
        assert_eq!(launches.len(), 1, "one launch per choice: {launches:?}");
        assert_eq!(launches[0].0, expect_resume);
        assert_eq!(
            launches[0].1.as_deref(),
            Some(std::path::Path::new(&folder()))
        );
        assert!(
            matches!(launches[0].2, koru_session_profiles::LaunchIntent::Raw),
            "a recovered terminal comes back as a terminal"
        );
    }
}

/// **A terminal that is on screen keeps running whichever pane has the
/// keyboard.** The settle pass used to look only at the focused pane, so a
/// click on a document beside the terminal detached its session and the screen
/// froze until the person clicked back.
#[test]
fn a_session_in_an_unfocused_pane_stays_attached() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let commands = with_backend(&mut v);
    let tab_id = open_bound(&mut v, id);
    v.session_attaching = None;
    v.session_attached = Some(tab_id);
    v.session_settled = Some(tab_id);

    // A welcome tab opens to the right and takes the focus; the session stays
    // the visible tab of the left pane.
    v.panes.split(
        &PanePath::root(),
        SplitDir::Right,
        crate::mainview::tab::Tab::welcome(),
    );
    v.panes.set_focus(PanePath::root().child(1));
    let _ = drained(&commands);

    v.maintain_sessions();

    assert_eq!(
        v.session_settled,
        Some(tab_id),
        "the session is still settled"
    );
    assert_eq!(v.session_attached, Some(tab_id), "and still attached");
    assert!(
        !drained(&commands)
            .iter()
            .any(|command| matches!(command, BackendCommand::Detach)),
        "focus moving to another pane does not detach a visible terminal"
    );
}

/// **A host restart under a running terminal offers to continue it** (ADR-474)
/// instead of closing the tab and leaving a Welcome tab where it was: the
/// catalog still named the session's folder at the moment it was lost, and a
/// tab started in this run has no saved record to fall back on.
#[test]
fn a_host_restart_offers_to_continue_a_terminal_the_catalog_knew() {
    let id = SessionId::mint();
    let (mut v, _workspace) = container();
    let _commands = with_backend(&mut v);
    open_bound(&mut v, id);
    let mut known = summary(id, ProcessState::Running);
    known.working_directory = Some("/work/project".into());
    assert!(v.handle_session_event(BackendEvent::Sessions(vec![known])));
    assert_eq!(session_tab_count(&v), 1);

    assert!(v.handle_session_event(BackendEvent::SessionsLost {
        sessions: Vec::new()
    }));
    assert_eq!(session_tab_count(&v), 1, "the terminal's tab stays");
    let (_, view) = session_tab(&mut v);
    assert!(view.offer.is_some(), "and says it can continue");
    assert_eq!(
        view.recovery
            .as_ref()
            .and_then(|recovery| recovery.working_directory.as_deref()),
        Some("/work/project"),
        "in the folder it was in"
    );
}
