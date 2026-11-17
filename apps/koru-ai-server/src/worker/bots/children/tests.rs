// SPDX-License-Identifier: MPL-2.0
//! Cohorts `m6_ask_checks`, `m6_ask_idempotent`, `m6_child_enroll_before_reply`
//! and `m6_child_reads`. The ask runs through the real spawned worker over an
//! in-memory ai.db, with a scripted delegate standing in for the execution
//! owner. No Store, provider, browser or network is opened.
use super::*;
use crate::worker::bots::admission::make_intent;
use crate::worker::bots::tests::{Authority, created, fixture, turn as pending};
use koru_ai_core::bots::replay::BotChatPreparation;
use koru_ai_core::bots::{BotAdmissionRefusal, BotReceiptStage};
use koru_delegate_api::bots::BotCommand as D;
use koru_delegate_core as d;
use koru_delegate_core::children::*;
use koru_delegate_core::decisions::*;
use std::sync::Mutex;

/// What the scripted owner answers to `RequestChild`.
#[derive(Clone)]
enum Answer {
    Create,
    Refuse(&'static str, Option<u8>),
    Away,
    /// An answer the worker cannot read as the ask's.
    Garbled,
}

struct ChildOwner {
    boot: d::DelegateBootId,
    answer: Mutex<Answer>,
    asks: Mutex<Vec<ChildRequest>>,
    /// Requests the owner holds, by its key; replay answers the same.
    held: Mutex<Option<(ChildRequested, Vec<ChildRequestReceipt>)>>,
}
impl ChildOwner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            boot: d::DelegateBootId::mint(),
            answer: Mutex::new(Answer::Create),
            asks: Mutex::default(),
            held: Mutex::default(),
        })
    }
    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }
    fn asks(&self) -> usize {
        self.asks.lock().unwrap().len()
    }
    fn requested(&self, request: &ChildRequest) -> ChildRequested {
        if let Some((requested, _)) = self.held.lock().unwrap().as_ref() {
            return requested.clone();
        }
        let wait = request.wait().unwrap();
        let origin = request.origin().unwrap();
        let receipts: Vec<ChildRequestReceipt> = request
            .requests
            .iter()
            .map(|item| ChildRequestReceipt {
                child_request: item.child_request,
                parent: d::BotRunKey {
                    owner: request.parent.owner,
                    bot: request.parent.bot,
                    run: request.parent.run,
                },
                wait,
                ordinal: item.binding.ordinal,
                recipient: item.binding.recipient_bot,
                child_run: item.binding.child_run,
                state: ChildRequestState::Open,
                settled_by: None,
                child: d::BotAdmissionState::Queued,
                clock: ChildClock::Running,
                elapsed_ms: 0,
                budget_ms: BOT_CHILD_WAIT_MS as u32,
                revision: DecisionRevision::new(1).unwrap(),
                binding_digest: item.binding.digest().unwrap(),
            })
            .collect();
        let requested = ChildRequested {
            wait: ChildWaitView {
                wait,
                phase: BotWaitPhase::HoldRequested,
                revision: DecisionRevision::new(1).unwrap(),
                open_requests: receipts.len() as u8,
            },
            enrollment: BotChildWaitCleanupIntentV1::frozen(wait, &origin).unwrap(),
            receipts: receipts.clone(),
        };
        *self.held.lock().unwrap() = Some((requested.clone(), receipts));
        requested
    }
}
impl BotDelegatePort for ChildOwner {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            D::Hello { .. } => Ok(BotReply::Hello {
                hello: d::BotHello {
                    boot: self.boot,
                    entries: vec![],
                    next: None,
                },
            }),
            D::RequestChild { request, boot } => {
                assert_eq!(boot, self.boot, "the ask carries the owner's boot");
                use koru_delegate_core::decisions::DecisionValidate;
                assert!(request.validate().is_ok(), "the ask is well formed");
                self.asks.lock().unwrap().push(request.clone());
                match self.answer.lock().unwrap().clone() {
                    Answer::Create => {
                        let requested = self.requested(&request);
                        Ok(BotReply::ChildRequested {
                            outcome: koru_delegate_api::bots::ChildRequestedOutcome::Created,
                            wait: requested.wait,
                            enrollment: requested.enrollment,
                            receipts: requested.receipts,
                        })
                    }
                    Answer::Refuse(code, ordinal) => {
                        let failure =
                            koru_delegate_api::bots::child_failure(code).unwrap_or_else(|| {
                                koru_delegate_api::Failure::new(code, "refused", false)
                            });
                        Err(match ordinal {
                            Some(ordinal) => failure.with_detail("ordinal", u64::from(ordinal)),
                            None => failure,
                        })
                    }
                    Answer::Away => Err(koru_delegate_api::Failure::unavailable()),
                    Answer::Garbled => Ok(BotReply::Applied),
                }
            }
            D::Reconcile { .. } => Ok(BotReply::Missing),
            D::ReadChildRequests { subject } => {
                let held = self.held.lock().unwrap().clone();
                Ok(match held {
                    Some((requested, receipts)) if receipts[0].parent == subject => {
                        BotReply::ChildRequests {
                            wait: Some(requested.wait),
                            requests: receipts,
                        }
                    }
                    _ => BotReply::ChildRequests {
                        wait: None,
                        requests: vec![],
                    },
                })
            }
            _ => Err(koru_delegate_api::Failure::new(
                "fixture_pending",
                "fixture recovery stays pending",
                true,
            )),
        }
    }
    fn execute_within(
        &self,
        command: D,
        _within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        self.execute(command)
    }
}

