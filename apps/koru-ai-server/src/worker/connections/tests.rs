// SPDX-License-Identifier: MPL-2.0

//! The worker's hosting of private connections: the plan, the announce, and
//! the service composed over the worker's own writer thread.
//!
//! Scripted vendor, lockable in-memory credential store, scripted delegate,
//! in-memory ai.db. No socket, no browser, no OS keychain, no real vendor.

use super::announce::{Announced, announce};
use super::plan::{
    PrivateItemName, PrivateReadPlanRequest, PrivateReadVerdict, PrivateSetupReason, Resolved,
    proposal, resolve, verdict,
};
use super::*;
use crate::fixtures::connections::{
    PrivateDelegate, PrivateWorld, SUBJECT_A, SUBJECT_B, captured, fence, owner, proof,
};
use koru_ai_core::connections::ConnectionItemState;
use koru_connections::support::FAKE_ORIGIN;
use koru_delegate_core::{
    BotExecutionFence, BotProvenance, CapturedBotPolicy, FreshBotPolicyProof, WorkspaceId,
};

fn provenance(fence: &BotExecutionFence) -> BotProvenance {
    BotProvenance {
        routine: None,
        owner: fence.owner,
        bot: fence.bot,
        submission: koru_delegate_core::BotRequestId::mint(),
        chat_id: Uuid::new_v4(),
        ai_attempt_id: None,
        assistant_message_id: None,
        config_revision: 1,
        policy_revision: 1,
        continued_from: None,
    }
}

/// A Routine Run of the same Bot, whose current grant is `grant`.
fn as_routine(
    provenance: &mut BotProvenance,
    proof: &mut FreshBotPolicyProof,
    grant: &CapturedBotPolicy,
) {
    use koru_delegate_core::{
        RoutineDefinitionIdentity, RoutineDigest32, RoutinePolicyBarrier, RoutineProvenance,
        ScheduleId,
    };
    let schedule = ScheduleId::from_uuid(Uuid::new_v4());
    provenance.routine = Some(RoutineProvenance {
        schedule,
        occurrence_ms: 900,
        definition: RoutineDefinitionIdentity {
            revision: 2,
            digest: RoutineDigest32::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    });
    proof.routine = Some(RoutinePolicyBarrier {
        owner: provenance.owner,
        bot: provenance.bot,
        schedule,
        request: Uuid::new_v4().try_into().unwrap(),
        grant_revision: 1,
        policy: grant.clone(),
        effects_allowed: true,
    });
}

fn request(
    fence: &BotExecutionFence,
    policy: &CapturedBotPolicy,
    item: PrivateItemName,
) -> PrivateReadPlanRequest {
    PrivateReadPlanRequest {
        run: fence.run,
        provenance: provenance(fence),
        captured: policy.clone(),
        operation_id: Uuid::new_v4(),
        item,
        connection: None,
    }
}

fn link(reference: &str) -> PrivateItemName {
    PrivateItemName::Link(format!("{FAKE_ORIGIN}/item/{reference}"))
}

#[test]
fn m5_private_plan_each_verdict_and_a_later_grant_does_not_broaden_a_running_run() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let connection = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    let item = world.item(connection.id, "doc-1");
    let fence = fence(owner);
    let home = WorkspaceId::new();
    let with = captured(home, &[connection.id]);
    let without = captured(home, &[]);
    let found = Resolved::One(Box::new((connection.clone(), item.clone())));
    let plan = |captured: &CapturedBotPolicy, current: &CapturedBotPolicy, questions: bool| {
        verdict(
            &provenance(&fence),
            captured,
            &proof(&fence, current),
            &found,
            questions,
        )
    };
    use PrivateReadVerdict as V;
    // Captured and current both hold the grant: read.
    assert_eq!(plan(&with, &with, true), V::Granted);
    // Neither holds it: ask.
    assert_eq!(plan(&without, &without, true), V::Ask);
    // A grant given after the Run was admitted does not broaden the Run.
    assert_eq!(plan(&without, &with, true), V::Ask);
    // A grant removed since: the Run no longer reads without asking.
    assert_eq!(plan(&with, &without, true), V::Ask);
    // Where no question can be asked, access must be given in advance.
    assert_eq!(
        plan(&without, &without, false),
        V::NeedsPerson(PrivateSetupReason::GiveAccess)
    );
    assert_eq!(plan(&with, &with, false), V::Granted);

    // Effects not allowed now: nothing is read and nothing is asked.
    let mut paused = proof(&fence, &with);
    paused.global.effects_allowed = false;
    for captured in [&with, &without] {
        assert_eq!(
            verdict(&provenance(&fence), captured, &paused, &found, true),
            V::Refused("native_policy_pending")
        );
    }
    // The current policy is homed elsewhere: not a question.
    let moved = captured(WorkspaceId::new(), &[]);
    assert_eq!(
        plan(&without, &moved, true),
        V::Refused("native_policy_pending")
    );

    // A Routine cannot ask (D6): it reads only under its own grant, captured
    // and current, whatever the Bot holds.
    let mut subject = provenance(&fence);
    let mut current = proof(&fence, &with);
    as_routine(&mut subject, &mut current, &with);
    assert_eq!(verdict(&subject, &with, &current, &found, true), V::Granted);
    let mut current = proof(&fence, &with);
    as_routine(&mut subject, &mut current, &without);
    for captured in [&with, &without] {
        assert_eq!(
            verdict(&subject, captured, &current, &found, true),
            V::Refused(ROUTINE)
        );
    }
    // A subject and a proof that do not name the Routine together.
    assert_eq!(
        verdict(&subject, &with, &proof(&fence, &with), &found, true),
        V::Refused("native_policy_unavailable")
    );
}

