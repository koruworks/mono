// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn indirect_keys_resolve_the_actual_bot_chat_before_read_or_mutation() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let authority: Arc<dyn BotAuthority> = authority;
    let pending = turn(bot.main_chat, Uuid::new_v4(), 1);
    repo.begin_turn(&pending).unwrap();
    let export = DocumentExport {
        id: DocumentExportId::new(),
        idempotency_key: Uuid::new_v4(),
        chat_id: bot.main_chat,
        through_message_id: pending.assistant_message.id,
        destination: DocumentDestination {
            profile_id: ProfileId::new(),
            workspace_id: bot.home_workspace,
            parent_document_id: None,
            index: None,
            title: "fixture".into(),
        },
        status: ExportStatus::Pending,
        document_id: None,
        error_class: None,
        created_at: at(1),
        completed_at: None,
    };
    repo.begin_export(&export).unwrap();
    let mut collision = export.clone();
    collision.chat_id = ChatId::new();
    assert!(repo.begin_export(&collision).is_err());
    let (reply, response) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::ReadAttempt {
                context: context(false).into(),
                attempt: pending.attempt.id,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(response.recv().unwrap().unwrap_err().code, "not_found");
    let (reply, response) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::ReadExport {
                context: context(false).into(),
                export: export.id,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(response.recv().unwrap().unwrap_err().code, "not_found");
    let (reply, response) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::CancelAttempt {
                context: context(true).into(),
                attempt: pending.attempt.id,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(
        response.recv().unwrap().unwrap_err().code,
        "unsupported_capability"
    );
    let (reply, response) = sync_channel(1);
    let request = SendTurnRequest {
        operation_id: pending.attempt.operation_id,
        chat_id: ChatId::new(),
        surface: AiSurface::Standalone,
        markdown: "fixture".into(),
        explicit_refs: vec![],
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: vec![],
        answers: None,
        skills: vec![],
    };
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::SendTurn {
                context: context(false).into(),
                request,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(
        response.recv().unwrap().err().unwrap().code,
        "request_conflict"
    );
    let (reply, response) = sync_channel(1);
    let request = RetryTurnRequest {
        operation_id: pending.attempt.operation_id,
        message_id: pending.assistant_message.id,
        surface: AiSurface::Standalone,
        explicit_refs: vec![],
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    };
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::RetryTurn {
                context: context(false).into(),
                chat: ChatId::new(),
                request,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(
        response.recv().unwrap().err().unwrap().code,
        "request_conflict"
    );
}
use std::sync::atomic::{AtomicBool, Ordering};

struct Authority {
    owner: BotOwner,
    workspace: WorkspaceId,
    profile: koru_ai_core::ProfileId,
    allowed: AtomicBool,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<koru_ai_core::ProfileId, BotError> {
        if self.allowed.load(Ordering::SeqCst) && id == self.workspace {
            Ok(self.profile)
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.workspace(page.workspace).map(|_| ())
    }
}
fn fixture() -> (Repository, Arc<Authority>, BotCommand) {
    let authority = Arc::new(Authority {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        workspace: WorkspaceId::new(),
        profile: koru_ai_core::ProfileId::new(),
        allowed: AtomicBool::new(true),
    });
    let command = BotCommand::Create {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: authority.workspace,
        config: BotConfig {
            name: "Fixture".into(),
            role: BotPage {
                workspace: authority.workspace,
                document: DocumentId::new(),
            },
            working_folder: if cfg!(windows) {
                "D:/isolated"
            } else {
                "/isolated"
            }
            .into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    (Repository::in_memory().unwrap(), authority, command)
}
fn context(owner: bool) -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "caller-controlled".into(),
            scopes: if owner {
                vec![AiScope::Chat, AiScope::ManageBots]
            } else {
                vec![AiScope::Chat]
            },
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}
fn created(repo: &mut Repository, authority: Arc<Authority>, command: BotCommand) -> Bot {
    let BotResponse::Bot { bot } =
        execute_bot_command(repo, authority, |_| Ok(()), context(true), command).unwrap()
    else {
        panic!("expected Bot")
    };
    bot
}

#[test]
fn create_retry_uses_same_main_chat_and_checks_current_authority_before_receipt() {
    let (mut repo, authority, command) = fixture();
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority.clone(),
            |_| panic!("not admitted"),
            context(false),
            command.clone()
        )
        .unwrap_err()
        .code,
        "forbidden"
    );
    let bot = created(&mut repo, authority.clone(), command.clone());
    let retry = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| panic!("receipt does not rediscover models"),
        context(true),
        command.clone(),
    )
    .unwrap();
    assert_eq!(retry, BotResponse::Bot { bot: bot.clone() });
    authority.allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority.clone(),
            |_| panic!("revoked"),
            context(true),
            command
        )
        .unwrap_err()
        .code,
        "wrong_owner"
    );
    assert_eq!(
        execute_bot_command(
            &mut repo,
            authority,
            |_| Ok(()),
            context(true),
            BotCommand::Read { bot_id: bot.id }
        )
        .unwrap_err()
        .code,
        "wrong_owner"
    );
}

struct Hidden {
    inner: Repository,
}
impl BotRepository for Hidden {
    fn bot(&self, _: BotId) -> Result<Option<Bot>, BotError> {
        Err(BotError::Forbidden)
    }
    fn bots(&self, s: UserScopeId, w: WorkspaceId) -> Result<Vec<Bot>, BotError> {
        self.inner.bots(s, w)
    }
    fn save_bot(
        &mut self,
        b: &Bot,
        p: koru_ai_core::ProfileId,
        r: BotRequestId,
        f: &str,
        e: Option<u64>,
    ) -> Result<Bot, BotError> {
        self.inner.save_bot(b, p, r, f, e)
    }
    fn bot_receipt(
        &self,
        s: UserScopeId,
        r: BotRequestId,
        f: &str,
    ) -> Result<Option<Bot>, BotError> {
        self.inner.bot_receipt(s, r, f)
    }
}

#[test]
fn retained_create_receipt_does_not_override_current_lifecycle() {
    let (mut repo, authority, command) = fixture();
    created(&mut repo, authority.clone(), command.clone());
    let mut hidden = Hidden { inner: repo };
    assert_eq!(
        execute_bot_command(&mut hidden, authority, |_| Ok(()), context(true), command)
            .unwrap_err()
            .code,
        "wrong_owner"
    );
}

#[test]
fn bot_chat_known_id_reads_require_owner_and_mutations_never_reach_launcher() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let authority: Arc<dyn BotAuthority> = authority;
    let (reply, response) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::ReadChat {
                context: context(false).into(),
                chat: bot.main_chat,
                reply
            }
        )
        .is_none()
    );
    assert_eq!(response.recv().unwrap().unwrap_err().code, "not_found");
    let (reply, _) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::ReadChat {
                context: context(true).into(),
                chat: bot.main_chat,
                reply
            }
        )
        .is_some()
    );
    let (reply, response) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::RenameChat {
                context: context(true).into(),
                chat: bot.main_chat,
                request: RenameChatRequest {
                    title: "Forbidden".into()
                },
                reply
            }
        )
        .is_none()
    );
    assert_eq!(
        response.recv().unwrap().unwrap_err().code,
        "unsupported_capability"
    );
    let (reply, _) = sync_channel(1);
    assert!(
        guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::ReadChat {
                context: context(false).into(),
                chat: ChatId::new(),
                reply
            }
        )
        .is_some()
    );
}

