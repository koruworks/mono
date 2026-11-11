// SPDX-License-Identifier: MPL-2.0
//! Bounded owner reads for the Bots view (P1.8). Each runs on the AI writer
//! against ai.db only — no Store, delegate or model call — after the same
//! prepared owner proof every other Bot read uses.
//!
//! The one exception is `read_run` (B1): the writer binds the Run to the
//! conversation from ai.db, and a bounded job on the Run reads' own lane
//! asks the execution owner for it, within a deadline, so neither the writer
//! nor any other Bot work waits on the delegate. Nothing it reads is stored.
use super::*;
use koru_delegate_api::bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply};
use koru_delegate_api::wire::TimelineQuery;
use koru_delegate_core::{ActorRef, BotRunKey, RunView, TimelineItem, TimelineRef, TimelineRefs};

/// How long a Run read may take from the writer's dispatch to its answer,
/// the wait in its lane and the execution owner's call included. Shorter
/// than the caller's own `RESPONSE_TIMEOUT` by more than a preparation hop,
/// so a slow owner is answered `bot_unavailable` (retryable), not a worker
/// timeout. Shorter under test, so a hung owner is proved in seconds.
#[cfg(not(test))]
pub(in crate::worker) const RUN_READ_DEADLINE: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(in crate::worker) const RUN_READ_DEADLINE: Duration = Duration::from_secs(3);
const _: () = assert!(RUN_READ_DEADLINE.as_millis() * 2 <= RESPONSE_TIMEOUT.as_millis());

/// Run reads waiting for their lane beside the one it is running; one more
/// is answered `ai_worker_saturated` at once.
pub(in crate::worker) const RUN_READ_QUEUE_CAPACITY: usize = 2;

/// A `read_run` the writer has checked and bound: what the job needs to
/// check authority again and ask the execution owner.
struct BotRunRead {
    authority: Arc<dyn BotAuthority>,
    owner: BotOwner,
    record_workspace: WorkspaceId,
    key: BotRunKey,
    window: TimelineQuery,
    delegate: Arc<dyn BotDelegatePort>,
}

/// One `read_run`, off the writer on its own lane: authority again for the
/// Run's record workspace, then the execution owner's `Read` within what is
/// left of its deadline, answered to the caller directly. Holds no writer
/// state; nothing is persisted.
pub(in crate::worker) struct BotRunReadJob {
    read: BotRunRead,
    deadline: Instant,
    reply: SyncSender<ApiResult<BotResponse>>,
}

/// The answer when the deadline passed: before the job could run (its
/// caller is gone or about to be), or while the owner was asked.
fn run_read_late() -> Failure {
    failure(
        "bot_unavailable",
        "The Bot's execution owner did not answer in time.",
        true,
    )
}

impl BotRunReadJob {
    pub(in crate::worker) fn run(self) {
        // A job that waited past its deadline asks nobody: its caller has
        // stopped waiting, or is about to.
        let answer = if Instant::now() >= self.deadline {
            Err(run_read_late())
        } else {
            catch_unwind(AssertUnwindSafe(|| self.read.read(self.deadline)))
                .unwrap_or_else(|_| Err(bot_failure(BotError::Unavailable)))
        };
        let _ = self.reply.send(answer);
    }

    /// Onto the Run reads' lane, or answered at once when it is full
    /// (`ai_worker_saturated`, retryable) or gone. The writer never waits.
    pub(in crate::worker) fn submit(self, pool: Option<&ProviderAdminPool>) {
        let Some(pool) = pool else {
            let _ = self.reply.send(Err(bot_failure(BotError::Unavailable)));
            return;
        };
        if let Err(error) = pool.try_submit(ProviderAdminJob::BotRunRead(self))
            && let ProviderAdminJob::BotRunRead(job) = error.job
        {
            let _ = job.reply.send(Err(error.failure));
        }
    }

    /// A read of `run` by `owner` for the lane tests, its authority,
    /// workspace and deadline given.
    #[cfg(test)]
    pub(in crate::worker) fn fixture(
        authority: Arc<dyn BotAuthority>,
        owner: BotOwner,
        workspace: WorkspaceId,
        key: BotRunKey,
        delegate: Arc<dyn BotDelegatePort>,
        deadline: Instant,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) -> Self {
        Self {
            read: BotRunRead {
                authority,
                owner,
                record_workspace: workspace,
                key,
                window: TimelineQuery {
                    from: 0,
                    len: 10,
                    workspace: Some(workspace),
                },
                delegate,
            },
            deadline,
            reply,
        }
    }
}

