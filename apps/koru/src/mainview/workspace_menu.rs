// SPDX-License-Identifier: MPL-2.0

//! **The workspace menu, and what the Manage tab's Workspaces ask of the
//! window.**
//!
//! One action menu for a workspace, reached from the current-workspace menu,
//! a Profile page's row or a right-click. It is built by
//! [`context_menu::workspace_items`] and enabled here; opening Manage is not a
//! prerequisite for its actions.
//!
//! The window's `WorkspacesController` holds the including-deleted catalog,
//! authorizations, requests and confirmations. Manage, the rail and menus
//! project that same controller. The window adds its current workspace and
//! Profile, retaining pushed-list facts until the catalog has answered.
//!
//! Every verb ends up in one of three places: the controller (pin, reorder,
//! move, archive, delete, restore — the Gateway calls it is entitled to
//! make), the shell (open, a new window, export), or a window modal (rename,
//! icon and color).

use super::*;
#[cfg(test)]
use crate::manage::ManageView;
use crate::manage::workspaces::{app_profile, catalog_profile, catalog_workspace};
use koru_workspaces_ui::{Action, WorkspacesController};

/// Why *Open in New Window* is dimmed for another Profile's workspace (maintainer
/// decision 11): a window shows one Profile, and a second window of another
/// Profile is a switch the person has not asked for.
pub(super) const SWITCH_FIRST: &str = "Switch to this profile first";
const ALREADY_OPEN: &str = "This workspace is already open";
const AT_TOP: &str = "Already at the top";
const AT_BOTTOM: &str = "Already at the bottom";

/// What the window knows about one workspace, from wherever it is listed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WorkspaceFacts {
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    /// The Profile it belongs to, when known.
    pub owner: Option<ProfileId>,
    pub pinned: bool,
    pub archived: bool,
    pub deleted: bool,
}

impl MainView {
    /// Catalog furniture is available even when no navigator content has
    /// been drawn. Opening a rail menu must not depend on a document rebuild.
    pub(super) fn refresh_explore_context(&mut self) {
        self.explore
            .set_workspace_controller(self.workspace_controller.clone());
        if self.context_stale
            && let Some((profiles, mut current_profile, mut workspaces, mut current)) =
                self.pane_context.clone()
        {
            let rail_focus = self.focused_rail_identity();
            if let Some(wait) = &self.profile_wait.waiting {
                if wait.attempt.profile != current_profile {
                    workspaces.clear();
                }
                current_profile = wait.attempt.profile;
                current = None;
            }
            let controller = self.workspace_controller.borrow();
            if let Some(rows) = controller.catalog_rows() {
                let mut projected: Vec<_> = rows
                    .iter()
                    .filter(|row| {
                        row.owner == catalog_profile(current_profile) && row.verified_live()
                    })
                    .map(|row| {
                        let id = crate::manage::workspaces::app_workspace(row.id);
                        let mut entry = workspaces
                            .iter()
                            .find(|entry| entry.id == id)
                            .cloned()
                            .unwrap_or_else(|| WorkspaceEntry::new(id, &row.name, 0));
                        entry.name = row.name.clone();
                        entry.icon = row.icon.clone();
                        entry.color = row.color.clone();
                        entry.pinned = controller.effective_pinned(row);
                        entry.archived = row.archived;
                        entry.deleted_at = None;
                        entry.order = controller
                            .pending_order(row.owner)
                            .and_then(|ids| ids.iter().position(|id| *id == row.id))
                            .map_or(row.position, |position| position as i64);
                        entry
                    })
                    .collect();
                projected.sort_by_key(|entry| (entry.order, entry.id));
                workspaces = projected;
            }
            self.context_stale = false;
            self.explore
                .set_context(profiles, current_profile, workspaces, current);
            drop(controller);
            self.retain_rail_focus(rail_focus);
        }
    }

