// SPDX-License-Identifier: MPL-2.0

//! Delivery authorizes availability resolution; it never proves applied bytes.

use koru_agent_wire::{AccountStage, AccountState, EnrollmentState};
use koru_store_core::ProfileId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailurePhase {
    Enrollment,
    Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProfileDelivery {
    NotApplicable,
    Unknown,
    InProgress {
        enrollment: bool,
        counts: Option<(u32, u32)>,
    },
    Failed {
        phase: FailurePhase,
        code: Option<String>,
        retry_at_millis: Option<i64>,
        automatic: bool,
    },
    Delivered,
}

impl ProfileDelivery {
    pub(super) fn read(
        profile: ProfileId,
        binding: Option<&str>,
        account: Option<&AccountState>,
    ) -> Self {
        let Some(binding) = binding else {
            return Self::NotApplicable;
        };
        let Some(bound) = binding.strip_prefix("koru:").filter(|id| !id.is_empty()) else {
            return Self::Unknown;
        };
        let Some(account) = account else {
            return Self::Unknown;
        };
        if account.stage == AccountStage::Unavailable
            || account.active_account_id.as_deref() != Some(bound)
        {
            return Self::Unknown;
        }
        let profile = profile.to_storage();
        let enrollment = account
            .enrollments
            .iter()
            .find(|row| row.profile_id == profile);
        if let Some(row) = enrollment {
            match row.state {
                EnrollmentState::Failed => {
                    return Self::Failed {
                        phase: FailurePhase::Enrollment,
                        code: row.failure_code.clone(),
                        retry_at_millis: row.retry_at_millis,
                        automatic: false,
                    };
                }
                EnrollmentState::Waiting | EnrollmentState::Enrolling => {
                    return Self::InProgress {
                        enrollment: true,
                        counts: None,
                    };
                }
                EnrollmentState::Idle | EnrollmentState::Complete => {}
            }
        }
        let Some(choice) = account
            .cloud_profiles
            .iter()
            .find(|row| row.profile_id == profile)
        else {
            return Self::NotApplicable;
        };
        let Some(receipt) = choice
            .discovery
            .as_ref()
            .filter(|receipt| receipt.account_id == bound)
        else {
            return Self::Unknown;
        };
        // A terminal failure must remain actionable even while its old counts
        // are stale. Admission and transport health are never readiness.
        if let Some(code) = &receipt.failure_code {
            return Self::Failed {
                phase: FailurePhase::Content,
                code: Some(code.clone()),
                retry_at_millis: receipt.retry_at_millis,
                automatic: receipt.automatic_retry,
            };
        }
        if receipt.stale
            || !receipt.exhausted
            || receipt.carried > 0
            || receipt.discovered > receipt.admitted
        {
            return Self::InProgress {
                enrollment: false,
                counts: (!receipt.stale).then_some((receipt.admitted, receipt.discovered)),
            };
        }
        Self::Delivered
    }

    pub(super) fn may_ensure_live(&self) -> bool {
        matches!(self, Self::NotApplicable | Self::Delivered)
    }
}

#[cfg(test)]
mod tests;
