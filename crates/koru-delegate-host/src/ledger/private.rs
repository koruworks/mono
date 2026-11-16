// SPDX-License-Identifier: MPL-2.0

//! The private-operation half of the production ledger (M5, IPC 18). It
//! borrows the same connection every other domain does; never a second writer.
use super::SharedLedger;
use koru_delegate_core::decisions::{
    DecisionBindingV1, DecisionExpected, DecisionPublicationBase, DecisionTransitionId,
};
use koru_delegate_core::{
    BotExecutionFence, BotOwner, DelegateBootId, FreshBotPolicyProof, PrivateDispatcherId,
    PrivateEffectKey, PrivateEffectTransition, PrivateOperationProposal, RunId,
};
use koru_delegate_runtime::PrivateEffectLedger;
use koru_delegate_sqlite::{
    BotLedgerError, DecisionRecord, PrivateEffectOutcome, PrivateEffectRecord, PrivateLedgerError,
    PrivateResolved,
};

impl PrivateEffectLedger for SharedLedger {
    fn announce_private_dispatcher(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<Vec<PrivateEffectRecord>, PrivateLedgerError> {
        self.0
            .lock()
            .unwrap()
            .announce_private_dispatcher(owner, dispatcher)
    }
    fn prepare_private_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, PrivateLedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_private_decision(binding, proof, boot, base)
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
        self.0.lock().unwrap().consume_private_decision(
            expected, transition, dispatcher, proposal, fence, proof, boot,
        )
    }
    fn prepare_private_effect(
        &mut self,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_private_effect(dispatcher, proposal, fence, proof)
    }
    fn advance_private_effect(
        &mut self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        self.0
            .lock()
            .unwrap()
            .advance_private_effect(key, dispatcher, transition)
    }
    fn read_private_effect(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        dispatcher: PrivateDispatcherId,
    ) -> Result<PrivateResolved<Option<PrivateEffectOutcome>>, PrivateLedgerError> {
        self.0
            .lock()
            .unwrap()
            .read_private_effect(run, operation, dispatcher)
    }
    fn private_effect(
        &self,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
        self.0.lock().unwrap().private_effect(run, operation)
    }
    fn private_decision_for_operation(
        &self,
        owner: BotOwner,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<DecisionRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .bot_private_decision_for_operation(owner, run, operation)
    }
}
