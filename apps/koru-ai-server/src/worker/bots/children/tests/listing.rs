// SPDX-License-Identifier: MPL-2.0
//! Cohort `m6_ask_listing`: what the worker answers when a Run's coding agent
//! lists its tools, in the order a real session does it. An agent lists when
//! its session starts, which is before the delegate has answered the first
//! input and so before this worker has bound the Run's turn.
use super::*;

/// A Bot's reply as it is when its Run's session starts: the submission is
/// prepared and its AI attempt is pending. Nothing of the execution is bound.
struct Starting {
    bot: Bot,
    subject: RunBotSubject,
    attempt: InferenceAttemptId,
    /// The Run's execution as the delegate would name it, with no turn yet.
    fence: d::BotExecutionFence,
}

fn starting(repo: &mut Repository, bot: &Bot, profile: koru_ai_core::ProfileId) -> Starting {
    let mut turn = pending(bot.main_chat, Uuid::new_v4(), 1);
    turn.attempt.provider = bot.config.model.provider.clone();
    turn.attempt.requested_model = bot.config.model.requested_model.clone();
    turn.assistant_message.provider = Some(turn.attempt.provider.clone());
    turn.assistant_message.requested_model = Some(turn.attempt.requested_model.clone());
    let pins = vec![d::RunContextItem::Document {
        workspace: bot.home_workspace,
        document: bot.config.role.document,
        version: d::DocumentVersionId::new(),
    }];
    let mut intent = make_intent(
        bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
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
        &"c".repeat(64),
        bot.revision,
        turn.attempt.queued_at,
    )
    .unwrap();
    let p = &intent.submission.provenance;
    let fence = d::BotExecutionFence {
        owner: p.owner,
        bot: bot.id,
        run: intent.submission.request.run,
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::FIRST,
        session: d::SessionId::mint(),
        turn: None,
    };
    Starting {
        bot: bot.clone(),
        subject: RunBotSubject {
            run: intent.submission.request.run,
            generation: fence.generation,
            provenance: intent.submission.provenance.clone(),
            captured: intent.submission.policy.clone(),
        },
        attempt: turn.attempt.id,
        fence,
    }
}

/// The delegate's answer about the Run's admission, stored on the receipt
/// (`acknowledge_bot_submission`).
fn acknowledge(
    repo: &mut Repository,
    reply: &Starting,
    state: d::BotAdmissionState,
    fence: Option<d::BotExecutionFence>,
) {
    let p = &reply.subject.provenance;
    repo.acknowledge_bot_submission(
        BotOwner {
            user_scope: p.owner.user_scope,
            device: p.owner.device,
        },
        reply.bot.id,
        p.submission,
        &d::BotAdmissionView {
            key: d::BotSubmissionKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission,
            },
            run: reply.subject.run,
            state,
            queue_sequence: 1,
            fence,
        },
        2,
    )
    .unwrap();
}

/// The delegate admitted the Run and its session exists: the worker has
/// bound the execution, still with no turn (`Phase::Admission`).
fn bind_session(repo: &mut Repository, reply: &Starting) {
    acknowledge(
        repo,
        reply,
        d::BotAdmissionState::Executing,
        Some(reply.fence.clone()),
    );
    repo.bind_bot_execution(reply.attempt, &reply.fence, Timestamp::now())
        .unwrap();
}

fn listing(subject: &RunBotSubject) -> RunAskableRequest {
    RunAskableRequest {
        subject: subject.clone(),
        deadline: Instant::now() + Duration::from_secs(5),
    }
}

/// The Run's first input was answered: the worker has bound its turn.
fn bind_turn(repo: &mut Repository, reply: &Starting) -> d::AgentTurnId {
    let turn = d::AgentTurnId::mint();
    let mut fence = reply.fence.clone();
    fence.turn = Some(turn);
    repo.bind_bot_execution(reply.attempt, &fence, Timestamp::now())
        .unwrap();
    turn
}

/// What the listing's question answers for a Run: `Ok`, or the code.
fn offered(w: &World, subject: &RunBotSubject) -> Result<(), String> {
    w.client
        .run_may_ask(listing(subject))
        .map_err(|error| error.code)
}