const ROUTINE: &str = super::plan::ROUTINE_NOT_GRANTED;

#[test]
fn m5_private_plan_an_ungranted_run_needs_the_person_for_every_setup_situation() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[]);
    let resolved = |world: &PrivateWorld, item: PrivateItemName| {
        world
            .rows(|rows| {
                resolve(
                    rows,
                    world.vendor.as_ref(),
                    world.owner,
                    &request(&fence, &policy, item),
                )
            })
            .unwrap()
    };
    let needs = |world: &PrivateWorld, item: PrivateItemName| {
        let resolved = resolved(world, item);
        let told = verdict(
            &provenance(&fence),
            &policy,
            &proof(&fence, &policy),
            &resolved,
            true,
        );
        // A Routine is told one thing too, and never what exists.
        let mut subject = provenance(&fence);
        let mut current = proof(&fence, &policy);
        as_routine(&mut subject, &mut current, &policy);
        assert_eq!(
            verdict(&subject, &policy, &current, &resolved, true),
            PrivateReadVerdict::Refused(ROUTINE)
        );
        match told {
            PrivateReadVerdict::NeedsPerson(reason) => reason,
            other => panic!("the read did not need the person: {other:?}"),
        }
    };
    use PrivateSetupReason as S;
    // No account is connected.
    assert_eq!(needs(&world, link("doc-1")), S::NoConnection);
    let first = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    // An account is connected and the item was never picked.
    assert_eq!(needs(&world, link("doc-2")), S::NotPicked);
    // Text that is not a link of this vendor, and an id nobody holds.
    assert_eq!(
        needs(&world, PrivateItemName::Link("NOT A LINK".into())),
        S::NotPicked
    );
    assert_eq!(
        needs(
            &world,
            PrivateItemName::Id(koru_ai_core::connections::ConnectionItemId::mint())
        ),
        S::NotPicked
    );
    // Another person's item is not this person's.
    let stranger = PrivateWorld::new(crate::fixtures::connections::owner());
    let theirs = stranger.connect("Theirs", SUBJECT_A, &["doc-1"]);
    let their_item = stranger.item(theirs.id, "doc-1");
    assert_eq!(
        needs(&world, PrivateItemName::Id(their_item.id)),
        S::NotPicked
    );

    // The picked item: by link and by id it is the same item and the same
    // request fingerprint, and it plans a question.
    let item = world.item(first.id, "doc-1");
    for name in [
        link("doc-1"),
        PrivateItemName::Id(item.id),
        PrivateItemName::Link("doc-1".into()),
    ] {
        let Resolved::One(found) = resolved(&world, name) else {
            panic!("the picked item was not found");
        };
        assert_eq!(found.1.id, item.id);
        let planned = proposal(Uuid::new_v4(), &found.0, &found.1).unwrap();
        assert!(planned.valid());
        assert_eq!(
            planned.request_fingerprint,
            world
                .proposal(Uuid::new_v4(), first.id, "doc-1")
                .request_fingerprint
        );
    }
    // Naming another connection does not find it.
    let mut elsewhere = request(&fence, &policy, PrivateItemName::Id(item.id));
    elsewhere.connection = Some(koru_ai_core::connections::ConnectionId::mint());
    assert!(matches!(
        world
            .rows(|rows| resolve(rows, world.vendor.as_ref(), world.owner, &elsewhere))
            .unwrap(),
        Resolved::None(S::NotPicked)
    ));

    // The item is no longer the one chosen.
    world
        .rows(|rows| rows.connection_item_changed(world.owner, item.id, 5))
        .unwrap();
    assert_eq!(
        world.item(first.id, "doc-1").state,
        ConnectionItemState::Changed
    );
    assert_eq!(needs(&world, PrivateItemName::Id(item.id)), S::ItemChanged);

    // The connection needs sign-in; then it is disconnected.
    let second = PrivateWorld::new(owner);
    let connection = second.connect("Work files", SUBJECT_A, &["doc-1"]);
    let item = second.item(connection.id, "doc-1");
    second
        .rows(|rows| rows.connection_needs_sign_in(second.owner, connection.id, 5))
        .unwrap();
    assert_eq!(needs(&second, PrivateItemName::Id(item.id)), S::NeedsSignIn);
    let third = PrivateWorld::new(owner);
    let connection = third.connect("Work files", SUBJECT_A, &["doc-1"]);
    let item = third.item(connection.id, "doc-1");
    third
        .service
        .disconnect(
            third.owner,
            koru_ai_core::bots::BotRequestId::mint(),
            connection.id,
            connection.revision,
        )
        .unwrap();
    assert_eq!(needs(&third, PrivateItemName::Id(item.id)), S::Disconnected);
    // By its link a disconnected account's item is not found at all.
    assert_eq!(needs(&third, link("doc-1")), S::NoConnection);
}

