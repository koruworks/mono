// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ModelSelection;
use koru_delegate_core as d;
use std::sync::Mutex;
const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Authority {
    owner: BotOwner,
    workspace: WorkspaceId,
    profile: ProfileId,
    allowed: bool,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, w: WorkspaceId) -> Result<ProfileId, BotError> {
        if self.allowed && w == self.workspace {
            Ok(self.profile)
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.workspace(p.workspace).map(|_| ())
    }
}
struct Delegate {
    calls: Mutex<Vec<DelegateCommand>>,
    replies: Mutex<std::collections::VecDeque<Result<BotReply, koru_delegate_api::Failure>>>,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        self.calls.lock().unwrap().push(command);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected delegate call")
    }
}
fn fixture() -> (Repository, Bot, Arc<Authority>, BotSubmissionIntent) {
    let w = WorkspaceId::new();
    let scope = UserScopeId::new();
    let device = DeviceId::from_uuid(Uuid::new_v4());
    let profile = ProfileId::new();
    let bot = Bot {
        id: BotId::mint(),
        home_workspace: w,
        user_scope: scope,
        owner_device: device,
        config: BotConfig {
            name: "fixture".into(),
            role: BotPage {
                workspace: w,
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
                requested_model: "fixture-model".into(),
                default_reasoning: ReasoningChoice::Level(ReasoningLevel::High),
            },
            mode: Some("ask".into()),
            fast: Some(false),
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
    };
    let authority = Arc::new(Authority {
        owner: owner(&bot),
        workspace: w,
        profile,
        allowed: true,
    });
    let mut repo = Repository::in_memory().unwrap();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "fixture-create", None)
        .unwrap();
    let pins = vec![d::RunContextItem::Document {
        workspace: w,
        document: bot.config.role.document,
        version: d::DocumentVersionId::new(),
    }];
    let intent = make_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        w,
        &BotTask {
            text: "CURRENT-TASK-ONLY".into(),
            documents: vec![],
        },
        None,
        &pins,
        profile,
    )
    .unwrap();
    (repo, bot, authority, intent)
}
fn prepared(repo: &mut Repository, intent: &BotSubmissionIntent) -> BotAdmissionReceipt {
    BotAdmissionReceipt::Submission(repo.prepare_bot_submission(intent, HASH, 1, 2).unwrap())
}
fn result(
    bot: &Bot,
    request: BotRequestId,
    action: Action,
    result: Result<BotReply, koru_delegate_api::Failure>,
) -> BotCoordinationResult {
    BotCoordinationResult {
        owner: owner(bot),
        bot: bot.id,
        request,
        action,
        result,
    }
}
fn admission(intent: &BotSubmissionIntent, state: BotAdmissionState) -> d::BotAdmissionView {
    let p = &intent.submission.provenance;
    d::BotAdmissionView {
        key: BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        },
        run: intent.submission.request.run,
        state,
        queue_sequence: 1,
        fence: None,
    }
}
#[test]
fn missing_ai_linked_intent_fences_without_resubmitting_interrupted_attempt() {
    let (mut repo, bot, _, mut intent) = fixture();
    intent.submission.provenance.ai_attempt_id = Some(Uuid::new_v4());
    intent.submission.provenance.assistant_message_id = Some(Uuid::new_v4());
    let receipt = prepared(&mut repo, &intent);
    let request = intent.submission.provenance.submission;
    let (next, action) = apply_result(
        &mut repo,
        result(&bot, request, Action::Reconcile, Ok(BotReply::Missing)),
        &receipt,
        3,
    )
    .unwrap()
    .unwrap();
    assert!(action.is_none());
    assert!(matches!(
        next,
        BotAdmissionReceipt::Submission(BotSubmissionReceipt {
            outcome: Some(BotExecutionOutcome::RefusalFence { .. }),
            ..
        })
    ));
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
}
#[test]
fn uncertain_submit_keeps_guard_until_exact_fence_ack_then_missing() {
    let (mut repo, bot, _, intent) = fixture();
    let receipt = prepared(&mut repo, &intent);
    let request = intent.submission.provenance.submission;
    let (next, action) = apply_result(
        &mut repo,
        result(&bot, request, Action::Submit, Err(delegate_unavailable())),
        &receipt,
        3,
    )
    .unwrap()
    .unwrap();
    assert!(action.is_none());
    let (round, barrier) = match &next {
        BotAdmissionReceipt::Submission(BotSubmissionReceipt {
            outcome: Some(BotExecutionOutcome::RefusalFence { round, barrier, .. }),
            ..
        }) => (*round, barrier.clone()),
        _ => panic!("missing fence"),
    };
    let mut wrong = barrier.clone();
    wrong.request = BotRequestId::mint();
    assert!(
        apply_result(
            &mut repo,
            result(
                &bot,
                request,
                Action::Fence {
                    round,
                    barrier: barrier.clone()
                },
                Ok(BotReply::Barrier { barrier: wrong })
            ),
            &next,
            4
        )
        .unwrap()
        .is_none()
    );
    assert!(
        apply_result(
            &mut repo,
            result(
                &bot,
                request,
                Action::FencedReconcile {
                    round,
                    barrier: barrier.clone()
                },
                Ok(BotReply::Missing)
            ),
            &next,
            4
        )
        .is_err()
    );
    let (acknowledged, _) = apply_result(
        &mut repo,
        result(
            &bot,
            request,
            Action::Fence {
                round,
                barrier: barrier.clone(),
            },
            Ok(BotReply::Barrier {
                barrier: barrier.clone(),
            }),
        ),
        &next,
        5,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
    apply_result(
        &mut repo,
        result(
            &bot,
            request,
            Action::FencedReconcile { round, barrier },
            Ok(BotReply::Missing),
        ),
        &acknowledged,
        6,
    )
    .unwrap();
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().folder_lock_request, None);
    assert_eq!(
        repo.bot_admission_receipt(owner(&bot), bot.id, request)
            .unwrap()
            .unwrap()
            .operation()
            .status,
        BotOperationStatus::Rejected
    );
}
#[test]
fn delayed_refused_admission_freezes_folder_instead_of_releasing_it() {
    let (mut repo, bot, _, intent) = fixture();
    let receipt = prepared(&mut repo, &intent);
    let request = intent.submission.provenance.submission;
    apply_result(
        &mut repo,
        result(
            &bot,
            request,
            Action::Reconcile,
            Ok(BotReply::Admission {
                admission: admission(&intent, BotAdmissionState::Refused),
            }),
        ),
        &receipt,
        3,
    )
    .unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_frozen_at_ms,
        Some(3)
    );
}
#[test]
fn submit_job_checks_fresh_authority_and_previous_task_settlement_before_ipc() {
    let (mut repo, bot, authority, intent) = fixture();
    let receipt = prepared(&mut repo, &intent);
    let request = intent.submission.provenance.submission;
    let delegate = Arc::new(Delegate {
        calls: Mutex::new(vec![]),
        replies: Mutex::new([Ok(BotReply::Missing)].into()),
    });
    let prior = BotSubmissionKey {
        owner: intent.submission.provenance.owner,
        bot: bot.id,
        request: BotRequestId::mint(),
    };
    let job = BotCoordinationJob {
        owner: owner(&bot),
        bot: bot.id,
        request,
        workspace: bot.home_workspace,
        authority: authority.clone(),
        delegate: delegate.clone(),
        action: Action::Submit,
        receipt: receipt.clone(),
        previous: Some(prior),
        ai_live: false,
    };
    assert_eq!(job.run().result.unwrap_err().code, "bot_previous_active");
    assert!(matches!(
        delegate.calls.lock().unwrap().as_slice(),
        [DelegateCommand::Reconcile { .. }]
    ));
    let denied = Arc::new(Authority {
        allowed: false,
        ..Authority {
            owner: authority.owner,
            workspace: authority.workspace,
            profile: authority.profile,
            allowed: true,
        }
    });
    let job = BotCoordinationJob {
        owner: owner(&bot),
        bot: bot.id,
        request,
        workspace: bot.home_workspace,
        authority: denied,
        delegate: delegate.clone(),
        action: Action::Submit,
        receipt,
        previous: None,
        ai_live: false,
    };
    assert!(job.run().result.is_err());
    assert_eq!(delegate.calls.lock().unwrap().len(), 1);
}
#[test]
fn intent_preserves_current_task_settings_and_requires_mandatory_role_pin() {
    let (_, bot, authority, intent) = fixture();
    assert_eq!(intent.submission.input.text, "CURRENT-TASK-ONLY");
    assert_eq!(
        intent.submission.input.reasoning,
        Some(koru_session_protocol::LaunchReasoning::High)
    );
    assert_eq!(
        intent.submission.input.mode.as_ref().unwrap().as_str(),
        "ask"
    );
    assert_eq!(intent.submission.input.fast, Some(false));
    assert_eq!(intent.submission.policy.destination, bot.home_workspace);
    assert!(intent.submission.provenance.ai_attempt_id.is_none());
    assert!(
        make_intent(
            &bot,
            BotRequestId::mint(),
            bot.main_chat,
            bot.home_workspace,
            &BotTask {
                text: "task".into(),
                documents: vec![]
            },
            None,
            &[],
            authority.profile
        )
        .is_err()
    );
    let mut wire = serde_json::to_value(BotCommand::Start {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
        chat_id: bot.main_chat,
        expected_revision: 1,
        destination: bot.home_workspace,
        task: BotTask {
            text: "task".into(),
            documents: vec![],
        },
    })
    .unwrap();
    wire["run_id"] = serde_json::json!(RunId::mint());
    assert!(serde_json::from_value::<BotCommand>(wire).is_err());
    assert!(
        BotTask {
            text: "x".repeat(8193),
            documents: vec![]
        }
        .validate(bot.home_workspace)
        .is_err()
    );
    assert!(!format!("{:?}", intent).contains("CURRENT-TASK"));
}
#[test]
fn recovery_skips_settled_or_foreign_rows_and_reaches_later_pending_work() {
    let (mut repo, bot, _, first) = fixture();
    let receipt = prepared(&mut repo, &first);
    let request = first.submission.provenance.submission;
    apply_result(
        &mut repo,
        result(
            &bot,
            request,
            Action::Reconcile,
            Ok(BotReply::Admission {
                admission: admission(&first, BotAdmissionState::Settled),
            }),
        ),
        &receipt,
        3,
    )
    .unwrap();
    let mut second = first.clone();
    second.submission.provenance.submission = BotRequestId::mint();
    second.submission.request.run = RunId::mint();
    prepared(&mut repo, &second);
    assert!(recovery_candidate(&repo, bot.id, bot.user_scope, request).is_none());
    assert!(
        recovery_candidate(
            &repo,
            bot.id,
            UserScopeId::new(),
            second.submission.provenance.submission
        )
        .is_none()
    );
    assert!(recovery_candidate(&repo, bot.id, bot.user_scope, BotRequestId::mint()).is_none());
    assert!(
        recovery_candidate(
            &repo,
            bot.id,
            bot.user_scope,
            second.submission.provenance.submission
        )
        .is_some()
    );
}
#[test]
fn ordinary_admin_completion_wakes_admission_recovery_without_a_timer() {
    let (output, events) = sync_channel(4);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut pool = ProviderAdminPool::spawn(None, None, output, shutdown).unwrap();
    let (reply, response) = sync_channel(1);
    assert!(
        pool.try_submit(ProviderAdminJob::Validate {
            provider: AiProviderId::parse("openai").unwrap(),
            reply
        })
        .is_ok()
    );
    let _ = response.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotRecover
    ));
    pool.stop();
}
