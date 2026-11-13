// SPDX-License-Identifier: MPL-2.0

//! Read, verify and decode an incoming Profile without blocking its window.

use super::*;
use crate::profile_wait::WaitAttempt;
use koru_workspace::fromstore::StoreLaunch;
use std::sync::{Arc, Mutex};

pub(super) struct Opening {
    pub attempt: WaitAttempt,
    began: Instant,
    origin: Option<profile_wait::Place>,
    removed_profile: Option<koru_core::ProfileId>,
    answer: Arc<Mutex<Option<koru_store_core::errors::Result<StoreLaunch>>>>,
    ready: mpsc::Receiver<()>,
    _release: mpsc::Sender<()>,
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
        if let Err(reason) = self.prepare_profile_replacement() {
            self.report(ErrorReport::new("Couldn't switch profiles", reason));
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
        let answer = Arc::new(Mutex::new(None));
        let output = Arc::clone(&answer);
        let (reply, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
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
                publish_prepared(result, output, reply, released, || {
                    let _ = wake.send_event(Wake::Workspaces);
                });
            }) {
            Ok(_) => {
                self.profile_opening = Some(Opening {
                    attempt,
                    began,
                    origin,
                    removed_profile,
                    answer,
                    ready,
                    _release: release,
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
        match opening.ready.try_recv() {
            Ok(()) => {}
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {}
        };
        let opening = self.profile_opening.take().unwrap();
        // Accepted writes can arrive while the incoming profile is opening.
        // Recheck and save BEFORE taking its result from worker ownership.
        if let Err(reason) = self.prepare_profile_replacement() {
            self.clear_profile_waits();
            self.report(ErrorReport::new("Couldn't switch profiles", reason));
            return;
        }
        let wrong_profile = opening.answer.lock().unwrap().as_ref().is_some_and(|result| {
            matches!(result, Ok(StoreLaunch::Open(opened)) if opened.profile.id() != Some(opening.attempt.profile))
        });
        if wrong_profile {
            self.clear_profile_waits();
            self.report(ErrorReport::new(
                "Couldn't switch profiles",
                "The selected profile is no longer available. Nothing was changed.",
            ));
            return;
        }
        let result = opening.answer.lock().unwrap().take().unwrap_or_else(|| {
            Err(koru_store_core::Error::bare(
                koru_store_core::Code::StorageFailed,
            ))
        });
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

    /// Both synchronous and asynchronous adopters check this immediately
    /// before accepting incoming ownership, on the same event-loop turn.
    pub(super) fn prepare_profile_replacement(&mut self) -> Result<(), &'static str> {
        if !self.prepare_private_creation_close(None) {
            return Err(koru_i18n::t(
                crate::accessibility::ui_lang(),
                koru_i18n::Key::DocumentReplacementPending,
            ));
        }
        self.collect_task_commands();
        if self.task_queue.has_pending() {
            return Err(pending_tasks_reason());
        }
        self.runtime
            .workspace
            .prepare_document_replacement()
            .map_err(replacement_reason)
    }
}

pub(super) fn pending_tasks_reason() -> &'static str {
    koru_i18n::t(
        crate::accessibility::ui_lang(),
        koru_i18n::Key::TaskCommandsPending,
    )
}

pub(super) fn replacement_reason(reason: koru_workspace::DocumentReplacementError) -> &'static str {
    koru_i18n::t(
        crate::accessibility::ui_lang(),
        match reason {
            koru_workspace::DocumentReplacementError::Pending => {
                koru_i18n::Key::DocumentReplacementPending
            }
            koru_workspace::DocumentReplacementError::Unsafe => {
                koru_i18n::Key::DocumentReplacementUnsafe
            }
        },
    )
}

pub(super) fn saving_reason(why: &str, lang: koru_i18n::Lang) -> &str {
    match why {
        koru_workspace::Unconfirmed::NOT_BUFFERED => {
            koru_i18n::t(lang, koru_i18n::Key::DocumentNotBuffered)
        }
        koru_workspace::Unconfirmed::SAVING_PENDING => {
            koru_i18n::t(lang, koru_i18n::Key::DocumentSavingPending)
        }
        _ => why,
    }
}

/// Keep a prepared result on its worker until the destination takes it or
/// cancels. The generic value lets the ownership boundary use a drop probe in
/// tests without constructing a Store, Profile, or native window.
fn publish_prepared<T>(
    value: T,
    output: Arc<Mutex<Option<T>>>,
    ready: mpsc::Sender<()>,
    released: mpsc::Receiver<()>,
    wake: impl FnOnce(),
) {
    *output.lock().unwrap() = Some(value);
    let _ = ready.send(());
    wake();
    let _ = released.recv();
    output.lock().unwrap().take();
}

#[cfg(test)]
mod tests;
