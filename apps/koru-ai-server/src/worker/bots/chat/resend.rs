// SPDX-License-Identifier: MPL-2.0
//! The automatic re-send of a Bot task whose pictures the runtime refused
//! (O30: "same as an AI chat").
//!
//! A Bot task's first input is frozen under one identity, so it is never
//! sent twice; a definite refusal of it means nothing ran. When the refusal
//! is one that sending the pictures as paths can get past
//! ([`BotInputRefusal::concerns_pictures`]), the AI side does what the
//! person's Retry does, once: it starts the same task again as a new linked
//! attempt and Run, with the pictures as paths and the "images sent as
//! files" notice ([`super::WorkerState::retries_refused_pictures`] already
//! sends them so for any Retry of such a reply).
//!
//! There is no second mechanism and no new record. The re-send is an
//! ordinary `RetryTurn` dispatched through the writer's own command path:
//! fresh preparation (owner, workspace, Role and pages, model discovery), the
//! guard with its lifecycle holds, and `retry_turn`'s durable records.
//!
//! **A re-send is owed only in the worker run that saw the refusal**, while
//! it drove the task live (`LiveChat::resend`, set where the refusal ends the
//! task). A restart's recovery never makes one owed, for either kind: what a
//! person did about it in between (a Stop on the refused reply, a Pause, a
//! Trash and its Restore, an Archive and its Unarchive) is not durable, and
//! durable state alone would send an old message by itself after the person
//! had already refused it (the fourth lifecycle review). The cost, accepted:
//! a restart between the refusal and the re-send loses the automatic
//! re-send; the refused reply stays as it ended, and the person's Retry
//! sends it. Durable state still decides whether the one owed in this run
//! may go: the attempt failed with a picture refusal (or ended refused as
//! stale, below), it was not itself such a re-send, its task carried a
//! picture, and its reply is still the conversation's last message (a
//! Retry, automatic or the person's, writes a newer one), so a restart
//! after the re-send finds the newer reply and nothing is ever sent twice.
//!
//! The re-send waits until the refused task's Run is proven settled: its
//! live state has ended, and the execution owner answers its admission
//! `Settled` or `Refused` (the check a Retry's own Submit makes before it may
//! continue a task). A person's Stop of the refused task cancels a re-send
//! that has not been prepared yet; the writer checks again, in the same
//! step that would prepare it, that it is still owed. Once this run is done
//! with a re-send (declined, refused by a hold or a pause, sent, or given
//! up) nothing else in the run makes it owed again (`Resends::closed`).
//!
//! The same mechanism, once, for a task its owner refused as stale (P1.7,
//! the third lifecycle review's F4): its Submit arrived after the Bot's
//! revision moved, typically because another conversation's Trash fenced
//! what it refused at that moment, so the owner answered `bot_stale` and the
//! refusal fence kept that reason (`rejected` with `stale`). Nothing of it
//! ran and nothing about it is wrong, so it is sent again at the current
//! revision as a Retry. Seen in this run, it may go while: the attempt ended
//! interrupted, its receipt is rejected as stale, its reply is still the
//! conversation's last message, and it is not itself a Retry of a reply
//! refused as stale, so a message is sent again at most once however often
//! the revision moves under it. The owner never admitted it, so the settled
//! probe's Missing is the expected answer, and the Retry's continuation
//! needs no owner proof that the Run settled (`previous_never_admitted`).
//! The Retry's guard and preparation decide everything else: a Bot paused,
//! archived, trashed or mid-operation, or a conversation in Trash or held,
//! refuses it, and then nothing is sent.
use super::*;
use koru_ai_api::AiResponseStream;
use koru_delegate_core::BotInputRefusal;
use std::sync::mpsc::{Receiver, TryRecvError};

/// How many times a re-send asks whether the refused Run settled, or retries
/// a preparation that answered a retryable failure, before it gives up and
/// leaves the refused reply as it is.
const ROUNDS: u32 = 12;

fn backoff(round: u32) -> Duration {
    Duration::from_millis(250) * (1u32 << round.min(7))
}

#[derive(Default)]
pub(in crate::worker) struct Resends {
    owed: HashMap<InferenceAttemptId, Resend>,
    /// Refused attempts this run is done with: their re-send was declined by
    /// a person's Stop, refused (a hold, a pause), sent, or given up. None of
    /// them is owed again in this run, whatever happens later in it (a
    /// Resume, an owner answering again).
    closed: HashSet<InferenceAttemptId>,
    /// Each automatic Retry's operation, so the writer recognizes it and
    /// checks once more, as it prepares it, that it is still owed.
    operations: HashMap<Uuid, InferenceAttemptId>,
}