/// A Bot whose reply is executing (its admission accepted with a fence and
/// its AI attempt bound to it), and two more Bots of the same person,
/// workspace and computer, in a spawned worker. The reply is prepared on the
/// writer after the worker started: a worker's start interrupts every reply
/// it finds live.
struct World {
    worker: WorkerHandle,
    client: AiWorkerClient,
    owner: Arc<ChildOwner>,
    parent: Bot,
    subject: RunBotSubject,
    fence: d::BotExecutionFence,
    recipients: Vec<Bot>,
}
impl World {
    fn stop(self) {
        self.worker.shutdown();
        let _ = self.worker.join();
    }
}

/// One step on the running worker's writer.
fn on_writer<T: Send + 'static>(
    client: &AiWorkerClient,
    work: impl FnOnce(&mut WorkerState) -> T + Send + 'static,
) -> T {
    let (reply, answer) = sync_channel(1);
    client
        .sender
        .send(Envelope::WithStateForFixture(Box::new(move |state| {
            let _ = reply.send(work(state));
        })))
        .unwrap();
    answer.recv_timeout(Duration::from_secs(10)).unwrap()
}

fn another(
    repo: &mut Repository,
    authority: &Arc<Authority>,
    home: WorkspaceId,
    name: &str,
) -> Bot {
    let (_, _, mut command) = fixture();
    if let BotCommand::Create {
        home_workspace,
        config,
        ..
    } = &mut command
    {
        *home_workspace = home;
        config.role.workspace = home;
        config.name = name.into();
        config.working_folder = if cfg!(windows) {
            format!("D:/isolated-{name}")
        } else {
            format!("/isolated-{name}")
        };
    }
    created(repo, authority.clone(), command)
}

fn world() -> World {
    let (mut repo, authority, create) = fixture();
    let parent = created(&mut repo, authority.clone(), create);
    let recipients = vec![
        another(&mut repo, &authority, parent.home_workspace, "helper"),
        another(&mut repo, &authority, parent.home_workspace, "reviewer"),
    ];
    let profile = repo
        .chat_with_access(
            parent.main_chat,
            &ChatReadAccess::authorized(vec![parent.main_chat]),
        )
        .unwrap()
        .unwrap()
        .profile_id
        .unwrap();
    let owner = ChildOwner::new();
    let (worker, client) = spawn(repo, authority, owner.clone());
    let bot = parent.clone();
    let (subject, fence) = on_writer(&client, move |state| {
        live_reply(&mut state.repository, &bot, profile)
    });
    World {
        worker,
        client,
        owner,
        parent,
        subject,
        fence,
        recipients,
    }
}

