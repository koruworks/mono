// SPDX-License-Identifier: MPL-2.0

//! Workspace and profile scope transitions for [`super::SearchSurface`].

use koru_store_core::ids::{ProfileId, WorkspaceId};

use super::{SearchSurface, remember_session_profile};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SearchScope {
    Global,
    Workspace {
        profile: ProfileId,
        workspace: WorkspaceId,
        name: String,
    },
}

impl SearchSurface {
    pub fn resume_in_workspace(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        name: String,
    ) {
        let selection = self.selection_snapshot();
        self.selected_profile = Some(profile);
        remember_session_profile(Some(profile));
        self.scope = SearchScope::Workspace {
            profile,
            workspace,
            name,
        };
        self.scope_home = None;
        self.archived_only = false;
        self.selection_restore =
            selection.filter(|selection| selection.context == self.selection_context());
        self.refresh();
    }

    /// Promote a workspace search to the active profile without discarding what
    /// was typed. Both the no-results affordance and Ctrl+Space use this exact
    /// transition so their result and filter state cannot drift apart.
    pub(super) fn switch_to_profile_scope(&mut self) -> bool {
        let SearchScope::Workspace { profile, .. } = &self.scope else {
            return false;
        };
        // Where to come back to: the scope the search was opened in.
        self.scope_home = Some(self.scope.clone());
        self.selected_profile = Some(*profile);
        remember_session_profile(Some(*profile));
        self.scope = SearchScope::Global;
        self.archived_only = false;
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.selected_recent_query = None;
        self.clear_selection_memory();
        self.filters.clear();
        self.menu = super::SearchMenu::None;
        self.refresh();
        true
    }

    /// Back from every workspace to the one the search was opened in. `false`
    /// when it was never in one — a search opened wide has nowhere to narrow
    /// to.
    pub(super) fn switch_to_workspace_scope(&mut self) -> bool {
        if !matches!(self.scope, SearchScope::Global) {
            return false;
        }
        let Some(home) = self.scope_home.clone() else {
            return false;
        };
        self.scope = home;
        self.archived_only = false;
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.selected_recent_query = None;
        self.clear_selection_memory();
        self.filters.clear();
        self.menu = super::SearchMenu::None;
        self.refresh();
        true
    }

    /// **Ctrl+Space, and the footer's scope chip**: this workspace only, or
    /// every workspace, and back.
    pub(super) fn toggle_scope(&mut self) -> bool {
        self.switch_to_profile_scope() || self.switch_to_workspace_scope()
    }

    pub fn workspace_scope(&self) -> Option<(ProfileId, WorkspaceId)> {
        match &self.scope {
            SearchScope::Global => None,
            SearchScope::Workspace {
                profile, workspace, ..
            } => Some((*profile, *workspace)),
        }
    }
}