/// The helper Bot's reply at its session's start, in the world's worker.
fn helper_starting(w: &World) -> Starting {
    let (bot, profile) = (w.recipients[0].clone(), w.profile);
    on_writer(&w.client, move |state| {
        starting(&mut state.repository, &bot, profile)
    })
}

#[test]
fn m6_ask_listing_a_bot_that_may_ask_is_offered_the_ask_tools_when_its_session_starts() {
    let w = world();
    // The session starts: the agent lists its tools. Nothing of the Run's
    // execution is bound in ai.db yet.
    let reply = helper_starting(&w);
    assert_eq!(offered(&w, &reply.subject), Ok(()));
    // The delegate admitted the Run and the worker bound its session, with
    // no turn: the first input has not been answered.
    let reply = on_writer(&w.client, move |state| {
        bind_session(&mut state.repository, &reply);
        reply
    });
    assert_eq!(offered(&w, &reply.subject), Ok(()));
    // The turn is bound: the same answer.
    let reply = on_writer(&w.client, move |state| {
        bind_turn(&mut state.repository, &reply);
        reply
    });
    assert_eq!(offered(&w, &reply.subject), Ok(()));
    // A resumed Run's new session lists before the worker's replay has
    // rolled over to it: the gateway names the next generation.
    let mut resumed = reply.subject.clone();
    resumed.generation = resumed.generation.next();
    assert_eq!(offered(&w, &resumed), Ok(()));
    assert_eq!(w.owner.asks(), 0, "listing asks nothing of the delegate");
    w.stop();
}

#[test]
fn m6_ask_listing_is_not_offered_with_the_setting_off_to_a_child_run_or_to_a_run_with_no_reply() {
    let w = world();
    let reply = helper_starting(&w);
    assert_eq!(offered(&w, &reply.subject), Ok(()));
    // A Run that another Bot asked for never asks (depth one).
    let mut child = reply.subject.clone();
    child.provenance.parent = Some(d::ChildParent {
        run: d::RunId::mint(),
        bot: w.parent.id,
        chat_id: Uuid::new_v4(),
        child_request: ChildRequestId::mint(),
    });
    assert_eq!(offered(&w, &child), Err(codes::ASK_NOT_ALLOWED.into()));
    // A Run this worker holds no admission for.
    let mut foreign = reply.subject.clone();
    foreign.run = d::RunId::mint();
    assert_eq!(offered(&w, &foreign), Err("permission_denied".into()));
    // A task started through the public API or by an external assistant has
    // no reply of its own (no AI attempt): it cannot hold a wait, so it is
    // not offered the tools.
    let (bot, profile) = (w.recipients[1].clone(), w.profile);
    let api = on_writer(&w.client, move |state| {
        let pins = vec![d::RunContextItem::Document {
            workspace: bot.home_workspace,
            document: bot.config.role.document,
            version: d::DocumentVersionId::new(),
        }];
        let intent = make_intent(
            &bot,
            BotRequestId::mint(),
            bot.main_chat,
            bot.home_workspace,
            &BotTask {
                text: "A task with no reply".into(),
                documents: Vec::new(),
            },
            None,
            &pins,
            profile,
        )
        .unwrap();
        assert_eq!(intent.submission.provenance.ai_attempt_id, None);
        state
            .repository
            .prepare_bot_submission(
                &intent,
                &"d".repeat(64),
                bot.revision,
                Timestamp::now().millis(),
            )
            .unwrap();
        RunBotSubject {
            run: intent.submission.request.run,
            generation: d::AttemptGeneration::FIRST,
            provenance: intent.submission.provenance.clone(),
            captured: intent.submission.policy.clone(),
        }
    });
    assert_eq!(offered(&w, &api), Err("permission_denied".into()));
    // "May ask other bots" is turned off while the Run is live: the same
    // Run is no longer offered the tools.
    let (id, profile) = (reply.bot.id, w.profile);
    on_writer(&w.client, move |state| {
        let bot = state.repository.bot(id).unwrap().unwrap();
        let mut desired = bot.clone();
        desired.revision += 1;
        desired.policy_revision += 1;
        desired.updated_at_ms += 1;
        desired.may_ask = false;
        let barrier = d::BotPolicyBarrier {
            key: d::BotPolicyBarrierKey {
                owner: d::BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot: bot.id,
                request: "12345678-1234-8234-8234-123456789abc".parse().unwrap(),
            },
            config_revision: desired.revision,
            policy_revision: desired.policy_revision,
            policy: koru_ai_core::bots::bot_native_policy(&desired),
            effects_allowed: true,
        };
        state
            .repository
            .prepare_bot_access(
                &desired,
                profile,
                BotRequestId::mint(),
                &"f".repeat(64),
                bot.revision,
                &barrier,
            )
            .unwrap();
    });
    assert_eq!(
        offered(&w, &reply.subject),
        Err(codes::ASK_NOT_ALLOWED.into())
    );
    w.stop();
}