impl BotRunRead {
    fn read(&self, deadline: Instant) -> ApiResult<BotResponse> {
        let checked = self
            .authority
            .inspect(&[self.record_workspace], &[])
            .map_err(bot_failure)?;
        if checked.owner != self.owner {
            return Err(bot_failure(BotError::Forbidden));
        }
        checked
            .workspace(self.record_workspace)
            .map_err(bot_failure)?;
        let within = deadline.saturating_duration_since(Instant::now());
        if within.is_zero() {
            return Err(run_read_late());
        }
        match self.delegate.execute_within(
            DelegateCommand::Read {
                key: self.key.clone(),
                window: self.window,
            },
            within,
        ) {
            Ok(BotReply::Run { mut view }) if view.header.run == self.key.run => {
                withhold_sessions(&mut view)?;
                Ok(BotResponse::Run { view })
            }
            Ok(_) => Err(run_unreadable(
                "The Bot's execution owner answered with another Run.",
            )),
            Err(refused) => Err(run_read_failure(&refused)),
        }
    }
}

/// A Run this read cannot hand out as it is: not retryable, and not an
/// unavailability (HTTP 500 at the gateway).
fn run_unreadable(message: &'static str) -> Failure {
    failure("bot_run_unreadable", message, false)
}

/// A read hands out no session handle: nothing that names the provider
/// session a Bot Run is driven in leaves through it (P1.8 B1). Every place a
/// `SessionId` can sit in a `RunView`, by the vocabulary crate's types:
/// - `execution.session`: cleared.
/// - a timeline step's refs, `TimelineRef::Session { session }` (the
///   owner's `bot_mark_started` writes one on the Started step) and an agent
///   actor, `TimelineRef::Actor { actor: ActorRef::Agent { session } }`: the
///   ref is dropped, the step kept.
/// - `header.requested_by` and `header.rating.rated_by`, an
///   `ActorRef::Agent { session }`: these cannot be dropped without
///   misreporting who acted. A Bot Run is requested by a person and is never
///   rated (the ordinary Rate route refuses Bot Runs), so such a header is
///   refused (`bot_run_unreadable`) rather than handed out.
///
/// Nothing else holds one: a step's `StepKind`, a `UsageFact`, the header's
/// Bot provenance, native results, delivery receipt, scope, environment,
/// context pins and artifacts, and the view's context states carry none. The
/// destructuring below names every field and variant, so a new one is a
/// build failure here rather than a leak.
fn withhold_sessions(view: &mut RunView) -> ApiResult<()> {
    let RunView {
        document: _,
        header,
        execution,
        context: _,
        timeline,
    } = view;
    let koru_delegate_core::RunHeader {
        bot: _,
        native_results: _,
        chat_delivery: _,
        run: _,
        created_at_ms: _,
        requested_by,
        objective: _,
        agent: _,
        requested_model: _,
        scope: _,
        context: _,
        grants: _,
        status: _,
        failure_reason: _,
        environment: _,
        artifacts: _,
        economics: _,
        rating,
        origin: _,
    } = header;
    let names_a_session = |actor: &ActorRef| match actor {
        ActorRef::Agent { session: _ } => true,
        ActorRef::Person { profile: _ } | ActorRef::Orchestrator { run: _ } => false,
    };
    if names_a_session(requested_by)
        || rating
            .as_ref()
            .is_some_and(|rating| names_a_session(&rating.rated_by))
    {
        return Err(run_unreadable(
            "The Run names an agent session where a Bot Run names a person.",
        ));
    }
    let koru_delegate_core::RunExecution {
        device: _,
        session,
        run_workspace: _,
    } = execution;
    *session = None;
    for item in &mut timeline.items {
        match item {
            TimelineItem::Step(entry) => {
                let kept: Vec<TimelineRef> = entry
                    .refs
                    .iter()
                    .filter(|reference| match reference {
                        TimelineRef::Session { session: _ } => false,
                        TimelineRef::Actor { actor } => !names_a_session(actor),
                        TimelineRef::Attempt { .. }
                        | TimelineRef::Turn { .. }
                        | TimelineRef::Document { .. }
                        | TimelineRef::Blob { .. }
                        | TimelineRef::Artifact { .. }
                        | TimelineRef::Device { .. }
                        | TimelineRef::Approval { .. }
                        | TimelineRef::Attachment { .. }
                        | TimelineRef::Model { .. }
                        | TimelineRef::Schedule { .. } => true,
                    })
                    .cloned()
                    .collect();
                // Fewer references than it had: always within the bound.
                entry.refs = TimelineRefs::new(kept)
                    .map_err(|_| run_unreadable("A step carries too many references."))?;
            }
            TimelineItem::Usage(_) | TimelineItem::Unrecognized => {}
        }
    }
    Ok(())
}