    /// The Profile this window has open.
    pub(super) fn current_profile_id(&self) -> Option<ProfileId> {
        if let Some(wait) = &self.profile_wait.waiting {
            return Some(wait.attempt.profile);
        }
        self.pane_context
            .as_ref()
            .map(|(_, profile, _, _)| *profile)
    }

    /// The Manage tab, wherever it is in the window.
    #[cfg(test)]
    pub(super) fn manage_view(&self) -> Option<&ManageView> {
        self.panes
            .iter_groups()
            .flat_map(|(_, group)| group.tabs().iter())
            .find_map(|tab| match &tab.view {
                TabView::Manage(view) => Some(view),
                _ => None,
            })
    }

    /// Run `call` on the window controller for a workspace it lists.
    /// `None` when the catalog has not heard of `id`.
    pub(super) fn act_on_listed<R>(
        &mut self,
        id: WorkspaceId,
        call: impl FnOnce(&mut WorkspacesController, koru_store_core::ids::WorkspaceId) -> R,
    ) -> Option<R> {
        let wid = catalog_workspace(id);
        let result = {
            let mut controller = self.workspace_controller.borrow_mut();
            controller.row(wid)?;
            call(&mut controller, wid)
        };
        self.refresh_manage_workspaces();
        Some(result)
    }

    /// One workspace, by id: the Manage tab's catalog first (every Profile's,
    /// archived and trashed included), then the open Profile's list, then the
    /// switcher's per-Profile catalog.
    pub(super) fn workspace_facts(&self, id: WorkspaceId) -> Option<WorkspaceFacts> {
        if let Some(row) = self
            .workspace_controller
            .borrow()
            .row(catalog_workspace(id))
        {
            return Some(WorkspaceFacts {
                name: row.name.clone(),
                icon: row.icon.clone(),
                color: row.color.clone(),
                owner: Some(app_profile(row.owner)),
                pinned: row.pinned,
                archived: row.archived,
                deleted: row.deleted,
            });
        }
        let entry = |owner: Option<ProfileId>, w: &WorkspaceEntry| WorkspaceFacts {
            name: w.name.clone(),
            icon: w.icon.clone(),
            color: w.color.clone(),
            owner,
            pinned: w.pinned,
            archived: w.archived,
            deleted: w.deleted_at.is_some(),
        };
        if let Some(w) = self.workspaces().iter().find(|w| w.id == id) {
            return Some(entry(self.current_profile_id(), w));
        }
        self.explore
            .catalog_workspace(id)
            .map(|(owner, w)| entry(Some(owner), w))
    }