fn live_reply(
    repo: &mut Repository,
    parent: &Bot,
    profile: koru_ai_core::ProfileId,
) -> (RunBotSubject, d::BotExecutionFence) {
    let mut turn = pending(parent.main_chat, Uuid::new_v4(), 1);
    turn.attempt.provider = parent.config.model.provider.clone();
    turn.attempt.requested_model = parent.config.model.requested_model.clone();
    turn.assistant_message.provider = Some(turn.attempt.provider.clone());
    turn.assistant_message.requested_model = Some(turn.attempt.requested_model.clone());
    let pins = vec![d::RunContextItem::Document {
        workspace: parent.home_workspace,
        document: parent.config.role.document,
        version: d::DocumentVersionId::new(),
    }];
    let mut intent = make_intent(
        parent,
        BotRequestId::mint(),
        parent.main_chat,
        parent.home_workspace,
        &BotTask {
            text: turn.user_message.markdown.clone(),
            documents: Vec::new(),
        },
        None,
        &pins,
        profile,
    )
    .unwrap();
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    repo.prepare_bot_chat(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &"a".repeat(64),
        1,
        turn.attempt.queued_at,
    )
    .unwrap();
    let p = &intent.submission.provenance;
    let fence = d::BotExecutionFence {
        owner: p.owner,
        bot: parent.id,
        run: intent.submission.request.run,
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::FIRST,
        session: d::SessionId::mint(),
        turn: Some(d::AgentTurnId::mint()),
    };
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    repo.acknowledge_bot_submission(
        owner,
        parent.id,
        p.submission,
        &d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission,
            },
            run: intent.submission.request.run,
            state: d::BotAdmissionState::Executing,
            queue_sequence: 1,
            fence: Some(fence.clone()),
        },
        2,
    )
    .unwrap();
    repo.bind_bot_execution(turn.attempt.id, &fence, Timestamp::now())
        .unwrap();
    let subject = RunBotSubject {
        run: intent.submission.request.run,
        generation: fence.generation,
        provenance: intent.submission.provenance.clone(),
        captured: intent.submission.policy.clone(),
    };
    (subject, fence)
}

fn spawn(
    repo: Repository,
    authority: Arc<Authority>,
    owner: Arc<ChildOwner>,
) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-child-asks"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_delegate(owner);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

fn item(bot: &Bot, task: &str) -> RunChildAskItem {
    RunChildAskItem {
        bot_id: bot.id,
        task: d::Objective::new(task).unwrap(),
        documents: vec![],
    }
}

fn ask(subject: &RunBotSubject, key: Uuid, items: Vec<RunChildAskItem>) -> RunChildAskRequest {
    RunChildAskRequest {
        subject: subject.clone(),
        ask_key: key,
        requests: items,
        deadline: Instant::now() + Duration::from_secs(10),
        cancellation: Cancellation::default(),
    }
}

fn code(result: ApiResult<RunChildAskPending>) -> (String, Option<u64>) {
    let error = result.expect_err("the ask is refused");
    let index = error
        .details
        .get("index")
        .and_then(serde_json::Value::as_u64);
    (error.code, index)
}

// ---- m6_ask_checks --------------------------------------------------------

#[test]
fn m6_ask_checks_the_shape_is_one_to_three_distinct_bots_under_a_v4_key() {
    let w = world();
    let one = item(&w.recipients[0], "Do the first thing");
    let mut empty = ask(&w.subject, Uuid::new_v4(), vec![]);
    assert!(ask::validate_request(&mut empty).is_err());
    let mut four = ask(&w.subject, Uuid::new_v4(), vec![one.clone(); 4]);
    assert!(ask::validate_request(&mut four).is_err());
    // The same Bot named twice is `invalid_request` (contract 2.5).
    let mut twice = ask(&w.subject, Uuid::new_v4(), vec![one.clone(), one.clone()]);
    assert_eq!(
        ask::validate_request(&mut twice).unwrap_err().code,
        "invalid_request"
    );
    let mut nil = ask(&w.subject, Uuid::nil(), vec![one.clone()]);
    assert!(ask::validate_request(&mut nil).is_err());
    let mut pages = item(&w.recipients[0], "Read these");
    pages.documents = (0..17).map(|_| koru_ai_core::DocumentId::new()).collect();
    let mut many = ask(&w.subject, Uuid::new_v4(), vec![pages]);
    assert!(ask::validate_request(&mut many).is_err());
    let mut fine = ask(&w.subject, Uuid::new_v4(), vec![one]);
    assert!(ask::validate_request(&mut fine).is_ok());
}

