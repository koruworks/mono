// SPDX-License-Identifier: MPL-2.0

//! Exact `cloud-plan-v1` account and Workspace read shapes. Every nullable
//! property is required on JSON; missing, new enum and malformed integer values
//! fail decoding so clients can show unavailable and refresh/update.

use koru_account_core::cloud_plan::{
    CloudAccessReason, CloudAccessState, CloudAllowance, CloudPlanPolicy, CloudPurchase, CloudTier,
    PurchaseInterval,
};
use koru_account_core::subscription::SubscriptionStatus;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};

mod schema;

/// Bounded page query. Hosts reject unknown/duplicate keys and validate these
/// bounds before starting the database read; no account identifier is accepted.
#[derive(Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudUsageWorkspacesQueryV1 {
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
    #[schemars(length(min = 1, max = 2048), regex(pattern = "^[A-Za-z0-9_-]+$"))]
    pub cursor: Option<String>,
}

impl CloudUsageWorkspacesQueryV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.limit.is_some_and(|n| !(1..=100).contains(&n)) {
            return Err("invalid page limit");
        }
        if self.cursor.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 2048
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) {
            return Err("invalid usage cursor");
        }
        Ok(())
    }
}

impl std::fmt::Debug for CloudUsageWorkspacesQueryV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudUsageWorkspacesQueryV1")
            .finish_non_exhaustive()
    }
}

/// Nonnegative signed SQL BIGINT, encoded as a canonical decimal JSON string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Decimal(i64);
impl Decimal {
    pub fn new(value: i64) -> Result<Self, &'static str> {
        if value < 0 {
            Err("negative integer")
        } else {
            Ok(Self(value))
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}
impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s.is_empty()
            || (s.len() > 1 && s.starts_with('0'))
            || !s.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(de::Error::custom("noncanonical integer"));
        }
        s.parse::<i64>().map(Self).map_err(de::Error::custom)
    }
}

/// Canonical lowercase UUID (the approved read contract does not require v4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadUuid(pub uuid::Uuid);
impl Serialize for ReadUuid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for ReadUuid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let id = uuid::Uuid::parse_str(&s).map_err(de::Error::custom)?;
        if id.to_string() != s {
            return Err(de::Error::custom("noncanonical UUID"));
        }
        Ok(Self(id))
    }
}

trait Validate {
    fn validate(&self) -> Result<(), &'static str>;
}
fn required<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    T::deserialize(d)
}

