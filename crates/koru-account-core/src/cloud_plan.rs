// SPDX-License-Identifier: MPL-2.0

//! Pure account policy. Adapters gather these inputs in the same transaction as
//! usage, then evaluate again at database time after locks and before commit.
//! A lookup failure is evidence of unavailability, never evidence of Local.

use crate::app_connections::{AppConnectionError, CloudEntitlement};
use crate::subscription::{
    SubscriptionSnapshot, SubscriptionStatus, SubscriptionSummary, current_subscription,
};
use serde::{Deserialize, Serialize};

pub const FREE_BYTES: u64 = 1_000_000_000;
pub const PLUS_BYTES: u64 = 100_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudTier {
    Local,
    Free,
    Plus,
    Unbilled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurchaseInterval {
    Month,
    Year,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudAccessState {
    ReadWrite,
    ReadOnly,
    None,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudAccessReason {
    PlusActive,
    TrialActive,
    ComplimentaryActive,
    FreeActive,
    Unbilled,
    CloudNotEnabled,
    EntitlementInactive,
    PolicyUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudAllowance {
    Finite {
        base_bytes: u64,
        effective_bytes: u64,
    },
    Unbounded,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudPurchase {
    Known {
        manageable: bool,
        status: Option<SubscriptionStatus>,
        interval: Option<PurchaseInterval>,
        available_intervals: Vec<PurchaseInterval>,
    },
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BillingMode {
    Billed,
    Unbilled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyUnavailable;

/// A transaction-local selection, not a cache or a repository port. `purchase`
/// is independent: inability to manage billing cannot revoke known access.
pub struct CloudPlanInput {
    pub billing_mode: BillingMode,
    pub subscription: Result<Option<SubscriptionSummary>, PolicyUnavailable>,
    pub complimentary_until_millis: Result<Option<i64>, PolicyUnavailable>,
    pub purchase: CloudPurchase,
}

/// The configured price list must include every allowed monthly/yearly price.
/// The existing subscription selector owns cancellation/rebuy conflict rules.
pub fn select_purchase(
    rows: &[SubscriptionSnapshot],
    prices: &[(String, PurchaseInterval)],
    livemode: bool,
    now_millis: i64,
) -> (
    Result<Option<SubscriptionSummary>, PolicyUnavailable>,
    CloudPurchase,
) {
    let rows: Vec<_> = rows
        .iter()
        .filter(|row| row.livemode == livemode && prices.iter().any(|(id, _)| *id == row.price_id))
        .cloned()
        .collect();
    let Ok(selected) = current_subscription(&rows) else {
        return (Err(PolicyUnavailable), CloudPurchase::Unavailable);
    };
    let mut available_intervals = Vec::new();
    for (_, interval) in prices {
        if !available_intervals.contains(interval) {
            available_intervals.push(*interval);
        }
    }
    let purchase = CloudPurchase::Known {
        manageable: selected.is_some(),
        status: selected.map(|s| s.status),
        interval: selected.and_then(|s| {
            prices
                .iter()
                .find(|(id, _)| *id == s.price_id)
                .map(|(_, interval)| *interval)
        }),
        available_intervals,
    };
    (Ok(selected.map(|s| s.summary(true, now_millis))), purchase)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudPlanPolicy {
    pub tier: CloudTier,
    pub allowance: CloudAllowance,
    pub access: CloudAccessState,
    pub reason: CloudAccessReason,
    pub mcp_allowed: Option<bool>,
    pub purchase: CloudPurchase,
    /// Earliest input time boundary, even when another source grants longer.
    pub valid_until_millis: Option<i64>,
    /// Combined access end used only by the legacy Plus/unbilled MCP port.
    pub access_until_millis: Option<i64>,
}

impl CloudPlanPolicy {
    pub fn evaluate(input: &CloudPlanInput, now: i64) -> Self {
        let mut result = Self {
            tier: CloudTier::Unknown,
            allowance: CloudAllowance::Unavailable,
            access: CloudAccessState::Unavailable,
            reason: CloudAccessReason::PolicyUnavailable,
            mcp_allowed: None,
            purchase: input.purchase.clone(),
            valid_until_millis: None,
            access_until_millis: None,
        };
        if input.billing_mode == BillingMode::Unbilled {
            result.tier = CloudTier::Unbilled;
            result.allowance = CloudAllowance::Unbounded;
            result.access = CloudAccessState::ReadWrite;
            result.reason = CloudAccessReason::Unbilled;
            result.mcp_allowed = Some(true);
            return result;
        }
        let (Ok(subscription), Ok(grant)) = (&input.subscription, input.complimentary_until_millis)
        else {
            return result;
        };
        let grant = grant.filter(|end| *end > now);
        let subscription = subscription.clone().map(|s| s.at(now));
        let paid = subscription
            .as_ref()
            .filter(|s| s.cloud_access)
            .and_then(|s| {
                s.paid_through_millis
                    .map(|end| s.cancel_at_millis.map_or(end, |cancel| end.min(cancel)))
            });
        result.valid_until_millis = grant.into_iter().chain(paid).min();
        result.access_until_millis = grant.into_iter().chain(paid).max();
        if result.access_until_millis.is_some() {
            result.tier = CloudTier::Plus;
            result.allowance = CloudAllowance::Finite {
                base_bytes: PLUS_BYTES,
                effective_bytes: PLUS_BYTES,
            };
            result.access = CloudAccessState::ReadWrite;
            result.reason = if grant.is_some() && grant > paid {
                CloudAccessReason::ComplimentaryActive
            } else if subscription
                .as_ref()
                .is_some_and(|s| s.status == SubscriptionStatus::Trialing)
            {
                CloudAccessReason::TrialActive
            } else {
                CloudAccessReason::PlusActive
            };
            result.mcp_allowed = Some(true);
        } else {
            result.tier = CloudTier::Local;
            result.allowance = CloudAllowance::Finite {
                base_bytes: 0,
                effective_bytes: 0,
            };
            result.access = CloudAccessState::None;
            result.reason = if subscription.is_some() {
                CloudAccessReason::EntitlementInactive
            } else {
                CloudAccessReason::CloudNotEnabled
            };
            result.mcp_allowed = Some(false);
        }
        result
    }

    /// Fixture only: no production input or deployment switch can mint Free.
    pub fn fake_free() -> Self {
        Self {
            tier: CloudTier::Free,
            allowance: CloudAllowance::Finite {
                base_bytes: FREE_BYTES,
                effective_bytes: FREE_BYTES,
            },
            access: CloudAccessState::ReadWrite,
            reason: CloudAccessReason::FreeActive,
            mcp_allowed: Some(false),
            purchase: CloudPurchase::Known {
                manageable: false,
                status: None,
                interval: None,
                available_intervals: vec![],
            },
            valid_until_millis: None,
            access_until_millis: None,
        }
    }

    /// Free read/write access must never become legacy MCP entitlement.
    pub fn mcp_entitlement(&self) -> Result<CloudEntitlement, AppConnectionError> {
        match self.mcp_allowed {
            Some(true) => Ok(CloudEntitlement::Active {
                until_millis: self.access_until_millis,
            }),
            Some(false) => Ok(CloudEntitlement::None),
            None => Err(AppConnectionError::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests;