#[test]
fn m6_ask_checks_every_delegate_refusal_has_one_tool_code_and_retryable_ones_decide_nothing() {
    let refused = |code: &str, ordinal: Option<u8>| {
        let failure = koru_delegate_api::bots::child_failure(code)
            .unwrap_or_else(|| koru_delegate_api::Failure::new(code, "x", false));
        let failure = match ordinal {
            Some(o) => failure.with_detail("ordinal", u64::from(o)),
            None => failure,
        };
        delegate_refusal(&failure)
    };
    let table = [
        (
            "bot_child_decision_open",
            None,
            Some(("decision_in_progress", None)),
        ),
        (
            "bot_child_wait_open",
            None,
            Some((codes::CHILD_WAIT_IN_PROGRESS, None)),
        ),
        (
            "bot_child_depth",
            None,
            Some((codes::ASK_NOT_ALLOWED, None)),
        ),
        (
            "bot_child_self_ask",
            Some(1),
            Some((codes::SELF_ASK, Some(1))),
        ),
        (
            "bot_child_recipient_limit",
            None,
            Some((codes::RECIPIENT_LIMIT, None)),
        ),
        (
            "bot_child_cycle",
            Some(2),
            Some((codes::WOULD_WAIT_ON_ITSELF, Some(2))),
        ),
        (
            "bot_child_recipient_paused",
            Some(0),
            Some((codes::RECIPIENT_PAUSED, Some(0))),
        ),
        (
            "bot_child_recipient_stale",
            Some(0),
            Some((codes::RECIPIENT_UNAVAILABLE, Some(0))),
        ),
        (
            "bot_child_recipient_full",
            Some(1),
            Some((codes::RECIPIENT_FULL, Some(1))),
        ),
        (
            "bot_child_capacity",
            None,
            Some((codes::CHILD_CAPACITY, None)),
        ),
        (
            "bot_child_budget_low",
            None,
            Some((codes::CHILD_BUDGET_LOW, None)),
        ),
        (
            "bot_child_folder_overlap",
            Some(0),
            Some((codes::RECIPIENT_FOLDER_OVERLAP, Some(0))),
        ),
        (
            "bot_child_connections",
            Some(0),
            Some((codes::RECIPIENT_UNAVAILABLE, Some(0))),
        ),
        (
            "bot_folder_unavailable",
            Some(0),
            Some((codes::RECIPIENT_UNAVAILABLE, Some(0))),
        ),
        ("bot_conflict", None, Some((codes::ASK_KEY_CONFLICT, None))),
        ("bot_invalid", None, Some(("invalid_request", None))),
        ("bot_fenced", None, Some(("permission_denied", None))),
        // The two retryable codes say nothing final: the receipts stay.
        ("bot_child_wait_closing", None, None),
    ];
    for (code, ordinal, expected) in table {
        assert_eq!(refused(code, ordinal), expected, "{code}");
    }
    let busy = koru_delegate_api::Failure::new("scope_recovery_pending", "x", true);
    assert_eq!(delegate_refusal(&busy), None);
    // Every mapped code is a tool code the gateway knows, with its index.
    for (code, ordinal, expected) in table {
        if let Some((tool, index)) = expected {
            let failure = coded(tool, index);
            assert_eq!(failure.code, tool, "{code}");
            assert_eq!(
                failure
                    .details
                    .get("index")
                    .and_then(serde_json::Value::as_u64),
                ordinal.filter(|_| index.is_some()).map(u64::from)
            );
        }
    }
}

#[test]
fn m6_ask_checks_ai_refusals_keep_their_code_and_index() {
    for (refusal, code, index) in [
        (
            BotChildAskRefusal::AskNotAllowed,
            codes::ASK_NOT_ALLOWED,
            None,
        ),
        (
            BotChildAskRefusal::RecipientUnavailable { index: 2 },
            codes::RECIPIENT_UNAVAILABLE,
            Some(2),
        ),
        (
            BotChildAskRefusal::RecipientPaused { index: 1 },
            codes::RECIPIENT_PAUSED,
            Some(1),
        ),
        (
            BotChildAskRefusal::SelfAsk { index: 0 },
            codes::SELF_ASK,
            Some(0),
        ),
        (
            BotChildAskRefusal::AskKeyConflict,
            codes::ASK_KEY_CONFLICT,
            None,
        ),
    ] {
        let failure = ai_refusal(refusal);
        assert_eq!(failure.code, code);
        assert!(!failure.retryable);
        assert_eq!(
            failure
                .details
                .get("index")
                .and_then(serde_json::Value::as_u64),
            index
        );
    }
}

