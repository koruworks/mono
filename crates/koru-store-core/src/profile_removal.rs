// SPDX-License-Identifier: MPL-2.0

//! Local removal coordination retained in the catalog state table.
//!
//! Everything here is **device-local removal**: an attempt to take a
//! Profile off *this installation* — its Workspaces into the Trash, then
//! `profiles.removed_at` — which is local suppression and never
//! global-delete consent (root-catalog TECHNICAL.final §2.1). The **shared
//! deletion** of a Profile, the root fact that converges between a user's
//! devices and is restored as a batch, is a different operation with no
//! attempt: `Store::delete_profile` / `Store::restore_profile` in
//! `service/root_lifecycle.rs`. Neither kind below is that operation.

use crate::{ProfileId, Timestamp, ids::RemovalAttemptId};
use serde::{Deserialize, Serialize};

/// Which device-local removal is running. Both kinds end in the local
/// `removed_at` marker and leave every other device untouched; neither is
/// the shared deletion (`Store::delete_profile`), which writes a root
/// deletion fact instead of an attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    /// Manage's "remove from this device": trash each Workspace here, then
    /// hide the Profile here.
    Local,
    /// The cleanup after a hosted account deletion the server already
    /// acknowledged; same local effects, driven by the Agent's journal.
    HostedCleanup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RemovalMark {
    pub attempt: RemovalAttemptId,
    pub kind: RemovalKind,
    pub started_at: Timestamp,
    pub last_step_at: Timestamp,
    pub stopped: bool,
}

impl RemovalMark {
    pub fn lease_live(&self, now: Timestamp) -> bool {
        !self.stopped && now.millis().saturating_sub(self.last_step_at.millis()) <= 60_000
    }
}

pub fn removal_key(profile: ProfileId) -> String {
    format!("profile_removal:{profile}")
}

/// Catalog transitions are compared and written in one transaction. Step also
/// checks stopped state, whereas Keep deliberately accepts a stopped attempt.
pub enum RemovalUpdate {
    Begin {
        kind: RemovalKind,
        at: Timestamp,
    },
    Stop {
        attempt: RemovalAttemptId,
    },
    Keep {
        attempt: RemovalAttemptId,
    },
    Step {
        attempt: RemovalAttemptId,
        at: Timestamp,
    },
}

pub fn require_attempt(
    mark: Option<&RemovalMark>,
    attempt: RemovalAttemptId,
) -> crate::Result<&RemovalMark> {
    mark.filter(|mark| mark.attempt == attempt && !mark.stopped)
        .ok_or_else(|| crate::Error::bare(crate::Code::RemovalAttemptStale))
}