    /// **Why a workspace menu row cannot be chosen**, or `None` when it can.
    /// `Some("")` is dimmed with nothing to say — the workspace has left
    /// every list since the menu opened.
    ///
    /// The window's own reasons first — the open workspace cannot be opened
    /// again, and another Profile's cannot open in a second window (decision
    /// 11) — then the controller's, in its order: nothing answering, this
    /// window's permission, the trash, the arithmetic.
    pub(super) fn workspace_refusal(&self, id: WorkspaceId, action: MenuAction) -> Option<String> {
        let Some(facts) = self.workspace_facts(id) else {
            return Some(String::new());
        };
        let current = self.current_workspace() == Some(id);
        let elsewhere = facts
            .owner
            .is_some_and(|owner| Some(owner) != self.current_profile_id());
        let wid = catalog_workspace(id);
        let controller = self.workspace_controller.borrow();
        let listed = controller.row(wid).map(|row| (&*controller, row));
        let ask = |action: Action| listed.and_then(|(c, row)| c.reason_for(&action, row));
        let unlisted = || listed.is_none().then(String::new);
        match action {
            MenuAction::Open if current => Some(ALREADY_OPEN.to_owned()),
            MenuAction::Open => ask(Action::Open(wid)),
            MenuAction::OpenInNewWindow if current => Some(ALREADY_OPEN.to_owned()),
            MenuAction::OpenInNewWindow if elsewhere => Some(SWITCH_FIRST.to_owned()),
            MenuAction::OpenInNewWindow => ask(Action::OpenInNewWindow(wid)),
            MenuAction::Rename => ask(Action::Rename(wid)),
            MenuAction::ChangeIconAndColor => ask(Action::Appearance(wid)),
            MenuAction::Pin | MenuAction::Unpin => ask(Action::TogglePinned(wid)).or_else(unlisted),
            MenuAction::MoveWorkspaceUp => {
                ask(Action::MoveUp(wid)).or_else(unlisted).or_else(|| {
                    listed
                        .is_some_and(|(c, _)| !c.can_step(wid, -1))
                        .then(|| AT_TOP.to_owned())
                })
            }
            MenuAction::MoveWorkspaceDown => {
                ask(Action::MoveDown(wid)).or_else(unlisted).or_else(|| {
                    listed
                        .is_some_and(|(c, _)| !c.can_step(wid, 1))
                        .then(|| AT_BOTTOM.to_owned())
                })
            }
            MenuAction::MoveToProfile | MenuAction::MoveWorkspaceTo(_) => {
                ask(Action::Migrate(wid)).or_else(unlisted)
            }
            MenuAction::CopyWorkspaceId => None,
            MenuAction::Export => ask(Action::Export(wid)),
            // A workspace the controller has not heard of keeps the window's
            // old rule: the lifecycle verbs follow the trash flag.
            MenuAction::Archive => match listed {
                Some(_) => ask(Action::Archive(wid, true)),
                None => facts.deleted.then(String::new),
            },
            MenuAction::Unarchive => match listed {
                Some(_) => ask(Action::Archive(wid, false)),
                None => facts.deleted.then(String::new),
            },
            MenuAction::Delete => match listed {
                Some(_) => ask(Action::SoftDelete(wid)),
                None => facts.deleted.then(String::new),
            },
            MenuAction::RestoreWorkspace => match listed {
                Some(_) => ask(Action::Restore(wid)),
                None => (!facts.deleted).then(String::new),
            },
            _ => Some(String::new()),
        }
    }