/// The execution owner's refusal of a `Read`, in the Bots API's codes. The AI
/// owner has already checked the caller owns this Bot and that the Run is
/// this reply's, so a Run the owner does not hold (`unknown_run`) or binds
/// differently (`bot_owner_mismatch`, its immutable owner, device or Bot) is
/// `not_found`: it never says the Run exists elsewhere. An owner that does
/// not answer, or answers retryably, is `bot_unavailable` (retryable); any
/// other definite refusal is `bot_run_unreadable` (not retryable).
fn run_read_failure(refused: &koru_delegate_api::Failure) -> Failure {
    match refused.code.as_str() {
        "unknown_run" | "bot_owner_mismatch" => not_found(),
        _ if refused.retryable => failure(
            "bot_unavailable",
            "The Bot's execution owner could not read the Run.",
            true,
        ),
        _ => run_unreadable("The Bot's execution owner refused to read the Run."),
    }
}

impl WorkerState {
    /// `read_run`: checked and bound here, read off the writer. Every refusal
    /// is answered here; an accepted read is answered by its job.
    pub(in crate::worker) fn read_bot_run(
        &mut self,
        context: CommandContext,
        request: BotCommand,
        reply: SyncSender<ApiResult<BotResponse>>,
    ) {
        let read = match self.bot_run_read(&context, &request) {
            Ok(read) => read,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        BotRunReadJob {
            read,
            deadline: Instant::now() + RUN_READ_DEADLINE,
            reply,
        }
        .submit(self.provider_admin.as_ref());
    }

    /// The owner first, then the conversation's Trash, then the Bot's
    /// workspace, then the binding (a reply's, or without `message_id` an
    /// API task's receipt), as a named conversation read is checked
    /// (`guard_command`, `require_held_owner`): only the owner learns a
    /// conversation is held, and a Run that is not this reply's is
    /// `not_found`, never someone else's.
    fn bot_run_read(
        &self,
        context: &CommandContext,
        request: &BotCommand,
    ) -> ApiResult<BotRunRead> {
        let BotCommand::ReadRun {
            bot_id,
            chat_id,
            message_id,
            run_id,
            from,
            len,
        } = *request
        else {
            return Err(bot_failure(BotError::Invalid));
        };
        authorize(context, AiScope::ManageBots)?;
        if len == 0 || len > BOT_RUN_WINDOW_LIMIT {
            return Err(bot_failure(BotError::Invalid));
        }
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        let (bot, standing) = self
            .repository
            .bot_any_lifecycle(bot_id)
            .map_err(bot_failure)?
            .ok_or_else(not_found)?;
        if (BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        }) != owner
        {
            return Err(bot_failure(BotError::Forbidden));
        }
        if self.repository.bot_for_chat(chat_id).map_err(bot_failure)? != Some(bot_id) {
            return Err(not_found());
        }
        // A conversation in Trash, on its own or with its Bot, is out of
        // every read; an archived Bot's history reads, and an unfinished
        // operation holds no read.
        let hold = match standing {
            BotLifecycle::Trashed => Some(koru_ai_sqlite::BotHold::BotTrashed),
            _ => self
                .repository
                .bot_chat_hold(chat_id)
                .map_err(bot_failure)?
                .filter(|hold| hold.hides()),
        };
        if let Some(hold) = hold {
            return Err(lifecycle::hold_failure(hold));
        }
        proof
            .authority
            .workspace(bot.home_workspace)
            .map_err(bot_failure)?;
        // A reply's Run through its reply; without one, an API task's Run
        // through the Bot's own admitted receipt. Neither binding admits the
        // other's Run.
        let record_workspace = match message_id {
            Some(message) => self
                .repository
                .bound_bot_run(bot_id, chat_id, message, run_id),
            None => self
                .repository
                .bound_bot_task_run(owner, bot_id, chat_id, run_id),
        }
        .map_err(bot_failure)?
        .ok_or_else(not_found)?;
        let unavailable = || {
            failure(
                "bot_unavailable",
                "The Bot's execution owner is not available.",
                true,
            )
        };
        let authority = self.bot_authority.clone().ok_or_else(unavailable)?;
        let delegate = self.bot_delegate.clone().ok_or_else(unavailable)?;
        Ok(BotRunRead {
            authority,
            owner,
            record_workspace,
            key: BotRunKey {
                owner: koru_delegate_core::BotOwner {
                    user_scope: owner.user_scope,
                    device: owner.device,
                },
                bot: bot_id,
                run: run_id,
            },
            window: TimelineQuery {
                from,
                len,
                workspace: Some(record_workspace),
            },
            delegate,
        })
    }