#[test]
fn m6_ask_checks_self_ask_unknown_recipient_and_a_foreign_run_never_reach_the_delegate() {
    let w = world();
    let stranger = {
        let (mut other_repo, other_authority, create) = fixture();
        created(&mut other_repo, other_authority, create)
    };
    let (client, owner) = (w.client.clone(), w.owner.clone());
    let self_ask = client.ask_bots_for_run(ask(
        &w.subject,
        Uuid::new_v4(),
        vec![item(&w.recipients[0], "One"), item(&w.parent, "Two")],
    ));
    assert_eq!(code(self_ask), (codes::SELF_ASK.into(), Some(1)));
    let unknown = client.ask_bots_for_run(ask(
        &w.subject,
        Uuid::new_v4(),
        vec![item(&stranger, "Hello")],
    ));
    assert_eq!(
        code(unknown),
        (codes::RECIPIENT_UNAVAILABLE.into(), Some(0))
    );
    // A Run that is not the asking reply's is refused before anything.
    let mut foreign = w.subject.clone();
    foreign.run = d::RunId::mint();
    let denied = client.ask_bots_for_run(ask(
        &foreign,
        Uuid::new_v4(),
        vec![item(&w.recipients[0], "Hello")],
    ));
    assert_eq!(code(denied).0, "permission_denied");
    assert_eq!(owner.asks(), 0, "no refusal reached the delegate");
    w.stop();
}

#[test]
fn m6_ask_checks_the_askable_list_is_the_persons_other_bots_here() {
    let w = world();
    let client = w.client.clone();
    let mut names: Vec<_> = client
        .askable_bots_for_run(RunAskableRequest {
            subject: w.subject.clone(),
            deadline: Instant::now() + Duration::from_secs(5),
        })
        .unwrap()
        .into_iter()
        .map(|bot| (bot.bot_id, bot.name))
        .collect();
    names.sort();
    let mut expected: Vec<_> = w
        .recipients
        .iter()
        .map(|bot| (bot.id, bot.config.name.clone()))
        .collect();
    expected.sort();
    assert_eq!(names, expected, "never the asking Bot itself");
    w.stop();
}

// ---- m6_ask_idempotent ----------------------------------------------------

#[test]
fn m6_ask_idempotent_the_pages_suffix_round_trips_and_the_last_one_wins() {
    let documents: Vec<koru_ai_core::DocumentId> =
        (0..3).map(|_| koru_ai_core::DocumentId::new()).collect();
    let text = child_text("Summarise", &documents);
    assert_eq!(child_documents(&text), Some(documents.clone()));
    assert_eq!(child_documents(&child_text("No pages", &[])), Some(vec![]));
    // A task that spells the marker itself is followed by the worker's own.
    let tricky = child_text(&child_text("Spoof", &documents), &[]);
    assert_eq!(child_documents(&tricky), Some(vec![]));
    assert_eq!(child_documents("no suffix at all"), None);
    assert_eq!(child_title("\n\n  First line\nsecond"), "First line");
}

#[test]
fn m6_ask_idempotent_the_same_key_answers_the_same_ids_and_another_list_conflicts() {
    let w = world();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    let key = Uuid::new_v4();
    let items = vec![
        item(&w.recipients[0], "Draft the outline"),
        item(&w.recipients[1], "Review the outline"),
    ];
    let first = client
        .ask_bots_for_run(ask(&w.subject, key, items.clone()))
        .expect("the ask is admitted");
    assert_eq!(first.requests.len(), 2);
    assert_eq!(first.requests[0].1, w.recipients[0].id);
    assert_eq!(first.requests[1].1, w.recipients[1].id);
    assert_eq!(owner.asks(), 1);
    let again = client
        .ask_bots_for_run(ask(&w.subject, key, items.clone()))
        .expect("the same key answers again");
    assert_eq!(again, first, "the same ids, no second ask");
    assert_eq!(owner.asks(), 1, "answered from ai.db");
    let other = client.ask_bots_for_run(ask(
        &w.subject,
        key,
        vec![item(&w.recipients[0], "Something else")],
    ));
    assert_eq!(code(other).0, codes::ASK_KEY_CONFLICT);
    // The ask the delegate saw names the parent's exact turn and person.
    let sent = owner.asks.lock().unwrap()[0].clone();
    assert_eq!(sent.parent, w.fence);
    assert_eq!(sent.ask_key, key);
    for item in &sent.requests {
        let p = &item.submission.provenance;
        assert_eq!(p.parent.unwrap().run, w.subject.run);
        assert!(item.binding.documents.is_empty());
        // The child holds only grants the parent also holds.
        assert!(
            item.submission
                .policy
                .connections
                .is_subset(&w.subject.captured.connections)
        );
    }
    w.stop();
}