#[test]
fn role_edits_do_not_require_bot_reacceptance_and_access_edits_use_cas() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let update = BotCommand::UpdateAccess {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
        expected_revision: 1,
        expected_policy_revision: 1,
        access: BotAccess::Workspace,
        selected_pages: vec![],
    };
    let BotResponse::Bot { bot: updated } = execute_bot_command(
        &mut repo,
        authority.clone(),
        |_| panic!("access does not rediscover runtime"),
        context(true),
        update,
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!((updated.revision, updated.policy_revision), (2, 2));
    let read = execute_bot_command(
        &mut repo,
        authority,
        |_| Ok(()),
        context(true),
        BotCommand::Read { bot_id: bot.id },
    )
    .unwrap();
    assert_eq!(read, BotResponse::Bot { bot: updated });
}

use koru_ai_core::*;
fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}
fn turn(chat_id: ChatId, operation_id: Uuid, offset: i64) -> PendingTurn {
    let user_id = MessageId::new();
    let assistant_id = MessageId::new();
    PendingTurn {
        user_message: Message {
            id: user_id,
            chat_id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "Question".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: Some(at(offset)),
        },
        assistant_message: Message {
            id: assistant_id,
            chat_id,
            ordinal: 1,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(AiProviderId::parse("mock").unwrap()),
            requested_model: Some("deterministic-v1".into()),
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: None,
        },
        attempt: InferenceAttempt {
            id: InferenceAttemptId::new(),
            operation_id,
            chat_id: Some(chat_id),
            assistant_message_id: Some(assistant_id),
            surface: AiSurface::Standalone,
            intent: AiIntent::Chat,
            provider: AiProviderId::parse("mock").unwrap(),
            requested_model: "deterministic-v1".into(),
            resolved_model: None,
            requested_reasoning: ReasoningChoice::Default,
            effective_reasoning: None,
            prompt_template_version: "mock-v1".into(),
            status: AttemptStatus::Pending,
            queued_at: at(offset),
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
            usage: AiUsage::default(),
            context_used_tokens: None,
            context_limit_tokens: None,
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        },
        attachment_ids: Vec::new(),
    }
}

