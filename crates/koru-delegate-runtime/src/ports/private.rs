// SPDX-License-Identifier: MPL-2.0

//! The private-operation ledger port (M5, delegate IPC 18).
//!
//! The decision and effect ledger of a private operation is the same sole
//! `runs.db` writer every other Bot domain uses; this port is how the
//! orchestrator reaches its private-operation half. A store that does not
//! compose it answers `None` from [`super::RunStore::private_effects`], and
//! every private command then refuses with `bot_private_unavailable`.
//!
//! Each method is the ledger's own. The commands that carry a dispatcher id
//! resolve first: the rows another dispatcher instance left open for the same
//! owner are closed in a transaction of their own and returned in
//! [`PrivateResolved::resolved`], even when the command itself is refused. The
//! orchestrator runs the decision settle hook for each of them.

use koru_delegate_core::decisions::{
    DecisionBindingV1, DecisionExpected, DecisionPublicationBase, DecisionTransitionId,
};
use koru_delegate_core::{
    BotExecutionFence, BotOwner, DelegateBootId, FreshBotPolicyProof, PrivateDispatcherId,
    PrivateEffectKey, PrivateEffectTransition, PrivateOperationProposal, RunId,
};
use koru_delegate_sqlite::{
    BotLedgerError, DecisionRecord, PrivateEffectOutcome, PrivateEffectRecord, PrivateLedgerError,
    PrivateResolved, RunsDb,
};

/// Implemented by the existing `RunsDb` connection, never another writer.
/// Authentication happens before this port.
pub trait PrivateEffectLedger {
    /// `AnnouncePrivateDispatcher`: close what a dead dispatcher instance
    /// left open for `owner`; answers those rows. Idempotent.
    fn announce_private_dispatcher(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<Vec<PrivateEffectRecord>, PrivateLedgerError>;
    /// `PreparePrivateDecision`: the decision, its wait and its initial
    /// projection in one transaction. Carries no dispatcher; resolves nothing.
    fn prepare_private_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, PrivateLedgerError>;
    /// `ConsumePrivateDecision`: the one-use consume and its effect row at
    /// `prepared`. `Execute` on the first fresh commit only.
    #[allow(clippy::too_many_arguments)]
    fn consume_private_decision(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError>;
    /// `PreparePrivateEffect`: the intent of a granted read, no decision.
    fn prepare_private_effect(
        &mut self,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError>;
    /// `AdvancePrivateEffect`: the row's own dispatcher records what it knows.
    fn advance_private_effect(
        &mut self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError>;
    /// `ReadPrivateEffect`: the retained row or `None`, after resolution.
    fn read_private_effect(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        dispatcher: PrivateDispatcherId,
    ) -> Result<PrivateResolved<Option<PrivateEffectOutcome>>, PrivateLedgerError>;
    /// The retained row alone: no dispatcher, no resolution, no write.
    fn private_effect(
        &self,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<PrivateEffectRecord>, BotLedgerError>;
    /// The decision that asked for one private read, by its operation id.
    /// Exact retained history; it grants nothing.
    fn private_decision_for_operation(
        &self,
        owner: BotOwner,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<DecisionRecord>, BotLedgerError>;
}

impl PrivateEffectLedger for RunsDb {
    fn announce_private_dispatcher(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<Vec<PrivateEffectRecord>, PrivateLedgerError> {
        RunsDb::announce_private_dispatcher(self, owner, dispatcher)
    }
    fn prepare_private_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, PrivateLedgerError> {
        RunsDb::prepare_private_decision(self, binding, proof, boot, base)
    }
    fn consume_private_decision(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        RunsDb::consume_private_decision(
            self, expected, transition, dispatcher, proposal, fence, proof, boot,
        )
    }
    fn prepare_private_effect(
        &mut self,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        RunsDb::prepare_private_effect(self, dispatcher, proposal, fence, proof)
    }
    fn advance_private_effect(
        &mut self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        RunsDb::advance_private_effect(self, key, dispatcher, transition)
    }
    fn read_private_effect(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        dispatcher: PrivateDispatcherId,
    ) -> Result<PrivateResolved<Option<PrivateEffectOutcome>>, PrivateLedgerError> {
        RunsDb::read_private_effect(self, run, operation, dispatcher)
    }
    fn private_effect(
        &self,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
        RunsDb::private_effect(self, run, operation)
    }
    fn private_decision_for_operation(
        &self,
        owner: BotOwner,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<DecisionRecord>, BotLedgerError> {
        RunsDb::bot_private_decision_for_operation(self, owner, run, operation)
    }
}

/// One private effect reaching a final stage, as
/// `delegate.private_operation.settled` carries it. Identifiers, closed
/// tokens, a flag and a size: nothing here can hold a label, a title, a
/// vendor reference, a link, a secret or content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivateOperationSettled {
    pub run: RunId,
    pub bot: koru_delegate_core::BotId,
    pub operation: uuid::Uuid,
    pub connection: koru_delegate_core::ConnectionId,
    pub kind: koru_delegate_core::PrivateOperationKind,
    /// `confirmed`, `refused` or `unknown`; never an open stage.
    pub stage: koru_delegate_core::PrivateEffectStage,
    pub refusal: Option<koru_delegate_core::PrivateEffectRefusal>,
    pub withheld: Option<bool>,
    pub decision: Option<koru_delegate_core::decisions::DecisionId>,
    pub outcome_bytes: Option<u32>,
    /// When the ledger closed the row.
    pub resolved_at_ms: i64,
}

impl PrivateOperationSettled {
    /// `None` while the row is open: only a final stage is a fact.
    pub fn of(record: &PrivateEffectRecord) -> Option<Self> {
        record.stage.is_final().then_some(Self {
            run: record.key.run,
            bot: record.bot,
            operation: record.key.operation_id,
            connection: record.operation.connection,
            kind: record.operation.operation,
            stage: record.stage,
            refusal: record.refusal,
            withheld: record.withheld,
            decision: record.decision,
            outcome_bytes: record.outcome_bytes,
            resolved_at_ms: record.resolved_at_ms.unwrap_or(record.updated_at_ms),
        })
    }

    /// The one event id of this effect's settlement. Derived from the Run and
    /// the operation, so a retry after a lost answer, a replayed advance and a
    /// later process all publish the same id and the journal keeps one row.
    /// Every final stage is final, so the fact under that id never changes.
    pub fn event_id(&self) -> koru_delegate_core::EventId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"koru.bot.private.settled.event.v1");
        hasher.update(self.run.as_uuid().as_bytes());
        hasher.update(self.operation.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        koru_delegate_core::EventId(uuid::Builder::from_random_bytes(bytes).into_uuid())
    }
}
