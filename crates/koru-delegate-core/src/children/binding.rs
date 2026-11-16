// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The exact policy a binding names: its two revisions and a digest of its
/// canonical bytes. A reference only; the policy itself is the submission's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildPolicyRef {
    pub config_revision: u64,
    pub policy_revision: u64,
    pub digest: DecisionDigest,
}
impl ChildPolicyRef {
    /// The reference to `policy` as captured at these revisions.
    pub fn of(
        config_revision: u64,
        policy_revision: u64,
        policy: &CapturedBotPolicy,
    ) -> Result<Self, DecisionError> {
        require(policy.valid())?;
        let value = Self {
            config_revision,
            policy_revision,
            digest: crate::routine_digest_json(
                b"koru.bot.child.policy.v1",
                policy,
                CHILD_BINDING_BYTES,
            )?,
        };
        value.validate()?;
        Ok(value)
    }
}
impl DecisionValidate for ChildPolicyRef {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            self.config_revision > 0
                && self.config_revision <= i64::MAX as u64
                && self.policy_revision > 0
                && self.policy_revision <= i64::MAX as u64,
        )
    }
}

/// The digest a binding carries in place of the task text.
pub fn child_task_digest(task: &str) -> Result<DecisionDigest, DecisionError> {
    crate::routine_digest_json(b"koru.bot.child.task.v1", &task, CHILD_BINDING_BYTES)
        .map_err(Into::into)
}

/// What one child request is bound to, frozen at the ask.
///
/// **Canonical encoding.** Compact JSON of the fields in the order declared
/// here, every field always present (`parent_routine` is `null` for an
/// interactive parent), `documents` sorted and distinct. The digest is BLAKE3
/// over the domain `koru.bot.child.request.v1` followed by those bytes. It
/// holds no task text, title or path.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequestBindingV1 {
    pub version: DecisionVersionV1,
    pub owner: BotOwner,
    pub parent_bot: BotId,
    pub parent_run: RunId,
    /// The parent's wait, which holds the exact origin attempt, generation,
    /// session and turn. Minted with the other ids, before the transaction.
    pub wait: BotWaitId,
    pub ask_key: Uuid,
    pub ordinal: u8,
    /// 1 for an interactive parent, 2 for a Routine parent.
    pub parent_submission: u32,
    pub parent_routine: Option<RoutineProvenance>,
    pub human: ProfileId,
    pub recipient_bot: BotId,
    pub child_run: RunId,
    pub child_request: ChildRequestId,
    pub workspace: WorkspaceId,
    pub task_digest: DecisionDigest,
    pub documents: Vec<DocumentId>,
    pub parent_policy: ChildPolicyRef,
    /// The reduced policy of the child, exactly as submitted.
    pub child_policy: ChildPolicyRef,
}
impl std::fmt::Debug for ChildRequestBindingV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildRequestBindingV1")
            .field("parent_run", &self.parent_run)
            .field("child_request", &self.child_request)
            .field("ordinal", &self.ordinal)
            .finish_non_exhaustive()
    }
}
impl ChildRequestBindingV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, DecisionError> {
        self.validate()?;
        encode_decision_value(self, CHILD_BINDING_BYTES)
    }
    pub fn digest(&self) -> Result<DecisionDigest, DecisionError> {
        self.validate()?;
        crate::routine_digest_json(b"koru.bot.child.request.v1", self, CHILD_BINDING_BYTES)
            .map_err(Into::into)
    }
    /// Strict decode of retained canonical bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecisionError> {
        if bytes.len() > CHILD_BINDING_BYTES {
            return Err(DecisionError::TooLarge);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| DecisionError::Invalid)?;
        value.validate()?;
        Ok(value)
    }
    /// The provenance the child submission must carry for this binding.
    pub fn parent(&self, chat_id: Uuid) -> ChildParent {
        ChildParent {
            run: self.parent_run,
            bot: self.parent_bot,
            chat_id,
            child_request: self.child_request,
        }
    }
    /// Shape comparison of a binding with the child submission it travels
    /// with. The ledger still proves the parent, the limits and the policy
    /// subset in its own transaction.
    pub fn matches_submission(&self, submission: &BotSubmission) -> bool {
        let p = &submission.provenance;
        let Some(parent) = p.parent else {
            return false;
        };
        let Ok(task) = child_task_digest(submission.request.objective.as_str()) else {
            return false;
        };
        let Ok(policy) =
            ChildPolicyRef::of(p.config_revision, p.policy_revision, &submission.policy)
        else {
            return false;
        };
        self.validate().is_ok()
            && submission.valid()
            && p.owner == self.owner
            && p.bot == self.recipient_bot
            && parent.run == self.parent_run
            && parent.bot == self.parent_bot
            && parent.child_request == self.child_request
            && submission.request.run == self.child_run
            && submission.workspace == self.workspace
            && submission.request.requested_by
                == (ActorRef::Person {
                    profile: self.human,
                })
            && task == self.task_digest
            && policy == self.child_policy
    }
}
impl DecisionValidate for ChildRequestBindingV1 {
    fn validate(&self) -> Result<(), DecisionError> {
        self.parent_policy.validate()?;
        self.child_policy.validate()?;
        let routine = match (self.parent_submission, &self.parent_routine) {
            (crate::INTERACTIVE_BOT_SUBMISSION_VERSION, None) => true,
            (crate::ROUTINE_BOT_SUBMISSION_VERSION, Some(routine)) => routine.valid(),
            _ => false,
        };
        require(
            routine
                && usize::from(self.ordinal) < CHILD_REQUEST_LIMIT
                && self.parent_bot != self.recipient_bot
                && self.parent_run != self.child_run
                && v4(self.ask_key)
                && v4(self.parent_bot.as_uuid())
                && v4(self.parent_run.as_uuid())
                && v4(self.wait.as_uuid())
                && v4(self.recipient_bot.as_uuid())
                && v4(self.child_run.as_uuid())
                && v4(self.child_request.as_uuid())
                && self.documents.len() <= CHILD_DOCUMENT_LIMIT
                && self.documents.windows(2).all(|pair| pair[0] < pair[1]),
        )
    }
}