struct Resend {
    operation: Uuid,
    due: Instant,
    rounds: u32,
    stage: Stage,
}

enum Stage {
    Probe,
    Probing,
    Dispatched(Receiver<ApiResult<AiResponseStream>>),
}

/// Why a task is sent again by itself, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The runtime refused its pictures: sent again with them as files.
    Pictures,
    /// The owner refused its Submit because the Bot's revision moved after
    /// the task was prepared (`bot_stale`, kept as the receipt's `stale`
    /// refusal), for example a sibling conversation's Trash fencing what it
    /// refused at the same moment. Nothing ran and nothing about the task is
    /// wrong: sent again at the current revision.
    Stale,
}

/// What the re-send of one refused attempt sends, read from durable state.
struct Plan {
    chat: ChatId,
    request: RetryTurnRequest,
    key: BotSubmissionKey,
    kind: Kind,
}

/// One finite owner call: whether the refused task's admission is settled.
pub(in crate::worker) struct ResendProbe {
    attempt: InferenceAttemptId,
    key: BotSubmissionKey,
    delegate: Arc<dyn BotDelegatePort>,
}

pub(in crate::worker) struct ResendProbed {
    attempt: InferenceAttemptId,
    settled: Settledness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settledness {
    Settled,
    /// Still queued or executing, or no answer: ask again later.
    NotYet,
    /// The owner does not know the task: a Retry could not continue it.
    Never,
}

impl ResendProbe {
    pub(in crate::worker) fn run(self) -> ResendProbed {
        let key = self.key.clone();
        let settled = catch_unwind(AssertUnwindSafe(|| {
            match self
                .delegate
                .execute(DelegateCommand::Reconcile { key: key.clone() })
            {
                Ok(BotReply::Admission { admission }) if admission.key == key => {
                    if matches!(
                        admission.state,
                        BotAdmissionState::Settled | BotAdmissionState::Refused
                    ) {
                        Settledness::Settled
                    } else {
                        Settledness::NotYet
                    }
                }
                Ok(BotReply::Missing) => Settledness::Never,
                _ => Settledness::NotYet,
            }
        }))
        .unwrap_or(Settledness::NotYet);
        ResendProbed {
            attempt: self.attempt,
            settled,
        }
    }
}

/// The context of an automatic re-send. It acts for the Bot's fixed owner,
/// whose authority preparation inspects afresh; the label says it is the
/// AI owner's own Retry, never a client's.
fn resend_context(attempt: InferenceAttemptId) -> CommandContext {
    CommandContext::from(RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "koru-ai-bot-resend".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: format!("bot-resend-{attempt}"),
        deadline_millis: None,
    })
}

impl WorkerState {
    /// A Bot conversation task this run drove live, and saw end
    /// automatically, has settled its live state: its re-send is owed if the
    /// durable state says one may go. Only this run's own refusals reach
    /// here; a task a restart rebuilt never does.
    pub(in crate::worker) fn bot_chat_refused(&mut self, id: InferenceAttemptId) {
        if self.bot_resends.owed.contains_key(&id)
            || self.bot_resends.closed.contains(&id)
            || self.bot_resend_plan(id).is_none()
        {
            return;
        }
        tracing::info!(
            target: "koru_ai::bots",
            attempt = %id,
            "Bot task's first input was refused; it is sent again once it settles"
        );
        self.bot_resends.owed.insert(
            id,
            Resend {
                operation: Uuid::new_v4(),
                due: Instant::now(),
                rounds: 0,
                stage: Stage::Probe,
            },
        );
    }

    /// The person stopped the refused task: nothing is sent again for it.
    pub(in crate::worker) fn decline_bot_resend(&mut self, id: InferenceAttemptId) {
        self.close_bot_resend(id);
    }

    /// This run is done with `id`'s re-send, whichever way it ended.
    fn close_bot_resend(&mut self, id: InferenceAttemptId) {
        self.bot_resends.closed.insert(id);
        self.bot_resends.owed.remove(&id);
    }

    /// In `retry_turn`, before anything is prepared: an automatic re-send
    /// is prepared only while it is still owed, so it never makes a second
    /// Retry beside the person's and never outlives the person's Stop.
    pub(in crate::worker) fn require_bot_resend_owed(&self, operation: Uuid) -> ApiResult<()> {
        let Some(attempt) = self.bot_resends.operations.get(&operation) else {
            return Ok(());
        };
        if self.bot_resends.closed.contains(attempt) || self.bot_resend_plan(*attempt).is_none() {
            return Err(failure(
                "invalid_request",
                "This automatic re-send is no longer owed.",
                false,
            ));
        }
        Ok(())
    }