#[test]
fn m6_ask_idempotent_a_refusal_is_answered_again_from_its_rejected_receipts() {
    let w = world();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    owner.answer(Answer::Refuse("bot_child_cycle", Some(0)));
    let key = Uuid::new_v4();
    let items = vec![item(&w.recipients[0], "Help me")];
    let refused = client.ask_bots_for_run(ask(&w.subject, key, items.clone()));
    assert_eq!(code(refused), (codes::WOULD_WAIT_ON_ITSELF.into(), Some(0)));
    owner.answer(Answer::Create);
    let again = client.ask_bots_for_run(ask(&w.subject, key, items));
    assert_eq!(code(again), (codes::WOULD_WAIT_ON_ITSELF.into(), Some(0)));
    assert_eq!(owner.asks(), 1, "the refusal is answered from ai.db");
    // A refused ask counts for nothing: a new key may ask.
    let fresh = client.ask_bots_for_run(ask(
        &w.subject,
        Uuid::new_v4(),
        vec![item(&w.recipients[0], "Help me now")],
    ));
    assert!(fresh.is_ok());
    w.stop();
}

#[test]
fn m6_ask_idempotent_an_unknown_outcome_is_retried_with_the_same_key() {
    let w = world();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    owner.answer(Answer::Away);
    let key = Uuid::new_v4();
    let items = vec![item(&w.recipients[0], "Help me")];
    let mut short = ask(&w.subject, key, items.clone());
    short.deadline = Instant::now() + Duration::from_secs(2);
    let unknown = client.ask_bots_for_run(short);
    assert_eq!(code(unknown).0, codes::CHILD_STATUS_UNKNOWN);
    assert!(owner.asks() >= 1, "a retryable refusal is asked again");
    // The delegate recorded nothing; the same key sends the same ask.
    owner.answer(Answer::Create);
    let admitted = client
        .ask_bots_for_run(ask(&w.subject, key, items))
        .expect("the retry is admitted");
    let sent = owner.asks.lock().unwrap().clone();
    assert!(
        sent.windows(2).all(|pair| pair[0] == pair[1]),
        "every send of one key carries the same bytes"
    );
    assert_eq!(admitted.requests[0].1, w.recipients[0].id);
    w.stop();
}

// ---- m6_child_enroll_before_reply -----------------------------------------

/// The cleanup route, answered once the worker has adopted its owner.
fn route(
    client: &AiWorkerClient,
    request: impl Fn() -> Result<
        crate::worker::routine_call::RoutineCall<BotChildWaitEnrollmentReply, BotWaitRefusal>,
        BotWaitRefusal,
    >,
) -> BotChildWaitEnrollmentReply {
    let _ = client;
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = match request() {
            Ok(call) => futures_lite_block(call)
                .unwrap_or_else(|code| BotChildWaitEnrollmentReply::Refused { code }),
            Err(code) => BotChildWaitEnrollmentReply::Refused { code },
        };
        match reply {
            BotChildWaitEnrollmentReply::Refused {
                code: BotWaitRefusal::Unavailable,
            } if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            reply => return reply,
        }
    }
}

/// Wait for one call on this thread.
fn futures_lite_block<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::task::{Context, Poll, Wake, Waker};
    struct Thread(std::thread::Thread);
    impl Wake for Thread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Thread(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
}

fn context() -> crate::worker::routine_call::RoutineCallContext {
    crate::worker::routine_call::RoutineCallContext::new(
        Uuid::new_v4(),
        Instant::now() + Duration::from_secs(4),
        Cancellation::default(),
    )
}

#[test]
fn m6_child_enroll_before_reply_the_enrollment_exists_when_the_answer_arrives() {
    let w = world();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    let pending = client
        .ask_bots_for_run(ask(
            &w.subject,
            Uuid::new_v4(),
            vec![item(&w.recipients[0], "Help me")],
        ))
        .expect("admitted");
    let held = owner.held.lock().unwrap().clone().unwrap().0;
    assert_eq!(held.wait.wait, pending.wait);
    let key = held.enrollment.key().unwrap();
    // The read route answers Enrolled at once: the row was written in the
    // same transaction that accepted the receipts, before the answer.
    let read = route(&client, || {
        client.read_bot_child_wait_cleanup(context(), ReadBotChildWaitCleanup { key: key.clone() })
    });
    assert!(
        matches!(&read, BotChildWaitEnrollmentReply::Enrolled { key: k, .. } if *k == key),
        "{read:?}"
    );
    // The delegate's enroll answers the same row, never Missing.
    let enrolled = route(&client, || {
        client.enroll_bot_child_wait_cleanup(
            context(),
            EnrollBotChildWaitCleanup {
                key: key.clone(),
                cleanup_intent: held.enrollment.clone(),
            },
        )
    });
    assert_eq!(enrolled, read);
    w.stop();
}

