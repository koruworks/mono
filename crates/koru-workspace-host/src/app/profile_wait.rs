// SPDX-License-Identifier: MPL-2.0

//! Availability attempts own receivers, so leaving a window drops its answer.
//! Neither Cloud observations nor catalog pushes can land a departed attempt.

mod host;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use koru_core::{ProfileId, WorkspaceId};
use koru_store_core::{errors::Result, service::EnsuredWorkspace};
use winit::window::WindowId;

use super::profile_delivery::{FailurePhase, ProfileDelivery};
use crate::profile_wait::{ProfileWait, WaitAttempt, WaitReading};

pub(super) fn full_window(view: &impl crate::View) -> bool {
    view.is_container()
        && !view.is_global_search()
        && !view.is_feedback_window()
        && !view.is_auxiliary()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub profile: ProfileId,
    pub workspace: WorkspaceId,
}

pub(super) struct Waiting {
    pub presentation: ProfileWait,
    pub origin: Option<Place>,
    delivery: Option<ProfileDelivery>,
    resolve: bool,
    requested_revision: u64,
    answer: Option<Receiver<Result<EnsuredWorkspace>>>,
    post_removal: bool,
}

#[derive(Default)]
pub(super) struct Waits {
    sequence: u64,
    catalog_revision: u64,
    windows: HashMap<WindowId, Waiting>,
}

impl Waits {
    pub fn begin(
        &mut self,
        window: WindowId,
        profile: ProfileId,
        origin: Option<Place>,
    ) -> WaitAttempt {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("wait sequence exhausted");
        let attempt = WaitAttempt {
            window,
            profile,
            sequence: self.sequence,
        };
        self.windows.insert(
            window,
            Waiting {
                presentation: ProfileWait {
                    attempt,
                    reading: WaitReading::Unknown,
                },
                origin,
                delivery: None,
                resolve: true,
                requested_revision: self.catalog_revision,
                answer: None,
                post_removal: false,
            },
        );
        attempt
    }

    pub fn get(&self, window: WindowId) -> Option<&Waiting> {
        self.windows.get(&window)
    }

    /// A verified removal of content that was open owes local availability
    /// resolution. EnsureLive still checks removal marks and unknown bytes.
    pub fn begin_post_removal(&mut self, window: WindowId, profile: ProfileId) -> WaitAttempt {
        let attempt = self.begin(window, profile, None);
        self.windows.get_mut(&window).unwrap().post_removal = true;
        attempt
    }

    pub fn opening(&mut self, attempt: WaitAttempt) -> Option<ProfileWait> {
        if !self.is_current(attempt) {
            return None;
        }
        let wait = self.windows.get_mut(&attempt.window)?;
        wait.presentation.reading = WaitReading::Preparing;
        wait.resolve = false;
        Some(wait.presentation.clone())
    }

    pub fn is_current(&self, attempt: WaitAttempt) -> bool {
        self.get(attempt.window)
            .is_some_and(|wait| wait.presentation.attempt == attempt)
    }

    pub fn leave(&mut self, window: WindowId) -> Option<Waiting> {
        self.windows.remove(&window)
    }

    pub fn clear(&mut self) {
        self.windows.clear();
    }

    pub fn observe(&mut self, window: WindowId, delivery: ProfileDelivery) {
        let Some(wait) = self.windows.get_mut(&window) else {
            return;
        };
        let delivery = if wait.post_removal {
            ProfileDelivery::NotApplicable
        } else {
            delivery
        };
        if wait.delivery.as_ref() == Some(&delivery) {
            return;
        }
        if !delivery.may_ensure_live() {
            // A result authorized by an older account reading cannot land
            // after that authority has changed. The Store operation may still
            // finish; its reply has no receiver and cannot move this window.
            wait.answer = None;
            wait.presentation.reading = match &delivery {
                ProfileDelivery::Unknown => WaitReading::Unknown,
                ProfileDelivery::InProgress { enrollment, counts } => WaitReading::Cloud {
                    enrollment: *enrollment,
                    counts: *counts,
                },
                ProfileDelivery::Failed {
                    phase: FailurePhase::Enrollment,
                    code,
                    retry_at_millis,
                    ..
                } => WaitReading::EnrollmentFailed {
                    code: code.clone(),
                    retry_at_millis: *retry_at_millis,
                },
                ProfileDelivery::Failed {
                    phase: FailurePhase::Content,
                    code,
                    retry_at_millis,
                    automatic,
                } => WaitReading::ContentFailed {
                    code: code.clone(),
                    retry_at_millis: *retry_at_millis,
                    automatic: *automatic,
                },
                ProfileDelivery::NotApplicable | ProfileDelivery::Delivered => unreachable!(),
            };
        }
        wait.resolve = true;
        wait.delivery = Some(delivery);
    }

