// SPDX-License-Identifier: MPL-2.0

//! The event loop only reads cached delivery and drains the window worker.

use super::*;
use crate::app::{App, Host, PaneRequest, View};
use crate::profile_runtime::ProfilePersistence;
use winit::event_loop::ActiveEventLoop;

impl<H: Host> App<H> {
    pub(in crate::app) fn clear_profile_waits(&mut self) {
        self.profile_opening = None;
        self.profile_waits.clear();
        for pane in self.panes.values_mut() {
            if pane.view.set_profile_wait(None) {
                pane.window.request_redraw();
            }
        }
    }

    pub(in crate::app) fn service_profile_waits(&mut self, event_loop: &ActiveEventLoop) {
        self.leave_retired_workspace();
        if self.profile_opening.is_some()
            || self.runtime.live_workspace
            || !matches!(self.runtime.profile_session, ProfilePersistence::Store)
        {
            return;
        }
        let Some(profile) = self.runtime.profile.id() else {
            return;
        };
        let Some(summary) = self
            .runtime
            .profile_catalog
            .iter()
            .find(|row| row.id == profile)
        else {
            return;
        };
        let account = self.host.account_state();
        let delivery = ProfileDelivery::read(
            koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            summary.account.as_deref(),
            Some(&account),
        );
        let windows: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(id, pane)| full_window(&pane.view).then_some(*id))
            .collect();
        for window in windows {
            // Startup and a newly opened full window have no outgoing place.
            // Explicit Profile switching has already installed its origin.
            if self.profile_waits.get(window).is_none() {
                self.profile_waits.begin(window, profile, None);
            }
            let local_content = self
                .panes
                .get(&window)
                .is_some_and(|pane| pane.view.profile_has_live_content(profile));
            let removing = self.profile_waits.observe_removal(
                window,
                self.panes
                    .get(&window)
                    .and_then(|pane| pane.view.profile_removal(profile)),
            );
            if !removing {
                self.profile_waits.observe(
                    window,
                    if local_content {
                        ProfileDelivery::NotApplicable
                    } else {
                        delivery.clone()
                    },
                );
            }
            if let Some((attempt, reply)) = self.profile_waits.request(window) {
                if let Some(pane) = self.panes.get_mut(&window) {
                    pane.view.ensure_live_workspace(
                        koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
                        koru_i18n::t(koru_i18n::current(), koru_i18n::Key::DefaultWorkspaceName)
                            .into(),
                        attempt.sequence,
                        reply,
                    );
                }
            }
            if let Some((attempt, workspace)) = self.profile_waits.poll(window) {
                self.serve_pane_request(
                    window,
                    PaneRequest::LandIn {
                        profile,
                        workspace,
                        attempt,
                    },
                    event_loop,
                );
                if self.runtime.live_workspace {
                    return;
                }
            }
            if let Some(wait) = self.profile_waits.get(window)
                && let Some(pane) = self.panes.get_mut(&window)
                && pane.view.set_profile_wait(Some(wait.presentation.clone()))
            {
                pane.window.request_redraw();
            }
        }
    }

    fn leave_retired_workspace(&mut self) {
        if self.profile_opening.is_some()
            || !self.runtime.live_workspace
            || !matches!(self.runtime.profile_session, ProfilePersistence::Store)
        {
            return;
        }
        let Some(profile) = self.runtime.profile.id() else {
            return;
        };
        let current = self.runtime.current;
        if !self.panes.values().any(|pane| {
            full_window(&pane.view)
                && (pane.view.workspace_retired(profile, current)
                    || pane.view.profile_removal(profile).flatten().is_some())
        }) {
            return;
        }
        // Drop every outgoing room before choosing. Catalog-only replacement
        // selection could reopen a durably removed neighbor during repair.
        if let Err(error) = self.show_empty_workspace() {
            self.report(crate::ErrorReport::new(
                "Couldn't leave the removed workspace",
                error,
            ));
            return;
        }
        for (window, pane) in &self.panes {
            if full_window(&pane.view) {
                self.profile_waits.begin_post_removal(*window, profile);
            }
        }
    }

    pub(in crate::app) fn cancel_profile_wait(
        &mut self,
        attempt: WaitAttempt,
        event_loop: &ActiveEventLoop,
    ) {
        if !self.profile_waits.is_current(attempt) {
            return;
        }
        let wait = self.profile_waits.leave(attempt.window).unwrap();
        if self
            .profile_opening
            .as_ref()
            .is_some_and(|opening| opening.attempt == attempt)
        {
            self.profile_opening = None;
        }
        if let Some(pane) = self.panes.get_mut(&attempt.window) {
            pane.view.set_profile_wait(None);
            pane.window.request_redraw();
        }
        if let Some(origin) = wait.origin {
            if self.runtime.profile.id() == Some(origin.profile)
                && self.runtime.live_workspace
                && self.runtime.current == origin.workspace
            {
                self.clear_profile_waits();
            } else {
                self.begin_profile_switch(attempt.window, origin.profile, Some(origin.workspace));
            }
            return;
        }
        // Store's Profile catalog is ordered by creation time then identity;
        // ProfileSummary projection preserves that order. Cancel itself never
        // creates a Profile or a workspace and never cancels Cloud delivery.
        if let Some(other) = self
            .runtime
            .profile_catalog
            .iter()
            .rev()
            .find(|row| row.id != attempt.profile)
        {
            let profile = other.id;
            let wanted = self
                .runtime
                .catalog_workspaces
                .iter()
                .filter(|row| {
                    row.owner.as_uuid() == profile.as_uuid()
                        && !row.archived
                        && row.deleted_at.is_none()
                })
                .max_by_key(|row| {
                    let id = WorkspaceId::from_uuid(row.id.as_uuid());
                    (
                        self.runtime.settings.opened_in(id).unwrap_or(0),
                        row.last_opened_at,
                        row.position,
                    )
                })
                .map(|row| WorkspaceId::from_uuid(row.id.as_uuid()));
            self.begin_profile_switch(attempt.window, profile, wanted);
        } else {
            self.close_pane(event_loop, attempt.window);
        }
    }
}
