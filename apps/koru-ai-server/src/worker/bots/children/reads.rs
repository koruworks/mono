// SPDX-License-Identifier: MPL-2.0
//! The two host-fed reads of M6 (contract 7.1): a parent conversation's
//! request cards and a child conversation's origin line.
//!
//! The writer reads ai.db (the request rows, names, the child replies' own
//! ends, Needs-you) and hands a [`CardsJob`] to preparation's lane; the job
//! asks the delegate for the requests' current state (`ReadChildRequests`,
//! one call per parent Run) and, for a completed request, the child Run's
//! saved results, then answers its caller itself. Nothing it reads is stored.
//! No row holds a task, a title or a page: ids, tokens and counts.
use super::*;
use koru_ai_core::bots::{
    BotChildOrigin, BotChildOutcome, BotChildProgress, BotChildRequestCard, BotChildResult,
};
use koru_delegate_api::bots::BotCommand as DelegateCommand;
use koru_delegate_api::wire::TimelineQuery;
use koru_delegate_core::children::{ChildRequestReceipt, ChildRequestState, ChildSettledBy};
use koru_delegate_core::{BotAdmissionState, BotRunKey};

/// How a child reply ended in the AI transcript, as the card needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum ChildEnd {
    /// Still running, or never started.
    Open,
    Completed,
    /// The person's Stop.
    Canceled,
    /// Ended by P4.2's "needed you" closing.
    NeededYou,
    Failed,
}

/// One request row with what ai.db knows of it.
#[derive(Clone, Debug)]
pub(in crate::worker) struct CardRow {
    pub row: BotChildChat,
    pub recipient_name: String,
    pub parent_name: String,
    pub end: ChildEnd,
    pub needs_you: bool,
}

/// Which read is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::worker) enum CardsRead {
    Cards,
    Origin,
}

pub(in crate::worker) struct CardsJob {
    pub(in crate::worker) owner: koru_delegate_core::BotOwner,
    pub(in crate::worker) home: WorkspaceId,
    pub(in crate::worker) rows: Vec<CardRow>,
    pub(in crate::worker) read: CardsRead,
    pub(in crate::worker) delegate: Port,
    pub(in crate::worker) deadline: Instant,
    pub(in crate::worker) reply: SyncSender<ApiResult<BotResponse>>,
}

fn unavailable() -> Failure {
    failure(
        "bot_unavailable",
        "The Bot's execution owner could not read the requests.",
        true,
    )
}

impl CardsJob {
    pub(in crate::worker) fn run(self) {
        let answer = catch_unwind(AssertUnwindSafe(|| self.answer()))
            .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)));
        let _ = self.reply.send(answer);
    }

    fn within(&self) -> ApiResult<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(unavailable());
        }
        Ok(left)
    }

    fn answer(&self) -> ApiResult<BotResponse> {
        let mut receipts: std::collections::BTreeMap<_, Vec<ChildRequestReceipt>> =
            Default::default();
        for row in &self.rows {
            let parent = (row.row.parent_bot, row.row.parent_run);
            if receipts.contains_key(&parent) {
                continue;
            }
            let reply = self
                .delegate
                .execute_within(
                    DelegateCommand::ReadChildRequests {
                        subject: BotRunKey {
                            owner: self.owner,
                            bot: parent.0,
                            run: parent.1,
                        },
                    },
                    self.within()?,
                )
                .map_err(|_| unavailable())?;
            let BotReply::ChildRequests { requests, .. } = reply else {
                return Err(unavailable());
            };
            receipts.insert(parent, requests);
        }
        let mut cards = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let receipt = receipts
                .get(&(row.row.parent_bot, row.row.parent_run))
                .and_then(|list| {
                    list.iter()
                        .find(|receipt| receipt.child_request == row.row.child_request)
                })
                .ok_or_else(unavailable)?;
            let outcome = outcome(receipt, row.end);
            let result = match outcome {
                Some(BotChildOutcome::Completed) => self.result(row, receipt)?,
                _ => BotChildResult::None,
            };
            cards.push((row, receipt, outcome, result));
        }
        Ok(match self.read {
            CardsRead::Cards => BotResponse::ChildRequests {
                requests: cards
                    .into_iter()
                    .map(|(row, receipt, outcome, result)| BotChildRequestCard {
                        child_request_id: row.row.child_request,
                        parent_assistant_message_id: row.row.parent_assistant_message,
                        recipient_bot_id: row.row.recipient_bot,
                        recipient_name: row.recipient_name.clone(),
                        child_chat_id: row.row.chat,
                        state: receipt.state,
                        child: progress(receipt, row.needs_you),
                        outcome,
                        clock: receipt.clock,
                        result,
                        needs_you: row.needs_you && receipt.state == ChildRequestState::Open,
                    })
                    .collect(),
            },
            CardsRead::Origin => BotResponse::ChildOrigin {
                origin: cards
                    .into_iter()
                    .next()
                    .map(|(row, receipt, outcome, _)| BotChildOrigin {
                        child_request_id: row.row.child_request,
                        parent_bot_id: row.row.parent_bot,
                        parent_name: row.parent_name.clone(),
                        parent_chat_id: row.row.parent_chat,
                        state: receipt.state,
                        outcome,
                    }),
            },
        })
    }

    /// What a completed request handed over: the pages the child Run saved,
    /// counted from its header (each document once), at most 20.
    fn result(&self, row: &CardRow, receipt: &ChildRequestReceipt) -> ApiResult<BotChildResult> {
        let reply = self
            .delegate
            .execute_within(
                DelegateCommand::Read {
                    key: BotRunKey {
                        owner: self.owner,
                        bot: receipt.recipient,
                        run: row.row.child_run,
                    },
                    window: TimelineQuery {
                        from: 0,
                        len: 1,
                        workspace: Some(self.home),
                    },
                },
                self.within()?,
            )
            .map_err(|_| unavailable())?;
        let BotReply::Run { view } = reply else {
            return Err(unavailable());
        };
        if view.header.run != row.row.child_run {
            return Err(unavailable());
        }
        let pages = view
            .header
            .native_results
            .iter()
            .map(|entry| entry.document)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            .min(20);
        Ok(if pages == 0 {
            BotChildResult::NoPageSaved
        } else {
            BotChildResult::Saved { pages: pages as u8 }
        })
    }
}