// Keep the decoded and encoded shape identical, including required nulls and
// cross-field invariants. Raw is private and cannot escape validation.
macro_rules! object {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name { $(pub $field: $ty),* }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.validate().map_err(ser::Error::custom)?;
                #[derive(Serialize)] struct Raw<'a> { $($field: &'a $ty),* }
                Raw { $($field: &self.$field),* }.serialize(serializer)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)] #[serde(deny_unknown_fields)]
                struct Raw { $(#[serde(deserialize_with = "required")] $field: $ty),* }
                let raw = Raw::deserialize(d)?;
                let value = Self { $($field: raw.$field),* };
                value.validate().map_err(de::Error::custom)?;
                Ok(value)
            }
        }
    };
}
macro_rules! vocabulary {
    ($name:ident { $($variant:ident),* $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),* }
    };
}
vocabulary!(UsageState {
    Known,
    Unknown,
    Building
});
vocabulary!(UsageReason {
    UsageUnknown,
    CloudSchemaUpdateRequired
});
vocabulary!(PurchaseState { Known, Unavailable });
vocabulary!(PurchaseReason {
    PurchaseUnavailable
});
vocabulary!(AllowanceReason { PolicyUnavailable });
vocabulary!(PlacementState {
    Assigned,
    Unassigned,
    Unknown
});
vocabulary!(NotImplementedState { NotImplemented });
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanContract {
    #[serde(rename = "cloud-plan-v1")]
    V1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceContract {
    #[serde(rename = "cloud-usage-workspaces-v1")]
    V1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesAlgorithm {
    #[serde(rename = "files-plain-v0")]
    PlainV0,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AllowanceV1 {
    Finite {
        base_bytes: Decimal,
        effective_bytes: Decimal,
    },
    Unbounded,
    Unavailable {
        reason: AllowanceReason,
    },
}
object!(AccessV1 { state: CloudAccessState, reason: CloudAccessReason, download_until_millis: Option<Decimal>, removal_at_millis: Option<Decimal> });
impl Validate for AccessV1 {
    fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}
object!(PurchaseV1 { state: PurchaseState, reason: Option<PurchaseReason>, manageable: Option<bool>, status: Option<SubscriptionStatus>, interval: Option<PurchaseInterval>, available_intervals: Option<Vec<PurchaseInterval>> });
impl Validate for PurchaseV1 {
    fn validate(&self) -> Result<(), &'static str> {
        match self.state {
            PurchaseState::Known
                if self.reason.is_none()
                    && self.manageable.is_some()
                    && self
                        .available_intervals
                        .as_ref()
                        .is_some_and(|a| a.len() <= 2 && (a.len() < 2 || a[0] != a[1])) =>
            {
                Ok(())
            }
            PurchaseState::Unavailable
                if self.reason == Some(PurchaseReason::PurchaseUnavailable)
                    && self.manageable.is_none()
                    && self.status.is_none()
                    && self.interval.is_none()
                    && self.available_intervals.is_none() =>
            {
                Ok(())
            }
            _ => Err("invalid purchase state"),
        }
    }
}
object!(PolicyV1 { tier: CloudTier, allowance: AllowanceV1, access: AccessV1, mcp_allowed: Option<bool>, purchase: PurchaseV1 });
impl Validate for PolicyV1 {
    fn validate(&self) -> Result<(), &'static str> {
        let finite = matches!(self.allowance, AllowanceV1::Finite { .. });
        let known = !matches!(self.access.state, CloudAccessState::Unavailable)
            && self.mcp_allowed.is_some();
        let valid = match self.tier {
            CloudTier::Unknown => {
                matches!(self.allowance, AllowanceV1::Unavailable { .. })
                    && self.access.state == CloudAccessState::Unavailable
                    && self.access.reason == CloudAccessReason::PolicyUnavailable
                    && self.mcp_allowed.is_none()
            }
            CloudTier::Local => {
                known
                    && self.allowance
                        == AllowanceV1::Finite {
                            base_bytes: Decimal(0),
                            effective_bytes: Decimal(0),
                        }
                    && self.access.state == CloudAccessState::None
                    && self.mcp_allowed == Some(false)
            }
            // Cloud MCP is a capability of its own on both finite tiers.
            CloudTier::Free | CloudTier::Plus => known && finite,
            CloudTier::Unbilled => {
                known && self.allowance == AllowanceV1::Unbounded && self.mcp_allowed == Some(true)
            }
        };
        if valid {
            Ok(())
        } else {
            Err("inconsistent policy")
        }
    }
}
impl TryFrom<&CloudPlanPolicy> for PolicyV1 {
    type Error = &'static str;
    fn try_from(value: &CloudPlanPolicy) -> Result<Self, Self::Error> {
        let allowance = match value.allowance {
            CloudAllowance::Finite {
                base_bytes,
                effective_bytes,
            } => AllowanceV1::Finite {
                base_bytes: Decimal::new(
                    i64::try_from(base_bytes).map_err(|_| "allowance overflow")?,
                )?,
                effective_bytes: Decimal::new(
                    i64::try_from(effective_bytes).map_err(|_| "allowance overflow")?,
                )?,
            },
            CloudAllowance::Unbounded => AllowanceV1::Unbounded,
            CloudAllowance::Unavailable => AllowanceV1::Unavailable {
                reason: AllowanceReason::PolicyUnavailable,
            },
        };
        let purchase = match &value.purchase {
            CloudPurchase::Known {
                manageable,
                status,
                interval,
                available_intervals,
            } => PurchaseV1 {
                state: PurchaseState::Known,
                reason: None,
                manageable: Some(*manageable),
                status: *status,
                interval: *interval,
                available_intervals: Some(available_intervals.clone()),
            },
            CloudPurchase::Unavailable => PurchaseV1 {
                state: PurchaseState::Unavailable,
                reason: Some(PurchaseReason::PurchaseUnavailable),
                manageable: None,
                status: None,
                interval: None,
                available_intervals: None,
            },
        };
        Ok(Self {
            tier: value.tier,
            allowance,
            access: AccessV1 {
                state: value.access,
                reason: value.reason,
                download_until_millis: None,
                removal_at_millis: None,
            },
            mcp_allowed: value.mcp_allowed,
            purchase,
        })
    }
}

object!(UsageV1 { state: UsageState, reason: Option<UsageReason>, algorithm: FilesAlgorithm, generation: Option<ReadUuid>, used_bytes: Option<Decimal>, files_bytes: Option<Decimal>, total_bytes: Option<Decimal>, available_bytes: Option<Decimal>, reserved_bytes: Option<Decimal>, upload_headroom_bytes: Option<Decimal> });
impl Validate for UsageV1 {
    fn validate(&self) -> Result<(), &'static str> {
        let no_current = self.used_bytes.is_none()
            && self.files_bytes.is_none()
            && self.available_bytes.is_none()
            && self.upload_headroom_bytes.is_none();
        let valid = match self.state {
            UsageState::Known => {
                self.reason.is_none()
                    && self.generation.is_some()
                    && self.used_bytes.is_some()
                    && self.files_bytes == self.used_bytes
                    && self.reserved_bytes.is_some()
            }
            UsageState::Unknown => {
                self.reason.is_some()
                    && self.generation.is_some()
                    && self.reserved_bytes.is_some()
                    && no_current
            }
            UsageState::Building => {
                self.reason.is_none() && self.generation.is_none() && no_current
            }
        };
        if valid {
            Ok(())
        } else {
            Err("inconsistent usage")
        }
    }
}
object!(NotImplementedV1 {
    state: NotImplementedState
});
impl Validate for NotImplementedV1 {
    fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}