#[test]
fn m6_child_enroll_before_reply_the_enroll_route_creates_the_row_when_the_reply_was_lost() {
    let w = world();
    let (client, owner) = (w.client.clone(), w.owner.clone());
    // The delegate admitted the ask, but its reply did not reach the worker
    // as the ask's (section 9 row 5).
    owner.answer(Answer::Garbled);
    let lost = client.ask_bots_for_run(ask(
        &w.subject,
        Uuid::new_v4(),
        vec![item(&w.recipients[0], "Help me")],
    ));
    assert_eq!(code(lost).0, codes::CHILD_STATUS_UNKNOWN);
    let sent = owner.asks.lock().unwrap()[0].clone();
    let intent =
        BotChildWaitCleanupIntentV1::frozen(sent.wait().unwrap(), &sent.origin().unwrap()).unwrap();
    let key = intent.key().unwrap();
    let missing = route(&client, || {
        client.read_bot_child_wait_cleanup(context(), ReadBotChildWaitCleanup { key: key.clone() })
    });
    assert!(
        matches!(missing, BotChildWaitEnrollmentReply::Missing { .. }),
        "{missing:?}"
    );
    let enrolled = route(&client, || {
        client.enroll_bot_child_wait_cleanup(
            context(),
            EnrollBotChildWaitCleanup {
                key: key.clone(),
                cleanup_intent: intent.clone(),
            },
        )
    });
    assert!(
        matches!(&enrolled, BotChildWaitEnrollmentReply::Enrolled { key: k, .. } if *k == key),
        "{enrolled:?}"
    );
    // A different intent for the same wait is a conflict, never a second row.
    let mut other = intent.clone();
    other.origin_turn = d::AgentTurnId::mint();
    let conflict = route(&client, || {
        client.enroll_bot_child_wait_cleanup(
            context(),
            EnrollBotChildWaitCleanup {
                key: other.key().unwrap(),
                cleanup_intent: other.clone(),
            },
        )
    });
    assert!(matches!(
        conflict,
        BotChildWaitEnrollmentReply::Refused { .. }
    ));
    w.stop();
}

// ---- m6_child_reads -------------------------------------------------------

fn receipt(
    state: ChildRequestState,
    settled_by: Option<ChildSettledBy>,
    child: d::BotAdmissionState,
) -> ChildRequestReceipt {
    ChildRequestReceipt {
        child_request: ChildRequestId::mint(),
        parent: d::BotRunKey {
            owner: d::BotOwner {
                user_scope: koru_ai_core::bots::UserScopeId::new(),
                device: d::DeviceId::new(),
            },
            bot: d::BotId::mint(),
            run: d::RunId::mint(),
        },
        wait: BotWaitId::mint(),
        ordinal: 0,
        recipient: d::BotId::mint(),
        child_run: d::RunId::mint(),
        state,
        settled_by,
        child,
        clock: ChildClock::Stopped,
        elapsed_ms: 0,
        budget_ms: BOT_CHILD_WAIT_MS as u32,
        revision: DecisionRevision::new(1).unwrap(),
        binding_digest: DecisionDigest::from_bytes([0; 32]),
    }
}

#[test]
fn m6_child_reads_every_card_state_derives_its_outcome_and_progress() {
    use ChildRequestState::{Open, Settled};
    use d::BotAdmissionState as A;
    use koru_ai_core::bots::{BotChildOutcome as O, BotChildProgress as P};
    use reads::{ChildEnd as E, outcome, progress};
    // Open requests have no outcome; progress is the child's admission.
    for (child, needs, expected) in [
        (A::Queued, false, P::Queued),
        (A::Reserved, false, P::Running),
        (A::Executing, false, P::Running),
        (A::Executing, true, P::NeedsYou),
        (A::Settled, false, P::Finished),
    ] {
        let r = receipt(Open, None, child);
        assert_eq!(outcome(&r, E::Open), None);
        assert_eq!(progress(&r, needs), expected);
    }
    let settled = |by, child, end| outcome(&receipt(Settled, Some(by), child), end);
    use ChildSettledBy::*;
    assert_eq!(
        settled(TimedOut, A::Executing, E::Completed),
        Some(O::TimedOut)
    );
    assert_eq!(
        settled(ParentStopped, A::Queued, E::Open),
        Some(O::ParentStopped)
    );
    assert_eq!(
        settled(ChildEnded, A::Settled, E::Completed),
        Some(O::Completed)
    );
    assert_eq!(
        settled(ChildEnded, A::Settled, E::Canceled),
        Some(O::Canceled)
    );
    assert_eq!(
        settled(ChildEnded, A::Settled, E::NeededYou),
        Some(O::NeededYou)
    );
    assert_eq!(settled(ChildEnded, A::Settled, E::Failed), Some(O::Failed));
    // Refused before it ran reads failed, whatever the transcript says.
    assert_eq!(
        settled(ChildEnded, A::Refused, E::Completed),
        Some(O::Failed)
    );
}

