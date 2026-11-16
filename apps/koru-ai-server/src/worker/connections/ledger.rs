// SPDX-License-Identifier: MPL-2.0

//! The delegate's effect ledger, as the dispatcher reaches it: the one
//! command `AdvancePrivateEffect` on the private Bot channel (IPC 18).

use koru_connections::{EffectLedger, LedgerError};
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
use koru_delegate_core::{
    PrivateDispatcherId, PrivateEffectKey, PrivateEffectTransition, PrivateEffectView,
};
use std::sync::Arc;

pub(super) struct DelegateEffectLedger {
    port: Arc<dyn BotDelegatePort>,
}
impl DelegateEffectLedger {
    pub(super) fn new(port: Arc<dyn BotDelegatePort>) -> Self {
        Self { port }
    }
}

impl EffectLedger for DelegateEffectLedger {
    fn advance(
        &self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateEffectView, LedgerError> {
        let answer = self.port.execute(BotCommand::AdvancePrivateEffect {
            key: key.clone(),
            dispatcher,
            transition: *transition,
        });
        match answer {
            // Only this row's own view is an answer about this row.
            Ok(BotReply::PrivateEffect { effect })
                if effect.key == *key && effect.dispatcher == dispatcher =>
            {
                Ok(effect)
            }
            // Any other reply says nothing about the row: the advance may
            // have committed, so the same advance is sent again.
            Ok(_) => Err(LedgerError::Unavailable),
            Err(failure) if failure.code == "bot_private_effect_stale" => Err(LedgerError::Stale),
            // A typed, non-retryable refusal committed nothing.
            Err(failure) if failure.code.starts_with("bot_") && !failure.retryable => {
                Err(LedgerError::Refused)
            }
            Err(_) => Err(LedgerError::Unavailable),
        }
    }
}
