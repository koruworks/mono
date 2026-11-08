// SPDX-License-Identifier: MPL-2.0

//! Local removal coordination retained in the catalog state table.

use crate::{ProfileId, Timestamp, ids::RemovalAttemptId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    Local,
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
