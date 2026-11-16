// SPDX-License-Identifier: MPL-2.0
//! A decision's card on the existing shared approval controls, and the
//! trusted hand-over of a prepared decision's receipt.
//!
//! The card never qualifies cleanup or enrollment: it is written after the
//! enrollment receipt commits (its own transaction), and a failure to write it
//! changes no enrollment answer. The rollover caller writes it again if needed
//! (O129). Exactly one card per decision; retries replay the same row.
use super::*;
use koru_ai_sqlite::{BotDecisionCard, BotNativeDecisionLink};
use koru_delegate_api::bots::DecisionReceiptView;
use koru_delegate_core::decisions::{DecisionBindingV1, DecisionValidate};

fn ai_owner(owner: koru_delegate_core::BotOwner) -> BotOwner {
    BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

impl AiWorkerClient {
    /// Trusted composition only, deliberately absent from `AiServerPort` and
    /// HTTP: the decision preparer hands over the exact receipt the delegate
    /// answered, so a card can name its binding, human and revision. A native
    /// decision's prompt row becomes its card. Nothing here grants an answer.
    pub fn record_bot_decision(&self, view: DecisionReceiptView) -> ApiResult<()> {
        let (reply, response) = sync_channel(1);
        self.sender
            .try_send(Envelope::DecisionRecorded {
                view: Box::new(view),
                reply,
            })
            .map_err(queue_failure)?;
        receive_api(response)
    }
}

impl WorkerState {
    /// Write (or replay) the Koru decision's card for an enrolled wait and
    /// show it in the live conversation. `None` when it cannot be written now.
    pub(in crate::worker) fn ensure_bot_decision_card(
        &mut self,
        key: &BotWaitEnrollmentKey,
    ) -> Option<BotDecisionCard> {
        let owner = ai_owner(key.owner);
        let receipt = self
            .repository
            .bot_readable_submission_for_run(owner, key.bot, key.run)
            .ok()??;
        let p = &receipt.intent.submission.provenance;
        let attempt = InferenceAttemptId::parse(&p.ai_attempt_id?.to_string())?;
        let chat = ChatId::parse(&p.chat_id.to_string())?;
        let assistant = MessageId::parse(&p.assistant_message_id?.to_string())?;
        let row = crate::worker::agents::agent_row(
            ToolInvocationId::new(),
            attempt,
            chat,
            assistant,
            AgentToolKind::Approval,
            ToolApproval::Pending,
            Timestamp::now(),
        );
        let card = match self.repository.record_bot_decision_card(
            owner,
            key,
            &row,
            Timestamp::now(),
        ) {
            Ok(card) => card,
            Err(error) => {
                tracing::debug!(target: "koru_ai::bots", code = error.code(), "decision card not written yet");
                return None;
            }
        };
        if card.invocation == row.id
            && let Ok(Some(activity)) = self.repository.tool_activity(card.invocation)
        {
            self.publish(AiEvent::ToolActivity {
                attempt_id: attempt,
                activity,
            });
        }
        Some(card)
    }

    pub(in crate::worker) fn record_bot_decision(
        &mut self,
        view: DecisionReceiptView,
        reply: SyncSender<ApiResult<()>>,
    ) {
        let result = (|| -> ApiResult<()> {
            view.receipt
                .validate()
                .map_err(|_| bot_failure(BotError::Invalid))?;
            let (owner, bot, run) = match &view.receipt.binding {
                DecisionBindingV1::KoruEdit(v) => (v.owner, v.bot, v.run),
                DecisionBindingV1::NativeApproval(v) => (v.owner, v.bot, v.run),
                // M5 piece A: no private-operation card is recorded until the
                // AI worker's piece (F) composes its owner.
                DecisionBindingV1::PrivateOperation(_) => {
                    return Err(bot_failure(BotError::Invalid));
                }
            };
            // The receipt's Run must be one this AI owns, through its sole
            // admission decoder; a foreign or unknown Run is refused.
            self.repository
                .bot_readable_submission_for_run(ai_owner(owner), bot, run)
                .map_err(bot_failure)?
                .ok_or_else(not_found)?;
            if let DecisionBindingV1::NativeApproval(native) = &view.receipt.binding {
                let link = BotNativeDecisionLink {
                    bot,
                    run,
                    decision: view.receipt.decision,
                    revision: view.receipt.state.revision,
                    prompt: native.prompt,
                };
                self.repository
                    .link_bot_native_decision_card(ai_owner(owner), &link, Timestamp::now())
                    .map_err(bot_failure)?;
            }
            if !self.decision_waits.remember(view) {
                return Err(bot_failure(BotError::Unavailable));
            }
            Ok(())
        })();
        let _ = reply.send(result);
    }
}
