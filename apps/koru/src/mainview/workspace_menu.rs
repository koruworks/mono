// SPDX-License-Identifier: MPL-2.0

//! **The workspace menu, and what the Manage tab's Workspaces ask of the
//! window.**
//!
//! One menu for a workspace, wherever it is opened — a Profile page's row
//! `•••`, or a right-click on the row — built by
//! [`context_menu::workspace_items`] and enabled here. The Manage tab is where it
//! opens (ADR-449-onemanagetab); the switcher's row menus went with the
//! drill-in switcher.
//!
//! **What a row is, and what may be done to it, is the Manage tab's
//! controller's** (`koru_workspaces_ui::WorkspacesController`): it holds the
//! including-deleted catalog of every Profile, this window's two
//! authorizations and the confirmations. The window adds what only it knows —
//! which workspace is open here, and which Profile — and keeps a fallback to
//! its own lists for a workspace the controller has not heard of yet.
//!
//! Every verb ends up in one of three places: the controller (pin, reorder,
//! move, archive, delete, restore — the Gateway calls it is entitled to
//! make), the shell (open, a new window, export), or a window modal (rename,
//! icon and color).

use super::*;
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
    /// The Profile this window has open.
    pub(super) fn current_profile_id(&self) -> Option<ProfileId> {
        self.pane_context
            .as_ref()
            .map(|(_, profile, _, _)| *profile)
    }

    /// The Manage tab, wherever it is in the window.
    pub(super) fn manage_view(&self) -> Option<&ManageView> {
        self.panes
            .iter_groups()
            .flat_map(|(_, group)| group.tabs().iter())
            .find_map(|tab| match &tab.view {
                TabView::Manage(view) => Some(view),
                _ => None,
            })
    }

    pub(super) fn manage_view_mut(&mut self) -> Option<&mut ManageView> {
        self.panes
            .iter_groups_mut()
            .flat_map(|(_, group)| group.tabs_mut().iter_mut())
            .find_map(|tab| match &mut tab.view {
                TabView::Manage(view) => Some(view),
                _ => None,
            })
    }

    /// The Manage tab's Workspaces controller, when there is a Manage tab.
    fn manage_workspaces(&self) -> Option<&WorkspacesController> {
        self.manage_view().map(ManageView::workspaces)
    }

    /// Run `call` on the Manage tab's controller for a workspace it lists.
    /// `None` when there is no Manage tab or it has not heard of `id`.
    fn act_on_listed<R>(
        &mut self,
        id: WorkspaceId,
        call: impl FnOnce(&mut WorkspacesController, koru_store_core::ids::WorkspaceId) -> R,
    ) -> Option<R> {
        let wid = catalog_workspace(id);
        let view = self.manage_view_mut()?;
        view.workspaces().row(wid)?;
        Some(view.with_workspaces(|c| call(c, wid)))
    }

    /// One workspace, by id: the Manage tab's catalog first (every Profile's,
    /// archived and trashed included), then the open Profile's list, then the
    /// switcher's per-Profile catalog.
    pub(super) fn workspace_facts(&self, id: WorkspaceId) -> Option<WorkspaceFacts> {
        if let Some(row) = self
            .manage_workspaces()
            .and_then(|c| c.row(catalog_workspace(id)))
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
        let listed = self
            .manage_workspaces()
            .and_then(|c| c.row(wid).map(|row| (c, row)));
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
            MenuAction::Pin | MenuAction::Unpin => ask(Action::TogglePinned(wid)),
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
            .manage_workspaces()
            .map(|c| {
                c.destinations(catalog_workspace(id))
                    .into_iter()
                    .map(|profile| (app_profile(profile.id), profile.name))
                    .collect()
            })
            .unwrap_or_default();
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
                    None => self.ask_pane(PaneRequest::PinWorkspace(id, pinned)),
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
            // puts its confirmation up on the Manage tab.
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
        if self.manage_view().is_none() {
            let owner = self
                .workspace_facts(id)
                .and_then(|facts| facts.owner)
                .or(self.current_profile_id());
            let Some(owner) = owner else {
                return false;
            };
            self.open_manage(crate::manage::Page::Profile(owner));
        }
        let wid = catalog_workspace(id);
        let Some(view) = self.manage_view_mut() else {
            return false;
        };
        let carried = view.with_workspaces(|c| match action {
            MenuAction::Archive => c.archive_workspace(wid, true),
            MenuAction::Unarchive => c.archive_workspace(wid, false),
            MenuAction::Delete => c.delete_workspace(wid),
            MenuAction::RestoreWorkspace => c.restore_workspace(wid),
            _ => false,
        });
        self.arm_workspaces_poll();
        carried
    }

    /// Open a workspace in this window: here when it is the open Profile's,
    /// switching Profile first when it is another's.
    pub(super) fn open_workspace(&mut self, id: WorkspaceId) -> bool {
        let owner = self.workspace_facts(id).and_then(|facts| facts.owner);
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

    /// Start the Workspaces tick while the Manage tab is in front.
    pub(super) fn arm_workspaces_poll(&mut self) {
        if matches!(self.tabs().active_tab().view, TabView::Manage(_)) {
            self.workspaces_poll_due = Some(Instant::now() + WORKSPACES_POLL);
        }
    }

    /// Take what the Manage tab's controller's link answered, while the tab
    /// is in front.
    pub(super) fn poll_manage_workspaces(&mut self) -> bool {
        match &mut self.tabs_mut().active_mut().view {
            TabView::Manage(view) => view.poll_workspaces(),
            _ => false,
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
