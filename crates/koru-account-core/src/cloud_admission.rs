// SPDX-License-Identifier: MPL-2.0

//! Memory-only client upload holds. These decide when to ask the server again,
//! never whether a write is authorized. Reads, removals and other candidates do
//! not consult them. No timer, notification or UI action is evidence of quota.

use crate::cloud_plan::CloudAllowance;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    PolicyUnavailable,
    PlanRequired,
    DocumentCount,
    StoredDocumentBytes,
    PhysicalBytes,
    StagingBytes,
    RemovalCapacity,
    FileAllowance,
    UsageUnknown,
    AccountQuota,
}

impl Reason {
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "cloud_policy_unavailable" => Self::PolicyUnavailable,
            "cloud_plan_required" => Self::PlanRequired,
            "cloud_document_count_limit" => Self::DocumentCount,
            "cloud_stored_document_bytes_limit" => Self::StoredDocumentBytes,
            "cloud_physical_bytes_limit" => Self::PhysicalBytes,
            "cloud_staging_bytes_limit" => Self::StagingBytes,
            "cloud_removal_capacity_limit" => Self::RemovalCapacity,
            "file_exceeds_plan_allowance" => Self::FileAllowance,
            "usage_unknown" => Self::UsageUnknown,
            "account_over_quota" => Self::AccountQuota,
            _ => return None,
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::PolicyUnavailable => "cloud_policy_unavailable",
            Self::PlanRequired => "cloud_plan_required",
            Self::DocumentCount => "cloud_document_count_limit",
            Self::StoredDocumentBytes => "cloud_stored_document_bytes_limit",
            Self::PhysicalBytes => "cloud_physical_bytes_limit",
            Self::StagingBytes => "cloud_staging_bytes_limit",
            Self::RemovalCapacity => "cloud_removal_capacity_limit",
            Self::FileAllowance => "file_exceeds_plan_allowance",
            Self::UsageUnknown => "usage_unknown",
            Self::AccountQuota => "account_over_quota",
        }
    }

    pub fn resource(self) -> bool {
        matches!(
            self,
            Self::DocumentCount
                | Self::StoredDocumentBytes
                | Self::PhysicalBytes
                | Self::StagingBytes
                | Self::RemovalCapacity
        )
    }
}

/// Supplied by current trusted authority, never by a writer's JSON or an
/// equality comparison between two accounts' revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub owner: Uuid,
    pub workspace: Uuid,
    pub incarnation: Uuid,
    pub account_generation: u64,
    pub authority_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub binding: Binding,
    pub usage_revision: u64,
    pub policy_revision: u64,
    pub generation: Option<Uuid>,
    pub observed_at_millis: i64,
    pub valid_until_millis: i64,
    pub allowance: CloudAllowance,
    pub read: bool,
    pub ordinary_write: bool,
    /// Only permission to request the separately verified removal command.
    pub remove: bool,
    /// Set only after a coherent current account/Workspace usage read proves
    /// the complete generation and reserved holds. Building/unknown is false.
    pub complete_usage: bool,
    pub used_bytes: Option<u64>,
    pub reserved_bytes: Option<u64>,
}

impl Observation {
    pub fn current(&self, now_millis: i64) -> bool {
        self.observed_at_millis <= now_millis
            && now_millis < self.valid_until_millis
            && now_millis.saturating_sub(self.observed_at_millis) < 60_000
    }

    fn complete(&self) -> bool {
        self.complete_usage
            && self.generation.is_some()
            && self.used_bytes.is_some()
            && self.reserved_bytes.is_some()
    }
}

/// One refused candidate. The engine's map key supplies its hash or exact
/// document head; replacing that candidate creates an independent admission.
#[derive(Clone, Debug)]
pub struct Hold {
    pub reason: Reason,
    binding: Option<Binding>,
    baseline: Option<Observation>,
    candidate_bytes: Option<u64>,
    last_attempt_millis: i64,
}

impl Hold {
    pub fn new(
        reason: Reason,
        binding: Binding,
        observation: Option<Observation>,
        candidate_bytes: Option<u64>,
        now_millis: i64,
    ) -> Self {
        Self {
            reason,
            binding: Some(binding),
            baseline: observation.filter(|o| o.binding == binding && o.current(now_millis)),
            candidate_bytes,
            last_attempt_millis: now_millis,
        }
    }

    /// A refusal may arrive before the first account refresh. Keep it held
    /// without inventing owner facts; the host must supply a trusted binding.
    pub fn unobserved(reason: Reason, candidate_bytes: Option<u64>, now_millis: i64) -> Self {
        Self {
            reason,
            binding: None,
            baseline: None,
            candidate_bytes,
            last_attempt_millis: now_millis,
        }
    }

    /// Consumes one relevant observation or bounded resource cycle. A caller
    /// retains the hold until the intended operation succeeds or refuses again.
    /// Thus duplicate wakes cannot replay an upload while its attempt is queued.
    pub fn retry(&mut self, observation: Option<Observation>, now_millis: i64) -> bool {
        let next = observation
            .filter(|o| self.binding.is_none_or(|b| o.binding == b) && o.current(now_millis));
        if let Some(next) = next {
            self.binding = Some(next.binding);
        }
        let changed = next.is_some_and(|next| {
            let old = self.baseline;
            match self.reason {
                Reason::FileAllowance => match (old.map(|o| o.allowance), next.allowance) {
                    (Some(CloudAllowance::Unbounded), _) => false,
                    (_, CloudAllowance::Unbounded) => true,
                    (
                        Some(CloudAllowance::Finite {
                            effective_bytes: before,
                            ..
                        }),
                        CloudAllowance::Finite {
                            effective_bytes: after,
                            ..
                        },
                    ) => after > before,
                    (
                        _,
                        CloudAllowance::Finite {
                            effective_bytes, ..
                        },
                    ) => self
                        .candidate_bytes
                        .is_some_and(|size| size <= effective_bytes),
                    _ => false,
                },
                Reason::UsageUnknown => {
                    next.complete()
                        && old.is_none_or(|o| {
                            !o.complete()
                                || o.generation != next.generation
                                || o.usage_revision != next.usage_revision
                        })
                }
                Reason::AccountQuota => old.is_none_or(|o| {
                    o.usage_revision != next.usage_revision
                        || o.generation != next.generation
                        || o.allowance != next.allowance
                        || o.used_bytes != next.used_bytes
                        || o.reserved_bytes != next.reserved_bytes
                }),
                Reason::PolicyUnavailable | Reason::PlanRequired => old.is_none_or(|o| {
                    o.policy_revision != next.policy_revision
                        || o.allowance != next.allowance
                        || o.ordinary_write != next.ordinary_write
                        || o.read != next.read
                        || o.remove != next.remove
                }),
                _ => false,
            }
        });
        let resource_due =
            self.reason.resource() && now_millis.saturating_sub(self.last_attempt_millis) >= 60_000;
        if changed || resource_due {
            self.baseline = next.or(self.baseline);
            self.last_attempt_millis = now_millis;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
