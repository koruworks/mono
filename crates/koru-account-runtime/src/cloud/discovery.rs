// SPDX-License-Identifier: MPL-2.0

//! What enumeration and admission proved, independently of session health.

use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryCounts {
    pub discovered: u32,
    pub admitted: u32,
    pub deferred: u32,
    pub deferred_code: Option<&'static str>,
    pub carried: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryReceipt {
    pub account_id: KoruIdentityId,
    pub pass: u64,
    pub stale: bool,
    pub exhausted: bool,
    pub profiles: BTreeMap<ProfileId, DiscoveryCounts>,
    pub failure_code: Option<&'static str>,
    pub retry_at_millis: Option<i64>,
    pub automatic_retry: bool,
}

impl DiscoveryReceipt {
    pub(crate) fn for_profile(&self, profile: ProfileId) -> koru_agent_wire::CloudDiscovery {
        // Exhausting an empty page is evidence for an accepted Profile with
        // zero candidates too; it does not require a synthetic dataset row.
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
        }
    }
}
