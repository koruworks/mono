// SPDX-License-Identifier: MPL-2.0

//! The per-window session backend, and how its events reach a session tab.
//!
//! One backend per `MainView`, created on the first session use — a person who
//! never opens a session pays nothing: no thread, no connection at app start.
//! The backend's command thread, queue and readers live in
//! `crates/koru-session-backend`; what this module owns is koru.exe's side of
//! that seam:
//!
//! - [`QueueSink`], the `BackendSink` the backend delivers into. **Queue
//!   first, wake second** — the same rule the Agent link's reader follows —
//!   and the wake is `Wake::Session`, which only ends the loop's wait. Nothing
//!   on the loop thread ever touches the session socket; `maintain` drains the
//!   queue on the next pass.
//! - The routing of each [`BackendEvent`] to the session tab it is about, by
//!   session identity — ADR-263's guard: a late frame for a tab that was
//!   switched away from is dropped, never presented into another tab.
//! - The active-tab-exclusive controller: the active session tab attaches
//!   (controller lease + render subscription) and a tab that stops being
//!   active detaches. An inactive session tab draws nothing, blinks nothing
//!   and negotiates no viewport.

use std::collections::VecDeque;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use koru_agents_ui::{PaneStatus, RenderOutcome, Watching};
use koru_i18n::{Key as I18nKey, t};
use koru_session_backend::{BackendCommand, BackendEvent, BackendSink, InteractionSender};
use koru_session_profiles::{built_in_profiles, resolve};
use koru_session_protocol::{
    ErrorCode, HandoverState, ProcessState, ProfileKind, ProjectId, RenderUpdate, SessionId,
    SessionMode, SessionProfileKey, StreamSequence,
};
use koru_session_view::PendingAttachmentStatus;
use koru_workspace_host::app::Wake;

use super::MainView;
use super::tab::{Tab, TabView};
use crate::session_tab::{PendingLaunch, SessionSender, SessionView};

/// What one catalog row means for the tab showing that session — the same
/// mapping Koru Terminal's `pane_for` states, `attached_here` said per tab.
fn pane_status_for(
    summary: &koru_session_protocol::SessionSummary,
    attached_here: bool,
) -> PaneStatus {
    use koru_session_protocol::{ControllerState, ProcessState};
    match summary.process {
        ProcessState::Starting => PaneStatus::Starting,
        ProcessState::Running if attached_here => PaneStatus::Running,
        ProcessState::Running => match summary.controller {
            ControllerState::Detached => PaneStatus::Detached,
            ControllerState::Controlled => PaneStatus::ControllerConflict,
        },
        ProcessState::Exited { .. } => PaneStatus::Exited,
        ProcessState::Failed { .. } => PaneStatus::Failed,
    }
}

/// What the strip calls a session tab for this profile — the display name
/// where the profile still resolves, and the stable key's own spelling where
/// it does not (a built-in this build no longer ships). A custom profile has
/// no name this process can know, so it gets the honest generic.
fn session_profile_label(key: &SessionProfileKey) -> String {
    match resolve(key) {
        Ok(profile) => profile.fallback_display_name().to_owned(),
        Err(_) => match key {
            SessionProfileKey::BuiltIn(token) => token.as_str().to_owned(),
            SessionProfileKey::Custom(_) => "Session".to_owned(),
        },
    }
}

/// This window's connection to the session backend.
pub(super) struct SessionBackend {
    pub(super) sender: InteractionSender,
    pub(super) events: Arc<Mutex<VecDeque<BackendEvent>>>,
}

impl SessionBackend {
    pub(super) fn drain(&self) -> Vec<BackendEvent> {
        let mut queue = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        queue.drain(..).collect()
    }
}

/// The loop's end of the backend: enqueue, then wake. Delivery answers whether
/// the event loop is still alive, which is what stops the backend's reader
/// threads once the window is gone.
#[derive(Clone)]
struct QueueSink {
    events: Arc<Mutex<VecDeque<BackendEvent>>>,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

impl BackendSink for QueueSink {
    fn deliver(&self, event: BackendEvent) -> bool {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(event);
        self.proxy.send_event(Wake::Session).is_ok()
    }
}

impl MainView {
    /// The backend, made on first use. `None` when the shell has not installed
    /// a wake proxy — every test container, where a session tab then presents
    /// the honest agent-unavailable state rather than dialling anything.
    /// First managed-menu use establishes the backend even with no session.
    /// Subsequent calls only return cached machinery and never refresh probes.
    pub(super) fn ensure_provider_readiness(&mut self) {
        let _ = self.ensure_session_backend();
    }

    pub(super) fn discover_delegate_models(&mut self) {
        let Some((request, target)) = self
            .delegation
            .as_mut()
            .and_then(|modal| modal.model_discovery())
        else {
            return;
        };
        let sent = self.ensure_session_backend().is_some_and(|backend| {
            backend
                .sender
                .discover_provider_models(request, target.provider, target.cwd)
        });
        if !sent && let Some(modal) = self.delegation.as_mut() {
            modal.accept_models(
                request,
                Err(koru_session_protocol::ErrorCode::HostUnavailable),
            );
        }
    }

    pub(super) fn refresh_managed_provider(&mut self, kind: koru_session_protocol::ProviderKind) {
        if let Some(backend) = self.ensure_session_backend() {
            backend.sender.refresh_provider(kind);
        }
    }

    fn ensure_session_backend(&mut self) -> Option<&SessionBackend> {
        if self.session_backend.is_none() {
            self.session_backend = Some(self.new_backend_connection(false)?);
        }
        self.session_backend.as_ref()
    }

    /// Start a connection to the session host: the window's main one, or - with
    /// `lane` - one terminal's own (`mainview/session_lanes.rs`).
    pub(super) fn new_backend_connection(&mut self, lane: bool) -> Option<SessionBackend> {
        let Some(proxy) = self.agent.wake_proxy() else {
            // No event loop: a test. A lane there is a channel the test holds,
            // with no thread behind it.
            #[cfg(test)]
            if lane {
                let (sender, commands) = koru_session_backend::channel();
                self.lane_commands.push(commands);
                return Some(SessionBackend {
                    sender,
                    events: Arc::new(Mutex::new(VecDeque::new())),
                });
            }
            return None;
        };
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let (sender, commands) = koru_session_backend::channel();
        let internal: SyncSender<BackendCommand> = sender.internal_sender();
        let sink = QueueSink {
            events: Arc::clone(&events),
            proxy,
        };
        // The channel root, the same one the Agent link dials - `--root`
        // threads through, and a mismatch would find nothing rather than the
        // wrong host.
        let root = koru_workspace_host::cli::root_from_args();
        if lane {
            koru_session_backend::spawn_lane(sink, root, commands, internal);
        } else {
            koru_session_backend::spawn(sink, root, built_in_profiles(), commands, internal);
        }
        Some(SessionBackend { sender, events })
    }

    /// The typed-verb sender for the container's project mutations —
    /// create/rename/delete a project, assign a session to one. `None` where
    /// no backend can exist (no wake proxy: every test container), and the
    /// gesture then changes nothing rather than pretending: the tree only
    /// ever reflects the catalog the host answers with (ADR-264).
    pub(super) fn session_verbs(&mut self) -> Option<InteractionSender> {
        self.ensure_session_backend()
            .map(|backend| backend.sender.clone())
    }

    /// Install a backend whose command channel the test holds, so a suite can
    /// assert which typed verbs a gesture sent without a thread or a socket.
    #[cfg(test)]
    pub(super) fn install_test_session_backend(
        &mut self,
    ) -> std::sync::mpsc::Receiver<BackendCommand> {
        let (sender, commands) = koru_session_backend::channel();
        self.session_backend = Some(SessionBackend {
            sender,
            events: Arc::new(Mutex::new(VecDeque::new())),
        });
        commands
    }

