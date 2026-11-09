// SPDX-License-Identifier: MPL-2.0

//! What enumeration and admission proved, independently of session health.
//!
//! Since root-catalog Plan 3.2 the counts group by **current committed
//! placement** (TECHNICAL.final §7.3 "`discovery_counts` may remain a UI
//! grouping, derived from current committed placement"): a Workspace is
//! counted once, under the Profile the receipt's projection places it under,
//! or under the unassigned group when its placement is explicitly null. A
//! move between passes re-groups it; it is never in two groups at once, and a
//! Profile is declared empty only under `Complete` coverage.

use super::*;
use koru_account_core::root_catalog::SubjectLifecycle;
use koru_account_wire::{CloudCoverageV2, CloudDiscoveryScopeV2};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryCounts {
    pub discovered: u32,
    pub admitted: u32,
    pub deferred: u32,
    pub deferred_code: Option<&'static str>,
    pub carried: u32,
}

/// How much of the admitted catalog the receipt's projection had committed
/// when it was read (`CloudCoverageV2`, with the pending list reduced to its
/// size: the UI needs the number, and a unit id is not for a status row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Complete,
    Partial { pending: u32 },
    Held,
}

impl Coverage {
    pub(crate) fn from_wire(coverage: &CloudCoverageV2) -> Self {
        match coverage {
            CloudCoverageV2::Complete => Self::Complete,
            CloudCoverageV2::Partial { pending } => Self::Partial {
                pending: u32::try_from(pending.len()).unwrap_or(u32::MAX),
            },
            CloudCoverageV2::Held { .. } => Self::Held,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial { .. } => "partial",
            Self::Held => "held",
        }
    }

    fn pending(self) -> u32 {
        match self {
            Self::Partial { pending } => pending,
            Self::Complete | Self::Held => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryReceipt {
    pub account_id: KoruIdentityId,
    pub pass: u64,
    pub stale: bool,
    pub exhausted: bool,
    /// Counts by current placement. A Profile absent here was placed nothing
    /// under in the receipt; whether that means *empty* or *not syncing* is
    /// `lifecycles` and `coverage`'s to say.
    pub profiles: BTreeMap<ProfileId, DiscoveryCounts>,
    /// Workspaces whose current placement is explicitly unassigned.
    pub unassigned: DiscoveryCounts,
    pub failure_code: Option<&'static str>,
    pub retry_at_millis: Option<i64>,
    pub automatic_retry: bool,
    /// The v2 receipt's facts; all `None`/empty on the v1 pair path, which
    /// groups by membership Profile and knows no coverage.
    pub scope: Option<CloudDiscoveryScopeV2>,
    pub authorization_generation: Option<u64>,
    pub coverage: Option<Coverage>,
    /// Every Profile the account's catalog holds an admitted unit for, with
    /// its lifecycle. A Profile not here has no Cloud-admitted unit: it is
    /// *not syncing*, never *empty*.
    pub lifecycles: BTreeMap<ProfileId, SubjectLifecycle>,
}

fn lifecycle_str(lifecycle: SubjectLifecycle) -> &'static str {
    match lifecycle {
        SubjectLifecycle::Live => "live",
        SubjectLifecycle::Deleted => "deleted",
        SubjectLifecycle::Purged => "purged",
    }
}

impl DiscoveryReceipt {
    pub(crate) fn for_profile(&self, profile: ProfileId) -> koru_agent_wire::CloudDiscovery {
        // Exhausting an empty page is evidence for an accepted Profile with
        // zero candidates too; it does not require a synthetic dataset row.
        // Whether zero means empty is `coverage` and `profile_lifecycle`'s
        // to say, not this row's.
        let counts = self.profiles.get(&profile).cloned().unwrap_or_default();
        koru_agent_wire::CloudDiscovery {
            account_id: self.account_id.to_string(),
            pass: self.pass,
            stale: self.stale,
            exhausted: self.exhausted,
            discovered: counts.discovered,
            admitted: counts.admitted,
            deferred: counts.deferred,
            deferred_code: counts.deferred_code.map(str::to_owned),
            carried: counts.carried,
            failure_code: self.failure_code.map(str::to_owned),
            retry_at_millis: self.retry_at_millis,
            automatic_retry: self.automatic_retry,
            coverage: self.coverage.map(|coverage| coverage.as_str().to_owned()),
            pending_units: self.coverage.map_or(0, Coverage::pending),
            scope: self.scope.map(|scope| {
                match scope {
                    CloudDiscoveryScopeV2::UserCatalog => "user_catalog",
                    CloudDiscoveryScopeV2::Workspaces => "workspaces",
                }
                .to_owned()
            }),
            authorization_generation: self.authorization_generation,
            profile_lifecycle: self
                .lifecycles
                .get(&profile)
                .map(|lifecycle| lifecycle_str(*lifecycle).to_owned()),
            unassigned: self.unassigned.discovered,
        }
    }
}