/// Listing is not authorization. A listed tool called before the turn is
/// bound is refused with the retryable `turn_not_started`: nothing is
/// written and the delegate is not asked. The same call, once the turn is
/// bound, is admitted and attributed to that turn.
#[test]
fn m6_ask_listing_a_call_before_the_turn_is_bound_is_refused_and_the_same_call_passes_after() {
    let w = world();
    let reply = helper_starting(&w);
    let key = Uuid::new_v4();
    let reviewer = w.recipients[1].clone();
    let early = |w: &World, subject: &RunBotSubject| {
        let list = w
            .client
            .askable_bots_for_run(listing(subject))
            .expect_err("the list is refused before the turn");
        assert_eq!(list.code, TURN_NOT_STARTED);
        assert!(list.retryable);
        let asked = w
            .client
            .ask_bots_for_run(ask(subject, key, vec![item(&reviewer, "Review this")]))
            .expect_err("the ask is refused before the turn");
        assert_eq!(asked.code, TURN_NOT_STARTED);
        assert!(asked.retryable);
        assert!(asked.details.is_empty());
    };
    // Listed, and called at once: no execution is bound.
    assert_eq!(offered(&w, &reply.subject), Ok(()));
    early(&w, &reply.subject);
    // The session is bound, still with no turn.
    let reply = on_writer(&w.client, move |state| {
        bind_session(&mut state.repository, &reply);
        reply
    });
    early(&w, &reply.subject);
    // Nothing was written and the delegate heard nothing.
    assert_eq!(w.owner.asks(), 0);
    let (owner, run) = (w.authority.owner().unwrap(), reply.subject.run);
    let rows = on_writer(&w.client, move |state| {
        state.repository.bot_child_ask(owner, run, key).unwrap()
    });
    assert!(rows.is_empty(), "no receipt of a refused early ask");
    // The turn is bound: the same key and requests are admitted, under
    // exactly that turn.
    let (reply, turn) = on_writer(&w.client, move |state| {
        let turn = bind_turn(&mut state.repository, &reply);
        // The delegate's view once the turn exists, stored as this module's
        // `live_reply` fixture stores it. The test below takes the order a
        // real Run takes, where nothing is stored after `Submit`'s answer.
        let mut fence = reply.fence.clone();
        fence.turn = Some(turn);
        acknowledge(
            &mut state.repository,
            &reply,
            d::BotAdmissionState::Executing,
            Some(fence),
        );
        (reply, turn)
    });
    let mut names: Vec<_> = w
        .client
        .askable_bots_for_run(listing(&reply.subject))
        .unwrap()
        .into_iter()
        .map(|bot| bot.bot_id)
        .collect();
    names.sort();
    let mut expected = vec![w.parent.id, reviewer.id];
    expected.sort();
    assert_eq!(names, expected, "never the asking Bot itself");
    let pending = w
        .client
        .ask_bots_for_run(ask(
            &reply.subject,
            key,
            vec![item(&reviewer, "Review this")],
        ))
        .unwrap();
    assert_eq!(pending.requests.len(), 1);
    assert_eq!(pending.requests[0].1, reviewer.id);
    let sent = w.owner.asks.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].parent.run, reply.subject.run);
    assert_eq!(sent[0].parent.turn, Some(turn));
    // A resumed Run's call before the replay rolled over to its new session
    // waits the same way; it is never admitted under the old turn.
    let mut resumed = reply.subject.clone();
    resumed.generation = resumed.generation.next();
    let ahead = w
        .client
        .askable_bots_for_run(listing(&resumed))
        .expect_err("the old turn is not the resumed session's");
    assert_eq!(ahead.code, TURN_NOT_STARTED);
    w.stop();
}

