// SPDX-License-Identifier: MPL-2.0

//! Read, verify and decode an incoming Profile without blocking its window.

use super::*;
use crate::profile_wait::WaitAttempt;
use koru_workspace::fromstore::StoreLaunch;

pub(super) struct Opening {
    pub attempt: WaitAttempt,
    began: Instant,
    origin: Option<profile_wait::Place>,
    removed_profile: Option<koru_core::ProfileId>,
    answer: mpsc::Receiver<koru_store_core::errors::Result<StoreLaunch>>,
}

impl<H: Host> App<H> {
    pub(super) fn begin_profile_switch(
        &mut self,
        source: WindowId,
        profile: koru_core::ProfileId,
        wanted: Option<WorkspaceId>,
    ) {
        self.begin_profile_switch_inner(source, profile, wanted, None);
    }

    pub(super) fn begin_profile_switch_after_removal(
        &mut self,
        source: WindowId,
        profile: koru_core::ProfileId,
        removed_profile: koru_core::ProfileId,
    ) {
        self.begin_profile_switch_inner(source, profile, None, Some(removed_profile));
    }

    fn begin_profile_switch_inner(
        &mut self,
        source: WindowId,
        profile: koru_core::ProfileId,
        wanted: Option<WorkspaceId>,
        removed_profile: Option<koru_core::ProfileId>,
    ) {
        if !self.panes.contains_key(&source) {
            return;
        }
        if self.runtime.profile.id() == Some(profile) {
            if self.profile_opening.is_some() {
                self.clear_profile_waits();
            }
            if let Some(workspace) = wanted {
                self.switch_workspace(workspace);
            }
            return;
        }
        let origin = if removed_profile.is_some() {
            // Cancel can choose another Profile or close the window, but must
            // never return to content whose removal already committed.
            None
        } else if self.runtime.live_workspace {
            self.runtime
                .profile
                .id()
                .map(|profile| profile_wait::Place {
                    profile,
                    workspace: self.runtime.current,
                })
        } else {
            self.profile_waits.get(source).and_then(|wait| wait.origin)
        };
        self.clear_profile_waits();
        let attempt = self.profile_waits.begin(source, profile, origin);
        if let Some(waiting) = self.profile_waits.opening(attempt)
            && let Some(pane) = self.panes.get_mut(&source)
        {
            pane.view.set_profile_wait(Some(waiting));
            pane.window.request_redraw();
        }
        let (reply, answer) = mpsc::channel();
        let root = self.gateway_root.clone();
        let wake = self.wake_proxy.clone();
        let began = Instant::now();
        match std::thread::Builder::new()
            .name("profile-open".into())
            .spawn(move || {
                let result = koru_workspace::fromstore::open_from_store_profile_preferring_empty(
                    koru_workspace::fromstore::segment(),
                    root.as_deref(),
                    profile,
                    wanted,
                );
                let _ = reply.send(result);
                let _ = wake.send_event(Wake::Workspaces);
            }) {
            Ok(_) => {
                self.profile_opening = Some(Opening {
                    attempt,
                    began,
                    origin,
                    removed_profile,
                    answer,
                })
            }
            Err(error) => {
                self.clear_profile_waits();
                self.report(
                    ErrorReport::new("Couldn't switch profiles", "Nothing was changed.")
                        .with_details(error),
                );
            }
        }
    }

    pub(super) fn service_profile_opening(&mut self) {
        let Some(opening) = self.profile_opening.as_ref() else {
            return;
        };
        // Retire immediately after navigation or closing, without waiting for
        // a network timeout. The worker may finish but owns no navigation.
        if !self.profile_waits.is_current(opening.attempt)
            || !self.panes.contains_key(&opening.attempt.window)
        {
            self.profile_opening = None;
            return;
        }
        let result = match opening.answer.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(koru_store_core::Error::bare(
                koru_store_core::Code::StorageFailed,
            )),
        };
        let opening = self.profile_opening.take().unwrap();
        match result {
            Ok(StoreLaunch::Open(opened))
                if opened.profile.id() == Some(opening.attempt.profile) =>
            {
                self.adopt_opened_profile(
                    opened,
                    opening.removed_profile.is_none(),
                    opening.began,
                    opening.origin,
                );
                if self.runtime.profile_id() == opening.attempt.profile
                    && let Some(removed) = opening.removed_profile
                    && self.hosted_profile_removals.contains_key(&removed)
                {
                    self.acknowledge_profile_cleanup(
                        removed,
                        koru_agent_wire::ProfileCleanupStage::NavigationSettled,
                    );
                    self.hosted_profile_removals.remove(&removed);
                }
            }
            Ok(_) => {
                self.clear_profile_waits();
                self.report(ErrorReport::new(
                    "Couldn't switch profiles",
                    "The selected profile is no longer available. Nothing was changed.",
                ));
            }
            Err(error) => {
                self.clear_profile_waits();
                self.report(
                    ErrorReport::new("Couldn't switch profiles", "Nothing was changed.")
                        .with_details(error),
                );
            }
        }
    }
}