#[test]
fn m5_private_plan_a_granted_run_is_told_the_specific_reason() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let connection = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    let item = world.item(connection.id, "doc-1");
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[connection.id]);
    let told = |world: &PrivateWorld| {
        let resolved = world
            .rows(|rows| {
                resolve(
                    rows,
                    world.vendor.as_ref(),
                    world.owner,
                    &request(&fence, &policy, PrivateItemName::Id(item.id)),
                )
            })
            .unwrap();
        verdict(
            &provenance(&fence),
            &policy,
            &proof(&fence, &policy),
            &resolved,
            true,
        )
    };
    assert_eq!(told(&world), PrivateReadVerdict::Granted);
    world
        .rows(|rows| rows.connection_item_changed(world.owner, item.id, 5))
        .unwrap();
    assert_eq!(told(&world), PrivateReadVerdict::Refused("item_changed"));
    world
        .rows(|rows| rows.connection_needs_sign_in(world.owner, connection.id, 6))
        .unwrap();
    assert_eq!(
        told(&world),
        PrivateReadVerdict::Refused("connection_needs_sign_in")
    );
}

#[test]
fn m5_private_plan_two_accounts_holding_one_item_is_the_persons_to_resolve() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    // The same file is reachable from both accounts and picked on both.
    world.vendor.add_item(
        "shared",
        SUBJECT_A,
        "MARK-TITLE-shared",
        "MARK-CONTENT shared",
    );
    let first = world.connect("Work files", SUBJECT_A, &["shared"]);
    world.vendor.transfer("shared", SUBJECT_B);
    let second = world.connect("Home files", SUBJECT_B, &["shared"]);
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[]);
    let resolved = |connection| {
        let mut request = request(&fence, &policy, link("shared"));
        request.connection = connection;
        world
            .rows(|rows| resolve(rows, world.vendor.as_ref(), world.owner, &request))
            .unwrap()
    };
    assert!(matches!(
        resolved(None),
        Resolved::None(PrivateSetupReason::ChooseAccount)
    ));
    // Naming the connection chooses it.
    for connection in [first.id, second.id] {
        assert!(
            matches!(resolved(Some(connection)), Resolved::One(found) if found.0.id == connection)
        );
    }
}

#[test]
fn m5_private_plan_no_link_survives_in_anything_the_plan_can_print() {
    let name = PrivateItemName::Link("https://vendor.example/item/MARK-LINK".into());
    assert!(!format!("{name:?}").contains("MARK-LINK"));
    let owner = owner();
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[]);
    let setup = PrivateWorld::new(owner).setup();
    assert!(!format!("{setup:?}").contains("vendor"));
    let plan = PrivateReadPlan {
        verdict: PrivateReadVerdict::NeedsPerson(PrivateSetupReason::NotPicked),
        proposal: None,
        proof: proof(&fence, &policy),
        profile: serde_json::from_value(serde_json::json!(Uuid::new_v4())).unwrap(),
    };
    let printed = format!("{plan:?}");
    assert!(printed.contains("NeedsPerson"), "{printed}");
    assert!(!printed.contains("MARK-"), "{printed}");
}

use super::plan::PrivateReadPlan;