/// The same ask in the order a real Run takes. The delegate answers the
/// submission when it admits the Run, before any session exists, and that
/// answer is the one stored on the receipt (`acknowledge_bot_submission`, the
/// coordination job's only write of it). The session and the turn are bound
/// later. The wait's enrollment (`enroll_on` in `koru-ai-sqlite`) requires
/// the receipt's execution to be the asking turn's, so the bind records the
/// bound execution on the receipt (`record_bound_execution_on`). Before it
/// did, this ask reached the delegate and then answered
/// `child_status_unknown`, for the same key every time.
#[test]
fn m6_ask_listing_an_ask_in_the_order_a_real_run_takes_is_admitted() {
    let w = world();
    let reply = helper_starting(&w);
    let reviewer = w.recipients[1].clone();
    let reply = on_writer(&w.client, move |state| {
        let repo = &mut state.repository;
        // `Submit` is answered: queued, no execution yet.
        acknowledge(repo, &reply, d::BotAdmissionState::Queued, None);
        // `Reconcile` names the session (`Phase::Admission`), then
        // `BeginTurn` the turn (`Phase::Begin`).
        repo.bind_bot_execution(reply.attempt, &reply.fence, Timestamp::now())
            .unwrap();
        bind_turn(repo, &reply);
        reply
    });
    let asked = w.client.ask_bots_for_run(ask(
        &reply.subject,
        Uuid::new_v4(),
        vec![item(&reviewer, "Review this")],
    ));
    let sent = w.owner.asks();
    w.stop();
    assert_eq!(
        asked
            .map(|pending| pending.requests.len())
            .map_err(|error| error.code),
        Ok(1),
        "the delegate was sent {sent} ask(s)"
    );
}

/// Cohort `m6_ask_wake`, the AI worker's half. A Bot with no unsettled work
/// holds no boot acknowledgment on the delegate, and without one its queued
/// Run is never dispatched. An admitted ask gives the asked Bot such work
/// through `RequestChild`, not through a `Submit`, so nothing asked the
/// delegate to synchronize it: the asked Bot's Run stayed queued until some
/// unrelated change of a Bot, or its fifteen-minute budget. The worker now
/// requests the boot pass when the ask is accepted, as after every Submit.
#[test]
fn m6_ask_wake_an_admitted_ask_asks_the_delegate_to_synchronize_the_asked_bot() {
    let w = world();
    let hellos = |w: &World| *w.owner.hellos.lock().unwrap();
    // The worker's own start probes and walks first: wait until it is quiet.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = hellos(&w);
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(2500) {
        assert!(Instant::now() < deadline, "the boot pass never went quiet");
        std::thread::sleep(Duration::from_millis(50));
        if hellos(&w) != seen {
            seen = hellos(&w);
            quiet_since = Instant::now();
        }
    }
    w.client
        .ask_bots_for_run(ask(
            &w.subject,
            Uuid::new_v4(),
            vec![item(&w.recipients[0], "Check this")],
        ))
        .unwrap();
    // The ask's own job probes the delegate's boot once (it sends the ask
    // under it). The boot pass the worker now requests is at least one more.
    let deadline = Instant::now() + Duration::from_secs(15);
    while hellos(&w) < seen + 2 {
        assert!(
            Instant::now() < deadline,
            "no boot pass followed the admitted ask: {} Hello(s) since it",
            hellos(&w) - seen
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    w.stop();
}