    /// Hand the window one backend event, the way the wake path would. The
    /// suite's only way to say *the host answered*, which is what half of
    /// this file's behaviour is a reaction to.
    #[cfg(test)]
    pub(super) fn deliver_test_session_event(&mut self, event: BackendEvent) -> bool {
        self.handle_session_event(event)
    }

    /// A launch surface chose a built-in profile: open it as a session tab.
    /// The launch itself goes out when the tab settles as the active one,
    /// because only a sized surface can name the grid it wants.
    ///
    /// `project` is where the session starts — a project row's *New session ▸*
    /// names its project; the `+` menu and empty tree space pass `None`, which
    /// is the Unassigned bucket. The placement rides the creation request
    /// (ADR-264), carried on the tab's [`PendingLaunch`] until the settle pass
    /// sends it.
    pub(super) fn open_session_tab_for_profile(
        &mut self,
        index: usize,
        project: Option<ProjectId>,
    ) -> bool {
        let Some(profile) = built_in_profiles().into_iter().nth(index) else {
            return false;
        };
        // **The one door a new session tab opens by**: an agent profile is
        // refused when this build hides agents, whichever menu named it.
        // Shells always launch (koru-features).
        if profile.kind == ProfileKind::Agent && !koru_features::agents() {
            return false;
        }
        // **S11, at the door** (Koru Terminal's rule): a project whose
        // recorded folder is not there refuses to launch — never a fallback
        // to a directory nobody chose — and says so where the person is
        // looking, rather than opening a tab for a session that will not
        // start the way they meant.
        if let Some(id) = project
            && let Some(root) = self.explore.agent_project_root(id).map(str::to_owned)
            && !std::path::Path::new(&root).is_dir()
        {
            let notice: String =
                t(crate::format::lang(), I18nKey::TerminalProjectRootMissing).into();
            if let Some(view) = self.active_session_view() {
                view.surface.notice = Some(notice);
                return true;
            }
            return false;
        }
        let label = profile.fallback_display_name().to_owned();
        let backend = self.ensure_session_backend();
        let sender = backend.map(|backend| backend.sender.clone());
        let view = match &sender {
            Some(sender) => SessionView::new(
                &SessionSender(sender.clone()),
                Some(PendingLaunch {
                    profile,
                    raw_fallback: false,
                    project,
                    working_directory: None,
                    resume: false,
                }),
            ),
            // No backend (no event loop yet, or a test). The tab still opens
            // and says the truth on its face rather than doing nothing.
            None => {
                let (sender, _commands) = koru_session_backend::channel();
                let mut view = SessionView::new(
                    &SessionSender(sender),
                    Some(PendingLaunch {
                        profile,
                        raw_fallback: false,
                        project,
                        working_directory: None,
                        resume: false,
                    }),
                );
                view.surface.status = PaneStatus::AgentUnavailable;
                view
            }
        };
        self.tabs_mut().push(Tab::session(view, &label));
        self.mark_tab_widths_stale();
        // Size before the settle that launches: the grid a session is created
        // at is measured from the surface's bounds, and a launch measured at
        // zero would open a 1×1 PTY and resize it in front of the person.
        self.size_visible_groups();
        // Settle now rather than on the next wake, so the launch leaves in
        // the same gesture that opened the tab.
        let _ = self.settle_session_tabs();
        true
    }

    /// Build a tab bound to an **existing** session — never a launch. Shared
    /// by the Agents navigator (open-or-focus) and restore (rebuild a saved
    /// tab). The tab comes back **connecting**: whether the session survived
    /// is the handshake's to say — the settle pass attaches it through the
    /// backend's `select` verb when it is the active tab, and the first
    /// catalog reconciles it: reattach while the host still reports the
    /// process alive, close the tab when the end is confirmed (an exited or
    /// absent row, or `SessionsLost`). Silence at startup is never loss.
    fn build_existing_session_tab(
        &mut self,
        session: SessionId,
        profile_key: Option<SessionProfileKey>,
        label: &str,
        watching: bool,
    ) -> Tab {
        let sender = self
            .ensure_session_backend()
            .map(|backend| backend.sender.clone());
        let mut view = match sender {
            Some(sender) => {
                let mut view = SessionView::new(&SessionSender(sender), None);
                // The starting placeholder: the tab holds its identity and
                // waits for the catalog rather than claiming a state it has
                // not confirmed.
                view.surface.status = PaneStatus::Starting;
                view
            }
            // No backend (no event loop yet, or a test): the honest
            // agent-unavailable state, same as opening a fresh session tab.
            None => {
                let (sender, _commands) = koru_session_backend::channel();
                let mut view = SessionView::new(&SessionSender(sender), None);
                view.surface.status = PaneStatus::AgentUnavailable;
                view
            }
        };
        view.session = Some(session);
        view.profile_key = profile_key;
        // **A run's session opens read-only.** The orchestrator holds its
        // controller for as long as the run works, so the tab is bound to it
        // the way any other is and reads it over a connection that takes no
        // lease — no caret, no composer, and a footer saying so with the one
        // control that asks for it (ADR-361).
        view.watches_run = watching;
        if watching {
            view.surface.set_watching(Watching::Yes);
        }
        Tab::session(view, label)
    }

    /// **Open, or focus, a read-only view of a delegated run's session.**
    ///
    /// The Agents navigator's answer for the session nested under a run. The
    /// same tab the navigator opens for any other session, bound the same
    /// way — what differs is that this window watches rather than drives, so
    /// the settle pass observes it instead of attaching to it.
    pub(super) fn open_session_tab_watching(&mut self, session: SessionId) -> bool {
        self.open_session_tab(session, true)
    }

    /// The Agents navigator chose a session: focus the tab already showing
    /// it, else open a new tab bound to that existing session (attaching
    /// through the settle pass, never launching). Returns whether anything
    /// changed.
    pub(super) fn open_session_tab_for_existing(&mut self, session: SessionId) -> bool {
        self.open_session_tab(session, false)
    }

