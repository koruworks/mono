// SPDX-License-Identifier: MPL-2.0
//! Delegate IPC 19: child requests. Cohorts `m6_ipc19_codec`,
//! `m6_ipc19_frame_version` and the wire half of `m6_child_codes_final`.
use super::*;
use koru_delegate_core::children::*;
use koru_delegate_core::decisions::{
    BotWaitId, BotWaitPhase, ConsumerCleanup, DecisionDigest, DecisionFence, DecisionRevision,
    DecisionValidate, DecisionVersionV1, FreshWaitStopEligibility, OriginWaitStop,
};
use koru_delegate_core::{
    ActorRef, AgentTurnId, BotAccess, BotAdmissionState, BotId, BotOwner, BotProvenance,
    BotRequestId, BotSubmission, BotWorkingFolder, ChildParent, ChildRequestId, DelegateBootId,
    DelegationRequest, DeviceId, IdempotencyKey, Objective, PendingInput, ProfileId, RunOrigin,
    SessionId, SessionProfileKey, WorkspaceScope,
};

const TASK: &str = "CHILD-TASK-SENTINEL";

fn owner() -> BotOwner {
    BotOwner {
        user_scope: serde_json::from_value(serde_json::json!(Uuid::new_v4())).unwrap(),
        device: DeviceId::from_uuid(Uuid::new_v4()),
    }
}

/// One valid ask of `items` recipients, built the way the AI owner builds it.
fn ask(items: usize) -> ChildRequest {
    let owner = owner();
    let workspace = WorkspaceId::from_uuid(Uuid::new_v4());
    let human = ProfileId::from_uuid(Uuid::new_v4());
    let parent = BotExecutionFence {
        owner,
        bot: BotId::mint(),
        run: RunId::mint(),
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
        turn: Some(AgentTurnId::mint()),
    };
    let wait = BotWaitId::mint();
    let ask_key = Uuid::new_v4();
    let policy = CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: BotAccess::Default,
        selected_pages: Default::default(),
        connections: Default::default(),
    };
    let parent_policy = ChildPolicyRef::of(2, 3, &policy).unwrap();
    let requests = (0..items)
        .map(|index| {
            let recipient = BotId::mint();
            let child_request = ChildRequestId::from_uuid(Uuid::new_v4());
            let child_run = RunId::mint();
            let submission = BotSubmission {
                provenance: BotProvenance {
                    parent: Some(ChildParent {
                        run: parent.run,
                        bot: parent.bot,
                        chat_id: Uuid::new_v4(),
                        child_request,
                    }),
                    owner,
                    bot: recipient,
                    submission: BotRequestId::from_uuid(child_request.as_uuid()),
                    chat_id: Uuid::new_v4(),
                    ai_attempt_id: None,
                    assistant_message_id: None,
                    config_revision: 4,
                    policy_revision: 5,
                    continued_from: None,
                    routine: None,
                },
                policy: policy.clone(),
                request: DelegationRequest {
                    run: child_run,
                    requested_by: ActorRef::Person { profile: human },
                    objective: Objective::new(TASK).unwrap(),
                    agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                    requested_model: None,
                    scope: WorkspaceScope::BotFolder {
                        folder: BotWorkingFolder::new(format!("/fixture/recipient-{index}"))
                            .unwrap(),
                    },
                    context: vec![],
                    grants: [workspace].into(),
                    executing_device: owner.device,
                    origin: RunOrigin::Manual,
                },
                workspace,
                input: PendingInput {
                    idempotency_key: IdempotencyKey::mint(),
                    text: TASK.into(),
                    attachment_ids: vec![],
                    references: vec![],
                    mode: None,
                    fast: None,
                    reasoning: None,
                    skills: vec![],
                    model: None,
                },
            };
            assert!(submission.valid(), "the child submission fixture is valid");
            let binding = ChildRequestBindingV1 {
                version: DecisionVersionV1,
                owner,
                parent_bot: parent.bot,
                parent_run: parent.run,
                wait,
                ask_key,
                ordinal: index as u8,
                parent_submission: koru_delegate_core::INTERACTIVE_BOT_SUBMISSION_VERSION,
                parent_routine: None,
                human,
                recipient_bot: recipient,
                child_run,
                child_request,
                workspace,
                task_digest: child_task_digest(TASK).unwrap(),
                documents: vec![],
                parent_policy,
                child_policy: ChildPolicyRef::of(4, 5, &submission.policy).unwrap(),
            };
            ChildRequestItem {
                child_request,
                binding,
                submission,
            }
        })
        .collect();
    let request = ChildRequest {
        parent,
        ask_key,
        human,
        requests,
    };
    request.validate().expect("the ask fixture is valid");
    request
}

