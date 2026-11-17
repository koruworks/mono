// SPDX-License-Identifier: MPL-2.0
//! The ask, on the writer: prove the asking Run, prepare the whole ask in one
//! ai.db transaction, and settle its receipts to the delegate's answer.
//!
//! The writer never waits on the Store or the delegate. The page checks and
//! `RequestChild` run in [`super::job::AskJob`] on a pool lane; each phase
//! comes back here and re-proves the asking Run before it writes.
use super::job::{AskJob, Caller, Evidence, Step};
use super::*;
use koru_ai_core::bots::{BotChildAsk, BotChildAskItem, BotChildAskPrepared};
use koru_delegate_core::children::{
    ChildPolicyRef, ChildRequest, ChildRequestBindingV1, ChildRequestItem, child_task_digest,
};
use koru_delegate_core::decisions::DecisionVersionV1;
use koru_delegate_core::{ActorRef, BotAdmissionView, BotSubmissionKey, ChildParent};

/// The asking Run, proven against ai.db: its retained admission, its live AI
/// attempt and that attempt's current execution with its turn.
pub(super) struct Subject {
    pub owner: BotOwner,
    pub parent: BotSubmissionReceipt,
    pub profile: koru_ai_core::ProfileId,
}

fn ai_owner(owner: koru_delegate_core::BotOwner) -> BotOwner {
    BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

/// 64 hexadecimal digits over the call as the tool sent it: the key and
/// every item in order. The same key with a different list is a conflict.
pub(super) fn ask_fingerprint(request: &RunChildAskRequest) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("koru.bot.child.ask.v1");
    hasher.update(request.ask_key.as_bytes());
    for item in &request.requests {
        hasher.update(item.bot_id.as_uuid().as_bytes());
        let task = item.task.as_str().as_bytes();
        hasher.update(&(task.len() as u64).to_le_bytes());
        hasher.update(task);
        hasher.update(&(item.documents.len() as u64).to_le_bytes());
        for document in &item.documents {
            hasher.update(document.to_string().as_bytes());
        }
    }
    hasher.finalize().to_hex().to_string()
}

/// Shape only. One to three items, a canonical v4 key, no Bot named twice
/// (`invalid_request`, contract 2.5), at most 16 distinct pages per item.
pub(super) fn validate_request(request: &mut RunChildAskRequest) -> ApiResult<()> {
    let v4 = |id: uuid::Uuid| {
        id.get_version() == Some(uuid::Version::Random)
            && id.get_variant() == uuid::Variant::RFC4122
    };
    if request.requests.is_empty()
        || request.requests.len() > koru_ai_core::bots::BOT_CHILD_ASK_LIMIT
        || !v4(request.ask_key)
    {
        return Err(invalid());
    }
    let mut named = std::collections::BTreeSet::new();
    for item in &mut request.requests {
        if !named.insert(item.bot_id) {
            return Err(invalid());
        }
        // The binding holds them sorted and distinct.
        item.documents.sort();
        item.documents.dedup();
        if item.documents.len() > koru_delegate_core::children::CHILD_DOCUMENT_LIMIT {
            return Err(invalid());
        }
        let text = child_text(item.task.as_str(), &item.documents);
        if koru_delegate_core::Objective::new(text).is_err() {
            return Err(invalid());
        }
    }
    Ok(())
}

