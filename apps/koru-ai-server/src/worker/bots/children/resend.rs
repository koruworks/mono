// SPDX-License-Identifier: MPL-2.0
//! The coordination job's one new case (contract A2): a child request's
//! `start` receipt still `prepared` is resent as `RequestChild` for its whole
//! ask. Found by the restart scan (`bot_unresolved_coordination`) like every
//! other prepared receipt; the delegate creates the rows once and answers a
//! resend `already_created` with the same frozen intent.
use super::job::{AskJob, Caller, Step};
use super::*;
use koru_ai_core::bots::BotReceiptStage;

/// One resend asks the delegate once, within this.
const RESEND_DEADLINE: Duration = Duration::from_secs(5);

impl WorkerState {
    /// `true` when a resend of the ask was handed to the pool.
    pub(in crate::worker) fn queue_child_resend(
        &mut self,
        bot: &Bot,
        receipt: &BotSubmissionReceipt,
    ) -> bool {
        let owner = BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        };
        let request = receipt.intent.submission.provenance.submission;
        let Ok(Some(row)) = self
            .repository
            .bot_child_chat(owner, ChildRequestId::from_uuid(request.as_uuid()))
        else {
            // A submission naming a parent that this worker never prepared as
            // an ask is never sent: the ledger would refuse it anyway.
            return false;
        };
        let Ok(rows) = self
            .repository
            .bot_child_ask(owner, row.parent_run, row.ask_key)
        else {
            return false;
        };
        if rows.is_empty() || rows.iter().any(|r| r.stage != BotReceiptStage::Prepared) {
            return false;
        }
        let held: Vec<(BotId, BotRequestId)> = rows
            .iter()
            .map(|r| {
                (
                    r.recipient_bot,
                    BotRequestId::from_uuid(r.child_request.as_uuid()),
                )
            })
            .collect();
        if held.iter().any(|key| self.bot_coordination.contains(key)) {
            return false;
        }
        let (Some(authority), Some(delegate), Some(_)) = (
            self.bot_authority.clone(),
            self.bot_delegate.clone(),
            self.provider_admin.as_ref(),
        ) else {
            return false;
        };
        let request = match self.child_request_of(owner, &rows) {
            Ok(request) => request,
            Err(_) => {
                // The asking turn ended before the ask reached the delegate
                // (section 9 row 3): it can never be admitted. Its receipts
                // are refused and the children's replies end.
                if !self.child_parent_live(owner, &rows[0])
                    && let Ok(rows) = self.repository.reject_bot_child_ask(
                        owner,
                        row.parent_run,
                        row.ask_key,
                        "permission_denied",
                        None,
                        BotAdmissionRefusal::Rejected,
                        Timestamp::now(),
                    )
                {
                    self.fail_child_replies(owner, &rows);
                }
                return false;
            }
        };
        let job = Box::new(AskJob {
            step: Step::Send {
                owner: request.parent.owner,
                request: Box::new(request),
            },
            caller: Caller::Resend { held: held.clone() },
            owner,
            parent_run: row.parent_run,
            ask_key: row.ask_key,
            authority,
            delegate,
            deadline: Instant::now() + RESEND_DEADLINE,
        });
        for key in held {
            self.bot_coordination.insert(key);
        }
        self.submit_child_job(job);
        true
    }

    /// Whether the asking Run's AI attempt still runs.
    fn child_parent_live(&self, owner: BotOwner, row: &BotChildChat) -> bool {
        let Ok(Some(parent)) =
            self.repository
                .bot_submission_for_run(owner, row.parent_bot, row.parent_run)
        else {
            return true;
        };
        let Some(attempt) = parent
            .intent
            .submission
            .provenance
            .ai_attempt_id
            .and_then(|id| InferenceAttemptId::parse(&id.to_string()))
        else {
            return false;
        };
        !matches!(self.repository.attempt(attempt), Ok(Some(a)) if a.status.is_terminal())
    }

    /// A resend's result came back: its receipts leave the coordination set,
    /// and one still owed waits for the backed-off pass.
    pub(super) fn release_child_resend(&mut self, held: &[(BotId, BotRequestId)], owner: BotOwner) {
        for key in held {
            self.bot_coordination.remove(key);
        }
        if let Some((bot, request)) = held.first() {
            self.note_bot_coordination(owner, *bot, *request);
        }
    }
}