fn origin(request: &ChildRequest) -> DecisionFence {
    request.origin().unwrap()
}

fn receipts(request: &ChildRequest) -> Vec<ChildRequestReceipt> {
    request
        .requests
        .iter()
        .map(|item| ChildRequestReceipt {
            child_request: item.child_request,
            parent: BotRunKey {
                owner: request.parent.owner,
                bot: request.parent.bot,
                run: request.parent.run,
            },
            wait: item.binding.wait,
            ordinal: item.binding.ordinal,
            recipient: item.binding.recipient_bot,
            child_run: item.binding.child_run,
            state: ChildRequestState::Open,
            settled_by: None,
            child: BotAdmissionState::Queued,
            clock: ChildClock::Running,
            elapsed_ms: 0,
            budget_ms: BOT_CHILD_WAIT_MS as u32,
            revision: DecisionRevision::new(1).unwrap(),
            binding_digest: item.binding.digest().unwrap(),
        })
        .collect()
}

fn view(request: &ChildRequest) -> ChildWaitView {
    ChildWaitView {
        wait: request.wait().unwrap(),
        phase: BotWaitPhase::HoldRequested,
        revision: DecisionRevision::new(1).unwrap(),
        open_requests: request.requests.len() as u8,
    }
}

fn intent(request: &ChildRequest) -> BotChildWaitCleanupIntentV1 {
    BotChildWaitCleanupIntentV1::frozen(request.wait().unwrap(), &origin(request)).unwrap()
}

fn observation(request: &ChildRequest) -> BotChildWaitObservation {
    let intent = intent(request);
    BotChildWaitObservation {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
        current_revision: DecisionRevision::new(1).unwrap(),
        fresh_stop_eligibility: FreshWaitStopEligibility::EligibleOriginWaitStop,
        origin_stop: OriginWaitStop::Pending {},
        consumer_cleanup: ConsumerCleanup::NotRequested {},
        disposition: ChildWaitDisposition::Open {},
        resume: None,
    }
}

fn commands(request: &ChildRequest) -> Vec<(BotCommand, &'static str)> {
    vec![
        (
            BotCommand::RequestChild {
                request: request.clone(),
                boot: DelegateBootId::mint(),
            },
            "request_child",
        ),
        (
            BotCommand::ReadChildRequests {
                subject: BotRunKey {
                    owner: request.parent.owner,
                    bot: request.parent.bot,
                    run: request.parent.run,
                },
            },
            "read_child_requests",
        ),
        (
            BotCommand::ReadBotChildWait {
                owner: request.parent.owner,
                bot: request.parent.bot,
                run: request.parent.run,
                wait_id: request.wait().unwrap(),
            },
            "read_bot_child_wait",
        ),
    ]
}

fn replies(request: &ChildRequest) -> Vec<(BotReply, &'static str)> {
    vec![
        (
            BotReply::ChildRequested {
                outcome: ChildRequestedOutcome::Created,
                wait: view(request),
                enrollment: intent(request),
                receipts: receipts(request),
            },
            "child_requested",
        ),
        (
            BotReply::ChildRequests {
                wait: Some(view(request)),
                requests: receipts(request),
            },
            "child_requests",
        ),
        (
            BotReply::ChildWait {
                observation: observation(request),
            },
            "child_wait",
        ),
    ]
}

#[test]
fn m6_ipc19_codec_commands_round_trip_bind_their_owner_and_refuse_unknown_fields() {
    assert_eq!(crate::ipc::WIRE_VERSION, 20);
    let request = ask(3);
    for (command, tag) in commands(&request) {
        assert_eq!(command.owner(), Some(&request.parent.owner), "{tag}");
        assert_eq!(command.child_valid(), Some(true), "{tag}");
        assert_eq!(command.private_valid(), None, "{tag}");
        assert_eq!(command.routine_control_valid(), None, "{tag}");
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["command"], tag);
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, crate::ipc::WIRE_VERSION);
        assert!(decoded == command, "{tag}");
        let mut extra = value.clone();
        extra["extra"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<BotCommand>(extra).is_err(),
            "{tag}"
        );
        // Nothing of a command reaches a derived diagnostic.
        assert_eq!(format!("{command:?}"), "BotCommand([REDACTED])");
    }
    // Unknown fields are refused in each nested record of the ask too.
    let value = serde_json::to_value(&commands(&request)[0].0).unwrap();
    for path in [
        vec!["request"],
        vec!["request", "parent"],
        vec!["request", "requests", "0"],
        vec!["request", "requests", "0", "binding"],
        vec!["request", "requests", "0", "binding", "parent_policy"],
        vec!["request", "requests", "0", "submission"],
        vec!["request", "requests", "0", "submission", "provenance"],
        vec![
            "request",
            "requests",
            "0",
            "submission",
            "provenance",
            "parent",
        ],
    ] {
        let mut changed = value.clone();
        let mut at = &mut changed;
        for part in &path {
            at = match part.parse::<usize>() {
                Ok(index) => &mut at[index],
                Err(_) => &mut at[*part],
            };
        }
        at["extra"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<BotCommand>(changed).is_err(),
            "{path:?}"
        );
    }
}