/// Where the child is, from the delegate's admission state in the same
/// snapshot as the request, and Needs-you from the AI transcript.
pub(in crate::worker) fn progress(
    receipt: &ChildRequestReceipt,
    needs_you: bool,
) -> BotChildProgress {
    match receipt.child {
        BotAdmissionState::Queued => BotChildProgress::Queued,
        BotAdmissionState::Reserved | BotAdmissionState::Executing if needs_you => {
            BotChildProgress::NeedsYou
        }
        BotAdmissionState::Reserved | BotAdmissionState::Executing => BotChildProgress::Running,
        BotAdmissionState::Settled | BotAdmissionState::Refused => BotChildProgress::Finished,
    }
}

/// The outcome a reader sees, derived at read time (contract 6.1).
/// Correction W6: for `child_ended`, the child's end is read from its AI
/// reply's durable ending, which the delegate's terminal status follows.
pub(in crate::worker) fn outcome(
    receipt: &ChildRequestReceipt,
    end: ChildEnd,
) -> Option<BotChildOutcome> {
    if receipt.state != ChildRequestState::Settled {
        return None;
    }
    Some(match receipt.settled_by {
        Some(ChildSettledBy::TimedOut) => BotChildOutcome::TimedOut,
        Some(ChildSettledBy::ParentStopped) => BotChildOutcome::ParentStopped,
        Some(ChildSettledBy::ChildEnded) | None => match (receipt.child, end) {
            (BotAdmissionState::Refused, _) => BotChildOutcome::Failed,
            (_, ChildEnd::Completed) => BotChildOutcome::Completed,
            (_, ChildEnd::Canceled) => BotChildOutcome::Canceled,
            (_, ChildEnd::NeededYou) => BotChildOutcome::NeededYou,
            (_, ChildEnd::Failed | ChildEnd::Open) => BotChildOutcome::Failed,
        },
    })
}

/// How an AI attempt ended, for the card.
pub(in crate::worker) fn child_end(
    attempt: Option<&InferenceAttempt>,
    person_stopped: bool,
) -> ChildEnd {
    let Some(attempt) = attempt else {
        return ChildEnd::Open;
    };
    match attempt.status {
        AttemptStatus::Complete => ChildEnd::Completed,
        AttemptStatus::Cancelled if person_stopped => ChildEnd::Canceled,
        AttemptStatus::Interrupted
            if matches!(
                attempt.stop_reason.as_deref(),
                Some(koru_ai_core::STOP_DECISION_NEEDED_YOU | koru_ai_core::STOP_DECISION_DECLINED)
            ) =>
        {
            ChildEnd::NeededYou
        }
        status if status.is_terminal() => ChildEnd::Failed,
        _ => ChildEnd::Open,
    }
}
