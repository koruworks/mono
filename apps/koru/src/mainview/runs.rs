// SPDX-License-Identifier: MPL-2.0

//! **The run tabs' pass**, and the four things a run view asks this window
//! for.
//!
//! `koru-runs-ui` reads a run through `/v1/runs` on a thread of its own and
//! draws it. What it cannot do is anything that needs this window's session
//! connection: asking the agent for the controller, giving it back, and
//! opening the session tab the person types in. All three are one
//! connection's business, because a controller lease belongs to the
//! connection holding it — a surface that opened a second one would be this
//! window competing with itself (§7.4, ADR-341). The fourth is a tab: a run
//! view names a result and this window opens it (ADR-368), because which
//! tab strip a document lands in was never a run view's to decide.
//!
//! **Nothing here reaches a run.** The listing behind the Agents
//! navigator's *Runs* group is `koru_runs_ui::RunIndex`, which is another
//! thread; this file drains what those threads said and pushes in the
//! things only a window knows — the language, the clock, who is rating,
//! what the devices are called, and where the session handover stands.

use koru_artifacts_ui::OpenedArtifact;
use koru_runs_ui::{OpenedResult, Request};
use koru_session_protocol::{HandoverState, SessionId};

use super::{MainView, tab::TabView};

#[cfg(test)]
mod tests;