    fn open_session_tab(&mut self, session: SessionId, watching: bool) -> bool {
        let showing = self.session_tab_for(session).and_then(|tab_id| {
            self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == tab_id)
                    .map(|index| (path, index))
            })
        });
        if let Some((path, index)) = showing {
            // Focus, never duplicate: one window holds one controller, and a
            // second tab on the same session would be two claims on it.
            self.panes.set_focus(path);
            self.select(index);
            return true;
        }
        // Owned before the mutable builder borrow: the label and the stable
        // profile key both come from the catalog summary when it is known, so
        // a navigator-opened tab carries the key its later capture needs.
        let summary = self.explore.agent_session(session);
        let label = summary
            .map(|summary| summary.display_name.clone())
            .unwrap_or_else(|| "Session".to_owned());
        let profile_key = summary.map(|summary| summary.profile.clone());
        let tab = self.build_existing_session_tab(session, profile_key, &label, watching);
        self.tabs_mut().push(tab);
        self.mark_tab_widths_stale();
        // Size before the settle that attaches: the attach names the grid it
        // wants, and one measured at zero would renegotiate in front of the
        // person — the same ordering the launch path keeps.
        self.size_visible_groups();
        let _ = self.settle_session_tabs();
        true
    }

    /// *Move to new window* (Q-E7 P8): koru.exe's pop-out for a session.
    ///
    /// A new **in-process** container window is opened by the shell and seated
    /// on this session — the same `duplicate`-then-open path a multi-document
    /// pop-out takes (`open_documents_in_new_window`), reused rather than
    /// forked. Here the source window gives the session up: closing its tab, if
    /// one is open, detaches — the process outlives the tab (P2), so the new
    /// window attaches to the living session by id. The shell drains
    /// [`wants_session_popout`](MainView::take_wants_session_popout) and calls
    /// [`open_session_tab`](MainView::open_session_tab) on the new container.
    ///
    /// **The limit, stated plainly:** the new window opens with the session
    /// tab, but no composer draft or scrollback travels with it — it attaches
    /// fresh, exactly as opening the session from the navigator would. The
    /// draft-preserving move coordinator was deliberately not built (Q-E5), and
    /// this reuses that same honest-conflict/attach-by-id path.
    pub(super) fn move_session_to_new_window(&mut self, session: SessionId) -> bool {
        let found = self.session_tab_for(session).and_then(|tab_id| {
            self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == tab_id)
                    .map(|index| (path, index))
            })
        });
        if let Some((path, index)) = found {
            self.panes.set_focus(path);
            self.close(index);
        }
        self.wants_session_popout = Some(session);
        true
    }

    /// *Terminate* (Q-E7 P8): the explicit kill, behind Koru Terminal's
    /// confirm. The backend's `Terminate` acts on the controller lease this
    /// window holds, so a session this window is not attached to is attached
    /// first — open or focus its tab and arm the confirm, which fires when the
    /// attach lands ([`terminate_if_armed`](Self::terminate_if_armed)) — the
    /// same two-step Koru Terminal takes. Distinct from closing a tab (detach,
    /// session alive) and from a process ending on its own (cleanup).
    pub(super) fn begin_terminate_session(&mut self, session: SessionId) -> bool {
        let tab_for = self.session_tab_for(session);
        if tab_for.is_some_and(|tab| self.tab_attached_anywhere(tab)) {
            self.open_terminate_confirm(session);
            return true;
        }
        self.terminate_after_attach = Some(session);
        self.open_session_tab_for_existing(session);
        true
    }

    /// Put up Koru Terminal's Terminate confirmation. The verb leaves on a yes,
    /// through `settle_confirm`, so a cancel costs nothing — and the yes is
    /// re-checked against the attached session there, since the person may have
    /// switched tabs while the dialog stood open.
    pub(super) fn open_terminate_confirm(&mut self, session: SessionId) {
        let lang = crate::format::lang();
        self.confirm = Some((
            koru_ui::Confirm::new(
                t(lang, I18nKey::TerminalTerminateTitle),
                t(lang, I18nKey::TerminalTerminateBody),
                t(lang, I18nKey::TerminalTerminate),
            ),
            crate::mainview::trash::ConfirmSubject::TerminateSession(session),
        ));
    }

    /// A *Terminate* that was waiting for this window to take the session's
    /// controller. Fired from the attach paths (managed and raw) once the
    /// lease is bound to the session's tab.
    pub(super) fn terminate_if_armed(&mut self, session: SessionId) {
        if self.terminate_after_attach == Some(session) {
            self.terminate_after_attach = None;
            self.open_terminate_confirm(session);
        }
    }

    /// Why *Terminate* cannot be taken for this session, or `None` when it can
    /// — Koru Terminal's rule (`can_terminate_session`/`terminate_refusal`) in
    /// this window's local terms: a **running** session, attached here or with
    /// its controller detached, can be terminated; a controlled-elsewhere or
    /// already-exited one cannot, and says which. The host still enforces this
    /// on the wire; the menu says so first.
    pub(super) fn session_terminate_refusal(&self, session: SessionId) -> Option<&'static str> {
        use koru_session_protocol::ControllerState;
        let lang = crate::format::lang();
        let Some(summary) = self.explore.agent_session(session) else {
            // A row the window has never heard of, or one already reaped: gone.
            return Some(t(lang, I18nKey::TerminalExited));
        };
        let running = matches!(summary.process, ProcessState::Running);
        let attached_here = self
            .session_tab_for(session)
            .is_some_and(|tab| self.tab_attached_anywhere(tab));
        let detached = matches!(summary.controller, ControllerState::Detached);
        if running && (attached_here || detached) {
            None
        } else if !running {
            Some(t(lang, I18nKey::TerminalExited))
        } else {
            Some(t(lang, I18nKey::TerminalControllerConflict))
        }
    }

    /// A saved session tab, rebuilt at restore (TERMINATAB W4.3), labeled by
    /// its stable profile key. Returned for the caller to place in tab order;
    /// nothing attaches here — the settle pass does when it is active.
    pub(super) fn restored_session_tab(
        &mut self,
        session: SessionId,
        profile_key: SessionProfileKey,
        recovery: Option<koru_workspace_host::SessionRecovery>,
    ) -> Tab {
        let label = session_profile_label(&profile_key);
        // A restored tab drives its session. Whether it was watching a run
        // is not persisted, and inferring it from a listing that has not
        // arrived yet would be a guess: the tab attaches, and the person
        // opens the run's session again from the navigator if they meant to
        // watch it.
        let mut tab = self.build_existing_session_tab(session, Some(profile_key), &label, false);
        // What the tab was, kept until the first catalog says whether the
        // session is still alive (ADR-474).
        if let TabView::Session(view) = &mut tab.view {
            view.recovery = recovery;
        }
        tab
    }

    /// The session half of `maintain`: reconcile which tab holds the
    /// controller, then drain and route everything the backend queued.
    pub(super) fn maintain_sessions(&mut self) -> bool {
        let mut changed = self.drain_takeover_requests();
        changed |= self.drain_recovery_choices();
        changed |= self.settle_session_tabs();
        changed |= self.settle_session_lanes();
        let Some(backend) = self.session_backend.as_ref() else {
            return changed;
        };
        let events = backend.drain();
        for event in events {
            changed |= self.handle_session_event(event);
        }
        changed |= self.drain_session_lanes();
        changed
    }

    /// The named session tabs are over processes that are **confirmed gone**
    /// — the person's `exit`, a Ctrl+C, a crash, or a host restart — so each
    /// closes (Q-E6 P2: no exited placeholder, no resume prompt; a gone
    /// session has no tab). Host-side the process is already dead and the
    /// host reaps an ended session from its own catalog; what a client owns
    /// is its controller lease, which is released here when one of these tabs
    /// held (or was acquiring) it. Distinct from a person closing a tab,
    /// which stays detach-only with the session alive (`settle_session_tabs`).
    fn close_gone_session_tabs(&mut self, tab_ids: &[u64]) -> bool {
        let mut changed = false;
        for &tab_id in tab_ids {
            // **A restored tab whose session did not survive offers a new one
            // instead of closing** (ADR-474): its layout, agent and folder were
            // saved, and the tab is where the person was working. Only a tab
            // restored with that record qualifies; a session that ends during a
            // run, or one restored from an older file, closes as before.
            if self.offer_recovery(tab_id) {
                changed = true;
                continue;
            }
            if self.session_attached == Some(tab_id) || self.session_attaching == Some(tab_id) {
                self.session_attached = None;
                self.session_attaching = None;
                // Release the lease the backend still holds on the dead
                // session; the backend swallows the already-exited answers.
                if let Some(backend) = self.session_backend.as_ref() {
                    backend.sender.detach();
                }
            }
            if self.session_settled == Some(tab_id) {
                self.session_settled = None;
            }
            let found = self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == tab_id)
                    .map(|index| (path, index))
            });
            if let Some((path, index)) = found {
                self.panes.set_focus(path);
                changed |= self.close(index);
            }
        }
        changed
    }

    /// Turn a restored tab whose session is gone into an offer, when it
    /// carries what is needed to start another. Returns whether it did, so
    /// the caller leaves the tab open.
    ///
    /// A shell is simply started again — a fresh prompt in the same folder
    /// costs nothing and asks nothing of the person — and an agent shows the
    /// panel: *Resume* where its CLI can continue a conversation, and *Start
    /// new* always.
    fn offer_recovery(&mut self, tab_id: u64) -> bool {
        let released =
            self.session_attached == Some(tab_id) || self.session_attaching == Some(tab_id);
        let Some(view) = self.session_view_by_tab(tab_id) else {
            return false;
        };
        if view.offer.is_some() {
            return true;
        }
        let Some(recovery) = view.recovery.clone() else {
            return false;
        };
        let Some(key) = view.profile_key.clone() else {
            return false;
        };
        let Some(profile) = recovery_profile(&key) else {
            return false;
        };
        let agent = profile.kind == ProfileKind::Agent;
        let resumable = recovery.raw
            && agent
            && koru_session_profiles::resume_arguments(profile.label).is_some();
        view.surface.notice = None;
        view.surface.status = PaneStatus::Exited;
        view.offer = Some(crate::session_offer::Offer::new(
            profile.fallback_display_name().to_owned(),
            recovery.working_directory.clone(),
            resumable,
        ));
        // A shell answers its own offer; an agent waits for the person.
        view.offer_auto_start = !agent;
        if released {
            self.session_attached = None;
            self.session_attaching = None;
            if let Some(backend) = self.session_backend.as_ref() {
                backend.sender.detach();
            }
        }
        if self.session_settled == Some(tab_id) {
            self.session_settled = None;
        }
        true
    }

    /// Carry out what the offers were answered with: each answered tab gets a
    /// fresh view that launches the replacement session on the next settle.
    fn drain_recovery_choices(&mut self) -> bool {
        use crate::session_offer::Choice;
        let mut answers: Vec<(u64, Choice)> = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let tab_id = tab.id;
                let TabView::Session(view) = &mut tab.view else {
                    continue;
                };
                let auto = std::mem::take(&mut view.offer_auto_start);
                let Some(offer) = view.offer.as_mut() else {
                    continue;
                };
                if auto {
                    answers.push((tab_id, Choice::StartNew));
                } else if let Some(choice) = offer.take_choice() {
                    answers.push((tab_id, choice));
                }
            }
        }
        let mut changed = false;
        for (tab_id, choice) in answers {
            changed |= self.start_recovered(tab_id, choice);
        }
        changed
    }

    /// Replace the offer in `tab_id` with a launching session: the same
    /// profile, folder and project, as a continuation or as a new
    /// conversation.
    fn start_recovered(&mut self, tab_id: u64, choice: crate::session_offer::Choice) -> bool {
        use crate::session_offer::Choice;
        let Some(sender) = self.session_backend.as_ref().map(|b| b.sender.clone()) else {
            return false;
        };
        let Some(view) = self.session_view_by_tab(tab_id) else {
            return false;
        };
        let (Some(key), Some(recovery)) = (view.profile_key.clone(), view.recovery.clone()) else {
            return false;
        };
        let Some(profile) = recovery_profile(&key) else {
            return false;
        };
        let resume = choice == Choice::Resume
            && recovery.raw
            && koru_session_profiles::resume_arguments(profile.label).is_some();
        let launch = PendingLaunch {
            profile,
            // A recovered raw terminal comes back raw: the person left a
            // terminal, and a managed attempt would swap it for a chat.
            raw_fallback: recovery.raw,
            project: recovery.project,
            working_directory: recovery.working_directory.map(std::path::PathBuf::from),
            resume,
        };
        let mut fresh = SessionView::new(&SessionSender(sender), Some(launch));
        fresh.surface.bounds = view.surface.bounds;
        *view = fresh;
        // The settle pass launches it for the active tab, or when it next is.
        let _ = self.settle_session_tabs();
        true
    }

    /// **Release on switch, attach on switch.** The active session tab takes
    /// the window's one controller; a session tab that stops being active
    /// gives it up, and a non-session active tab means the window holds none.
    /// Edge-triggered on the active tab's identity, so a failed attach is
    /// presented once rather than retried every pass.
    fn settle_session_tabs(&mut self) -> bool {
        // The active flag on every session surface — R2's inactive-tab rule.
        // Each group's own active tab is the flag's answer for the surfaces
        // in that group.
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            let active_id = group.tabs().active_tab().id;
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Session(view) = &mut tab.view {
                    let is_active = tab.id == active_id;
                    if view.surface.active != is_active {
                        if is_active {
                            view.activated();
                        } else {
                            view.deactivated();
                        }
                        changed = true;
                    }
                }
            }
        }
        // **A terminal that is on screen keeps running**, whichever pane has
        // the keyboard: the focused pane's session tab when there is one,
        // else the session this window is already settled on if it is still
        // the visible tab of some pane, else the first visible session tab.
        // Looking only at the focused pane detached the session the moment
        // the person clicked a document beside it, and the screen froze until
        // they clicked back.
        let owned_by_a_lane = self.lane_owned_tabs();
        let focused = self.tabs().active_tab();
        let visible: Vec<u64> = self
            .panes
            .iter_groups()
            .filter_map(|(_, group)| {
                let tab = group.tabs().active_tab();
                (matches!(tab.view, TabView::Session(_)) && !owned_by_a_lane.contains(&tab.id))
                    .then_some(tab.id)
            })
            .collect();
        let target = if matches!(focused.view, TabView::Session(_))
            && !owned_by_a_lane.contains(&focused.id)
        {
            Some(focused.id)
        } else {
            self.session_settled
                .filter(|id| visible.contains(id))
                .or_else(|| visible.first().copied())
        };
        let active_is_session = target.is_some();
        let active_id = target.unwrap_or(focused.id);
        if !active_is_session {
            if self.session_settled.take().is_some() {
                // Closing or leaving a session tab detaches; it never
                // terminates — the process outlives the tab that was looking
                // at it, and terminating is a separate, confirmed action.
                // A watch ends the same way and for the same reason: it is
                // this window's reading of a session, not the session.
                self.session_attached = None;
                self.session_attaching = None;
                if let Some(backend) = self.session_backend.as_ref() {
                    backend.sender.detach();
                    backend.sender.stop_observing();
                }
            }
            return changed;
        }
        if self.session_settled == Some(active_id) {
            return changed;
        }
        // A surface that has never been measured cannot name a grid. The `+`
        // menu path sizes before it settles; a *restored* tab reaches here on
        // the first maintain pass, which can precede the first viewport — and
        // attaching then would resize the surviving session to a 1×1 grid in
        // front of the person. Not settled yet, so the next pass retries.
        if let Some(view) = self.session_view_by_tab(active_id)
            && (view.surface.bounds.w <= 0.0 || view.surface.bounds.h <= 0.0)
        {
            return changed;
        }
        self.session_settled = Some(active_id);
        // **Watching and driving are settled by two different verbs.** A tab
        // that is watching must never take the controller — that is the
        // whole of what makes it read-only — so the choice is made here,
        // once, rather than inside a verb that would have to refuse it.
        let watching = self
            .session_view_by_tab(active_id)
            .is_some_and(|view| view.surface.watching().watching());
        if watching {
            return self.watch_session(active_id) || changed;
        }
        if let Some(backend) = self.session_backend.as_ref() {
            backend.sender.stop_observing();
        }
        self.take_session_controller(active_id) || changed
    }

    /// The active session tab **watches** its session: give up whatever
    /// controller this window still holds, then read the session's event
    /// stream over a connection that takes none (ADR-361).
    fn watch_session(&mut self, tab_id: u64) -> bool {
        let Some(sender) = self
            .session_backend
            .as_ref()
            .map(|backend| backend.sender.clone())
        else {
            return false;
        };
        let Some(session) = self
            .session_view_by_tab(tab_id)
            .and_then(|view| view.session)
        else {
            return false;
        };
        // The tab this window was driving keeps its process and loses this
        // window's lease — the same release leaving a session tab performs.
        self.session_attached = None;
        self.session_attaching = None;
        sender.detach();
        sender.observe(session);
        true
    }

    /// The active session tab takes the controller: attach its session, or
    /// send the launch it was opened for.
    pub(super) fn take_session_controller(&mut self, tab_id: u64) -> bool {
        let Some(sender) = self
            .session_backend
            .as_ref()
            .map(|backend| backend.sender.clone())
        else {
            return false;
        };
        if self.session_attached == Some(tab_id) || self.session_attaching == Some(tab_id) {
            return false;
        }
        // The launch's working directory, resolved before the mutable view
        // borrow below: the project's folder when the pending launch names a
        // project that has one on this filesystem, else home — `launch_in`'s
        // own contract for a windowed host ("its home, a project root"). A
        // recorded-but-missing folder was refused at the door (S11); a folder
        // that vanished since falls back rather than failing the spawn.
        let pending_project = self
            .panes
            .iter_groups()
            .flat_map(|(_, group)| group.tabs().iter())
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.view {
                TabView::Session(view) => view.pending_launch.as_ref().and_then(|l| l.project),
                _ => None,
            });
        // A recovered tab names the folder the lost session was in; it wins
        // over the project's folder when it still exists. A folder that is
        // gone falls through to the same defaults every launch has.
        let named_directory = self
            .panes
            .iter_groups()
            .flat_map(|(_, group)| group.tabs().iter())
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.view {
                TabView::Session(view) => view
                    .pending_launch
                    .as_ref()
                    .and_then(|l| l.working_directory.clone()),
                _ => None,
            })
            .filter(|directory| directory.is_dir());
        let working_directory = named_directory
            .or_else(|| {
                pending_project
                    .and_then(|id| {
                        self.explore
                            .agent_project_root(id)
                            .map(std::path::PathBuf::from)
                    })
                    .filter(|root| root.is_dir())
            })
            .or_else(std::env::home_dir);
        let readiness = self.provider_readiness.clone();
        let Some(view) = self.session_view_by_tab(tab_id) else {
            return false;
        };
        // A recovered tab is showing its offer: its old session is gone and
        // there is nothing to attach to or launch until the person answers.
        if view.offer.is_some() {
            return false;
        }
        match (view.session, view.pending_launch.take()) {
            (Some(session), _) => {
                // Attach the tab's own session, keeping its previous frame on
                // screen until a correctly sized snapshot for this session
                // arrives (the sized-snapshot gate).
                let opens_raw = !view.surface.session_is_managed;
                let previous_managed = view.surface.session_is_managed;
                let size = view.surface.running_grid_size();
                view.surface
                    .begin_switch(opens_raw, previous_managed, opens_raw.then_some(size));
                view.surface.notice = None;
                self.session_attached = None;
                self.session_attaching = Some(tab_id);
                sender.select(session, size.columns);
                true
            }
            (None, Some(launch)) => {
                let size = view.surface.running_grid_size();
                let intent = if launch.raw_fallback {
                    koru_session_profiles::LaunchIntent::Raw
                } else {
                    koru_session_profiles::LaunchIntent::Ordinary
                };
                let opens_raw = !matches!(
                    koru_session_profiles::managed_launch_mode(
                        &launch.profile,
                        intent,
                        koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
                        readiness.as_deref(),
                    ),
                    Ok(koru_session_protocol::SessionMode::Managed { .. })
                );
                view.surface
                    .begin_switch(opens_raw, false, opens_raw.then_some(size));
                view.surface.status = PaneStatus::Starting;
                view.surface.notice = None;
                let display_name = launch.profile.fallback_display_name().to_owned();
                self.session_attached = None;
                self.session_attaching = Some(tab_id);
                // The placement rides the creation request (ADR-264): the
                // project the tab was opened into, or `None` for the bucket.
                // The directory was resolved above, before this borrow.
                match (launch.resume, working_directory) {
                    // A continuation: the agent's own *continue* form, as a raw
                    // terminal, in the folder the lost session was in.
                    (true, Some(directory)) => sender.launch_resumed(
                        launch.profile,
                        display_name,
                        size,
                        launch.project,
                        directory,
                    ),
                    (_, directory) => sender.launch_in(
                        launch.profile,
                        display_name,
                        size,
                        intent,
                        launch.project,
                        directory,
                    ),
                }
                true
            }
            (None, None) => false,
        }
    }

    pub(super) fn session_view_by_tab(&mut self, tab_id: u64) -> Option<&mut SessionView> {
        self.panes.iter_groups_mut().find_map(|(_, group)| {
            group.tabs_mut().iter_mut().find_map(|tab| {
                if tab.id != tab_id {
                    return None;
                }
                match &mut tab.view {
                    TabView::Session(view) => Some(view),
                    _ => None,
                }
            })
        })
    }

    /// The tab id whose session view is for `session` — the surface-identity
    /// half of ADR-263's guard, asked before any stream event is applied.
    pub(super) fn session_tab_for(&self, session: SessionId) -> Option<u64> {
        self.panes.iter_groups().find_map(|(_, group)| {
            group.tabs().iter().find_map(|tab| match &tab.view {
                TabView::Session(view) if view.session == Some(session) => Some(tab.id),
                _ => None,
            })
        })
    }

    fn session_view_for(&mut self, session: SessionId) -> Option<&mut SessionView> {
        let tab_id = self.session_tab_for(session)?;
        self.session_view_by_tab(tab_id)
    }

    /// The view for `session` **at this epoch** — both halves of ADR-263's
    /// guard, for the managed stream arms.
    fn owning_session_view(&mut self, session: SessionId, epoch: u64) -> Option<&mut SessionView> {
        let view = self.session_view_for(session)?;
        (view.surface.owns(session) && view.surface.active_epoch == epoch).then_some(view)
    }

    /// The active tab's session view, for events that name no session.
    fn active_session_view(&mut self) -> Option<&mut SessionView> {
        match &mut self.tabs_mut().active_mut().view {
            TabView::Session(view) => Some(view),
            _ => None,
        }
    }

    fn session_failure_notice(code: &ErrorCode) -> String {
        let lang = crate::format::lang();
        match code {
            ErrorCode::ControllerConflict => t(lang, I18nKey::TerminalControllerConflict).into(),
            ErrorCode::SessionExited | ErrorCode::SessionNotFound => {
                t(lang, I18nKey::TerminalExited).into()
            }
            ErrorCode::HostUnavailable => t(lang, I18nKey::TerminalAgentUnavailable).into(),
            ErrorCode::InputTooLarge | ErrorCode::StaleLease | ErrorCode::StaleGeneration => {
                t(lang, I18nKey::TerminalInputFailed).into()
            }
            ErrorCode::Backpressure => t(lang, I18nKey::TerminalBackpressure).into(),
            _ => t(lang, I18nKey::TerminalFailed).into(),
        }
    }

    /// The session host (or the connection to it) is away. Present that truth
    /// on every session tab; the backend reconnects on its own thread and
    /// exactly one of `Recovered` or `SessionsLost` follows.
    fn sessions_unavailable(&mut self) -> bool {
        self.session_attached = None;
        self.session_attaching = None;
        // No controller will be taken while the host is away, so an armed
        // Terminate has nothing to wait for.
        self.terminate_after_attach = None;
        let notice: String = t(crate::format::lang(), I18nKey::TerminalAgentUnavailable).into();
        let mut changed = false;
        self.each_tab_mut(|tab| {
            if let TabView::Session(view) = &mut tab.view {
                view.surface.clear_raw_switch();
                view.surface.reset_presentation();
                view.surface.status = PaneStatus::AgentUnavailable;
                view.surface.notice = Some(notice.clone());
                changed = true;
            }
            false
        });
        changed
    }

    /// One backend event, routed to the tab it is about. Answers whether
    /// anything visible changed.
    pub(super) fn handle_session_event(&mut self, event: BackendEvent) -> bool {
        match event {
            // koru.exe's `+` menu reads `built_in_profiles()` directly; the
            // probed catalogue feeds a menu-dimming increment, not this one.
            BackendEvent::Profiles(_) => false,
            BackendEvent::ProviderModels { request, result } => self
                .delegation
                .as_mut()
                .is_some_and(|modal| modal.accept_models(request, result)),
            BackendEvent::Providers(providers) => {
                if self.provider_readiness.as_ref() == Some(&providers) {
                    // A retry can return the same host snapshot while the open
                    // picker locally says Checking; that answer must settle it.
                    return self.update_delegate_readiness();
                }
                self.provider_readiness = Some(providers);
                self.update_delegate_readiness();
                true
            }
            // §7.4, the moment the host says so rather than on the refresh
            // that follows it: a run view learns its takeover was granted
            // (or that it is waiting) without a round trip in between.
            BackendEvent::Handover {
                session_id,
                handover,
            } => {
                let mut changed = self.deliver_handover(session_id, handover);
                changed |= self.settle_watched_tab(session_id, handover);
                changed
            }
            // A watch was established: the transcript arrives from here on,
            // stamped with this epoch, and no lease was taken to get it.
            BackendEvent::Observing { session_id, epoch } => {
                let Some(view) = self.session_view_for(session_id) else {
                    return false;
                };
                view.surface.clear_raw_switch();
                view.surface.session = Some(session_id);
                // Managed: the only kind of session that can be watched, and
                // the only kind a delegated run ever creates (§15).
                view.surface.attach(session_id, epoch, true);
                view.surface.status = PaneStatus::Running;
                // The composer stays away. A managed *attach* reveals it,
                // because a managed agent's input is the composer; a watch
                // has no input at all, and the band is the footer instead.
                view.surface.composer_visible = false;
                view.surface.notice = None;
                true
            }
            // **The catalog is how a tab learns its session's fate.** A shell
            // that exits on its own never sends `Terminated` — the render
            // stream closes and the refreshed catalog arrives here — so every
            // session tab's status is reconciled against it, the same one
            // function Koru Terminal's `sync_tab_status` runs. A late session
            // host also clears the unavailable sentence every tab was
            // carrying; a same-instance recovery is the same quiet resume,
            // and a re-attach, when one follows, arrives as its own
            // `Attached`.
            BackendEvent::Connected(sessions)
            | BackendEvent::Sessions(sessions)
            | BackendEvent::Recovered { sessions } => {
                let attached = self.session_attached;
                let lane_attached: Vec<u64> = self
                    .session_lanes
                    .iter()
                    .filter_map(|lane| lane.attached)
                    .collect();
                // The Agents navigator projects this same catalog — one push,
                // on the same wake, so the tree and the tabs cannot disagree
                // about which sessions exist.
                let mut changed = self.explore.set_agent_sessions(sessions.clone());
                let mut gone = Vec::new();
                for (_, group) in self.panes.iter_groups_mut() {
                    for tab in group.tabs_mut().iter_mut() {
                        let tab_id = tab.id;
                        let TabView::Session(view) = &mut tab.view else {
                            continue;
                        };
                        if view.surface.status == PaneStatus::AgentUnavailable {
                            view.surface.notice = None;
                            changed = true;
                        }
                        let Some(session) = view.session else {
                            continue;
                        };
                        match sessions.iter().find(|summary| summary.id == session) {
                            // The process still exists: reconcile status and
                            // managed state. "Controller detached" and "controlled
                            // elsewhere" are both *alive* — only the process's own
                            // end is ever loss.
                            Some(summary)
                                if !matches!(
                                    summary.process,
                                    ProcessState::Exited { .. } | ProcessState::Failed { .. }
                                ) =>
                            {
                                // **A watched session is not a conflict.**
                                // The orchestrator holding its controller is
                                // exactly the state this tab exists to show,
                                // so it reads *running* rather than the
                                // sentence about another window taking it
                                // (ADR-361).
                                // Alive after a restart: nothing to recover. From
                                // here on a session that ends closes its tab, as
                                // a session started in this run always did.
                                view.recovery = None;
                                let showing_it = attached == Some(tab_id)
                                    || lane_attached.contains(&tab_id)
                                    || view.surface.watching().watching();
                                let status = pane_status_for(summary, showing_it);
                                if view.surface.status != status {
                                    view.surface.status = status;
                                    changed = true;
                                }
                                if view.surface.managed_state != summary.managed {
                                    view.surface.managed_state = summary.managed;
                                    changed = true;
                                }
                            }
                            // The row says the process exited or failed, or the
                            // row is gone entirely: a **confirmed** end, and a
                            // gone session has no tab (Q-E6 P2). This is never
                            // startup silence — a catalog had to arrive to get
                            // here; an unreachable host takes the
                            // `sessions_unavailable` path and keeps every tab.
                            _ => gone.push(tab_id),
                        }
                    }
                }
                changed | self.close_gone_session_tabs(&gone)
            }
            // The project registry, for the Agents navigator's groups. Names
            // only reach the screen; every mutation verb stays with the
            // session host (Q-E4).
            BackendEvent::Projects(projects) => self.explore.set_agent_projects(projects),
            BackendEvent::Attached {
                session_id,
                mode,
                epoch,
            } => self.session_attached_event(session_id, mode, epoch),
            BackendEvent::RawFallback { session_id } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.notice = Some(
                        t(
                            crate::format::lang(),
                            I18nKey::TerminalManagedOpenedRawFallback,
                        )
                        .into(),
                    );
                    true
                } else {
                    false
                }
            }
            // A deliberate terminate was confirmed: the process is over, and a
            // gone session has no tab.
            BackendEvent::Terminated { session_id } => {
                if self
                    .session_attached
                    .is_some_and(|tab| self.session_view_by_tab(tab).is_none())
                {
                    self.session_attached = None;
                }
                match self.session_tab_for(session_id) {
                    Some(tab) => self.close_gone_session_tabs(&[tab]),
                    None => false,
                }
            }
            BackendEvent::Render(update) => self.session_render(update),
            BackendEvent::ManagedBatch {
                session_id,
                epoch,
                events,
                earlier_truncated,
            } => {
                let lang = crate::format::lang();
                let Some(view) = self.owning_session_view(session_id, epoch) else {
                    return false;
                };
                if earlier_truncated {
                    let actual = events
                        .first()
                        .and_then(|event| {
                            event
                                .sequence
                                .get()
                                .checked_sub(1)
                                .and_then(|value| StreamSequence::new(value).ok())
                        })
                        .or_else(|| events.last().map(|event| event.sequence));
                    if let Some(actual) = actual {
                        view.surface.managed.apply_gap(
                            StreamSequence::new(1).expect("one is a valid stream sequence"),
                            actual,
                        );
                        view.surface.record_stream_gap();
                    }
                }
                if !events.is_empty() {
                    if view.surface.managed.apply_batch(events).is_err() {
                        view.surface.notice =
                            Some(t(lang, I18nKey::TerminalManagedTranscriptGap).into());
                        view.surface.record_stream_gap();
                    }
                    view.surface.delta_received = Some(std::time::Instant::now());
                }
                true
            }
            BackendEvent::ManagedGap {
                session_id,
                epoch,
                expected,
                actual,
            } => {
                let Some(view) = self.owning_session_view(session_id, epoch) else {
                    return false;
                };
                view.surface.managed.apply_gap(expected, actual);
                view.surface.delta_received = Some(std::time::Instant::now());
                view.surface.record_stream_gap();
                true
            }
            BackendEvent::ManagedCommandFailure {
                session_id,
                epoch,
                approval_id,
                code,
            } => {
                let Some(view) = self.owning_session_view(session_id, epoch) else {
                    return false;
                };
                if let Some(approval_id) = approval_id {
                    view.surface
                        .managed
                        .approval_failed(approval_id, code.clone());
                }
                view.surface.notice = Some(view.surface.managed.copy().error(&code));
                true
            }
            BackendEvent::ManagedStreamFailure {
                session_id,
                epoch,
                failure,
            } => {
                let Some(view) = self.owning_session_view(session_id, epoch) else {
                    return false;
                };
                let notice = view.surface.managed.copy().error(&failure.code);
                view.surface.notice = Some(notice);
                true
            }
            BackendEvent::Scrollback(page) => {
                if let Some(view) = self.session_view_for(page.session_id) {
                    let _ = view.surface.view.apply_scrollback_page(page);
                    true
                } else {
                    false
                }
            }
            BackendEvent::Attachments {
                session_id,
                attachments,
            } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.attach(session_id, attachments);
                    true
                } else {
                    false
                }
            }
            BackendEvent::AttachmentIngesting {
                session_id,
                attachment_id,
                source,
            } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.begin_ingest(attachment_id, source);
                    true
                } else {
                    false
                }
            }
            BackendEvent::AttachmentIngested {
                session_id,
                attachment,
            } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.ingest_succeeded(attachment);
                    true
                } else {
                    false
                }
            }
            BackendEvent::AttachmentPreview {
                session_id,
                attachment_id,
                preview,
            } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.set_preview(attachment_id, preview);
                    true
                } else {
                    false
                }
            }
            BackendEvent::AttachmentRemoved {
                session_id,
                attachment_id,
            } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.removed(attachment_id);
                    true
                } else {
                    false
                }
            }
            BackendEvent::PendingInputSent { session_id, result } => {
                if let Some(view) = self.session_view_for(session_id) {
                    view.surface.pending.submission_succeeded(result);
                    if view.surface.focus == koru_agents_ui::SurfaceFocus::Composer {
                        view.surface.focus = koru_agents_ui::SurfaceFocus::Terminal;
                        view.surface.pending.set_focused(false);
                    }
                    true
                } else {
                    false
                }
            }
            BackendEvent::PendingInputFailure {
                session_id,
                attachment_id,
                code,
            } => {
                let Some(view) = self.session_view_for(session_id) else {
                    return false;
                };
                let ingesting = attachment_id.is_some_and(|id| {
                    view.surface
                        .pending
                        .model()
                        .attachments()
                        .iter()
                        .any(|item| {
                            item.id == id && item.status == PendingAttachmentStatus::Ingesting
                        })
                });
                if ingesting {
                    view.surface.pending.ingest_failed(
                        attachment_id.expect("ingesting failure has an attachment"),
                        code,
                    );
                } else if attachment_id.is_some() {
                    view.surface.pending.set_error(code);
                } else {
                    view.surface.pending.submission_failed(code);
                }
                true
            }
            BackendEvent::PendingDraftPaste { session_id, text } => {
                if let Some(view) = self.session_view_for(session_id) {
                    let _ = view.surface.pending.insert_text(&text);
                    true
                } else {
                    false
                }
            }
            BackendEvent::TerminalPaste { session_id, text } => {
                let Some(view) = self.session_view_for(session_id) else {
                    return false;
                };
                if view.surface.session_is_managed {
                    return false;
                }
                if view.surface.view.paste(&text).unwrap_or(false) {
                    let eligible = view.surface.blink_eligible();
                    let _ = view
                        .surface
                        .reset_cursor_blink(eligible, std::time::Instant::now());
                }
                true
            }
            BackendEvent::OperationFailure {
                operation,
                session_id,
                failure,
            } => {
                if failure.code == ErrorCode::HostUnavailable {
                    return self.sessions_unavailable();
                }
                let launch = matches!(operation, "launch" | "attach");
                // An attach answered "that session exited / does not exist":
                // a confirmed end learned through the verb instead of the
                // catalog, and the same rule applies — a gone session has no
                // tab. Only a tab already bound to a session id qualifies; a
                // failed *launch* never had one, and keeps its tab to show
                // the failure.
                if launch
                    && matches!(
                        failure.code,
                        ErrorCode::SessionExited | ErrorCode::SessionNotFound
                    )
                {
                    let mut tab = session_id.and_then(|id| self.session_tab_for(id));
                    if tab.is_none()
                        && let Some(waiting) = self.session_attaching
                        && self
                            .session_view_by_tab(waiting)
                            .is_some_and(|view| view.session.is_some())
                    {
                        tab = Some(waiting);
                    }
                    tracing::info!(
                        target: "koru::session",
                        operation,
                        tab = ?tab,
                        "a session was gone when this window asked for it"
                    );
                    if let Some(tab) = tab {
                        self.session_attaching = None;
                        return self.close_gone_session_tabs(&[tab]);
                    }
                }
                let notice = Self::session_failure_notice(&failure.code);
                let attaching = self.session_attaching;
                let view = match session_id.and_then(|id| self.session_tab_for(id)) {
                    Some(tab) => self.session_view_by_tab(tab),
                    // A failed launch has no session named yet: the tab that
                    // asked shows its own error state, never a silent
                    // fallback (ADR-263).
                    None => match attaching {
                        Some(tab) => self.session_view_by_tab(tab),
                        None => self.active_session_view(),
                    },
                };
                let Some(view) = view else {
                    return false;
                };
                if launch {
                    view.surface.clear_raw_switch();
                    view.surface.status = match failure.code {
                        ErrorCode::ControllerConflict => PaneStatus::ControllerConflict,
                        ErrorCode::SessionExited => PaneStatus::Exited,
                        _ => PaneStatus::Failed,
                    };
                }
                view.surface.notice = Some(notice);
                if launch {
                    // The attach that was in flight is not in flight any more.
                    self.session_attaching = None;
                }
                true
            }
            BackendEvent::Failure(failure) => {
                if failure.code == ErrorCode::HostUnavailable {
                    return self.sessions_unavailable();
                }
                let notice = Self::session_failure_notice(&failure.code);
                self.session_attaching = None;
                if let Some(view) = self.active_session_view() {
                    view.surface.clear_raw_switch();
                    view.surface.notice = Some(notice);
                    true
                } else {
                    false
                }
            }
            BackendEvent::Disconnected => {
                if let Some(modal) = self.delegation.as_mut() {
                    modal.models.target = None;
                    if let Some(request) = modal.models.request {
                        modal.accept_models(
                            request,
                            Err(koru_session_protocol::ErrorCode::HostUnavailable),
                        );
                    }
                }
                self.sessions_unavailable()
            }
            // The handshake said **restart**: every session this window knew
            // died with the old host. A gone session has no tab (Q-E6 P2), so
            // each confirmed one closes; the navigator shows the new
            // instance's catalog — empty in practice, which is the honest
            // shape of "those sessions died". A tab still waiting on a launch
            // holds no session id and stays: its answer arrives as its own
            // event.
            BackendEvent::SessionsLost { sessions } => {
                self.session_attached = None;
                self.session_attaching = None;
                self.terminate_after_attach = None;
                // **What each tab was, read before the catalog is replaced.**
                // A tab started in this run holds no saved record; the old
                // catalog still names its folder and project, and the new one
                // will not. With it the tab offers to continue (ADR-474)
                // instead of vanishing and leaving a Welcome tab where the
                // terminal was.
                let lost: Vec<(u64, Option<koru_workspace_host::SessionRecovery>)> = self
                    .panes
                    .iter_groups()
                    .flat_map(|(_, group)| group.tabs().iter())
                    .filter_map(|tab| match &tab.view {
                        TabView::Session(view) if view.session.is_some() => {
                            Some((tab.id, self.session_recovery(view)))
                        }
                        _ => None,
                    })
                    .collect();
                let changed = self.explore.set_agent_sessions(sessions);
                let mut gone = Vec::new();
                for (tab_id, recovery) in lost {
                    tracing::info!(
                        target: "koru::session",
                        tab = tab_id,
                        has_recovery = recovery.is_some(),
                        "the session host restarted under a terminal tab"
                    );
                    if let Some(view) = self.session_view_by_tab(tab_id)
                        && view.recovery.is_none()
                    {
                        view.recovery = recovery;
                    }
                    gone.push(tab_id);
                }
                changed | self.close_gone_session_tabs(&gone)
            }
            BackendEvent::Backpressure => {
                if let Some(view) = self.active_session_view() {
                    view.surface.notice =
                        Some(t(crate::format::lang(), I18nKey::TerminalBackpressure).into());
                    if view.surface.pending.model().submitting() {
                        view.surface
                            .pending
                            .submission_failed(ErrorCode::Backpressure);
                    }
                    true
                } else {
                    false
                }
            }
        }
    }

    /// A controller was granted. Bind it to the tab that asked — which is
    /// `session_attaching`, not "the active one": the person may have switched
    /// away by the time the host answers.
    /// **Where a watched tab stands after the host reported a handover.**
    ///
    /// The surface never decides this (§7.4): it asked, and this is it being
    /// told. Granting turns the read-only tab into the ordinary editable
    /// one — the same tab, the same transcript — and giving the session back
    /// turns it into a watcher again rather than leaving a person looking at
    /// a session they no longer hold.
    fn settle_watched_tab(&mut self, session: SessionId, handover: HandoverState) -> bool {
        let Some(tab_id) = self.session_tab_for(session) else {
            return false;
        };
        let Some(view) = self.session_view_by_tab(tab_id) else {
            return false;
        };
        if !view.watches_run {
            return false;
        }
        let next = match handover {
            // Held by this window: the ordinary session tab, with its
            // composer and its caret back.
            HandoverState::Released | HandoverState::Taken => Watching::No,
            HandoverState::Requested => Watching::Asked,
            // The agent has it — from the start, and again after a return.
            HandoverState::Idle | HandoverState::Returned => Watching::Yes,
        };
        if !view.surface.set_watching(next) {
            return false;
        }
        // The relationship changed, so the settle pass has to make it again:
        // attach where it was watching, observe where it was driving.
        if self.session_settled == Some(tab_id) {
            self.session_settled = None;
        }
        true
    }

    /// **The presses on *Take the session* at the foot of a watched chat.**
    ///
    /// Reused whole from P1.16: the same `request_control` on the same
    /// connection the run views' chip goes through (§7.4, ADR-354). The
    /// window says it is waiting; only the host's `Handover` says it is
    /// held.
    fn drain_takeover_requests(&mut self) -> bool {
        let mut asked = Vec::new();
        self.each_tab_mut(|tab| {
            if let TabView::Session(view) = &mut tab.view
                && view.surface.take_takeover_request()
                && let Some(session) = view.session
            {
                asked.push(session);
            }
            false
        });
        if asked.is_empty() {
            return false;
        }
        let Some(sender) = self.session_verbs() else {
            return false;
        };
        for session in asked {
            // `AtTurnBoundary`, never `Now`: the foot of a chat is not where
            // a person ends the step the agent is on. Neither ends the run —
            // that control is the run views', red and confirmed (ADR-354).
            sender.request_control(
                session,
                koru_session_protocol::TakeoverUrgency::AtTurnBoundary,
            );
            if let Some(view) = self.session_view_for(session) {
                view.surface.set_watching(Watching::Asked);
            }
        }
        true
    }

    fn session_attached_event(
        &mut self,
        session_id: SessionId,
        mode: SessionMode,
        epoch: u64,
    ) -> bool {
        let waiting = self.session_attaching.filter(|tab| {
            self.panes.iter_groups().any(|(_, group)| {
                group.tabs().iter().any(|candidate| {
                    candidate.id == *tab
                        && matches!(&candidate.view, TabView::Session(view) if view.session.is_none())
                })
            })
        });
        let tab = self.session_tab_for(session_id).or(waiting);
        let Some(tab) = tab else {
            return false;
        };
        let raw = matches!(mode, SessionMode::RawPty);
        let sender = self
            .session_backend
            .as_ref()
            .map(|backend| backend.sender.clone());
        let Some(view) = self.session_view_by_tab(tab) else {
            return false;
        };
        if view.session.is_none() {
            view.session = Some(session_id);
        }
        if raw {
            // The surface arms its own sized-snapshot wait and answers the
            // grid the first acceptable snapshot must arrive at; the backend
            // is asked to resize to exactly that. The previous frame stays on
            // screen until it lands — `Render` finishes the attach.
            let size = view.surface.begin_raw_attachment(session_id, epoch);
            if let Some(sender) = sender {
                sender.resize(size);
            }
            return false;
        }
        view.surface.clear_raw_switch();
        view.surface.session = Some(session_id);
        view.surface.attach(
            session_id,
            epoch,
            matches!(mode, SessionMode::Managed { .. }),
        );
        view.surface.status = PaneStatus::Running;
        // A managed agent's input *is* the composer, so a managed attach
        // reveals the band a session tab otherwise opens without (Q-E6 P1) —
        // before the grid is measured, so the size below already accounts for
        // it. The toggle chord can still put it away.
        if view.surface.session_is_managed {
            view.surface.composer_visible = true;
        }
        let size = view.surface.grid_size();
        let managed = view.surface.session_is_managed;
        let _ = view.surface.resize_view(managed, size);
        self.session_attached = self.session_attaching.take();
        // A Terminate that was waiting for this window to hold the controller
        // now can confirm (Q-E7 P8).
        self.terminate_if_armed(session_id);
        true
    }

    /// A render update, routed by session and gated by size — the gate itself
    /// lives on the surface (`SessionSurface::apply_render`); what stays here
    /// is which surface owns the session and the attach bookkeeping.
    fn session_render(&mut self, update: RenderUpdate) -> bool {
        let session = match &update {
            RenderUpdate::Snapshot { snapshot } => snapshot.session_id,
            RenderUpdate::Delta { delta } => delta.session_id,
            RenderUpdate::Gap { gap } => gap.session_id,
        };
        // The identity guard: a frame whose session no tab owns — closed, or
        // switched away from with its pending attachment cleared — is dropped
        // rather than presented.
        let Some(tab) = self.session_tab_for(session) else {
            return false;
        };
        let active = self
            .panes
            .iter_groups()
            .any(|(_, group)| group.tabs().active_tab().id == tab);
        let Some(view) = self.session_view_by_tab(tab) else {
            return false;
        };
        match view.surface.apply_render(update) {
            RenderOutcome::Dropped => false,
            RenderOutcome::SwitchPresented { .. } => {
                self.session_attached = self.session_attaching.take().or(Some(tab));
                // The raw attach completes here rather than in `Attached`, so
                // an armed Terminate confirms from here too (Q-E7 P8).
                self.terminate_if_armed(session);
                true
            }
            RenderOutcome::ApplyFailed => true,
            // Only the active tab's frame is a reason to draw. An update for
            // an inactive session tab is applied to its own model — it is
            // still that session's truth — and asks for nothing.
            RenderOutcome::Applied => active,
        }
    }
}

#[cfg(test)]
mod tests;

/// Whether this build shows a session tab over `key`: always for a shell or a
/// custom profile, and for a built-in **agent** profile only with agents on
/// (koru-features). A key no built-in names is not an agent's.
pub(super) fn session_profile_shown(key: &SessionProfileKey) -> bool {
    koru_features::agents()
        || !built_in_profiles()
            .iter()
            .any(|profile| &profile.id == key && profile.kind == ProfileKind::Agent)
}

/// The launch profile a saved key names, for a recovered tab: a built-in by
/// its key, or a custom profile where this build can still resolve it.
fn recovery_profile(key: &SessionProfileKey) -> Option<koru_session_profiles::LaunchProfile> {
    koru_session_profiles::resolve(key).ok()
}