impl WorkerState {
    /// The asking Run, current. Not a child itself, a Bot reply with a live
    /// AI attempt bound to an execution with a turn, and the same admission
    /// the gateway resolved.
    pub(super) fn retained_child_subject(&self, requested: &RunBotSubject) -> ApiResult<Subject> {
        let p = &requested.provenance;
        if !p.valid() || !requested.captured.valid() || requested.generation.get() == 0 {
            return Err(invalid());
        }
        // Depth one: a Run asked by a Bot cannot ask (the delegate refuses
        // it too, `bot_child_depth`).
        if p.parent.is_some() {
            return Err(coded(codes::ASK_NOT_ALLOWED, None));
        }
        let owner = ai_owner(p.owner);
        let parent = self
            .repository
            .bot_submission_for_run(owner, p.bot, requested.run)
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        let s = &parent.intent.submission;
        if s.provenance != *p || s.policy != requested.captured {
            return Err(denied());
        }
        let ActorRef::Person { profile } = s.request.requested_by else {
            return Err(denied());
        };
        let id = p.ai_attempt_id.ok_or_else(denied)?;
        let attempt_id = InferenceAttemptId::parse(&id.to_string()).ok_or_else(invalid)?;
        let attempt = self
            .repository
            .attempt(attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(denied)?;
        let replay = self
            .repository
            .bot_replay_state(attempt_id)
            .map_err(ai_failure)?;
        if attempt.status.is_terminal()
            || attempt.chat_id.map(|id| id.as_uuid()) != Some(p.chat_id)
            || attempt.assistant_message_id.map(|id| id.as_uuid()) != p.assistant_message_id
            || replay.stop_reason.is_some()
        {
            return Err(denied());
        }
        let fence = replay.fence.ok_or_else(denied)?;
        if fence.turn.is_none()
            || fence.run != requested.run
            || fence.bot != p.bot
            || fence.generation != requested.generation
        {
            return Err(denied());
        }
        Ok(Subject {
            owner,
            parent,
            profile,
        })
    }

    /// `list_askable_bots`: the Bots that pass checks 2 and 3 of contract
    /// 3.2, ai.db only. The asking Bot's switch governs.
    pub(super) fn askable_bots(
        &self,
        request: &RunAskableRequest,
    ) -> ApiResult<Vec<RunAskableBot>> {
        let subject = self.retained_child_subject(&request.subject)?;
        let me = subject.parent.intent.submission.provenance.bot;
        let bot = self
            .repository
            .bot(me)
            .map_err(bot_failure)?
            .ok_or_else(denied)?;
        if !bot.may_ask {
            return Err(coded(codes::ASK_NOT_ALLOWED, None));
        }
        let mut bots: Vec<RunAskableBot> = self
            .repository
            .bots(subject.owner.user_scope, bot.home_workspace)
            .map_err(bot_failure)?
            .into_iter()
            .filter(|other| {
                other.id != me
                    && other.owner_device == subject.owner.device
                    && other.home_workspace == bot.home_workspace
                    && other.lifecycle == BotLifecycle::Active
            })
            .map(|other| RunAskableBot {
                bot_id: other.id,
                name: other.config.name,
            })
            .collect();
        bots.truncate(ASKABLE_LIMIT);
        Ok(bots)
    }

    /// `ask_bot` arrives. A key this Run already used is answered from its
    /// rows (no page check is repeated); a new one first checks its pages.
    pub(super) fn start_child_ask(
        &mut self,
        mut request: RunChildAskRequest,
        reply: SyncSender<ApiResult<RunChildAskPending>>,
    ) {
        let started = (|| -> ApiResult<Option<Box<AskJob>>> {
            validate_request(&mut request)?;
            if request.cancellation.is_cancelled() || Instant::now() >= request.deadline {
                return Err(status_unknown());
            }
            let subject = self.retained_child_subject(&request.subject)?;
            let run = subject.parent.intent.submission.request.run;
            let retained = self
                .repository
                .bot_child_ask(subject.owner, run, request.ask_key)
                .map_err(bot_failure)?;
            let authority = self.bot_authority.clone().ok_or_else(unavailable_failure)?;
            let delegate = self.bot_delegate.clone().ok_or_else(unavailable_failure)?;
            if retained.is_empty() {
                let home = subject.parent.intent.submission.workspace;
                let pages = request
                    .requests
                    .iter()
                    .enumerate()
                    .flat_map(|(index, item)| {
                        item.documents
                            .iter()
                            .map(move |document| (index as u8, *document))
                    })
                    .collect();
                return Ok(Some(Box::new(AskJob {
                    step: Step::Pages {
                        owner: subject.owner,
                        home,
                        profile: subject.profile,
                        pages,
                    },
                    owner: subject.owner,
                    parent_run: run,
                    ask_key: request.ask_key,
                    authority,
                    delegate,
                    deadline: request.deadline,
                    caller: Caller::Tool {
                        request,
                        reply: reply.clone(),
                    },
                })));
            }
            Ok(Some(Box::new(AskJob {
                step: Step::Prepare,
                owner: subject.owner,
                parent_run: run,
                ask_key: request.ask_key,
                authority,
                delegate,
                deadline: request.deadline,
                caller: Caller::Tool {
                    request,
                    reply: reply.clone(),
                },
            })))
        })();
        match started {
            Ok(Some(job)) if matches!(job.step, Step::Prepare) => self.prepare_child_ask(job),
            Ok(Some(job)) => self.submit_child_job(job),
            Ok(None) => {}
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// The whole ask in one transaction, then the delegate (or the stored
    /// answer when every row is already settled).
    pub(super) fn prepare_child_ask(&mut self, mut job: Box<AskJob>) {
        let request = match &job.caller {
            Caller::Tool { request, .. } => request.clone(),
            Caller::Resend { .. } => {
                job.answer(Err(unavailable_failure()));
                return;
            }
        };
        if request.cancellation.is_cancelled() || Instant::now() >= request.deadline {
            job.answer(Err(status_unknown()));
            return;
        }
        let next = (|| -> ApiResult<Result<RunChildAskPending, ChildRequest>> {
            let subject = self.retained_child_subject(&request.subject)?;
            let ask = self.build_child_ask(&subject, &request)?;
            let rows = match self
                .repository
                .prepare_bot_child_ask(&ask, Timestamp::now())
            {
                Ok(BotChildAskPrepared::Prepared(rows) | BotChildAskPrepared::Retained(rows)) => {
                    rows
                }
                Ok(BotChildAskPrepared::Refused(refusal)) => return Err(ai_refusal(refusal)),
                Err(BotError::Invalid) => return Err(invalid()),
                Err(BotError::NotFound | BotError::Forbidden) => return Err(denied()),
                Err(error) => return Err(bot_failure(error)),
            };
            if let Some(answer) = settled_answer(&rows) {
                return answer.map(Ok);
            }
            self.child_request_of(subject.owner, &rows).map(Err)
        })();
        match next {
            Ok(Err(request)) => {
                job.step = Step::Send {
                    owner: request.parent.owner,
                    request: Box::new(request),
                };
                self.submit_child_job(job);
            }
            Ok(Ok(pending)) => job.answer(Ok(pending)),
            Err(error) => job.answer(Err(error)),
        }
    }

    /// Every item of a new ask: ids minted here, the new conversation, its
    /// opening turn and the recipient's ordinary `start` submission with
    /// `provenance.parent` set and the reduced policy of contract 3.3.
    fn build_child_ask(
        &self,
        subject: &Subject,
        request: &RunChildAskRequest,
    ) -> ApiResult<BotChildAsk> {
        let p = &subject.parent.intent.submission.provenance;
        let parent_bot = self
            .repository
            .bot(p.bot)
            .map_err(bot_failure)?
            .ok_or_else(denied)?;
        let parent_chat = ChatId::parse(&p.chat_id.to_string()).ok_or_else(invalid)?;
        let assistant = p
            .assistant_message_id
            .and_then(|id| koru_ai_core::MessageId::parse(&id.to_string()))
            .ok_or_else(invalid)?;
        let run = subject.parent.intent.submission.request.run;
        let wait = BotWaitId::mint();
        let mut items = Vec::with_capacity(request.requests.len());
        for (index, item) in request.requests.iter().enumerate() {
            let index = index as u8;
            if item.bot_id == parent_bot.id {
                return Err(coded(codes::SELF_ASK, Some(index)));
            }
            let recipient = self
                .repository
                .bot(item.bot_id)
                .map_err(bot_failure)?
                .filter(|bot| {
                    bot.user_scope == parent_bot.user_scope
                        && bot.owner_device == parent_bot.owner_device
                        && bot.home_workspace == parent_bot.home_workspace
                })
                .ok_or_else(|| coded(codes::RECIPIENT_UNAVAILABLE, Some(index)))?;
            let child_request = ChildRequestId::mint();
            let now = Timestamp::now();
            let chat = Chat {
                id: ChatId::new(),
                title: child_title(item.task.as_str()),
                private: false,
                current_model: recipient.config.model.clone(),
                branched_from: None,
                agent_working_folder: Some(recipient.config.working_folder.clone()),
                agent_mode: recipient.config.mode.clone(),
                agent_fast: recipient.config.fast,
                pinned_at: None,
                profile_id: Some(subject.profile),
                bot: None,
                created_at: now,
                updated_at: now,
            };
            // The opening message is exactly the child's first input: the
            // repository requires the two to be equal.
            let text = child_text(item.task.as_str(), &item.documents);
            let turn = child_turn(&chat, &recipient, &text, now);
            let mut intent = admission::bare_intent(
                &recipient,
                BotRequestId::from_uuid(child_request.as_uuid()),
                chat.id,
                recipient.home_workspace,
                &text,
                None,
                &[],
                subject.profile,
            )?;
            let s = &mut intent.submission;
            s.provenance.parent = Some(ChildParent {
                run,
                bot: parent_bot.id,
                chat_id: parent_chat.as_uuid(),
                child_request,
            });
            s.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
            s.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
            // The intersection (contract 3.3): the recipient's policy with
            // only the grants the parent Run captured and still holds.
            s.policy = koru_ai_core::bots::bot_child_policy(
                &s.policy,
                &subject.parent.intent.submission.policy,
                &parent_bot.connections,
            );
            intent.validate().map_err(bot_failure)?;
            let fingerprint =
                blake3::hash(&serde_json::to_vec(&intent.submission).map_err(|_| invalid())?)
                    .to_hex()
                    .to_string();
            items.push(BotChildAskItem {
                child_request,
                chat,
                turn,
                intent,
                fingerprint,
                expected_revision: recipient.revision,
            });
        }
        Ok(BotChildAsk {
            owner: subject.owner,
            parent_bot: parent_bot.id,
            parent_chat,
            parent_run: run,
            parent_assistant_message: assistant,
            ask_key: request.ask_key,
            ask_fingerprint: ask_fingerprint(request),
            wait,
            items,
        })
    }

    /// `RequestChild` for the stored rows of one ask: the submissions as they
    /// were prepared, each binding rebuilt from them (the same bytes every
    /// time), and the parent's current execution. Shared by the ask and the
    /// restart scan's resend.
    pub(super) fn child_request_of(
        &self,
        owner: BotOwner,
        rows: &[BotChildChat],
    ) -> ApiResult<ChildRequest> {
        let first = rows.first().ok_or_else(invalid)?;
        let parent = self
            .repository
            .bot_submission_for_run(owner, first.parent_bot, first.parent_run)
            .map_err(bot_failure)?
            .ok_or_else(denied)?;
        let ps = &parent.intent.submission;
        let ActorRef::Person { profile: human } = ps.request.requested_by else {
            return Err(denied());
        };
        let attempt = ps
            .provenance
            .ai_attempt_id
            .and_then(|id| InferenceAttemptId::parse(&id.to_string()))
            .ok_or_else(denied)?;
        let fence = self
            .repository
            .bot_replay_state(attempt)
            .map_err(ai_failure)?
            .fence
            .filter(|fence| fence.turn.is_some())
            .ok_or_else(denied)?;
        let parent_policy = ChildPolicyRef::of(
            ps.provenance.config_revision,
            ps.provenance.policy_revision,
            &ps.policy,
        )
        .map_err(|_| invalid())?;
        let mut requests = Vec::with_capacity(rows.len());
        for row in rows {
            let receipt = self
                .repository
                .bot_admission_receipt(
                    owner,
                    row.recipient_bot,
                    BotRequestId::from_uuid(row.child_request.as_uuid()),
                )
                .map_err(bot_failure)?
                .ok_or_else(invalid)?;
            let BotAdmissionReceipt::Submission(receipt) = receipt else {
                return Err(invalid());
            };
            let submission = receipt.intent.submission;
            let cp = &submission.provenance;
            let objective = submission.request.objective.as_str();
            let binding = ChildRequestBindingV1 {
                version: DecisionVersionV1,
                owner: ps.provenance.owner,
                parent_bot: row.parent_bot,
                parent_run: row.parent_run,
                wait: row.wait,
                ask_key: row.ask_key,
                ordinal: row.ordinal,
                parent_submission: if ps.provenance.routine.is_some() {
                    koru_delegate_core::ROUTINE_BOT_SUBMISSION_VERSION
                } else {
                    koru_delegate_core::INTERACTIVE_BOT_SUBMISSION_VERSION
                },
                parent_routine: ps.provenance.routine,
                human,
                recipient_bot: row.recipient_bot,
                child_run: row.child_run,
                child_request: row.child_request,
                workspace: submission.workspace,
                task_digest: child_task_digest(objective).map_err(|_| invalid())?,
                documents: child_documents(objective).ok_or_else(invalid)?,
                parent_policy,
                child_policy: ChildPolicyRef::of(
                    cp.config_revision,
                    cp.policy_revision,
                    &submission.policy,
                )
                .map_err(|_| invalid())?,
            };
            requests.push(ChildRequestItem {
                child_request: row.child_request,
                binding,
                submission,
            });
        }
        let request = ChildRequest {
            parent: fence,
            ask_key: first.ask_key,
            human,
            requests,
        };
        use koru_delegate_core::decisions::DecisionValidate;
        request.validate().map_err(|_| invalid())?;
        Ok(request)
    }

    pub(super) fn submit_child_job(&mut self, job: Box<AskJob>) {
        let Some(pool) = &self.provider_admin else {
            job.answer(Err(unavailable_failure()));
            return;
        };
        let work = if job.caller.is_resend() {
            ProviderAdminJob::BotChildResend(job)
        } else {
            ProviderAdminJob::BotChildAsk(job)
        };
        match pool.try_submit(work) {
            Ok(()) => {}
            Err(AdminSubmitError {
                job: ProviderAdminJob::BotChildAsk(job) | ProviderAdminJob::BotChildResend(job),
                failure,
            }) => {
                // Before the delegate heard of it, a full lane is a plain
                // retryable refusal; after the ai.db commit it is unknown.
                let failure = if matches!(job.step, Step::Send { .. }) {
                    status_unknown()
                } else {
                    failure
                };
                job.answer(Err(failure));
            }
            Err(_) => unreachable!("the child ask lane returned another job"),
        }
    }

    /// One phase of the ask came back.
    pub(super) fn complete_child_ask(&mut self, job: Box<AskJob>, result: ApiResult<Evidence>) {
        let resend = match &job.caller {
            Caller::Resend { held } => Some((held.clone(), job.owner)),
            Caller::Tool { .. } => None,
        };
        self.complete_child_phase(job, result);
        if let Some((held, owner)) = resend {
            self.release_child_resend(&held, owner);
        }
    }

    fn complete_child_phase(&mut self, mut job: Box<AskJob>, result: ApiResult<Evidence>) {
        let evidence = match result {
            Ok(evidence) => evidence,
            Err(error) => {
                job.answer(Err(error));
                return;
            }
        };
        match (std::mem::replace(&mut job.step, Step::Prepare), evidence) {
            (Step::Pages { .. }, Evidence::Pages) => self.prepare_child_ask(job),
            (Step::Send { .. }, Evidence::Sent(reply)) => self.settle_child_ask(job, reply),
            _ => job.answer(Err(unavailable_failure())),
        }
    }

    /// The delegate answered `RequestChild`, or could not be asked.
    fn settle_child_ask(&mut self, job: Box<AskJob>, reply: Reply) {
        let (owner, run, key) = (job.owner, job.parent_run, job.ask_key);
        let now = Timestamp::now();
        match reply {
            Ok(BotReply::ChildRequested {
                enrollment,
                receipts,
                ..
            }) => {
                let admissions: Vec<BotAdmissionView> = receipts
                    .iter()
                    .map(|receipt| BotAdmissionView {
                        key: BotSubmissionKey {
                            owner: receipt.parent.owner,
                            bot: receipt.recipient,
                            request: BotRequestId::from_uuid(receipt.child_request.as_uuid()),
                        },
                        run: receipt.child_run,
                        state: receipt.child,
                        // Correction W5: the receipt carries no queue position.
                        queue_sequence: 1,
                        fence: None,
                    })
                    .collect();
                // Contract 3.5 step 18: the enrollment and the accepted
                // receipts in one transaction, before any answer.
                match self.repository.accept_bot_child_ask(
                    owner,
                    run,
                    key,
                    &enrollment,
                    &admissions,
                    now,
                ) {
                    Ok((_, rows)) => {
                        for row in &rows {
                            self.start_child_reply(owner, row);
                        }
                        let answer = settled_answer(&rows).unwrap_or_else(|| Err(status_unknown()));
                        job.answer(answer);
                    }
                    Err(_) => job.answer(Err(status_unknown())),
                }
            }
            Ok(_) => job.answer(Err(status_unknown())),
            Err(error) => match delegate_refusal(&error) {
                Some((code, index)) => {
                    match self.repository.reject_bot_child_ask(
                        owner,
                        run,
                        key,
                        code,
                        index,
                        BotAdmissionRefusal::Rejected,
                        now,
                    ) {
                        Ok(rows) => {
                            self.fail_child_replies(owner, &rows);
                            job.answer(Err(coded(code, index)));
                        }
                        Err(_) => job.answer(Err(status_unknown())),
                    }
                }
                // Contract 3.5 step 20: the receipts stay `prepared`.
                None => job.answer(Err(status_unknown())),
            },
        }
    }

    /// An admitted child's opening reply starts like any Bot reply: its
    /// task observes the delegate from admission on.
    fn start_child_reply(&mut self, owner: BotOwner, row: &BotChildChat) {
        let Ok(Some(BotAdmissionReceipt::Submission(receipt))) =
            self.repository.bot_admission_receipt(
                owner,
                row.recipient_bot,
                BotRequestId::from_uuid(row.child_request.as_uuid()),
            )
        else {
            return;
        };
        if let Some(attempt) = receipt
            .intent
            .submission
            .provenance
            .ai_attempt_id
            .and_then(|id| InferenceAttemptId::parse(&id.to_string()))
        {
            self.start_child_bot_chat(attempt);
        }
    }

    /// A refused ask's opening replies end as any rejected submission's do:
    /// interrupted, never started.
    pub(super) fn fail_child_replies(&mut self, owner: BotOwner, rows: &[BotChildChat]) {
        for row in rows {
            if let Ok(Some(BotAdmissionReceipt::Submission(receipt))) =
                self.repository.bot_admission_receipt(
                    owner,
                    row.recipient_bot,
                    BotRequestId::from_uuid(row.child_request.as_uuid()),
                )
                && let Some(attempt) = receipt
                    .intent
                    .submission
                    .provenance
                    .ai_attempt_id
                    .and_then(|id| InferenceAttemptId::parse(&id.to_string()))
            {
                self.interrupt_bot_chat(attempt);
            }
        }
    }
}

/// The child conversation's opening turn: the task as the request (the
/// person's message would be), and the pending reply of the recipient.
fn child_turn(chat: &Chat, recipient: &Bot, task: &str, now: Timestamp) -> PendingTurn {
    let user = koru_ai_core::MessageId::new();
    let assistant = koru_ai_core::MessageId::new();
    let model = &recipient.config.model;
    PendingTurn {
        user_message: Message {
            id: user,
            chat_id: chat.id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: task.to_owned(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: Some(now),
        },
        assistant_message: Message {
            id: assistant,
            chat_id: chat.id,
            ordinal: 1,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(model.provider.clone()),
            requested_model: Some(model.requested_model.clone()),
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        },
        attempt: InferenceAttempt {
            id: InferenceAttemptId::new(),
            operation_id: uuid::Uuid::new_v4(),
            chat_id: Some(chat.id),
            assistant_message_id: Some(assistant),
            surface: koru_ai_core::AiSurface::Standalone,
            intent: AiIntent::Chat,
            provider: model.provider.clone(),
            requested_model: model.requested_model.clone(),
            resolved_model: None,
            requested_reasoning: model.default_reasoning,
            effective_reasoning: None,
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            status: AttemptStatus::Pending,
            queued_at: now,
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: false,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: 1,
            context_document_count: 0,
            context_message_block_count: 0,
            usage: Default::default(),
            context_used_tokens: None,
            context_limit_tokens: None,
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        },
        attachment_ids: Vec::new(),
    }
}