fn delegate(world: &PrivateWorld, owner: koru_delegate_core::BotOwner) -> Arc<PrivateDelegate> {
    Arc::new(PrivateDelegate::new(
        world,
        fence(owner),
        captured(WorkspaceId::new(), &[]),
    ))
}

#[test]
fn m5_announce_is_acknowledged_refused_or_owed_and_never_guessed() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let delegate = delegate(&world, owner);
    assert_eq!(announce(delegate.as_ref(), owner), Announced::Acknowledged);
    assert_eq!(delegate.script().announces, 1);
    // The reply was lost: owed still, although the delegate has it.
    delegate.script().lose.insert("announce");
    assert_eq!(announce(delegate.as_ref(), owner), Announced::Unanswered);
    // The private owner is not composed in the delegate: nothing is owed.
    delegate
        .script()
        .refuse
        .insert("announce", "bot_private_unavailable");
    assert_eq!(announce(delegate.as_ref(), owner), Announced::Refused);
    // Two services in one process carry the one dispatcher id.
    let other = PrivateWorld::new(owner);
    assert_eq!(world.service.dispatcher(), other.service.dispatcher());
    assert_eq!(
        world.service.dispatcher(),
        koru_connections::process_dispatcher()
    );
}

#[test]
fn m5_private_perform_the_ledger_port_maps_each_delegate_answer_to_what_the_dispatcher_does() {
    use koru_connections::{EffectLedger, LedgerError};
    use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
    use koru_delegate_core::{PrivateEffectStage, PrivateEffectTransition};
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let connection = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[connection.id]);
    let port = Arc::new(PrivateDelegate::new(&world, fence.clone(), policy.clone()));
    let boot = port.script().boot;
    let dispatcher = koru_connections::process_dispatcher();
    let Ok(BotReply::PrivateEffect { effect }) = port.execute(BotCommand::PreparePrivateEffect {
        fence: fence.clone(),
        dispatcher,
        operation: world.proposal(Uuid::new_v4(), connection.id, "doc-1"),
        proof: proof(&fence, &policy),
        boot,
    }) else {
        panic!("the intent was not recorded");
    };
    let ledger = super::ledger::DelegateEffectLedger::new(port.clone());
    let mark = PrivateEffectTransition::Dispatched {};
    // The delegate did not answer: the same advance is sent again.
    world.ledger.unavailable(1);
    assert_eq!(
        ledger.advance(&effect.key, dispatcher, &mark),
        Err(LedgerError::Unavailable)
    );
    let marked = ledger.advance(&effect.key, dispatcher, &mark).unwrap();
    assert_eq!(marked.stage, PrivateEffectStage::Dispatched);
    assert_eq!(port.count("advance"), 2);
    // Someone else closed the row: whatever was read is dropped.
    world.ledger.resolve_for_another_dispatcher(&effect.key);
    assert_eq!(
        ledger.advance(
            &effect.key,
            dispatcher,
            &PrivateEffectTransition::Confirmed {
                outcome_bytes: 1,
                withheld: false
            }
        ),
        Err(LedgerError::Stale)
    );
}

#[test]
fn m5_private_card_a_private_binding_holds_ids_only_and_gives_the_retry_its_bound_operation() {
    use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply};
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let connection = world.connect("Work files", SUBJECT_A, &["doc-1"]);
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[]);
    let port = PrivateDelegate::new(&world, fence.clone(), policy.clone());
    let boot = port.script().boot;
    let bound = world.proposal(Uuid::new_v4(), connection.id, "doc-1");
    let Ok(BotReply::Decision { decision: view }) =
        port.execute(BotCommand::PreparePrivateDecision {
            fence: fence.clone(),
            operation: bound.clone(),
            proof: proof(&fence, &policy),
            boot,
        })
    else {
        panic!("the question was not asked");
    };
    let binding = &view.receipt.binding;
    // The binding holds ids and digests only.
    let stored = serde_json::to_string(binding).unwrap();
    assert!(!stored.contains("MARK-"), "{stored}");
    assert_eq!(
        super::plan::private_binding(binding).unwrap().proposal(),
        bound
    );

    // The retained receipt gives the exact retry the operation as it was
    // bound, whatever the connection's revision is by then.
    let mut waits = bots::decision_wait::DecisionWaitDriver::new(None);
    assert!(waits.bound_private(fence.run, bound.operation_id).is_none());
    assert!(waits.remember(view.clone()));
    assert_eq!(
        waits.bound_private(fence.run, bound.operation_id),
        Some(bound.clone())
    );
    assert!(waits.bound_private(fence.run, Uuid::new_v4()).is_none());
    assert!(
        waits
            .bound_private(koru_delegate_core::RunId::mint(), bound.operation_id)
            .is_none()
    );
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn m5_announce_is_sent_for_every_adoption_and_repeated_until_the_delegate_answers() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let delegate = delegate(&world, owner);
    let board = Arc::new(AnnounceBoard::default());
    let shutdown = Arc::new(AtomicBool::new(false));
    super::announce::spawn(board.clone(), delegate.clone(), shutdown.clone()).unwrap();
    // Nothing is sent before an owner is adopted.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(delegate.count("announce"), 0);

    // The first answer is lost: the announce stays owed and is sent again.
    delegate.script().lose.insert("announce");
    board.adopted(owner);
    wait_for("the repeated announce", || delegate.count("announce") >= 2);
    wait_for("the acknowledgment", || !board.counts().2);
    assert_eq!(
        delegate.count("announce"),
        2,
        "acknowledged: nothing more is sent"
    );

    // The owner is adopted again (the owner changed, or the delegate's boot
    // did): announced again.
    board.adopted(owner);
    wait_for("the second adoption's announce", || {
        delegate.count("announce") == 3 && !board.counts().2
    });
    assert_eq!(board.counts().0, 2);
    shutdown.store(true, Ordering::Release);
}