    /// Open the workspace menu — a Manage row's `•••` (`from_button`, its
    /// top-right corner under the button) or a right-click at the pointer.
    ///
    /// The rows are fixed as it opens, and so is what the *Move to Profile ▸*
    /// flyout lists and the reason each dimmed row gives.
    pub(super) fn open_workspace_menu(
        &mut self,
        id: WorkspaceId,
        at: (f32, f32),
        from_button: bool,
    ) -> bool {
        self.explore.highlight_popover_workspace(id);
        let Some(facts) = self.workspace_facts(id) else {
            return false;
        };
        let items = context_menu::workspace_items(facts.pinned, facts.archived, facts.deleted);
        let destinations: Vec<(ProfileId, String)> = self
            .workspace_controller
            .borrow()
            .destinations(catalog_workspace(id))
            .into_iter()
            .map(|profile| (app_profile(profile.id), profile.name))
            .collect();
        let refusals: Vec<(MenuAction, String)> = items
            .iter()
            .copied()
            .chain(
                destinations
                    .iter()
                    .map(|(profile, _)| MenuAction::MoveWorkspaceTo(*profile)),
            )
            .filter(|action| *action != MenuAction::Divider)
            .filter_map(|action| {
                self.workspace_refusal(id, action)
                    .filter(|reason| !reason.is_empty())
                    .map(|reason| (action, reason))
            })
            .collect();
        context_menu::set_workspace_menu_snapshot(destinations, refusals);
        let align = if from_button {
            MenuAnchor::ButtonRight
        } else {
            MenuAnchor::Cursor
        };
        let mut m = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::Workspace(id),
            at,
            items,
            align,
        );
        let enabled = self.enabled_items(&m);
        m.step(&enabled, 1);
        self.menu = Some(m);
        true
    }

    /// Do what a workspace menu row means.
    ///
    /// The menu can hang over the who/where popover's page (a right-click on
    /// its row). A verb that leaves the list — opening, exporting, the
    /// lifecycle — shuts the page it was chosen from, the rule the heading's
    /// menu keeps (QUESTIONS.md E29); one that changes the list in place, or
    /// copies, leaves it up with the list redrawn. **Rename keeps it up
    /// too**: its field is drawn over the row it renames, and shutting the
    /// page took the row away before the field could appear (E41).
    pub(super) fn run_workspace_menu_action(
        &mut self,
        id: WorkspaceId,
        action: MenuAction,
    ) -> bool {
        let leaves_the_list = !matches!(
            action,
            MenuAction::Pin
                | MenuAction::Unpin
                | MenuAction::MoveWorkspaceUp
                | MenuAction::MoveWorkspaceDown
                | MenuAction::MoveToProfile
                | MenuAction::CopyWorkspaceId
                | MenuAction::Rename
        );
        let done = self.run_workspace_menu_verb(id, action);
        let shut = leaves_the_list && self.explore.close_popover();
        done | shut
    }

    fn run_workspace_menu_verb(&mut self, id: WorkspaceId, action: MenuAction) -> bool {
        match action {
            MenuAction::Open => self.open_workspace(id),
            MenuAction::OpenInNewWindow => self.ask_pane(PaneRequest::OpenInNewWindow(id)),
            // Through the controller where it lists the workspace — the
            // catalog's appearance route reaches any Profile's — and the
            // shell's route otherwise.
            MenuAction::Pin | MenuAction::Unpin => {
                let pinned = action == MenuAction::Pin;
                match self.act_on_listed(id, |c, wid| {
                    if c.row(wid).is_some_and(|row| row.pinned != pinned) {
                        c.perform(Action::TogglePinned(wid));
                    }
                }) {
                    Some(()) => true,
                    None => false,
                }
            }
            MenuAction::MoveWorkspaceUp => self
                .act_on_listed(id, |c, wid| c.perform(Action::MoveUp(wid)))
                .is_some(),
            MenuAction::MoveWorkspaceDown => self
                .act_on_listed(id, |c, wid| c.perform(Action::MoveDown(wid)))
                .is_some(),
            // A flyout's parent acts on nothing itself.
            MenuAction::MoveToProfile => false,
            // The controller asks the gateway what the move would close, then
            // puts its confirmation up over the window.
            MenuAction::MoveWorkspaceTo(profile) => {
                let moved = self
                    .act_on_listed(id, |c, wid| {
                        c.perform(Action::MigrateTo(wid, catalog_profile(profile)))
                    })
                    .is_some();
                self.arm_workspaces_poll();
                moved
            }
            MenuAction::CopyWorkspaceId => {
                koru_editor::clipboard::put_system_text(&catalog_workspace(id).to_storage());
                true
            }
            MenuAction::Export => self.ask_pane(PaneRequest::ExportWorkspace(id)),
            // The four lifecycle actions live in the Manage tab's controller,
            // which holds the vouched client and server-issued confirmation
            // (ADR-267). The shell's unscoped client cannot perform them.
            MenuAction::Archive
            | MenuAction::Unarchive
            | MenuAction::Delete
            | MenuAction::RestoreWorkspace => self.run_workspace_menu_lifecycle(id, action),
            MenuAction::Rename => self.start_workspace_rename(id),
            MenuAction::ChangeIconAndColor => self.start_workspace_icon(id),
            _ => false,
        }
    }

    /// Carry a workspace menu's lifecycle verb into the Manage tab's
    /// controller — opening Manage on the workspace's Profile when no Manage
    /// tab is there to carry it.
    pub(super) fn run_workspace_menu_lifecycle(
        &mut self,
        id: WorkspaceId,
        action: MenuAction,
    ) -> bool {
        let wid = catalog_workspace(id);
        let carried = {
            let mut c = self.workspace_controller.borrow_mut();
            match action {
                MenuAction::Archive => c.archive_workspace(wid, true),
                MenuAction::Unarchive => c.archive_workspace(wid, false),
                MenuAction::Delete => c.delete_workspace(wid),
                MenuAction::RestoreWorkspace => c.restore_workspace(wid),
                _ => false,
            }
        };
        self.arm_workspaces_poll();
        carried
    }

    /// Open a workspace in this window: here when it is the open Profile's,
    /// switching Profile first when it is another's.
    pub(super) fn open_workspace(&mut self, id: WorkspaceId) -> bool {
        if self.workspace_controller.borrow().catalog_rows().is_some()
            && self.workspace_controller.borrow().availability()
                != koru_workspaces_ui::Availability::Live
        {
            return false;
        }
        if self
            .workspace_controller
            .borrow()
            .catalog_rows()
            .is_some_and(|rows| {
                !rows
                    .iter()
                    .any(|row| row.id == catalog_workspace(id) && row.navigation_ready())
            })
        {
            return false;
        }
        let owner = self.workspace_facts(id).and_then(|facts| facts.owner);
        if owner.is_some_and(|owner| {
            self.workspace_controller
                .borrow()
                .profile_removal(catalog_profile(owner))
                .is_some()
        }) {
            return false;
        }
        match owner {
            Some(owner) if Some(owner) != self.current_profile_id() => {
                self.ask_pane(PaneRequest::SwitchToWorkspaceIn(owner, id))
            }
            _ => self.switch_workspace_here(id),
        }
    }

    /// *Manage workspaces…* and `Ctrl+Shift+O`: the Manage tab on `profile`'s
    /// page — the open Profile's when none is named — scrolled to its
    /// Workspaces.
    pub(super) fn open_manage_workspaces(&mut self, profile: Option<ProfileId>) -> bool {
        let Some(profile) = profile.or(self.current_profile_id()) else {
            return false;
        };
        self.open_manage(crate::manage::Page::Profile(profile));
        if let TabView::Manage(view) = &mut self.tabs_mut().active_mut().view {
            view.reveal_workspaces();
        }
        true
    }

    /// A bounded fallback while a command is awaiting its keyed answer.
    pub(super) fn arm_workspaces_poll(&mut self) {
        self.arm_workspaces_poll_at(Instant::now());
    }

    pub(super) fn arm_workspaces_poll_at(&mut self, now: Instant) {
        if self.workspace_controller.borrow().has_pending() {
            self.workspaces_poll_due
                .get_or_insert(now + WORKSPACES_POLL);
        } else {
            self.workspaces_poll_due = None;
        }
    }

    /// Drain answers for this window independently of the selected tab.
    pub(super) fn poll_manage_workspaces(&mut self) -> bool {
        let (changed, notice) = {
            let mut controller = self.workspace_controller.borrow_mut();
            controller.set_lang(crate::format::lang());
            let previous = controller.notice().cloned();
            let changed = controller.poll();
            let notice = (controller.notice() != previous.as_ref())
                .then(|| controller.notice().map(|notice| notice.text().to_owned()))
                .flatten();
            (changed, notice)
        };
        if let Some(notice) = notice {
            tracing::info!(target: "koru::workspaces", reason = %notice, "workspace operation needs attention");
            if self.menu.is_none() && !self.explore.popover_open() {
                self.show_notice(notice);
            }
        }
        if changed {
            self.refresh_manage_workspaces();
        }
        changed
    }

    fn refresh_manage_workspaces(&mut self) {
        self.context_stale = true;
        self.refresh_explore_context();
        let notice = self
            .workspace_controller
            .borrow()
            .notice()
            .map(|notice| notice.text().to_owned());
        self.explore.set_workspace_notice(notice);
        if let TabView::Manage(view) = &mut self.tabs_mut().active_mut().view {
            view.refresh_workspaces();
        }
    }

    /// *New workspace* on a Profile page: the name prompt, then the workspace
    /// made in that Profile — here when it is the open one.
    pub(super) fn new_workspace_for(&mut self, profile: ProfileId) -> bool {
        if Some(profile) == self.current_profile_id() {
            self.prompt_new_workspace()
        } else {
            self.prompt_new_workspace_in(profile)
        }
    }
}

#[cfg(test)]
mod tests;
