// SPDX-License-Identifier: MPL-2.0

//! Window-scoped availability intent and the reading presented by its pane.

use koru_core::ProfileId;
use koru_store_core::{
    WorkspaceId, profile_removal::RemovalMark, service::WorkspaceAvailabilityIssue,
};
use winit::window::WindowId;

/// A reply may navigate only while this exact intent is still current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitAttempt {
    pub window: WindowId,
    pub profile: ProfileId,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileWait {
    pub attempt: WaitAttempt,
    pub reading: WaitReading,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitReading {
    Unknown,
    Cloud {
        enrollment: bool,
        counts: Option<(u32, u32)>,
    },
    EnrollmentFailed {
        code: Option<String>,
        retry_at_millis: Option<i64>,
    },
    ContentFailed {
        code: Option<String>,
        retry_at_millis: Option<i64>,
        automatic: bool,
    },
    Preparing,
    Pending {
        workspaces: Vec<WorkspaceId>,
    },
    Uncertain {
        issues: Vec<WorkspaceAvailabilityIssue>,
    },
    ListingPending {
        issues: Vec<WorkspaceAvailabilityIssue>,
    },
    Removing {
        mark: RemovalMark,
    },
    /// The Profile has no live workspace and none is manufactured
    /// (root-catalog §3.10): a valid state on open, restart and after a
    /// move or delete. The pane says so and offers *New workspace*.
    Empty,
    Unavailable {
        cause: String,
    },
}