fn hosting_worker(world: &PrivateWorld, port: Arc<PrivateDelegate>) -> WorkerHandle {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-private-connections"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_connections(world.setup())
    .with_bot_delegate(port);
    config.test_repository = Some(Repository::in_memory().unwrap());
    AiWorker::new(config).spawn().unwrap()
}

#[test]
fn m5_connection_commands_run_through_the_workers_own_writer() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let worker = hosting_worker(&world, delegate(&world, owner));
    let client = worker.client();
    let service = client
        .connection_service()
        .expect("the worker hosts the service");
    // Connect, with the consent trip answered by the scripted vendor. Every
    // row is written by the worker's writer thread, asked from this one.
    let request = koru_ai_core::bots::BotRequestId::mint();
    let consent = match service
        .begin_connect(
            world.owner,
            request,
            "Work files",
            "http://127.0.0.1:49152/callback",
        )
        .unwrap()
    {
        koru_connections::TripStart::Consent { consent, .. } => consent,
        other => panic!("the connect did not begin: {other:?}"),
    };
    let receipt = service
        .complete_trip(
            world.owner,
            request,
            world
                .vendor
                .approve(&consent, SUBJECT_A, &["doc-1", "doc-2"]),
        )
        .unwrap();
    let connection = receipt.connection.expect("a connection");
    // The same command again replays its receipt.
    assert_eq!(
        service
            .complete_trip(world.owner, request, world.vendor.deny(&consent))
            .unwrap(),
        receipt
    );
    // Disconnect: fenced, revoked at the vendor, tombstoned.
    let done = service
        .disconnect(
            world.owner,
            koru_ai_core::bots::BotRequestId::mint(),
            connection,
            1,
        )
        .unwrap();
    assert_eq!(
        done.connection.state,
        koru_ai_core::connections::ConnectionState::Disconnected
    );
    assert!(world.vendor.revoked(SUBJECT_A));
    // Upkeep and recovery run on the service's own thread without help.
    assert_eq!(service.open_obligations(), 0);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn m5_connection_commands_a_worker_without_a_vendor_starts_cleanly_and_is_unavailable() {
    let owner = owner();
    let world = PrivateWorld::new(owner);
    let port = delegate(&world, owner);
    // No setup: no service, no thread, no delegate command.
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-private-connections"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_delegate(port.clone());
    config.test_repository = Some(Repository::in_memory().unwrap());
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    assert!(client.connection_service().is_none());
    let fence = fence(owner);
    let policy = captured(WorkspaceId::new(), &[]);
    let refused = client
        .private_read_plan(request(&fence, &policy, link("doc-1")), true)
        .unwrap_err();
    assert_eq!(refused.code, super::plan::PRIVATE_UNAVAILABLE);
    assert!(!refused.retryable);
    assert_eq!(client.private_setup_request(fence.run), None);
    assert!(port.log().is_empty(), "nothing was sent to the delegate");
    // A setup without a delegate hosts nothing either: a read needs both.
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-private-connections"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_connections(world.setup());
    config.test_repository = Some(Repository::in_memory().unwrap());
    let unhosted = AiWorker::new(config).spawn().unwrap();
    assert!(unhosted.client().connection_service().is_none());
    worker.shutdown();
    unhosted.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    assert_eq!(unhosted.join(), WorkerExit::Clean);
}