#[test]
fn m6_ipc19_codec_an_ask_is_refused_before_it_is_sent_unless_it_is_well_formed() {
    let request = ask(3);
    let command = |request: ChildRequest| BotCommand::RequestChild {
        request,
        boot: DelegateBootId::mint(),
    };
    assert!(LocalBotDelegate::encodable(&command(request.clone())).is_ok());
    // Four items, none, a missing turn, a foreign person, a recipient named
    // twice, a binding that names another wait and items out of order.
    let mut four = request.clone();
    four.requests.push(ask(1).requests.remove(0));
    let mut none = request.clone();
    none.requests.clear();
    let mut turnless = request.clone();
    turnless.parent.turn = None;
    let mut person = request.clone();
    person.human = ProfileId::from_uuid(Uuid::new_v4());
    let mut twice = request.clone();
    twice.requests[1] = twice.requests[0].clone();
    let mut wait = request.clone();
    wait.requests[1].binding.wait = BotWaitId::mint();
    let mut order = request.clone();
    order.requests.swap(0, 1);
    let mut task = request.clone();
    task.requests[0].submission.request.objective = Objective::new("ANOTHER-TASK").unwrap();
    let mut parentless = request.clone();
    parentless.requests[0].submission.provenance.parent = None;
    for (bad, why) in [
        (four, "four items"),
        (none, "no item"),
        (turnless, "no turn"),
        (person, "another person"),
        (twice, "a recipient twice"),
        (wait, "another wait"),
        (order, "out of order"),
        (task, "a task the binding does not name"),
        (parentless, "a submission without a parent"),
    ] {
        let command = command(bad);
        assert_eq!(command.child_valid(), Some(false), "{why}");
        let failure = LocalBotDelegate::encodable(&command).unwrap_err();
        assert_eq!(failure.code, "invalid_request", "{why}");
        assert!(!failure.retryable, "{why}");
        assert!(!failure.message.contains(TASK), "{why}");
    }
    // The whole ask stays within the existing frame bound; one that does not
    // fit is not encodable as a frame at all.
    let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command(request)).unwrap();
    assert!(bytes.len() < 4 * 1024 * 1024);
}

