// SPDX-License-Identifier: MPL-2.0

//! AI-owned admission receipts. No live controller or transcript lives here.
use super::*;
use koru_delegate_core::{BotAdmissionBarrier, BotAdmissionView, BotSubmission, RunId};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotTask {
    pub text: String,
    pub documents: Vec<BotPage>,
}
impl std::fmt::Debug for BotTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotTask([REDACTED])")
    }
}
impl BotTask {
    pub fn validate(&self, workspace: WorkspaceId) -> Result<(), BotError> {
        koru_delegate_core::Objective::new(&self.text).map_err(|_| BotError::Invalid)?;
        if self.documents.len() > 16
            || self.documents.iter().any(|p| p.workspace != workspace)
            || self
                .documents
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.documents.len()
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotOperationKind {
    UpdateAccess,
    Start,
    Continue,
    Pause,
    Resume,
}
impl BotOperationKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::UpdateAccess => "update_access",
            Self::Start => "start",
            Self::Continue => "continue",
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "update_access" => Some(Self::UpdateAccess),
            "start" => Some(Self::Start),
            "continue" => Some(Self::Continue),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotOperationStatus {
    Pending,
    Accepted,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotOperation {
    pub request_id: BotRequestId,
    pub bot_id: BotId,
    pub kind: BotOperationKind,
    pub run_id: Option<RunId>,
    pub status: BotOperationStatus,
}
/// A change to a Bot that is still being delivered to its execution owner:
/// what a change of the Bot answered `bot_operation_pending` waits on. Reads
/// fill it in (`read`, `list`, `list_lifecycle`); it is never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotPendingChange {
    /// The change's own operation, readable with `read_operation`. It may be
    /// one the AI owner made itself to deliver the same change: a pause or
    /// resume its execution owner refused for good as conflicting
    /// (`bot_conflict`, `bot_stale`) is settled `rejected` and offered again
    /// under a new request, so the client's own request then reads
    /// `rejected` while this one names the renewal.
    pub request_id: BotRequestId,
    /// `pause`, `resume` or `update_access`.
    pub kind: BotOperationKind,
    /// When the change was made, in epoch milliseconds (UTC).
    pub since_ms: i64,
    /// The stable code of the last failed or refused attempt to deliver it
    /// since the AI owner last started (such as `bot_unavailable` when the
    /// execution owner could not be reached). Omitted when there was none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answer: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotSubmissionIntent {
    pub version: u32,
    pub submission: BotSubmission,
}
impl BotSubmissionIntent {
    pub fn validate(&self) -> Result<(), BotError> {
        if self.version != 1 || !self.submission.valid() {
            return Err(BotError::Invalid);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| BotError::Invalid)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotAdmissionRefusal {
    Rejected,
    Capacity,
    Stale,
    Invalid,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotExecutionOutcome {
    Accepted {
        version: u32,
        admission: BotAdmissionView,
    },
    Rejected {
        version: u32,
        reason: BotAdmissionRefusal,
    },
    RefusalFence {
        version: u32,
        round: u32,
        barrier: BotAdmissionBarrier,
        acknowledged: bool,
        reason: BotAdmissionRefusal,
    },
}
impl BotExecutionOutcome {
    pub fn validate(&self, intent: &BotSubmissionIntent) -> Result<(), BotError> {
        let p = &intent.submission.provenance;
        match self {
            Self::Accepted {
                version: 1,
                admission,
            } if admission.run == intent.submission.request.run
                && admission.key.owner == p.owner
                && admission.key.bot == p.bot
                && admission.key.request == p.submission
                && admission.queue_sequence > 0
                && admission.fence.as_ref().is_none_or(|f| {
                    f.owner == p.owner
                        && f.bot == p.bot
                        && f.run == admission.run
                        && f.generation.get() > 0
                }) =>
            {
                Ok(())
            }
            Self::Rejected { version: 1, .. } => Ok(()),
            Self::RefusalFence {
                version: 1,
                round,
                barrier,
                ..
            } if *round > 0
                && barrier.owner == p.owner
                && barrier.bot == p.bot
                && barrier.epoch > p.config_revision
                && barrier.epoch <= i64::MAX as u64 =>
            {
                Ok(())
            }
            _ => Err(BotError::Invalid),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPauseIntent {
    pub version: u32,
    pub barrier: BotAdmissionBarrier,
    pub expected_revision: u64,
}
impl BotPauseIntent {
    pub fn validate(&self) -> Result<(), BotError> {
        if self.version != 1
            || self.expected_revision == 0
            || self.expected_revision >= i64::MAX as u64
            || self.barrier.epoch != self.expected_revision + 1
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotReceiptStage {
    Prepared,
    Accepted,
    Rejected,
    Completed,
}
impl BotReceiptStage {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotSubmissionReceipt {
    pub intent: BotSubmissionIntent,
    pub fingerprint: String,
    pub kind: BotOperationKind,
    pub stage: BotReceiptStage,
    pub outcome: Option<BotExecutionOutcome>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotPauseReceipt {
    pub intent: BotPauseIntent,
    pub fingerprint: String,
    pub stage: BotReceiptStage,
    pub acknowledged: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotAdmissionReceipt {
    Submission(BotSubmissionReceipt),
    Pause(BotPauseReceipt),
}
impl BotAdmissionReceipt {
    pub fn operation(&self) -> BotOperation {
        match self {
            Self::Submission(r) => {
                let p = &r.intent.submission.provenance;
                BotOperation {
                    request_id: p.submission,
                    bot_id: p.bot,
                    kind: r.kind,
                    run_id: Some(r.intent.submission.request.run),
                    status: match r.stage {
                        BotReceiptStage::Prepared => BotOperationStatus::Pending,
                        BotReceiptStage::Rejected => BotOperationStatus::Rejected,
                        _ => BotOperationStatus::Accepted,
                    },
                }
            }
            Self::Pause(r) => BotOperation {
                request_id: r.intent.barrier.request,
                bot_id: r.intent.barrier.bot,
                kind: if r.intent.barrier.paused {
                    BotOperationKind::Pause
                } else {
                    BotOperationKind::Resume
                },
                run_id: None,
                status: if r.acknowledged {
                    BotOperationStatus::Accepted
                } else if r.stage == BotReceiptStage::Rejected {
                    BotOperationStatus::Rejected
                } else {
                    BotOperationStatus::Pending
                },
            },
        }
    }
}