#[test]
fn m6_child_reads_cards_and_origin_through_the_worker() {
    let w = world();
    let parent_chat = w.parent.main_chat;
    let parent_id = w.parent.id;
    let recipient = w.recipients[0].clone();
    let client = w.client.clone();
    let pending = client
        .ask_bots_for_run(ask(
            &w.subject,
            Uuid::new_v4(),
            vec![item(&recipient, "Help me")],
        ))
        .expect("admitted");
    let context = owner_context();
    let cards = client.bots(
        context.clone(),
        BotCommand::ListChildRequests {
            bot_id: parent_id,
            chat_id: parent_chat,
        },
    );
    let Ok(BotResponse::ChildRequests { requests }) = cards else {
        panic!("{cards:?}");
    };
    assert_eq!(requests.len(), 1);
    let card = &requests[0];
    assert_eq!(card.child_request_id, pending.requests[0].0);
    assert_eq!(card.recipient_bot_id, recipient.id);
    assert_eq!(card.recipient_name, recipient.config.name);
    assert_eq!(card.state, ChildRequestState::Open);
    assert_eq!(card.child, koru_ai_core::bots::BotChildProgress::Queued);
    assert_eq!(card.outcome, None);
    assert_eq!(card.result, koru_ai_core::bots::BotChildResult::None);
    // The child conversation names who asked for it.
    let origin = client.bots(
        owner_context(),
        BotCommand::ReadChildOrigin {
            bot_id: recipient.id,
            chat_id: card.child_chat_id,
        },
    );
    let Ok(BotResponse::ChildOrigin {
        origin: Some(origin),
    }) = origin
    else {
        panic!("{origin:?}");
    };
    assert_eq!(origin.parent_bot_id, parent_id);
    assert_eq!(origin.parent_chat_id, parent_chat);
    // A conversation nobody asked for answers no origin, without asking.
    let other_chat = client.bots(
        context,
        BotCommand::ReadChildOrigin {
            bot_id: parent_id,
            chat_id: parent_chat,
        },
    );
    assert!(
        matches!(other_chat, Ok(BotResponse::ChildOrigin { origin: None })),
        "{other_chat:?}"
    );
    w.stop();
}

fn owner_context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "fixture".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "fixture".into(),
        deadline_millis: None,
    }
}

#[test]
fn m6_child_reads_rejected_and_prepared_asks_are_listed_nowhere() {
    let rows = |stage| BotChildChat {
        child_request: ChildRequestId::mint(),
        parent_bot: BotId::mint(),
        parent_chat: ChatId::new(),
        parent_run: d::RunId::mint(),
        parent_assistant_message: koru_ai_core::MessageId::new(),
        ask_key: Uuid::new_v4(),
        ordinal: 0,
        wait: BotWaitId::mint(),
        recipient_bot: BotId::mint(),
        chat: ChatId::new(),
        child_run: d::RunId::mint(),
        stage,
        created_at_ms: 1,
        refusal: (stage == BotReceiptStage::Rejected).then(|| codes::RECIPIENT_FULL.into()),
        refusal_ordinal: (stage == BotReceiptStage::Rejected).then_some(0),
    };
    assert!(settled_answer(&[rows(BotReceiptStage::Prepared)]).is_none());
    assert!(matches!(
        settled_answer(&[rows(BotReceiptStage::Accepted)]),
        Some(Ok(_))
    ));
    let refused = settled_answer(&[rows(BotReceiptStage::Rejected)]).unwrap();
    assert_eq!(refused.unwrap_err().code, codes::RECIPIENT_FULL);
    let _ = BotAdmissionRefusal::Rejected;
}