object!(CloudPlanResponseV1 { contract: PlanContract, revision: Decimal, policy_revision: Decimal, evaluated_at_millis: Decimal, valid_until_millis: Option<Decimal>, policy: PolicyV1, usage: UsageV1, free_availability: NotImplementedV1, feedback: NotImplementedV1 });
impl Validate for CloudPlanResponseV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self
            .valid_until_millis
            .is_some_and(|until| until <= self.evaluated_at_millis)
        {
            return Err("expired policy snapshot");
        }
        let total = match self.policy.allowance {
            AllowanceV1::Finite {
                effective_bytes, ..
            } => Some(effective_bytes),
            _ => None,
        };
        if self.usage.total_bytes != total {
            return Err("allowance does not match usage total");
        }
        if self.usage.state == UsageState::Known {
            let used = self.usage.used_bytes.ok_or("missing used bytes")?.get();
            let reserved = self
                .usage
                .reserved_bytes
                .ok_or("missing reserved bytes")?
                .get();
            let available = total.map(|total| Decimal((total.get() - used).max(0)));
            let headroom = available.map(|available| Decimal((available.get() - reserved).max(0)));
            if self.usage.available_bytes != available
                || self.usage.upload_headroom_bytes != headroom
            {
                return Err("invalid available bytes");
            }
        }
        Ok(())
    }
}

object!(StaleWorkspaceUsageV1 {
    files_bytes: Decimal,
    revision: Decimal,
    captured_at_millis: Decimal,
    stale: bool
});
impl Validate for StaleWorkspaceUsageV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self.stale {
            Ok(())
        } else {
            Err("historical usage must be stale")
        }
    }
}
object!(WorkspaceUsageV1 { state: UsageState, reason: Option<UsageReason>, files_bytes: Option<Decimal>, last_known: Option<StaleWorkspaceUsageV1> });
impl Validate for WorkspaceUsageV1 {
    fn validate(&self) -> Result<(), &'static str> {
        let valid = match self.state {
            UsageState::Known => {
                self.reason.is_none() && self.files_bytes.is_some() && self.last_known.is_none()
            }
            UsageState::Unknown => self.reason.is_some() && self.files_bytes.is_none(),
            UsageState::Building => {
                self.reason.is_none() && self.files_bytes.is_none() && self.last_known.is_none()
            }
        };
        if valid {
            Ok(())
        } else {
            Err("inconsistent Workspace usage")
        }
    }
}
object!(PlacementV1 { state: PlacementState, profile_id: Option<ReadUuid> });
impl Validate for PlacementV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if (self.state == PlacementState::Assigned) == self.profile_id.is_some() {
            Ok(())
        } else {
            Err("inconsistent placement")
        }
    }
}
object!(WorkspaceUsageItemV1 { workspace_id: ReadUuid, incarnation_id: ReadUuid, source_revision: Decimal, display_name: Option<String>, usage: WorkspaceUsageV1, placement: PlacementV1 });
impl Validate for WorkspaceUsageItemV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self
            .display_name
            .as_ref()
            .is_none_or(|name| name.chars().count() <= 1024)
        {
            Ok(())
        } else {
            Err("display name too long")
        }
    }
}
object!(CloudUsageWorkspacesResponseV1 { contract: WorkspaceContract, revision: Decimal, generation: Option<ReadUuid>, catalog_revision_digest: String, items: Vec<WorkspaceUsageItemV1>, next_cursor: Option<String> });
impl Validate for CloudUsageWorkspacesResponseV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self.catalog_revision_digest.len() != 64
            || !self
                .catalog_revision_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid catalog digest");
        }
        if self.items.len() > 100 {
            return Err("too many Workspaces");
        }
        let mut workspaces = std::collections::HashSet::new();
        if self
            .items
            .iter()
            .any(|row| !workspaces.insert(row.workspace_id.0))
        {
            return Err("duplicate Workspace identity");
        }
        if self.next_cursor.as_ref().is_some_and(|cursor| {
            cursor.is_empty()
                || cursor.len() > 2048
                || !cursor
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) {
            return Err("invalid cursor encoding");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

pub mod documents;