    pub(in crate::worker) fn bot_read_command(
        &self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        match command {
            BotCommand::ListBotChats {
                bot_id,
                cursor,
                limit,
            } => Some(self.list_bot_chats(context, *bot_id, cursor.as_ref(), *limit)),
            BotCommand::ListActivity { workspace } => {
                Some(self.list_bot_activity(context, *workspace))
            }
            BotCommand::ListTrashedBotChats {
                workspace,
                cursor,
                limit,
            } => Some(self.list_trashed_bot_chats(context, *workspace, cursor.as_ref(), *limit)),
            _ => None,
        }
    }

    /// Running and Needs-you conversations of the owner's active Bots whose
    /// home is `workspace`, which the owner must currently reach. Durable AI
    /// state only: nothing here asks the delegate, so a slow or absent
    /// delegate cannot hold the writer.
    fn list_bot_activity(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        let activity = self
            .repository
            .bot_activity(owner, workspace, BOT_ACTIVITY_LIMIT)
            .map_err(bot_failure)?;
        Ok(BotResponse::Activity { activity })
    }

    /// The owner's Bot conversations in Trash whose Bot lives in `workspace`,
    /// which the owner must currently reach, whatever each Bot's lifecycle,
    /// so a client that knows only the workspace can find each one and the
    /// operation to name in its Restore. ai.db only, like every read here.
    fn list_trashed_bot_chats(
        &self,
        context: &CommandContext,
        workspace: WorkspaceId,
        cursor: Option<&BotTrashedChatCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let proof = context
            .prepared
            .as_ref()
            .ok_or_else(|| bot_failure(BotError::Unavailable))?;
        let owner = proof.authority.owner().map_err(bot_failure)?;
        proof.authority.workspace(workspace).map_err(bot_failure)?;
        let page = self
            .repository
            .trashed_bot_chat_page(owner, workspace, cursor, limit)
            .map_err(bot_failure)?;
        Ok(BotResponse::TrashedChats { page })
    }

    /// Fill in, for each archived or trashed Bot of a read the owner has
    /// already been authorized for, the completed operation that holds it
    /// there (`held_by`), so a client that knows only the workspace can name
    /// it in the Restore after a restart. One journal read serves the whole
    /// list; an active Bot is left without one.
    pub(super) fn with_lifecycle_holders(
        &self,
        owner: BotOwner,
        bots: &mut [Bot],
    ) -> ApiResult<()> {
        let inactive: Vec<_> = bots
            .iter()
            .filter(|bot| !bot.lifecycle.is_active())
            .map(|bot| (bot.id, bot.lifecycle))
            .collect();
        if inactive.is_empty() {
            return Ok(());
        }
        let mut holders = self
            .repository
            .bot_lifecycle_holders(owner, &inactive)
            .map_err(bot_failure)?;
        for bot in bots {
            bot.held_by = holders.remove(&bot.id);
        }
        Ok(())
    }

    /// The owner's view of one Bot's conversations. An archived Bot's stay
    /// listable by name (its history is kept in the Archived filter, as
    /// `bot_read_command` admits); a trashed Bot is `not_found`, as `read`
    /// answers it. A Bot of another owner or device, or one out of the
    /// caller's current reach, reads as refused before any conversation is
    /// looked at.
    fn list_bot_chats(
        &self,
        context: &CommandContext,
        bot_id: BotId,
        cursor: Option<&BotChatCursor>,
        limit: u16,
    ) -> ApiResult<BotResponse> {
        authorize(context, AiScope::ManageBots)?;
        let bot = self.readable_owned_bot(context, bot_id)?;
        let page = self
            .repository
            .bot_chat_page(bot.id, cursor, limit)
            .map_err(bot_failure)?;
        Ok(BotResponse::Chats { page })
    }
}
