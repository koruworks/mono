// SPDX-License-Identifier: MPL-2.0

//! What survives a restart: the open tabs, which one was showing, where each
//! was scrolled to, the two columns, and the recents.
//!
//! Everything read here came off disk and none of it is trusted — a node can
//! have been deleted by another peer between one run and the next, and the file
//! is hand-editable by design.

use crate::panes::explore::rail::{Bucket, RailTarget};
use koru_draw::theme;
use koru_workspace::Workspace;
use koru_workspace_host::View;
use koru_workspace_host::settings::{
    GroupActiveState, PaneNodeState, PaneTreeState, PinnedTabRef, Settings, SplitAxisState, TabRef,
};

use super::MainView;
use super::layout::{AI_MAX_W, AI_MIN_W, SIDEBAR_MAX_W, SIDEBAR_MIN_W};
use super::panes::{PaneNode, PanePath, PaneTree, SplitAxis, TabGroup};
use super::tab::{Tab, TabView};
use super::tab_rules::KoruTabs;
use crate::leaf::Leaf;

impl MainView {
    pub(super) fn restore_session(&mut self, workspace: &Workspace, settings: &Settings) {
        crate::terminal_prefs::load_agent_looks(&settings.terminal_looks);
        // **The whole workspace, not one document.** A tab is over a document
        // and a window has several of them, so rebuilding the strip needs each
        // one — the shell has already opened the remembered ones, and
        // `Workspace::document` is how each is reached. Everything below that
        // works on the *active* document still says `ws`.
        self.recents = settings.recents.clone();
        self.recents_stale = true;
        self.pinned_show_title = settings.pinned_show_title;
        // Set rather than slid: motion on startup is motion nobody asked for,
        // and `animating` would be true before the first frame was even drawn.
        // The width **before** the open state, so `set_immediately` commits it
        // to the content in the same breath: a restored width is not a drag,
        // and the first frame should be laid out against it rather than snapping
        // to it a moment later.
        if let Some(w) = settings.workspace.and_then(|id| settings.sidebar_w_in(id)) {
            self.sidebar.set_full(w.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W));
        }
        self.sidebar.set_immediately(settings.sidebar_open);
        self.ai_panel
            .set_full(settings.ai_w.clamp(AI_MIN_W, AI_MAX_W));
        // A build that hides Koru AI never reopens its pane (koru-features).
        let ai_open = settings.ai_open && koru_features::ai();
        self.ai_panel.set_immediately(ai_open);
        // AI and Context share the right-hand slot. If an older settings file
        // says both were open, the explicitly restored AI pane wins.
        self.context
            .set_immediately(settings.context_open && !ai_open);
        self.resolve_regions();
        self.explore_stale = true;
        // What somebody deliberately shut, **in this workspace**. Restored
        // before the first rebuild, so the tree comes back the shape it was left
        // rather than flashing open and folding a frame later.
        //
        // `settings.workspace` is the key rather than the pane's context,
        // because `restore` runs before the shell has told the pane anything —
        // and the shell sets it to the workspace it is about to open, on the
        // startup path and on a switch alike.
        let collapsed = settings
            .workspace
            .map(|id| settings.collapsed_in(id).to_vec())
            .unwrap_or_default();
        self.explore.set_collapsed(collapsed);

        // Rebuild the list, dropping anything whose node has gone. Every tab is
        // rebuilt now: there is no permanent one to keep alive across the swap.
        if let Some(pt) = &settings.pane_tree {
            self.panes = self.build_pane_tree(workspace, settings, pt);
        } else {
            self.restore_legacy_tabs(workspace, settings);
        }
        // The same reason the columns are set rather than slid: a tab arriving
        // mid-glide is motion nobody asked for, and `animating` would be true
        // before the first frame had been drawn. Apply to every group so splits
        // reopen without spurious animation.
        for (_, group) in self.panes.iter_groups_mut() {
            group.slides.clear();
        }