#[test]
fn m6_ipc19_codec_the_two_reads_are_bounded_and_probed_before_decoding() {
    let request = ask(1);
    for (command, tag) in commands(&request).into_iter().skip(1) {
        let frame = crate::ipc::Request::Bot {
            authority: BotDelegateAuthority::new("A".repeat(256)).unwrap(),
            command,
        };
        let body = serde_json::to_vec(&frame).unwrap();
        assert!(body.len() <= CHILD_FRAME_LIMIT, "{tag}");
        assert!(!child_frame_oversized(&body), "{tag}");
        // The same command padded past the frame bound is named by the probe
        // without decoding its content.
        let mut padded = body.clone();
        padded.extend(std::iter::repeat_n(b' ', CHILD_FRAME_LIMIT));
        assert!(child_frame_oversized(&padded), "{tag}");
    }
    // An ask is not one of the bounded commands, whatever its size.
    let ask_frame = crate::ipc::Request::Bot {
        authority: BotDelegateAuthority::new("A".repeat(256)).unwrap(),
        command: commands(&request).remove(0).0,
    };
    let mut body = serde_json::to_vec(&ask_frame).unwrap();
    body.extend(std::iter::repeat_n(b' ', CHILD_FRAME_LIMIT));
    assert!(!child_frame_oversized(&body));
    // Ids that are not version 4 are malformed reads.
    let nil: BotId = BotId::from_uuid(Uuid::nil());
    let malformed = BotCommand::ReadChildRequests {
        subject: BotRunKey {
            owner: request.parent.owner,
            bot: nil,
            run: request.parent.run,
        },
    };
    assert_eq!(malformed.child_valid(), Some(false));
    assert_eq!(
        LocalBotDelegate::encodable(&malformed).unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn m6_ipc19_codec_replies_round_trip_within_their_bounds_and_hold_no_text() {
    for items in 1..=3 {
        let request = ask(items);
        for (reply, tag) in replies(&request) {
            assert_eq!(reply.child_valid(), Some(true), "{tag}");
            assert_eq!(reply.private_valid(), None, "{tag}");
            let value = serde_json::to_value(&reply).unwrap();
            assert_eq!(value["result"], tag);
            let text = value.to_string();
            assert!(!text.contains(TASK), "{tag}");
            assert!(
                text.len()
                    <= if tag == "child_wait" {
                        CHILD_WAIT_WIRE_LIMIT
                    } else {
                        CHILD_WIRE_LIMIT
                    },
                "{tag}"
            );
            let response = crate::ipc::Response::Bot(reply.clone());
            let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &response).unwrap();
            assert!(
                crate::local::read_current_response(&mut bytes.as_slice()).unwrap() == response,
                "{tag}"
            );
            assert!(LocalBotDelegate::answer(response).is_ok(), "{tag}");
            let mut extra = value;
            extra["extra"] = serde_json::json!(true);
            assert!(serde_json::from_value::<BotReply>(extra).is_err(), "{tag}");
            assert_eq!(format!("{reply:?}"), "BotReply([REDACTED])");
        }
    }
}

#[test]
fn m6_ipc19_codec_a_malformed_reply_never_reaches_the_caller() {
    let request = ask(2);
    let refused = |reply: BotReply, why: &str| {
        assert_eq!(reply.child_valid(), Some(false), "{why}");
        let failure = LocalBotDelegate::answer(crate::ipc::Response::Bot(reply)).unwrap_err();
        assert_eq!(failure.code, "delegate_unexpected_response", "{why}");
    };
    let requested = |edit: &dyn Fn(
        &mut ChildWaitView,
        &mut BotChildWaitCleanupIntentV1,
        &mut Vec<ChildRequestReceipt>,
    )| {
        let (mut wait, mut enrollment, mut receipts) =
            (view(&request), intent(&request), receipts(&request));
        edit(&mut wait, &mut enrollment, &mut receipts);
        BotReply::ChildRequested {
            outcome: ChildRequestedOutcome::AlreadyCreated,
            wait,
            enrollment,
            receipts,
        }
    };
    refused(requested(&|_, _, receipts| receipts.clear()), "no receipt");
    refused(
        requested(&|_, _, receipts| receipts.swap(0, 1)),
        "receipts out of order",
    );
    refused(
        requested(&|wait, _, _| wait.wait = BotWaitId::mint()),
        "another wait",
    );
    refused(
        requested(&|_, _, receipts| receipts[0].elapsed_ms = receipts[0].budget_ms + 1),
        "elapsed time over the budget",
    );
    refused(
        requested(&|_, _, receipts| receipts[1].settled_by = Some(ChildSettledBy::TimedOut)),
        "an open request with a settler",
    );
    refused(
        requested(&|_, enrollment, _| {
            enrollment.cause =
                koru_delegate_core::decisions::BotWaitCleanupCause::DecisionWaitCleanup
        }),
        "a decision's cause",
    );
    refused(
        requested(&|wait, _, _| wait.open_requests = 3),
        "more open requests than receipts",
    );
    let mut four = receipts(&ask(3));
    four.push(four[0].clone());
    refused(
        BotReply::ChildRequests {
            wait: None,
            requests: four,
        },
        "four requests",
    );
    let mut foreign = observation(&request);
    foreign.key.wait_id = BotWaitId::mint();
    refused(
        BotReply::ChildWait {
            observation: foreign,
        },
        "a key that is not its intent's",
    );
    let mut closed = observation(&request);
    closed.disposition = ChildWaitDisposition::Closed {
        cause: koru_delegate_core::decisions::DecisionClosureReason::Expired,
    };
    refused(
        BotReply::ChildWait {
            observation: closed,
        },
        "a closure cause a child wait never has",
    );
}

/// The build has one `WIRE_VERSION`, so this is the frame-level half: a
/// frame stamped 18 is not read as a 19 frame, whatever its body. The
/// worker's own version check is tested with the host.
#[test]
fn m6_ipc19_frame_version_a_frame_stamped_18_is_refused_by_the_19_reader() {
    assert_eq!(crate::ipc::WIRE_VERSION, 20);
    let request = ask(1);
    for (command, tag) in commands(&request) {
        let reply = crate::ipc::Response::Bot(BotReply::Missing);
        // A reply stamped 18 never becomes an answer to a child command.
        let old = koru_ipc::frame::encode(18, &reply).unwrap();
        let failure = crate::local::read_current_response(&mut old.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible", "{tag}");
        assert!(!failure.retryable);
        // The version is checked before the body: an undecodable 18 body is
        // the same refusal, never a decode error.
        let undecodable = koru_ipc::frame::encode(18, &serde_json::json!({"old":true})).unwrap();
        let failure = crate::local::read_current_response(&mut undecodable.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible", "{tag}");
        // A child command stamped 18 carries its stamp, so a reader that
        // compares it never mistakes it for a current frame.
        let stamped = koru_ipc::frame::encode(18, &command).unwrap();
        let (version, _) = koru_ipc::frame::read_raw(&mut stamped.as_slice()).unwrap();
        assert_eq!(version, 18);
        assert_ne!(version, crate::ipc::WIRE_VERSION);
    }
    for (reply, tag) in replies(&request) {
        let old = koru_ipc::frame::encode(18, &crate::ipc::Response::Bot(reply)).unwrap();
        let failure = crate::local::read_current_response(&mut old.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible", "{tag}");
    }
}

#[test]
fn m6_child_codes_final_every_code_has_one_fixed_text_and_only_wait_closing_retries() {
    let all = [
        ChildRefusal::DecisionOpen,
        ChildRefusal::WaitOpen,
        ChildRefusal::WaitClosing,
        ChildRefusal::Depth,
        ChildRefusal::SelfAsk,
        ChildRefusal::RecipientLimit,
        ChildRefusal::Cycle,
        ChildRefusal::RecipientPaused,
        ChildRefusal::RecipientStale,
        ChildRefusal::RecipientFull,
        ChildRefusal::Capacity,
        ChildRefusal::BudgetLow,
        ChildRefusal::FolderOverlap,
        ChildRefusal::ConnectionsNotSubset,
    ];
    // The table is exactly the ledger's refusals plus the unknown wait.
    let mut expected: Vec<&str> = all.iter().map(|refusal| refusal.code()).collect();
    expected.push(CHILD_WAIT_UNKNOWN);
    expected.sort_unstable();
    let mut listed: Vec<&str> = CHILD_FAILURES.iter().map(|(code, _, _)| *code).collect();
    listed.sort_unstable();
    assert_eq!(listed, expected);
    for refusal in all {
        let failure = child_failure(refusal.code()).expect("a listed code");
        assert_eq!(failure.code, refusal.code());
        assert!(failure.code.starts_with("bot_child_"));
        assert_eq!(failure.retryable, refusal.retryable(), "{}", refusal.code());
        assert_eq!(
            failure.retryable,
            refusal == ChildRefusal::WaitClosing,
            "{}",
            refusal.code()
        );
        // One fixed text: no id, digit or brace is interpolated.
        assert!(
            !failure
                .message
                .contains(|c: char| c.is_ascii_digit() || c == '{'),
            "{}",
            refusal.code()
        );
        assert!(failure.details.is_empty());
        // A per-recipient refusal names its item and nothing else.
        let named = child_refused(ChildRefused {
            code: refusal,
            ordinal: Some(2),
        });
        assert_eq!(named.code, refusal.code());
        assert_eq!(named.retryable, refusal.retryable());
        assert_eq!(
            named.details.get("ordinal"),
            Some(&serde_json::json!(2)),
            "{}",
            refusal.code()
        );
        assert_eq!(named.details.len(), 1);
        assert!(
            child_refused(ChildRefused {
                code: refusal,
                ordinal: None
            })
            .details
            .is_empty()
        );
        // A refusal crosses the wire as a failure and comes back unchanged.
        let response = crate::ipc::Response::Failure(named.clone());
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &response).unwrap();
        assert_eq!(
            crate::local::read_current_response(&mut bytes.as_slice()).unwrap(),
            response
        );
        assert_eq!(LocalBotDelegate::answer(response).unwrap_err(), named);
    }
    let unknown = child_failure(CHILD_WAIT_UNKNOWN).unwrap();
    assert!(!unknown.retryable);
    assert!(child_failure("bot_fenced").is_none());
    assert!(private_failure("bot_child_cycle").is_none());
}

#[test]
fn m6_ipc19_codec_the_bridge_routes_carry_their_own_payloads_under_the_same_envelope() {
    use crate::decision_bridge::*;
    let request = ask(1);
    let intent = intent(&request);
    let key = intent.key().unwrap();
    let enroll = EnrollBotChildWaitCleanup {
        key: key.clone(),
        cleanup_intent: intent,
    };
    assert_ne!(CHILD_ENROLL_ROUTE, CHILD_READ_ROUTE);
    for route in [CHILD_ENROLL_ROUTE, CHILD_READ_ROUTE] {
        assert!(route.starts_with("/private/bots/children/"));
        assert!(route != DECISION_ENROLL_ROUTE && route != DECISION_READ_ROUTE);
    }
    let envelope = DecisionBridgeEnvelope::new(enroll.clone()).unwrap();
    let bytes = envelope.encode().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(
        value["payload"]["cleanup_intent"]["cause"],
        "child_wait_cleanup"
    );
    assert!(
        value["payload"]["cleanup_intent"]
            .get("decision_id")
            .is_none()
    );
    assert_eq!(
        DecisionBridgeEnvelope::<EnrollBotChildWaitCleanup>::decode(&bytes).unwrap(),
        envelope
    );
    // The payload type follows from the route: a child payload is never a
    // decision payload, and a decision payload is never a child payload.
    assert!(
        DecisionBridgeEnvelope::<koru_delegate_core::decisions::EnrollBotWaitCleanup>::decode(
            &bytes
        )
        .is_err()
    );
    let decision = DecisionBridgeEnvelope::new(crate::decision_bridge::tests::enroll()).unwrap();
    assert!(
        DecisionBridgeEnvelope::<EnrollBotChildWaitCleanup>::decode(&decision.encode().unwrap())
            .is_err()
    );
    for mutate in [
        (|v: &mut serde_json::Value| v["payload"]["extra"] = serde_json::json!(true))
            as fn(&mut serde_json::Value),
        |v| v["version"] = serde_json::json!(2),
        |v| v["payload"]["key"]["receipt_digest"] = serde_json::json!("00".repeat(32)),
        |v| v["payload"]["cleanup_intent"]["cause"] = serde_json::json!("decision_wait_cleanup"),
    ] {
        let mut changed = value.clone();
        mutate(&mut changed);
        assert!(
            DecisionBridgeEnvelope::<EnrollBotChildWaitCleanup>::decode(
                &serde_json::to_vec(&changed).unwrap()
            )
            .is_err(),
            "{changed}"
        );
    }
    // Enroll answers Enrolled or Refused, never Missing; only Read may.
    let enrolled = BotChildWaitEnrollmentReply::Enrolled {
        receipt_digest: key.receipt_digest,
        key: key.clone(),
        ai_attempt: Uuid::new_v4(),
    };
    DecisionBridgeEnvelope::reply_to(&envelope, enrolled.clone()).unwrap();
    DecisionBridgeEnvelope::reply_to(
        &envelope,
        BotChildWaitEnrollmentReply::Refused {
            code: koru_delegate_core::decisions::BotWaitRefusal::Stale,
        },
    )
    .unwrap();
    assert_eq!(
        DecisionBridgeEnvelope::reply_to(
            &envelope,
            BotChildWaitEnrollmentReply::Missing { key: key.clone() }
        )
        .unwrap_err(),
        DecisionBridgeCodecError::Mismatch
    );
    let mut foreign = key.clone();
    foreign.receipt_digest = DecisionDigest::from_bytes([9; 32]);
    assert!(
        DecisionBridgeEnvelope::reply_to(
            &envelope,
            BotChildWaitEnrollmentReply::Enrolled {
                receipt_digest: foreign.receipt_digest,
                key: foreign,
                ai_attempt: Uuid::new_v4(),
            }
        )
        .is_err()
    );
    let read = DecisionBridgeEnvelope::new(ReadBotChildWaitCleanup { key: key.clone() }).unwrap();
    DecisionBridgeEnvelope::reply_to(&read, BotChildWaitEnrollmentReply::Missing { key }).unwrap();
    DecisionBridgeEnvelope::reply_to(&read, enrolled).unwrap();
    // The envelope's Debug names no payload.
    assert!(!format!("{envelope:?}").contains("cleanup_intent"));
}