    pub fn observe_removal(
        &mut self,
        window: WindowId,
        mark: Option<Option<koru_store_core::profile_removal::RemovalMark>>,
    ) -> bool {
        let Some(wait) = self.windows.get_mut(&window) else {
            return false;
        };
        let Some(mark) = mark else {
            return matches!(wait.presentation.reading, WaitReading::Removing { .. });
        };
        if let Some(mark) = mark {
            wait.answer = None;
            wait.resolve = false;
            wait.delivery = None;
            wait.presentation.reading = WaitReading::Removing { mark };
            return true;
        }
        if matches!(wait.presentation.reading, WaitReading::Removing { .. }) {
            wait.presentation.reading = WaitReading::Unknown;
            wait.delivery = None;
            wait.post_removal = false;
            wait.resolve = true;
        }
        false
    }

    pub fn catalog_changed(&mut self) {
        self.catalog_revision = self
            .catalog_revision
            .checked_add(1)
            .expect("catalog revision exhausted");
        for wait in self.windows.values_mut() {
            if matches!(wait.presentation.reading, WaitReading::Pending { .. }) {
                wait.resolve = true;
            }
        }
    }

    pub fn retry(&mut self, attempt: WaitAttempt) {
        if self.is_current(attempt) {
            let wait = self.windows.get_mut(&attempt.window).unwrap();
            if wait.answer.is_none() {
                wait.resolve = true;
            }
        }
    }

    pub fn request(
        &mut self,
        window: WindowId,
    ) -> Option<(WaitAttempt, Sender<Result<EnsuredWorkspace>>)> {
        let wait = self.windows.get_mut(&window)?;
        if !wait.resolve
            || wait.answer.is_some()
            || !wait
                .delivery
                .as_ref()
                .is_some_and(ProfileDelivery::may_ensure_live)
        {
            return None;
        }
        let (reply, answer) = mpsc::channel();
        wait.answer = Some(answer);
        wait.resolve = false;
        wait.requested_revision = self.catalog_revision;
        wait.presentation.reading = WaitReading::Preparing;
        Some((wait.presentation.attempt, reply))
    }

    pub fn poll(&mut self, window: WindowId) -> Option<(WaitAttempt, WorkspaceId)> {
        let wait = self.windows.get_mut(&window)?;
        let result = match wait.answer.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(koru_store_core::Error::bare(
                koru_store_core::Code::StorageFailed,
            )),
        };
        wait.answer = None;
        match result {
            Ok(
                EnsuredWorkspace::Active { workspace, .. }
                | EnsuredWorkspace::Created { workspace },
            ) => {
                if workspace.owner.as_uuid() != wait.presentation.attempt.profile.as_uuid() {
                    wait.presentation.reading = WaitReading::Unavailable {
                        cause: "profile_mismatch".into(),
                    };
                    return None;
                }
                return Some((
                    wait.presentation.attempt,
                    WorkspaceId::from_uuid(workspace.id.as_uuid()),
                ));
            }
            Ok(EnsuredWorkspace::Pending { workspaces }) => {
                wait.presentation.reading = WaitReading::Pending { workspaces };
                wait.resolve = wait.requested_revision != self.catalog_revision;
            }
            Ok(EnsuredWorkspace::Uncertain { issues }) => {
                wait.presentation.reading = WaitReading::Uncertain { issues }
            }
            Ok(EnsuredWorkspace::ListingPending { issues }) => {
                wait.presentation.reading = WaitReading::ListingPending { issues }
            }
            Ok(EnsuredWorkspace::Removing { mark }) => {
                wait.presentation.reading = WaitReading::Removing { mark }
            }
            Err(error) => {
                wait.presentation.reading = WaitReading::Unavailable {
                    cause: error.code().as_str().into(),
                }
            }
        }
        None
    }

    pub fn landing_failed(&mut self, attempt: WaitAttempt) {
        if self.is_current(attempt) {
            self.windows
                .get_mut(&attempt.window)
                .unwrap()
                .presentation
                .reading = WaitReading::Unavailable {
                cause: "workspace_open_failed".into(),
            };
        }
    }
}

#[cfg(test)]
mod tests;