        for (_, group) in self.panes.iter_groups_mut() {
            for t in group.tabs_mut().iter_mut() {
                // **Neither tab that owns a private workspace.** `View::restore`
                // reads this workspace's `DocViewPrefs` and applies them per
                // document root — and both of these are rooted in a document that
                // exists only in memory, so the lookup would either miss or, worse,
                // collide with a real document's saved preferences.
                if t.draft.is_some() || t.whats_new.is_some() {
                    continue;
                }
                // Both leaves and the welcome page restore from `Settings`: the
                // welcome reads the global default view mode and canvas colour it
                // offers, so it must not be skipped the way it used to be.
                match &mut t.view {
                    TabView::Leaf(l) => View::restore(l, workspace, settings),
                    TabView::Welcome(w) => {
                        View::restore(w, workspace, settings);
                    }
                    // Manage Profiles is a utility destination, not a place
                    // Koru should strand somebody on at startup; older
                    // settings still parse but reopen on the durable Welcome
                    // floor, so a restored tab is welcome-shaped.
                    TabView::Manage(w) => View::restore(w, workspace, settings),
                    TabView::Trash(t) => View::restore(t, workspace, settings),
                    // **The rail is put back from the tab**, not the other way
                    // round: the container pushes this target into
                    // `rail_mut().set_selected` on the first refresh, so a
                    // window that reopens on Upcoming has an Upcoming column.
                    TabView::Tasks(t) => View::restore(t, workspace, settings),
                    // The Reminders page keeps no per-document prefs either;
                    // its restore is the `TabRef` arm that rebuilt the tab.
                    TabView::Reminders(r) => View::restore(r, workspace, settings),
                    TabView::Settings(s) => View::restore(s, workspace, settings),
                    // Home keeps no per-document prefs; it is fed each frame.
                    TabView::Home(_) => {}
                    TabView::Feedback(f) => View::restore(f, workspace, settings),
                    TabView::Diff(d) => View::restore(d, workspace, settings),
                    // A chat is a live conversation rather than a place in the
                    // library: there is nothing in `Settings` to apply to it.
                    TabView::AiChat(_) => {}
                    TabView::Socials(_) => {}
                    // A session shows a process, not a document: it has no
                    // `DocViewPrefs` to read, and its real restore — reattach or
                    // session-lost — is the backend's.
                    TabView::Session(_) => {}
                    // A run view shows a run, whose state is the run's own
                    // document read through the gateway; a result tab shows an
                    // artifact version, which is immutable. There is nothing in
                    // `Settings` that is about either.
                    TabView::Run(_) | TabView::Result(_) => {}
                    // The camera will be the surface's (`DocViewPrefs`, P1.5);
                    // the placeholder reads nothing.
                    TabView::Canvas(c) => View::restore(c, workspace, settings),
                }
            }
        }
    }

    /// What a session tab keeps so it can offer a replacement when its session
    /// is gone at the next start (ADR-474): the folder and project the host
    /// reports for the live session, or — for a restored tab whose session has
    /// not been confirmed, or is gone and now offered — what it was saved with,
    /// so a second quit before a choice does not lose it.
    pub(super) fn session_recovery(
        &self,
        view: &crate::session_tab::SessionView,
    ) -> Option<koru_workspace_host::SessionRecovery> {
        let live = view
            .session
            .and_then(|id| self.explore.agent_session(id))
            .filter(|_| view.offer.is_none());
        match live {
            Some(summary) => Some(koru_workspace_host::SessionRecovery {
                working_directory: summary.working_directory.clone(),
                project: summary.project_id,
                raw: matches!(summary.mode, koru_session_protocol::SessionMode::RawPty),
            }),
            None => view.recovery.clone(),
        }
    }

    pub(super) fn capture_session(&self, settings: &mut Settings) {
        settings.terminal_looks = crate::terminal_prefs::agent_looks();
        // Per workspace, and **the width that was asked for** rather than
        // `regions.sidebar.w`: `solve` clamps against `CONTENT_MIN_W`, so
        // storing the clamped one would mean opening on a laptop permanently
        // shrank the width chosen on a desktop.
        if let Some(id) = settings.workspace {
            settings.set_sidebar_w_in(id, self.sidebar.full());
        }
        // Persist the full split tree. The legacy focused-group fields below
        // are kept so older builds can still restore *something*.
        settings.pane_tree = Some(self.pane_tree_state());

        // `pinned_show_title` is a window-level preference surfaced on the
        // welcome page; any open welcome tab in any group has the live value.
        settings.pinned_show_title = self
            .panes
            .iter_groups()
            .flat_map(|(_, g)| g.tabs().iter())
            .find_map(|t| match &t.view {
                TabView::Welcome(w) => Some(w.pinned_show_title()),
                _ => None,
            })
            .unwrap_or(self.pinned_show_title);
        let (pinned, pinned_tab) = self.saved_pins(self.panes.focused().tabs());
        settings.pinned_tabs = pinned;
        settings.pinned_tab = pinned_tab;
        settings.tabs = self.saved_main_tabs(self.panes.focused().tabs());
        settings.tab = self.panes.focused().tabs().main_active();
        settings.recents = self.recents.clone();
        // Only this workspace's row is touched: writing the field would
        // discard every other workspace's, which is precisely the bug that made
        // a tree come back expanded after a switch.
        if let Some(id) = settings.workspace {
            settings.set_collapsed_in(id, self.explore.collapsed());
        }
        // ...and a workspace that no longer exists stops being remembered. The
        // pane knows the list because the shell hands it one; before it has,
        // nothing is pruned, which is the safe direction.
        if let Some((_, _, workspaces, _)) = &self.pane_context {
            let live: Vec<_> = workspaces.iter().map(|w| w.id).collect();
            settings.prune_collapsed(&live);
        }
        settings.sidebar_open = self.sidebar.is_open();
        settings.context_open = self.context.is_open();
        settings.ai_open = self.ai_panel.is_open();
        settings.ai_w = self.ai_panel.full();

        // Each view writes its own presentation prefs: documents their
        // per-document view-mode/canvas override, Settings the global
        // default `view_mode`/`canvas_bg`. This is the only place those are
        // captured, and it runs for every tab in every split pane.
        for (_, group) in self.panes.iter_groups() {
            for t in group.tabs().iter() {
                match &t.view {
                    TabView::Leaf(l) => View::capture(l, settings),
                    TabView::Welcome(w) => View::capture(w, settings),
                    TabView::Manage(w) => View::capture(w, settings),
                    TabView::Trash(t) => View::capture(t, settings),
                    TabView::Tasks(t) => View::capture(t, settings),
                    TabView::Reminders(r) => View::capture(r, settings),
                    TabView::Settings(s) => View::capture(s, settings),
                    TabView::Home(_) => {}
                    TabView::AiChat(_) => {}
                    TabView::Socials(_) => {}
                    TabView::Session(_) => {}
                    TabView::Run(_) | TabView::Result(_) => {}
                    TabView::Canvas(c) => View::capture(c, settings),
                    TabView::Feedback(f) => View::capture(f, settings),
                    TabView::Diff(_) => {}
                }
            }
        }
        // The global defaults are process-globals Settings sets live, so sync
        // them here regardless of whether a Settings tab is still open — a
        // choice made there and then closed before save must still persist.
        // (When a Settings tab *is* open this writes the same value its
        // `capture` just did.) The shell also copies them in
        // `capture_preferences` so the click that chose them reaches the
        // authority without waiting for this path.
        settings.view_mode = theme::default_view_mode();
        settings.canvas_bg = theme::default_canvas_bg();
        // "Apply to all" asked for every per-document override in this workspace
        // to be forgotten, so documents not currently open follow the global
        // default too. Done after the captures: the open tabs now match the
        // global and write nothing, so this only clears the rest.
        if self.clear_doc_view_prefs.take() {
            if let Some(id) = settings.workspace {
                settings.doc_view_prefs.retain(|(w, _)| *w != id);
            }
        }

        // `hide_completed` belongs to the outline, and the container has no
        // outline tab any more — every leaf here is a document, whose `capture`
        // is a no-op. So the stored value is carried through untouched rather
        // than read from a view that has no opinion about it. It comes back to
        // life with the `TaskGroup` node, which is where completed rows will
        // live.
    }

    fn restore_legacy_tabs(&mut self, workspace: &Workspace, settings: &Settings) {
        let ws = &workspace.ws;
        let mut tabs: Vec<Tab> = Vec::new();
        let mut restored_active = 0;
        for (stored_index, (which, scroll)) in settings.tabs.iter().enumerate() {
            let mut tab = match which {
                TabRef::Welcome => Tab::welcome(),
                // The Workspaces tab's token restores Manage (maintainers' decision
                // 9): its Profile pages are where the workspaces went.
                TabRef::Workspaces => Tab::manage(),
                // Manage Profiles is a utility destination, not a place Koru
                // should strand somebody on at startup. Older settings files
                // still parse, but reopen on the durable Welcome floor.
                TabRef::ManageProfiles => Tab::welcome(),
                // The Manage tab comes back on the open Profile's page (maintainer
                // decision 9): it opens with no page, and the first context
                // push lands it there.
                TabRef::Manage => Tab::manage(),
                TabRef::Trash => Tab::trash(),
                // **The rail is put back from the tab**, not the other way
                // round: the container pushes this target into
                // `rail_mut().set_selected` on the first refresh, so a window
                // that reopens on Upcoming has an Upcoming column.
                // A hidden surface's tab is dropped, not degraded: it was
                // never this build's to show (koru-features).
                TabRef::Tasks(target) => {
                    let target = rail_target(*target);
                    if !super::tasks::rail_target_shown(target) {
                        continue;
                    }
                    Tab::tasks(target)
                }
                TabRef::Draft => Tab::draft(None).unwrap_or_else(Tab::welcome),
                TabRef::WhatsNew => {
                    let version = koru_channel::build_info().version;
                    let notes = crate::whatsnew::notes_beside_the_exe();
                    match Tab::whats_new(version, notes.as_deref()) {
                        Some(tab) => tab,
                        None => continue,
                    }
                }
                TabRef::SyncSettings => match Tab::sync_settings() {
                    Some(tab) => tab,
                    None => continue,
                },
                TabRef::Node(id) => {
                    if ws.read(*id).is_err() {
                        continue;
                    }
                    let name = koru_workspace::name_of(ws, *id);
                    Tab::document(Leaf::document(ws, *id), &name)
                }
                // **Every saved session tab returns as a tab** — it comes back
                // *connecting*, holding its saved `SessionId` and
                // `SessionProfileKey`; whether the session survived is the
                // handshake's to say, once the backend connects
                // (`session_backend.rs` reconciles against the catalog).
                // Restoring never terminates anything.
                //
                // An **agent** session's tab is dropped when this build hides
                // agents; the session itself is untouched (koru-features).
                TabRef::Session {
                    session,
                    profile,
                    recovery,
                } => {
                    if !super::session_backend::session_profile_shown(profile) {
                        continue;
                    }
                    self.restored_session_tab(*session, profile.clone(), recovery.clone())
                }
                TabRef::Chat(chat) => match restored_chat_tab(*chat) {
                    Some(tab) => tab,
                    None => continue,
                },
                TabRef::Document {
                    id,
                    workspace: home,
                } => {
                    let Some(doc) = workspace.document(*id) else {
                        continue;
                    };
                    let Some(root) = koru_workspace::document_root(doc) else {
                        continue;
                    };
                    let name = koru_workspace::name_of(doc, root);
                    let home = home.or(settings.workspace);
                    let Some(view) = self.restored_document_view(workspace, doc, root, *id) else {
                        continue;
                    };
                    Tab::document_at(view, &name, Some(*id), home)
                }
            };
            // A session that held Welcome twice — the second entry written as
            // ManageProfiles or by a build that degraded Settings to Welcome —
            // reopens on one Welcome tab. The active index follows the entry
            // it was stored against to the welcome tab that survived.
            if matches!(which, TabRef::Welcome | TabRef::ManageProfiles)
                && let Some(existing) = tabs
                    .iter()
                    .position(|tab| matches!(tab.view, TabView::Welcome(_)))
            {
                if stored_index == settings.tab {
                    restored_active = existing;
                }
                continue;
            }
            // And one Manage tab, though a session written while both tabs
            // existed may hold both a `manage` and a `workspaces` token.
            if matches!(which, TabRef::Manage | TabRef::Workspaces)
                && let Some(existing) = tabs
                    .iter()
                    .position(|tab| matches!(tab.view, TabView::Manage(_)))
            {
                if stored_index == settings.tab {
                    restored_active = existing;
                }
                continue;
            }
            tab.view.set_scroll(*scroll);
            tab.invalidate_size();
            if stored_index == settings.tab {
                restored_active = tabs.len();
            }
            tabs.push(tab);
        }
        if self.panes.focused().tabs().pinned_count() == 0 {
            let pins: Vec<Tab> = settings
                .pinned_tabs
                .iter()
                .filter_map(|pin| self.pin_from_ref(workspace, settings, pin))
                .collect();
            if !pins.is_empty() {
                self.panes
                    .focused_mut()
                    .tabs_mut()
                    .restore_pinned(pins, settings.pinned_tab);
            }
        }
        if !tabs.is_empty() {
            self.panes
                .focused_mut()
                .tabs_mut()
                .restore_main(tabs, restored_active);
        }
        self.panes
            .focused_mut()
            .tabs_mut()
            .drop_lone_welcome_if_pinned();
        if let Some(i) = settings
            .pinned_tab
            .filter(|&i| i < self.panes.focused().tabs().pinned_count())
        {
            let _ = self.panes.focused_mut().tabs_mut().select(i);
        }
    }

    fn build_pane_tree(
        &mut self,
        workspace: &Workspace,
        settings: &Settings,
        state: &PaneTreeState,
    ) -> PaneTree {
        fn build_node(
            view: &mut MainView,
            workspace: &Workspace,
            settings: &Settings,
            node: &PaneNodeState,
        ) -> PaneNode {
            match node {
                PaneNodeState::Group {
                    main,
                    pinned,
                    active,
                } => {
                    // The same single-Welcome rule the legacy path keeps: a
                    // group never reopens with two Welcome tabs, whatever the
                    // second entry degraded from (ManageProfiles, Settings,
                    // Feedback).
                    let mut main_tabs: Vec<Tab> = Vec::new();
                    for (which, scroll) in main {
                        if let Some(tab) = view.tab_from_ref(workspace, settings, which, *scroll) {
                            if matches!(tab.view, TabView::Welcome(_))
                                && main_tabs
                                    .iter()
                                    .any(|t| matches!(t.view, TabView::Welcome(_)))
                            {
                                continue;
                            }
                            // One Manage tab: `manage` and the legacy
                            // `workspaces` token both restore it.
                            if matches!(tab.view, TabView::Manage(_))
                                && main_tabs
                                    .iter()
                                    .any(|t| matches!(t.view, TabView::Manage(_)))
                            {
                                continue;
                            }
                            main_tabs.push(tab);
                        }
                    }
                    let pin_tabs: Vec<Tab> = pinned
                        .iter()
                        .filter_map(|pin| view.pin_from_ref(workspace, settings, pin))
                        .collect();
                    let active_main = match active {
                        GroupActiveState::Main(i) => *i,
                        _ => 0,
                    };
                    let active_main = active_main.min(main_tabs.len().saturating_sub(1));
                    // A placeholder welcome tab gives `restore_main`/`restore_pinned`
                    // something to replace; `drop_lone_welcome_if_pinned` removes it
                    // when the group is pinned-only.
                    let mut group = TabGroup::new(Tab::welcome());
                    if !pin_tabs.is_empty() {
                        let showing = match active {
                            GroupActiveState::Pinned(i) => Some(*i),
                            _ => None,
                        };
                        group.tabs_mut().restore_pinned(pin_tabs, showing);
                    }
                    group.tabs_mut().restore_main(main_tabs, active_main);
                    group.tabs_mut().drop_lone_welcome_if_pinned();
                    PaneNode::Group(group)
                }
                PaneNodeState::Split {
                    axis,
                    ratio,
                    left,
                    right,
                } => PaneNode::Split {
                    axis: match axis {
                        SplitAxisState::Horizontal => SplitAxis::Horizontal,
                        SplitAxisState::Vertical => SplitAxis::Vertical,
                    },
                    ratio: *ratio,
                    children: [
                        Box::new(build_node(view, workspace, settings, left)),
                        Box::new(build_node(view, workspace, settings, right)),
                    ],
                },
            }
        }
        let root = build_node(self, workspace, settings, &state.root);
        let mut tree = PaneTree::from_root(root);
        let path = state
            .focused
            .iter()
            .fold(PanePath::root(), |p: PanePath, &i| p.child(i as usize));
        // A path that no longer names a group — a hand-edited or truncated
        // row — falls back the way a collapsed pane does: `focused()` expects
        // a group and a split root's own path is not one.
        if tree.contains_group(&path) {
            tree.set_focus(path);
        } else {
            let first = tree.first_group_path();
            tree.set_focus(first);
        }
        tree
    }

    fn tab_from_ref(
        &mut self,
        workspace: &Workspace,
        settings: &Settings,
        which: &TabRef,
        scroll: f32,
    ) -> Option<Tab> {
        let ws = &workspace.ws;
        let mut tab = match which {
            TabRef::Welcome => Tab::welcome(),
            // The legacy token restores Manage (maintainers' decision 9).
            TabRef::Workspaces => Tab::manage(),
            // Manage Profiles is a utility destination, not a place Koru
            // should strand somebody on at startup. Older settings files
            // still parse, but reopen on the durable Welcome floor.
            TabRef::ManageProfiles => Tab::welcome(),
            TabRef::Manage => Tab::manage(),
            TabRef::Trash => Tab::trash(),
            // **The rail is put back from the tab**, not the other way round:
            // the container pushes this target into `rail_mut().set_selected`
            // on the first refresh, so a window that reopens on Upcoming has
            // an Upcoming column.
            // A hidden surface's tab is dropped (koru-features).
            TabRef::Tasks(target) => {
                let target = rail_target(*target);
                if !super::tasks::rail_target_shown(target) {
                    return None;
                }
                Tab::tasks(target)
            }
            TabRef::Draft => Tab::draft(None).unwrap_or_else(Tab::welcome),
            TabRef::WhatsNew => {
                let version = koru_channel::build_info().version;
                let notes = crate::whatsnew::notes_beside_the_exe();
                Tab::whats_new(version, notes.as_deref())?
            }
            TabRef::SyncSettings => Tab::sync_settings()?,
            TabRef::Node(id) => {
                if ws.read(*id).is_err() {
                    return None;
                }
                let name = koru_workspace::name_of(ws, *id);
                Tab::document(Leaf::document(ws, *id), &name)
            }
            // **Every saved session tab returns as a tab** — it comes back
            // *connecting*, holding its saved `SessionId` and
            // `SessionProfileKey`; whether the session survived is the
            // handshake's to say, once the backend connects
            // (`session_backend.rs` reconciles against the catalog).
            // Restoring never terminates anything.
            //
            // An **agent** session's tab is dropped when this build hides
            // agents; the session itself is untouched (koru-features).
            TabRef::Session {
                session,
                profile,
                recovery,
            } => {
                if !super::session_backend::session_profile_shown(profile) {
                    return None;
                }
                self.restored_session_tab(*session, profile.clone(), recovery.clone())
            }
            TabRef::Chat(chat) => restored_chat_tab(*chat)?,
            TabRef::Document {
                id,
                workspace: home,
            } => {
                let doc = workspace.document(*id)?;
                let root = koru_workspace::document_root(doc)?;
                let name = koru_workspace::name_of(doc, root);
                let home = home.or(settings.workspace);
                let view = self.restored_document_view(workspace, doc, root, *id)?;
                Tab::document_at(view, &name, Some(*id), home)
            }
        };
        tab.view.set_scroll(scroll);
        tab.invalidate_size();
        Some(tab)
    }

    /// A saved pin, reopened **through the same door a main tab is**
    /// ([`tab_from_ref`](Self::tab_from_ref)) and then pinned again: a pin is
    /// a place in the strip, not a second way of opening a tab.
    fn pin_from_ref(
        &mut self,
        workspace: &Workspace,
        settings: &Settings,
        pin: &PinnedTabRef,
    ) -> Option<Tab> {
        let mut tab = self.tab_from_ref(workspace, settings, &pin.tab(), pin.scroll)?;
        // What `TabStrip::pin` stamps, put back.
        tab.home = Some(pin.workspace);
        tab.pinned = Some(pin.workspace);
        tab.show_title = pin.show_title;
        Some(tab)
    }

    /// **The token a tab is saved as**, or `None` for a tab that has no saved
    /// form: Settings, Feedback, Home, a version comparison, Socials, a run's
    /// pages, a chat not sent yet and a session not yet named. The one
    /// mapping the flat `tabs` line, every pane group's main tabs and every
    /// pin are written from — it was written out twice, and a pin had a
    /// third, shorter vocabulary that could say only *document* or *chat*.
    fn saved_tab_ref(&self, t: &Tab) -> Option<TabRef> {
        // A draft names nothing in this document, so it comes back as an
        // empty draft rather than as a node that will not parse. What it
        // *was* is a blank page, and a blank page restores perfectly.
        Some(match &t.view {
            _ if t.draft.is_some() => TabRef::Draft,
            // **Before the `Leaf` arm, and that ordering is the whole
            // of it.** A notes tab *is* a `Leaf`, so the arm below
            // matches it perfectly well and would write
            // `View::document_root` — a `NodeId` in a private in-memory
            // workspace that no future launch can resolve. It degrades
            // rather than breaks (the entry is skipped and the strip
            // silently shortens), which is exactly why the compiler
            // will not tell you.
            _ if t.whats_new.is_some() => private_page_ref(t)?,
            TabView::Welcome(_) => TabRef::Welcome,
            TabView::Manage(_) => TabRef::Manage,
            TabView::Trash(_) => TabRef::Trash,
            TabView::Tasks(t) => TabRef::Tasks(tasks_tab_ref(t.target())),
            // A Reminders page comes back as a Reminders page: one
            // target, one spelling, restored through `Tab::tasks` like
            // every other rail target.
            TabView::Reminders(_) => TabRef::Tasks(koru_workspace_host::TasksTabRef::Reminders),
            // **No `TabRef` for these.** A flag is not one (until SP-7
            // Settings *was* a `WelcomeView` with a flag set), and recording
            // them properly is a new `TabRef` variant in the session file.
            // As a main tab they come back as Welcome (`saved_main_tabs`);
            // pinned, they are not written.
            TabView::Settings(_) | TabView::Feedback(_) | TabView::Home(_) => return None,
            // A diff is a working artifact over two fetched snapshots;
            // restoring it would need a re-fetch that can fail at startup,
            // so it is deliberately not part of the session. Nor is Socials.
            TabView::Diff(_) | TabView::Socials(_) => return None,
            // **A chat tab by its chat** (ADR-483-chattabrestore). A
            // new chat not sent yet has no id the service knows — the
            // `?` — and skips itself: there is nothing to reopen.
            TabView::AiChat(a) => chat_tab_ref(a)?,
            // **The saved record is `{session_id, profile_key}`**
            // (TERMINATAB), in tab order with the rest of the strip.
            // A tab whose launch has not come back named yet has no
            // session id to save and skips itself (the `?`s) — a
            // window killed inside that heartbeat loses one tab that
            // never held a session, never a session. Capturing
            // terminates nothing; it is a read.
            TabView::Session(s) => TabRef::Session {
                session: s.session?,
                profile: s.profile_key.clone()?,
                recovery: self.session_recovery(s),
            },
            // **A run view is not reopened.** It is a live reading of
            // a run through the gateway, and a tab strip that brought
            // one back would be asking a worker about a run before
            // anybody had asked to look at it. The run itself is
            // durable and is reopened from wherever it was reached,
            // and so is a result: it is reopened from its run.
            TabView::Run(_) | TabView::Result(_) => return None,
            // **A canvas persists as a document**, like a page: the
            // settings format does not change, and the kind is read
            // again from the manifest when it comes back.
            TabView::Canvas(c) => {
                TabRef::document(t.document.unwrap_or(c.document()), t.home_workspace())
            }
            // **What the tab is over, by preference.** `Tab.document`
            // is the manifest's id and survives the round trip; the
            // `NodeId` under it is a node in *that document's* tree,
            // which the next launch will not have opened and cannot
            // resolve. The node spelling stays for a workspace with no
            // manifest — every workspace in the editor's own suite.
            //
            // A leaf with neither is nothing this build can reopen, so
            // it comes back as a welcome tab rather than as a slot that
            // parses to nothing and silently shortens the list.
            TabView::Leaf(l) => match t.document {
                Some(id) => TabRef::document(id, t.home_workspace()),
                None => match View::document_root(l) {
                    Some(id) => TabRef::Node(id),
                    None => TabRef::Welcome,
                },
            },
        })
    }

    /// A main tab's saved token. **A tab with no saved form comes back as a
    /// Welcome tab** where it always has (Settings, Feedback, Home — until
    /// SP-7 Settings *was* a `WelcomeView` with a flag set); a diff, Socials
    /// and a run's pages are not part of the session at all.
    fn saved_main_tabs(&self, tabs: &koru_app_shell::TabStrip<Tab>) -> Vec<(TabRef, f32)> {
        tabs.iter()
            .filter(|t| !t.is_pinned())
            .filter_map(|t| {
                let which = self.saved_tab_ref(t).or_else(|| {
                    matches!(
                        t.view,
                        TabView::Settings(_) | TabView::Feedback(_) | TabView::Home(_)
                    )
                    .then_some(TabRef::Welcome)
                })?;
                Some((which, t.view.scroll()))
            })
            .collect()
    }

    /// The pins, and which of them is showing **by its place in the saved
    /// list**. A pin with no saved form stays pinned for the session and is
    /// not written, so the showing index is counted over the pins that are.
    fn saved_pins(
        &self,
        tabs: &koru_app_shell::TabStrip<Tab>,
    ) -> (Vec<PinnedTabRef>, Option<usize>) {
        let showing = tabs.pinned_showing();
        let mut saved_showing = None;
        let mut pins = Vec::new();
        for (i, t) in tabs.iter().filter(|t| t.is_pinned()).enumerate() {
            let Some(pin) = self.pinned_tab_ref(t) else {
                continue;
            };
            if showing == Some(i) {
                saved_showing = Some(pins.len());
            }
            pins.push(pin);
        }
        (pins, saved_showing)
    }

    /// What a pinned tab saves: the token it would be saved as unpinned, and
    /// the workspace it was pinned in.
    fn pinned_tab_ref(&self, tab: &Tab) -> Option<PinnedTabRef> {
        Some(PinnedTabRef {
            target: self.saved_tab_ref(tab)?,
            workspace: tab.pinned?,
            scroll: tab.view.scroll(),
            show_title: tab.show_title,
        })
    }

    fn pane_tree_state(&self) -> PaneTreeState {
        fn group_state(view: &MainView, group: &TabGroup) -> PaneNodeState {
            let main = view.saved_main_tabs(group.tabs());
            let (pinned, pinned_showing) = view.saved_pins(group.tabs());
            let active = match pinned_showing {
                Some(i) => GroupActiveState::Pinned(i),
                None => GroupActiveState::Main(group.tabs().main_active()),
            };
            PaneNodeState::Group {
                main,
                pinned,
                active,
            }
        }
        fn node_state(view: &MainView, node: &PaneNode) -> PaneNodeState {
            match node {
                PaneNode::Group(g) => group_state(view, g),
                PaneNode::Split {
                    axis,
                    ratio,
                    children,
                } => PaneNodeState::Split {
                    axis: match axis {
                        SplitAxis::Horizontal => SplitAxisState::Horizontal,
                        SplitAxis::Vertical => SplitAxisState::Vertical,
                    },
                    ratio: *ratio,
                    left: Box::new(node_state(view, &children[0])),
                    right: Box::new(node_state(view, &children[1])),
                },
            }
        }
        PaneTreeState {
            root: node_state(self, self.panes.root()),
            focused: self
                .panes
                .focused_path()
                .as_bytes()
                .iter()
                .copied()
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;

/// A stored Tasks target, in the rail's vocabulary.
///
/// The two spellings meet **here and nowhere else**: `koru-workspace-host` is
/// below the rail (`panes::explore::rail`) and carries the store's five words, this crate has the
/// rail's enum, and one function each way is what keeps a third spelling from
/// appearing the first time somebody needs the other direction. A token this
/// build does not know reopens on Today rather than dropping the tab, because a
/// Tasks tab is a place and every place in this app has a first page.
fn rail_target(stored: koru_workspace_host::TasksTabRef) -> RailTarget {
    use koru_workspace_host::TasksTabRef;
    match stored {
        TasksTabRef::List(id) => RailTarget::List(id),
        TasksTabRef::Reminders => RailTarget::Reminders,
        TasksTabRef::Bucket("next") => RailTarget::Bucket(Bucket::Next),
        TasksTabRef::Bucket("upcoming") => RailTarget::Bucket(Bucket::Upcoming),
        TasksTabRef::Bucket("later") => RailTarget::Bucket(Bucket::Later),
        TasksTabRef::Bucket("completed") => RailTarget::Bucket(Bucket::Completed),
        TasksTabRef::Bucket(_) => RailTarget::Bucket(Bucket::Today),
    }
}

/// [`rail_target`]'s other direction.
fn tasks_tab_ref(target: RailTarget) -> koru_workspace_host::TasksTabRef {
    use koru_workspace_host::TasksTabRef;
    match target {
        RailTarget::List(id) => TasksTabRef::List(id),
        RailTarget::Reminders => TasksTabRef::Reminders,
        RailTarget::Bucket(bucket) => TasksTabRef::Bucket(match bucket {
            Bucket::Today => "today",
            Bucket::Next => "next",
            Bucket::Upcoming => "upcoming",
            Bucket::Later => "later",
            Bucket::Completed => "completed",
        }),
    }
}

/// The saved word for a chat tab: the chat it is over, or `None` for a new
/// chat whose first message has not been sent — the service holds nothing
/// to reopen — which the caller then skips (ADR-483-chattabrestore). A tab
/// still opening a chat by link names that chat, so a restore quit before
/// the chat has loaded loses nothing.
fn chat_tab_ref(view: &crate::ai_chat_view::AiChatView) -> Option<TabRef> {
    view.chat_id().map(|chat| TabRef::Chat(chat.as_uuid()))
}

/// A saved chat tab, opened back onto its chat (ADR-483-chattabrestore).
///
/// **Nothing here waits on the AI service**: the tab starts its surface's
/// reader thread and asks for the chat, and the answer lands on a later pass.
/// A chat deleted since is answered as missing, and the tab becomes a new
/// chat (`koru_ai_ui`'s boot). `None` in a build that hides Koru AI, which
/// drops the tab as a hidden surface's tab is dropped.
fn restored_chat_tab(chat: uuid::Uuid) -> Option<Tab> {
    koru_features::ai().then(|| Tab::ai_chat_existing(koru_ai_ui::ChatId::from_uuid(chat)))
}

/// Which token a tab over a private workspace writes: the release notes or
/// the Sync settings page. **None for the sample document**, which is not
/// saved: it is a view of the Settings tab, and that comes back as Welcome —
/// a sample with no Settings beside it would be a page nobody asked for.
fn private_page_ref(tab: &Tab) -> Option<TabRef> {
    use super::tab::PrivatePage;
    match tab.whats_new.as_ref().map(|n| n.page) {
        Some(PrivatePage::SyncSettings) => Some(TabRef::SyncSettings),
        Some(PrivatePage::SampleDocument) => None,
        _ => Some(TabRef::WhatsNew),
    }
}