struct BlockingAuthority {
    inner: Arc<Authority>,
    entered: SyncSender<()>,
    release: std::sync::Mutex<Option<Receiver<()>>>,
}
impl BotAuthority for BlockingAuthority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn inspect(&self, w: &[WorkspaceId], p: &[BotPage]) -> Result<BotAuthoritySnapshot, BotError> {
        if let Some(release) = self.release.lock().unwrap().take() {
            assert_eq!(
                std::thread::current().name(),
                Some("koru-ai-provider-admin")
            );
            let _ = self.entered.send(());
            let _ = release.recv();
        }
        self.inner.inspect(w, p)
    }
}
#[test]
fn stalled_bot_authority_is_bounded_and_ordinary_chat_keeps_progressing() {
    use koru_ai_api::AiServerPort;
    let (_, authority, _) = fixture();
    let workspace = authority.workspace;
    let (entered_tx, entered) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    let authority = Arc::new(BlockingAuthority {
        inner: authority,
        entered: entered_tx,
        release: std::sync::Mutex::new(Some(release)),
    });
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(
        WorkerConfig::deterministic_mock(directory.path().to_owned(), "internal-debug")
            .with_bot_authority(authority),
    )
    .spawn()
    .unwrap();
    struct Release(Option<SyncSender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let release_guard = Release(Some(release_tx));
    let client = worker.client();
    let chat = ChatId::new();
    client
        .create_chat(
            context(true),
            CreateChatRequest {
                chat_id: chat,
                title: "ordinary".into(),
                profile_id: None,
                model: ModelSelection {
                    provider: AiProviderId::parse("mock").unwrap(),
                    requested_model: "deterministic-v1".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    let enqueue = || {
        let (reply, response) = sync_channel(1);
        client
            .send(WorkerCommand::Bots {
                context: context(true).into(),
                request: BotCommand::List { workspace },
                reply,
            })
            .unwrap();
        response
    };
    let first = enqueue();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let queued: Vec<_> = (0..PROVIDER_ADMIN_QUEUE_CAPACITY)
        .map(|_| enqueue())
        .collect();
    assert_eq!(
        enqueue()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err()
            .code,
        "ai_worker_saturated"
    );
    worker.ping().unwrap();
    assert_eq!(client.read_chat(context(true), chat).unwrap().chat.id, chat);
    let stream = client
        .send_turn(
            context(true),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: chat,
                surface: AiSurface::Standalone,
                markdown: "fixture".into(),
                explicit_refs: vec![],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: vec![],
                answers: None,
                skills: vec![],
            },
        )
        .unwrap();
    let attempt = stream.accepted.attempt_id;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client
            .read_attempt(context(true), attempt)
            .unwrap()
            .attempt
            .status;
        if status == AttemptStatus::Complete {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ordinary provider did not progress"
        );
        std::thread::yield_now();
    }
    assert!(client.cancel_attempt(context(true), attempt).is_ok());
    drop(release_guard);
    assert!(first.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    for result in queued {
        assert!(result.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());
    }
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn named_bot_preparation_captures_only_that_bot_and_ordinary_owner_chat_bypasses_it() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let (_, other_authority, other_command) = fixture();
    let other = created(&mut repo, other_authority, other_command);
    let (reply, _) = sync_channel(1);
    let command = WorkerCommand::ReadChat {
        context: context(true).into(),
        chat: bot.main_chat,
        reply,
    };
    assert_eq!(
        preparation::bot_targets(&repo, &command).unwrap(),
        Some(vec![(bot.id, bot.main_chat)])
    );
    let (reply, _) = sync_channel(1);
    let ordinary = WorkerCommand::ReadChat {
        context: context(true).into(),
        chat: ChatId::new(),
        reply,
    };
    assert!(
        preparation::bot_targets(&repo, &ordinary)
            .unwrap()
            .is_none()
    );
    assert_ne!(bot.id, other.id);
}

fn branch_attachment(chat_id: ChatId) -> koru_ai_core::AttachmentRecord {
    koru_ai_core::AttachmentRecord {
        chat_id,
        attachment: koru_ai_core::ChatAttachment {
            id: koru_ai_core::ChatAttachmentId::new(),
            message_id: None,
            ordinal: None,
            kind: koru_ai_core::AttachmentKind::Image,
            source: koru_ai_core::AttachmentSource::Clipboard,
            file_name: "fixture.png".into(),
            media_type: "image/png".into(),
            byte_count: 1,
            dimensions: Some((1, 1)),
            origin: None,
            reference_path: None,
            created_at: at(1),
        },
        has_sent_copy: false,
        content_blake3: "a".repeat(64),
    }
}

fn branch_fixture(repo: &mut Repository, bot: &Bot, destination: ChatId) -> BranchCopy {
    let access = ChatReadAccess::authorized(vec![bot.main_chat]);
    let mut chat = repo
        .chat_with_access(bot.main_chat, &access)
        .unwrap()
        .unwrap();
    chat.id = ChatId::new();
    repo.create_chat(&chat).unwrap();
    let pending = turn(chat.id, Uuid::new_v4(), 1);
    repo.begin_turn(&pending).unwrap();
    let mut message = pending.user_message;
    let original = message.id;
    message.id = MessageId::new();
    message.chat_id = destination;
    message.origin_message_id = Some(original);
    let mut tool_policy = repo.chat_tool_policy(chat.id).unwrap().unwrap();
    tool_policy.chat_id = destination;
    chat.branched_from = Some(BranchOrigin {
        chat_id: chat.id,
        through_message_id: original,
    });
    chat.id = destination;
    let mut attachment = branch_attachment(destination);
    attachment.attachment.message_id = Some(message.id);
    attachment.attachment.ordinal = Some(0);
    BranchCopy {
        chat,
        tool_policy,
        messages: vec![message],
        blocks: vec![],
        citations: vec![],
        pinned_documents: vec![],
        attachments: vec![attachment],
    }
}

#[test]
fn branch_destination_guard_refuses_hidden_bot_before_attachment_dispatch() {
    use crate::attachments::{Cleanup, CleanupJob};
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let existing_attachment = branch_attachment(bot.main_chat);
    repo.stage_attachment(&existing_attachment).unwrap();
    let copy = branch_fixture(&mut repo, &bot, bot.main_chat);
    assert!(repo.chat(bot.main_chat).unwrap().is_none());
    let (cleanup, jobs) = Cleanup::recording();
    let authority: Arc<dyn BotAuthority> = authority;
    for owner in [false, true] {
        let (reply, response) = sync_channel(1);
        let dispatched = guard_command(
            &repo,
            Some(&authority),
            WorkerCommand::BranchChat {
                context: context(owner).into(),
                chat: copy.chat.branched_from.as_ref().unwrap().chat_id,
                request: BranchChatRequest {
                    branch_chat_id: bot.main_chat,
                    through_message_id: copy
                        .chat
                        .branched_from
                        .as_ref()
                        .unwrap()
                        .through_message_id,
                    title: copy.chat.title.clone(),
                },
                reply,
            },
        );
        // A recording worker is enough: a rejected command must never reach copy dispatch.
        if dispatched.is_some() {
            cleanup.submit(CleanupJob::CopyBranch {
                folders: vec![],
                done: Box::new(|_| {}),
            });
        }
        assert!(dispatched.is_none());
        assert_eq!(
            response.recv().unwrap().unwrap_err().code,
            "idempotency_conflict"
        );
        assert!(jobs.try_recv().is_err());
    }
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot));
    assert_eq!(
        repo.attachment(existing_attachment.attachment.id).unwrap(),
        Some(existing_attachment)
    );
}

#[test]
fn branch_copy_completion_collision_cleans_only_owned_attachment_ids() {
    use super::super::attachments::commit_branch_copy;
    use crate::attachments::{Cleanup, CleanupJob};
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority, command);
    let preserved = branch_attachment(bot.main_chat);
    repo.stage_attachment(&preserved).unwrap();
    let (cleanup, jobs) = Cleanup::recording();
    let copy = branch_fixture(&mut repo, &bot, bot.main_chat);
    assert_eq!(
        commit_branch_copy(&mut repo, &cleanup, &copy)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    let CleanupJob::RemoveAttachments(removed) = jobs.try_recv().unwrap() else {
        panic!("must never remove an existing chat folder");
    };
    assert_eq!(
        removed,
        vec![(bot.main_chat, copy.attachments[0].attachment.id)]
    );
    assert_ne!(removed[0].1, preserved.attachment.id);
    assert_eq!(
        repo.attachment(preserved.attachment.id).unwrap(),
        Some(preserved)
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    // A destination created while the asynchronous copy ran is also a collision,
    // even if its branch metadata matches. This copy owns no persisted row there.
    let copy = branch_fixture(&mut repo, &bot, ChatId::new());
    repo.create_chat(&copy.chat).unwrap();
    assert_eq!(
        commit_branch_copy(&mut repo, &cleanup, &copy)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert!(matches!(
        jobs.try_recv().unwrap(),
        CleanupJob::RemoveAttachments(_)
    ));
    assert_eq!(repo.chat(copy.chat.id).unwrap(), Some(copy.chat.clone()));
    assert!(repo.messages(copy.chat.id).unwrap().is_empty());
}

#[test]
fn ordinary_branch_copy_commits_and_inflight_destination_is_reserved() {
    use super::super::attachments::{commit_branch_copy, require_unreserved_branch};
    use crate::attachments::Cleanup;
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority, command);
    let copy = branch_fixture(&mut repo, &bot, ChatId::new());
    let (cleanup, jobs) = Cleanup::recording();
    let reservations = HashSet::from([copy.chat.id]);
    assert_eq!(
        require_unreserved_branch(&reservations, copy.chat.id)
            .unwrap_err()
            .code,
        "chat_busy"
    );
    assert!(require_unreserved_branch(&reservations, ChatId::new()).is_ok());
    commit_branch_copy(&mut repo, &cleanup, &copy).unwrap();
    assert!(jobs.try_recv().is_err());
    assert_eq!(repo.chat(copy.chat.id).unwrap(), Some(copy.chat.clone()));
    assert_eq!(repo.messages(copy.chat.id).unwrap(), copy.messages);
    assert_eq!(
        repo.attachment_records(copy.chat.id).unwrap(),
        copy.attachments
    );
}