/// One item of an ask: the request's id, its binding and the child's own
/// submission (version 1, `provenance.parent` set).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequestItem {
    pub child_request: ChildRequestId,
    pub binding: ChildRequestBindingV1,
    pub submission: BotSubmission,
}
impl std::fmt::Debug for ChildRequestItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildRequestItem")
            .field("child_request", &self.child_request)
            .finish_non_exhaustive()
    }
}

/// One ask: one to three requests under one wait of the parent Run.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequest {
    /// The parent's exact executing attempt. `turn` is required.
    pub parent: BotExecutionFence,
    /// The model's own key for the whole call.
    pub ask_key: Uuid,
    /// The parent Run's originating person.
    pub human: ProfileId,
    /// In ordinal order.
    pub requests: Vec<ChildRequestItem>,
}
impl std::fmt::Debug for ChildRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildRequest")
            .field("parent", &self.parent.run)
            .field("requests", &self.requests.len())
            .finish_non_exhaustive()
    }
}
impl ChildRequest {
    /// The wait every item names. Only meaningful after `validate`.
    pub fn wait(&self) -> Option<BotWaitId> {
        self.requests.first().map(|item| item.binding.wait)
    }
    /// The parent's fence with its required turn.
    pub fn origin(&self) -> Option<DecisionFence> {
        Some(DecisionFence {
            owner: self.parent.owner,
            bot: self.parent.bot,
            run: self.parent.run,
            attempt: self.parent.attempt,
            generation: self.parent.generation,
            session: self.parent.session,
            turn: self.parent.turn?,
        })
    }
}
impl DecisionValidate for ChildRequest {
    /// Shape only: one to three items in ordinal order, all of one ask, one
    /// wait, one parent and one person, no recipient or id named twice, each
    /// binding matching its submission.
    fn validate(&self) -> Result<(), DecisionError> {
        let origin = self.origin().ok_or(DecisionError::Invalid)?;
        origin.validate()?;
        require(
            v4(self.ask_key)
                && !self.requests.is_empty()
                && self.requests.len() <= CHILD_REQUEST_LIMIT,
        )?;
        let wait = self.requests[0].binding.wait;
        let mut recipients = std::collections::BTreeSet::new();
        let mut runs = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        for (index, item) in self.requests.iter().enumerate() {
            let binding = &item.binding;
            require(
                usize::from(binding.ordinal) == index
                    && item.child_request == binding.child_request
                    && binding.owner == self.parent.owner
                    && binding.parent_bot == self.parent.bot
                    && binding.parent_run == self.parent.run
                    && binding.ask_key == self.ask_key
                    && binding.wait == wait
                    && binding.human == self.human
                    && binding.parent_submission == self.requests[0].binding.parent_submission
                    && binding.parent_routine == self.requests[0].binding.parent_routine
                    && binding.parent_policy == self.requests[0].binding.parent_policy
                    && binding.matches_submission(&item.submission)
                    && recipients.insert(binding.recipient_bot)
                    && runs.insert(binding.child_run)
                    && ids.insert(binding.child_request),
            )?;
        }
        Ok(())
    }
}

/// Why the ledger (or, for `FolderOverlap`, the runtime before it) refused an
/// ask. A refusal commits nothing. Each maps to one final wire code, except
/// `WaitClosing`, which is the one retryable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildRefusal {
    DecisionOpen,
    WaitOpen,
    WaitClosing,
    Depth,
    SelfAsk,
    RecipientLimit,
    Cycle,
    RecipientPaused,
    RecipientStale,
    RecipientFull,
    Capacity,
    BudgetLow,
    FolderOverlap,
    ConnectionsNotSubset,
}
impl ChildRefusal {
    /// The stable wire code. Every one starts with `bot_`.
    pub fn code(self) -> &'static str {
        match self {
            Self::DecisionOpen => "bot_child_decision_open",
            Self::WaitOpen => "bot_child_wait_open",
            Self::WaitClosing => "bot_child_wait_closing",
            Self::Depth => "bot_child_depth",
            Self::SelfAsk => "bot_child_self_ask",
            Self::RecipientLimit => "bot_child_recipient_limit",
            Self::Cycle => "bot_child_cycle",
            Self::RecipientPaused => "bot_child_recipient_paused",
            Self::RecipientStale => "bot_child_recipient_stale",
            Self::RecipientFull => "bot_child_recipient_full",
            Self::Capacity => "bot_child_capacity",
            Self::BudgetLow => "bot_child_budget_low",
            Self::FolderOverlap => "bot_child_folder_overlap",
            Self::ConnectionsNotSubset => "bot_child_connections",
        }
    }
    /// Only a finished wait that still owes its last publication is worth a
    /// retry by the same caller.
    pub fn retryable(self) -> bool {
        matches!(self, Self::WaitClosing)
    }
}

/// A refusal and, for a per-recipient code, the item it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRefused {
    pub code: ChildRefusal,
    pub ordinal: Option<u8>,
}
