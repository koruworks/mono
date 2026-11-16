// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The two stored states of a request. Queued, running and how the child
/// ended are read from the child's own rows, never copied here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildRequestState {
    Open,
    Settled,
}
/// Who closed a request. A request settles exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildSettledBy {
    ChildEnded,
    TimedOut,
    ParentStopped,
}
impl ChildSettledBy {
    pub fn token(self) -> &'static str {
        match self {
            Self::ChildEnded => "child_ended",
            Self::TimedOut => "timed_out",
            Self::ParentStopped => "parent_stopped",
        }
    }
}
/// The request's own 15-minute clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildClock {
    Running,
    Paused,
    Stopped,
}

/// One request as retained, with the child's admission state read in the
/// same snapshot. Ids, tokens, numbers and a digest only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequestReceipt {
    pub child_request: ChildRequestId,
    pub parent: BotRunKey,
    pub wait: BotWaitId,
    pub ordinal: u8,
    pub recipient: BotId,
    pub child_run: RunId,
    pub state: ChildRequestState,
    pub settled_by: Option<ChildSettledBy>,
    pub child: BotAdmissionState,
    pub clock: ChildClock,
    /// Counted time as of this read, never above `budget_ms`.
    pub elapsed_ms: u32,
    pub budget_ms: u32,
    pub revision: DecisionRevision,
    pub binding_digest: DecisionDigest,
}
impl DecisionValidate for ChildRequestReceipt {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            usize::from(self.ordinal) < CHILD_REQUEST_LIMIT
                && u64::from(self.budget_ms) == BOT_CHILD_WAIT_MS
                && self.elapsed_ms <= self.budget_ms
                && self.parent.bot != self.recipient
                && self.parent.run != self.child_run
                && match self.state {
                    ChildRequestState::Open => {
                        self.settled_by.is_none() && self.clock != ChildClock::Stopped
                    }
                    ChildRequestState::Settled => {
                        self.settled_by.is_some() && self.clock == ChildClock::Stopped
                    }
                },
        )
    }
}

/// What an admitted (or replayed) ask answers: the wait, the intent frozen in
/// the same transaction, and one receipt per item in ordinal order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequested {
    pub wait: ChildWaitView,
    pub enrollment: BotChildWaitCleanupIntentV1,
    pub receipts: Vec<ChildRequestReceipt>,
}

/// The ledger's answer to an ask. `Refused` committed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildRequestOutcome {
    Created(ChildRequested),
    AlreadyCreated(ChildRequested),
    Refused(ChildRefused),
}

/// A parent Run's requests, in creation order, with its open child wait.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequests {
    pub wait: Option<ChildWaitView>,
    pub requests: Vec<ChildRequestReceipt>,
}