    /// Advance every owed re-send: ask whether its refused Run settled, and
    /// read the answer of a dispatched Retry. Nothing here waits.
    pub(in crate::worker) fn schedule_bot_resends(&mut self) {
        if self.bot_resends.owed.is_empty() {
            return;
        }
        let now = Instant::now();
        // One probe outstanding at a time: it runs on the shared
        // administration lane, which the other background passes share and
        // which runs one job at a time anyway, so several re-sends owed at
        // once never fill it. (The re-send's Retry itself is prepared on
        // preparation's lane, as a person's Retry is.)
        let mut probing = self
            .bot_resends
            .owed
            .values()
            .any(|resend| matches!(resend.stage, Stage::Probing));
        let ids: Vec<_> = self.bot_resends.owed.keys().copied().collect();
        for id in ids {
            let Some(resend) = self.bot_resends.owed.get_mut(&id) else {
                continue;
            };
            match &resend.stage {
                Stage::Dispatched(answer) => match answer.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    Ok(Ok(_stream)) => {
                        // The Retry is durable; the conversation shows it.
                        tracing::info!(target: "koru_ai::bots", attempt = %id, "Bot task sent again with its pictures as files");
                        self.close_bot_resend(id);
                    }
                    Ok(Err(error)) if error.retryable && resend.rounds < ROUNDS => {
                        resend.rounds += 1;
                        resend.due = now + backoff(resend.rounds);
                        resend.stage = Stage::Probe;
                    }
                    Ok(Err(error)) => {
                        tracing::info!(target: "koru_ai::bots", attempt = %id, code = %error.code, "Bot task's automatic re-send was refused");
                        self.close_bot_resend(id);
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.close_bot_resend(id);
                    }
                },
                Stage::Probe if resend.due <= now && !probing => {
                    let Some(plan) = self.bot_resend_plan(id) else {
                        self.close_bot_resend(id);
                        continue;
                    };
                    let (Some(delegate), Some(pool)) = (&self.bot_delegate, &self.provider_admin)
                    else {
                        self.close_bot_resend(id);
                        continue;
                    };
                    let job = ResendProbe {
                        attempt: id,
                        key: plan.key,
                        delegate: delegate.clone(),
                    };
                    let resend = self.bot_resends.owed.get_mut(&id).expect("owed");
                    if pool
                        .try_submit(ProviderAdminJob::BotResendProbe(job))
                        .is_ok()
                    {
                        resend.stage = Stage::Probing;
                        probing = true;
                    } else {
                        resend.due = now + Duration::from_millis(250);
                    }
                }
                Stage::Probe | Stage::Probing => {}
            }
        }
    }

    pub(in crate::worker) fn complete_bot_resend_probe(&mut self, probed: ResendProbed) {
        let id = probed.attempt;
        if !self.bot_resends.owed.contains_key(&id) {
            return;
        }
        let stale = self
            .bot_resend_plan(id)
            .is_some_and(|plan| plan.kind == Kind::Stale);
        let Some(resend) = self.bot_resends.owed.get_mut(&id) else {
            return;
        };
        match probed.settled {
            Settledness::Settled => {}
            // A task refused behind its fence was never admitted: the owner
            // not knowing it is the proof nothing of it runs.
            Settledness::Never if stale => {}
            Settledness::NotYet if resend.rounds < ROUNDS => {
                resend.rounds += 1;
                resend.due = Instant::now() + backoff(resend.rounds);
                resend.stage = Stage::Probe;
                return;
            }
            Settledness::NotYet | Settledness::Never => {
                tracing::info!(target: "koru_ai::bots", attempt = %id, "Bot task's refused Run did not settle; it is not sent again");
                self.close_bot_resend(id);
                return;
            }
        }
        let operation = resend.operation;
        let Some(plan) = self.bot_resend_plan(id) else {
            self.close_bot_resend(id);
            return;
        };
        let (reply, answer) = sync_channel(1);
        if let Some(resend) = self.bot_resends.owed.get_mut(&id) {
            resend.stage = Stage::Dispatched(answer);
        }
        self.bot_resends.operations.insert(operation, id);
        let mut request = plan.request;
        request.operation_id = operation;
        // The person's Retry path, prepared afresh like any command.
        self.handle_command(WorkerCommand::RetryTurn {
            context: resend_context(id),
            chat: plan.chat,
            request,
            reply,
        });
    }

    /// Whether `id` is a refused Bot task whose re-send is owed now, from
    /// durable state only, and the Retry that sends it again.
    fn bot_resend_plan(&self, id: InferenceAttemptId) -> Option<Plan> {
        let attempt = self.repository.attempt(id).ok()??;
        let (chat, reply) = (attempt.chat_id?, attempt.assistant_message_id?);
        let receipt = self.repository.bot_submission_for_attempt(id).ok()??;
        let kind = if attempt.status == AttemptStatus::Interrupted && refused_as_stale(&receipt) {
            // Exactly once: a Retry of a reply that was itself refused as
            // stale is never sent again by itself, however often the
            // revision moves under it.
            if self.retries_stale_refusal(id) {
                return None;
            }
            Kind::Stale
        } else {
            let refused = attempt
                .stop_reason
                .as_deref()
                .and_then(BotInputRefusal::from_code)?;
            if attempt.status != AttemptStatus::Failed || !refused.concerns_pictures() {
                return None;
            }
            // Exactly once: a re-send that already went as paths is not re-sent.
            if self.retries_refused_pictures(id) {
                return None;
            }
            Kind::Pictures
        };
        let messages = self.repository.messages(chat).ok()?;
        let failed = messages.last().filter(|last| last.id == reply)?;
        if kind == Kind::Pictures {
            // Paths can get past the refusal only if the task carried a picture.
            let user = messages
                .iter()
                .rev()
                .find(|m| m.ordinal < failed.ordinal && m.role == ChatRole::User)?;
            let attachments = self.attachment_inputs_by_message(chat).ok()?;
            koru_ai_provider_agent::native_pictures(attachments.get(&user.id)?.as_slice())
                .next()?;
        }
        let submission = &receipt.intent.submission;
        let bot = self
            .repository
            .bot_any_lifecycle(submission.provenance.bot)
            .ok()??
            .0;
        // The same task: its destination, reasoning and selected pages, read
        // again at their current versions as a Retry reads them.
        let explicit_refs = submission
            .request
            .context
            .iter()
            .filter_map(|item| match item {
                koru_delegate_core::RunContextItem::Document { document, .. }
                    if *document != bot.config.role.document =>
                {
                    Some(ContextRef::Document {
                        document_id: *document,
                    })
                }
                _ => None,
            })
            .collect();
        let destination = WorkspaceId::parse(&submission.policy.destination.as_uuid().to_string())?;
        Some(Plan {
            chat,
            key: BotSubmissionKey {
                owner: submission.provenance.owner,
                bot: submission.provenance.bot,
                request: submission.provenance.submission,
            },
            request: RetryTurnRequest {
                destination_workspace: Some(destination),
                operation_id: Uuid::nil(),
                message_id: reply,
                surface: attempt.surface,
                explicit_refs,
                reasoning_override: Some(attempt.requested_reasoning),
                web_search: false,
                replace_pinned_documents: true,
            },
            kind,
        })
    }

    /// Whether this attempt is a Retry of a reply whose task the owner
    /// refused as stale: the bound that keeps a re-send from being sent
    /// again. Only a Retry's reply names the reply it replaces.
    fn retries_stale_refusal(&self, id: InferenceAttemptId) -> bool {
        let Ok(Some(attempt)) = self.repository.attempt(id) else {
            return true;
        };
        let (Some(chat), Some(reply)) = (attempt.chat_id, attempt.assistant_message_id) else {
            return true;
        };
        let origin = match self.repository.message(reply) {
            Ok(Some(message)) => message.origin_message_id,
            // Unreadable: never guess that it may be sent again.
            _ => return true,
        };
        let Some(origin) = origin else {
            return false;
        };
        let Ok(attempts) = self.repository.attempts_for_chat(chat) else {
            return true;
        };
        attempts
            .into_iter()
            .find(|prior| prior.assistant_message_id == Some(origin))
            .is_none_or(|prior| {
                !matches!(
                    self.repository.bot_submission_for_attempt(prior.id),
                    Ok(Some(receipt)) if !refused_as_stale(&receipt)
                )
            })
    }
}

/// The owner refused this task's Submit as stale, behind its fence: never
/// admitted, nothing of it ran, and nothing about it is at fault.
fn refused_as_stale(receipt: &BotSubmissionReceipt) -> bool {
    receipt.stage == BotReceiptStage::Rejected
        && matches!(
            receipt.outcome,
            Some(BotExecutionOutcome::Rejected {
                reason: BotAdmissionRefusal::Stale,
                ..
            })
        )
}