impl MainView {
    /// Focus the tab already showing `run`, or open one on it.
    ///
    /// A run is a durable record rather than a live process, so this is
    /// always an open: nothing here starts anything, and delegating is the
    /// modal's (P1.14).
    ///
    /// **It is a jump, and it has to say so itself.** A run reached from a
    /// pill in a document is reached *from* somewhere, and Back has to return
    /// to the row the pill is on with the caret and the scroll it had.
    /// `select` records a jump only when it changes which tab is active, and
    /// pushing a tab has already made the new one active by the time it is
    /// called — so on the path that opens a run for the first time, which is
    /// every first press of a pill, nothing was ever written down. Recorded
    /// here instead, where both paths pass.
    ///
    /// `record_jump` does nothing while a restore is in flight, which is what
    /// lets `restore_spot` come back through this same door without recording
    /// the arrival as a departure.
    pub(super) fn open_run_tab(&mut self, run: koru_delegate_core::RunId) -> bool {
        // **The one door a Run tab opens by**: none in a build that hides
        // Koru AI and Delegate (koru-features).
        if !koru_features::ai() {
            return false;
        }
        // Standing still is not a jump. Pressing a pill for the run already in
        // front should leave Back meaning what it meant a moment ago, not
        // spend a press returning to where the pointer already is.
        if matches!(&self.tabs().active_tab().view, TabView::Run(view) if view.run() == Some(run)) {
            return true;
        }
        if let Some((path, index)) = self.run_tab_for(run) {
            self.record_jump();
            self.panes.set_focus(path);
            self.select(index);
            return true;
        }
        // Recorded after this, not before: a window with no workspace opens
        // nothing, and a jump written down for a move that did not happen is a
        // Back press that appears to do nothing.
        let Some(workspace) = self.current_workspace() else {
            return false;
        };
        self.record_jump();
        let mut view = crate::run_view::RunView::new();
        view.open(
            run,
            koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid()),
        );
        view.maintain_presentation(
            &self.reminder_environment.locale,
            &self.reminder_environment.display_rules,
        );
        // **The tab is named after the run, not after the view showing in
        // it.** Three run tabs all reading *Overview* told a person nothing
        // about which run each one held; a derived two-word name (ADR-360)
        // is the same on every device and is not a translated word, so it
        // does not move when the language does.
        self.tabs_mut().push(super::tab::Tab::run(
            view,
            &koru_delegate_core::RunName::of(run).to_string(),
        ));
        self.mark_tab_widths_stale();
        let index = self.tabs().len() - 1;
        self.select(index);
        true
    }

    /// **Open one artifact version as a tab, or focus the tab already on
    /// it** (ADR-368, ADR-369).
    ///
    /// The one way a document out of the Artifact Store becomes a tab in
    /// this window, and both surfaces that name a result reach it: a run's
    /// Result view through [`Request::OpenResult`], and the Artifacts
    /// column through `take_wants_open`. Neither builds a second one, which
    /// is the whole reason it is a function rather than three lines inside
    /// the request that happened to need it first.
    ///
    /// **Focused rather than opened twice.** A version is immutable
    /// (ADR-344), so two tabs on one version could only ever show the same
    /// thing — and the second would take the strip's room and the person's
    /// attention to say nothing new.
    pub(super) fn open_artifact_tab(&mut self, opened: OpenedArtifact) -> bool {
        // The existing availability-only result view has no body renderer.
        // Preserve its remote-holder sentence even when this gateway cannot
        // resolve metadata or read bytes; a hint never instantiates an app.
        if opened.held_elsewhere.is_some() {
            return self.open_markdown_artifact_tab(opened);
        }
        self.request_artifact_open(opened.version)
    }

    /// Only authoritative worker metadata can select the Markdown route.
    pub(super) fn open_markdown_artifact_tab(&mut self, opened: OpenedArtifact) -> bool {
        // A result is reached from a run (Delegate) or the Artifacts column
        // (agent views); with both hidden there is no Result tab
        // (koru-features).
        if !(koru_features::ai() || koru_features::agent_views()) {
            return false;
        }
        if let Some((path, index)) = self.result_tab_for(opened.version) {
            self.panes.set_focus(path);
            self.select(index);
            return true;
        }
        // **The tab is named after the result**, which is the name a person
        // recognises: they asked for *the quarterly summary* and that is
        // what the strip should say. It is drawn and never logged — a
        // title is somebody's writing (standing requirement 7).
        let label = opened.title.clone();
        let run_available = opened
            .produced_by
            .is_some_and(|run| self.run_is_available(run));
        let mut view = crate::result_view::ResultView::new();
        view.set_lang(crate::format::lang());
        view.open(opened);
        view.set_run_available(run_available);
        self.tabs_mut().push(super::tab::Tab::result(view, &label));
        self.mark_tab_widths_stale();
        let index = self.tabs().len() - 1;
        self.select(index);
        true
    }

    /// **What a run said about a result, as the tab wants it.**
    ///
    /// The run's header names the device holding the bytes and the run views
    /// turned that id into a name (D27); the tab takes the answer rather than
    /// the question. The run itself does not travel: nothing about it is
    /// touched by reading what it produced.
    pub(super) fn artifact_of_result(opened: OpenedResult) -> OpenedArtifact {
        OpenedArtifact {
            version: opened.version,
            title: opened.title,
            size_bytes: opened.size_bytes,
            produced_by: Some(opened.run),
            held_elsewhere: opened.held_elsewhere.map(Some),
        }
    }

    /// Whether a run still has a durable record this window can open. A run
    /// in Trash is still a run; only permanent deletion retires the link.
    fn run_is_available(&self, run: koru_delegate_core::RunId) -> bool {
        self.run_index.rows().iter().any(|row| row.run == run)
            || self.run_index.trashed().iter().any(|row| row.run == run)
    }

    fn result_tab_for(
        &self,
        version: koru_delegate_core::ArtifactVersionId,
    ) -> Option<(super::panes::PanePath, usize)> {
        for (path, group) in self.panes.iter_groups() {
            for (index, tab) in group.tabs().iter().enumerate() {
                if let TabView::Result(view) = &tab.view
                    && view.version() == Some(version)
                {
                    return Some((path, index));
                }
            }
        }
        None
    }

    fn run_tab_for(
        &self,
        run: koru_delegate_core::RunId,
    ) -> Option<(super::panes::PanePath, usize)> {
        for (path, group) in self.panes.iter_groups() {
            for (index, tab) in group.tabs().iter().enumerate() {
                if let TabView::Run(view) = &tab.view
                    && view.run() == Some(run)
                {
                    return Some((path, index));
                }
            }
        }
        None
    }

    /// The run views' pass: tell them what only a window knows, take what
    /// their workers said, and perform what they asked for. Answers whether
    /// anything visible changed.
    pub(super) fn maintain_runs(&mut self) -> bool {
        let lang = crate::format::lang();
        let mut changed = self.run_index.set_lang(lang);
        self.run_index.set_workspace(
            self.current_workspace()
                .map(|id| koru_delegate_core::WorkspaceId::from_uuid(id.as_uuid())),
        );
        changed |= self.run_index.poll();
        if changed {
            let rows = self.run_index.rows().to_vec();
            changed |= self.explore.set_agent_runs(rows);
        }
        // Runs and ordinary documents share one Trash. The two listings arrive
        // independently, so recompute their combined state whenever this pass
        // observes the run index rather than leaving Empty Trash gated by the
        // document half alone.
        changed |= self.sync_trash_count();
        // **The bin's runs, into the tab that draws the bin.** They are
        // manifest entries like every other trashed document, but the listing
        // a person sees filters a run out by kind — nobody opens or drags one
        // as a page — so the Trash tab is given them by their own road.
        //
        // Pushed whenever a Trash tab is open rather than only when the
        // listing moved: a tab opened *after* the last change has to be given
        // it too, and nothing pushes again in between. `set_runs` compares
        // before it stores, so a tab that already has them costs the compare
        // and no frame — the same contract `set_documents` beside it keeps.
        if self.has_trash_tabs() {
            let trashed = self.run_index.trashed().to_vec();
            for (_, group) in self.panes.iter_groups_mut() {
                for tab in group.tabs_mut().iter_mut() {
                    if let TabView::Trash(view) = &mut tab.view {
                        changed |= view.set_runs(trashed.clone());
                    }
                }
            }
        }
        // What the worker did about a deletion, finished here: the Run
        // document moves, and the receipt says what happened.
        changed |= self.drain_run_deletions();

        // **The result tabs' half of the pass.** Content is immutable and
        // belongs to each tab's worker; the one moving fact is whether its
        // producing run still has a durable record in this workspace. Runs
        // in Trash remain openable. Once purged, the icon disappears.
        let available_runs: std::collections::HashSet<_> = self
            .run_index
            .rows()
            .iter()
            .map(|row| row.run)
            .chain(self.run_index.trashed().iter().map(|row| row.run))
            .collect();
        let mut wanted_runs = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let TabView::Result(view) = &mut tab.view else {
                    continue;
                };
                view.set_lang(lang);
                let available = view
                    .producing_run()
                    .is_some_and(|run| available_runs.contains(&run));
                changed |= view.set_run_available(available);
                changed |= view.poll();
                wanted_runs.extend(view.take_wants_run());
            }
        }
        for run in wanted_runs {
            if available_runs.contains(&run) {
                changed |= self.open_run_tab(run);
            }
        }

        if !self.has_run_tabs() {
            return changed;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let profile = self
            .pane_context
            .as_ref()
            .map(|(_, profile, _, _)| koru_delegate_core::ProfileId::from_uuid(profile.as_uuid()));
        // Devices are named at display time: the run stores an id, and what
        // that machine is called today is the Agent's to say. Read without
        // consuming the change flag — the Link Device dialog owns that.
        let (this_device, devices) = self.agent.linked_devices();
        let handovers: Vec<(SessionId, HandoverState)> = self
            .explore
            .agent_sessions()
            .iter()
            .map(|summary| (summary.id, summary.handover))
            .collect();
        let locale = &self.reminder_environment.locale;
        let display_rules = &self.reminder_environment.display_rules;

        let mut requests = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let TabView::Run(view) = &mut tab.view else {
                    continue;
                };
                view.set_now_ms(now_ms);
                view.set_profile(profile);
                for device in this_device.iter().chain(devices.iter()) {
                    if let Some(id) = koru_delegate_core::DeviceId::parse(&device.device_id) {
                        view.set_device_name(id, device.display_name.clone());
                    }
                }
                if let Some(session) = view.session()
                    && let Some((_, handover)) = handovers.iter().find(|(id, _)| *id == session)
                {
                    view.set_handover(*handover);
                }
                changed |= view.poll();
                changed |= view.maintain_presentation(locale, display_rules);
                requests.extend(view.take_requests());
            }
        }
        for request in requests {
            changed |= self.perform_run_request(request);
        }
        changed
    }

    fn has_trash_tabs(&self) -> bool {
        self.panes.iter_groups().any(|(_, group)| {
            group
                .tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Trash(_)))
        })
    }

    fn has_run_tabs(&self) -> bool {
        self.panes.iter_groups().any(|(_, group)| {
            group
                .tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Run(_)))
        })
    }

    /// **What a run view asks for and cannot do itself**: the three verbs
    /// that belong to this window's session connection, the delegation
    /// surface, and a tab.
    fn perform_run_request(&mut self, request: Request) -> bool {
        match request {
            // §7.4. `AtTurnBoundary` waits for the agent to finish its step;
            // `Now` ends that step with `CancelReason::TakeoverBoundary` so
            // the boundary arrives sooner. **Neither ends the run** — that
            // is the run surface's own, red, confirmed control (D36).
            Request::TakeControl { session, urgency } => {
                let Some(sender) = self.session_verbs() else {
                    return false;
                };
                sender.request_control(session, urgency);
                true
            }
            Request::GiveBackControl { session } => {
                let Some(sender) = self.session_verbs() else {
                    return false;
                };
                let _ = session;
                sender.return_control();
                true
            }
            // The lease is free: open the tab the person types in. An
            // ordinary session tab once control is held (D17).
            Request::OpenSession { session } => self.open_session_tab_for_existing(session),
            // **The run views ask; the window performs** -- the same rule the
            // three handover verbs follow. The delegation surface names
            // documents, projects and a workspace, all of which that crate is
            // chartered to name none of; and this makes a *new* run, so
            // nothing here goes back to the run it came from.
            Request::ReRun(seed) => {
                self.begin_delegation(crate::mainview::delegate::DelegationOrigin::ReRun { seed })
            }
            // **A result opens as a tab, the way every other document in
            // Koru opens** (ADR-368). The run's own views stay exactly
            // where the person left them: nothing here goes back to the
            // run, and the tab reads the version for itself.
            Request::OpenResult(opened) => {
                self.open_artifact_tab(Self::artifact_of_result(*opened))
            }
            // **Show the folder in Explorer or Finder**, through the one
            // platform seam the shell already has — the same
            // `PaneRequest::RevealPath` an error's *Reveal Folder* and the
            // run trash's receipt send. Spawning a file manager is not this
            // window's to do either: `ask_pane` hands it to the shell, which
            // performs it off the window thread, and the shell reports a
            // failure the way it reports every other one.
            Request::RevealFolder(path) => {
                self.ask_pane(koru_workspace_host::PaneRequest::RevealPath(path))
            }
        }
    }

    /// A handover the backend reported the moment it happened, rather than
    /// on the refresh that follows it.
    pub(super) fn deliver_handover(&mut self, session: SessionId, handover: HandoverState) -> bool {
        let mut changed = false;
        let mut requests = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let TabView::Run(view) = &mut tab.view else {
                    continue;
                };
                if view.session() != Some(session) {
                    continue;
                }
                view.set_handover(handover);
                requests.extend(view.take_requests());
                changed = true;
            }
        }
        for request in requests {
            changed |= self.perform_run_request(request);
        }
        changed
    }
}
