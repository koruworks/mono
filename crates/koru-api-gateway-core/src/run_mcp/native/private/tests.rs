// SPDX-License-Identifier: MPL-2.0

//! The private read tool, driven through `invoke` with the real connection
//! service over the scripted vendor and credential store, the real ai.db
//! writer on an in-memory database, and a scripted delegate.
//!
//! **Doubles.** The plan is scripted here (the AI worker's own plan is held
//! by `m5_private_plan`); the delegate is the scripted one of
//! `koru_ai_server::fixtures::connections`, not the real actor, ledger or
//! IPC 18 transport. No socket, no browser, no OS keychain, no real vendor.

use super::*;
use koru_ai_server::PrivateSetupReason;
use koru_ai_server::fixtures::connections::{
    CONTENT_1, LABEL_A, PrivateDelegate, PrivateWorld, SUBJECT_A, TITLE_1, captured, fence, owner,
    proof, successor,
};
use koru_connections::{Clock, PresentCaller};
use koru_delegate_core::{BotRequestId, WorkspaceId};
use std::sync::Mutex;
use std::time::Duration;

const PACING: DecisionPacing = DecisionPacing {
    replays: 3,
    first: Duration::ZERO,
    ceiling: Duration::ZERO,
};
const OPEN: PrivateGate = PrivateGate {
    tools: true,
    questions: Some(PACING),
};
const NO_QUESTIONS: PrivateGate = PrivateGate {
    tools: true,
    questions: None,
};
const CLOSED: PrivateGate = PrivateGate {
    tools: false,
    questions: None,
};

struct Plans {
    verdict: PrivateReadVerdict,
    /// The picked item the plan resolves every call to, if any.
    item: Option<(ConnectionId, &'static str)>,
    requests: usize,
    questions: Vec<bool>,
    recorded: Vec<DecisionReceiptView>,
}

struct Tool {
    world: PrivateWorld,
    delegate: PrivateDelegate,
    record: RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    proof: FreshBotPolicyProof,
    connection: ConnectionId,
    item: ConnectionItemId,
    plans: Mutex<Plans>,
}

impl Planner for Tool {
    fn plan(
        &self,
        request: PrivateReadPlanRequest,
        questions: bool,
    ) -> Result<PrivateReadPlan, ToolFailure> {
        let mut plans = self.plans.lock().unwrap();
        plans.requests += 1;
        plans.questions.push(questions);
        Ok(PrivateReadPlan {
            verdict: plans.verdict,
            proposal: plans.item.map(|(connection, reference)| {
                self.world
                    .proposal(request.operation_id, connection, reference)
            }),
            proof: self.proof.clone(),
            profile: serde_json::from_value(json!(Uuid::new_v4())).unwrap(),
        })
    }
    fn record_decision(&self, view: &DecisionReceiptView) -> Result<(), ToolFailure> {
        self.plans.lock().unwrap().recorded.push(view.clone());
        Ok(())
    }
}

impl Tool {
    /// A Bot Run whose owner connected one account and picked two files.
    /// `granted`: the Run's captured and current policy hold the grant.
    fn new(granted: bool) -> Self {
        let owner = owner();
        let world = PrivateWorld::new(owner);
        world.vendor.add_item(
            "big",
            SUBJECT_A,
            "MARK-TITLE-big",
            &"MARK-CONTENT ".repeat(READ_LIMIT_BYTES / 8),
        );
        let connection = world.connect("Work files", SUBJECT_A, &["doc-1", "big"]);
        let item = world.item(connection.id, "doc-1").id;
        let live = fence(owner);
        let workspace = WorkspaceId::new();
        let held = [connection.id];
        let policy = captured(workspace, if granted { &held } else { &[] });
        let delegate = PrivateDelegate::new(&world, live.clone(), policy.clone());
        let provenance = BotProvenance {
            parent: None,
            routine: None,
            owner,
            bot: live.bot,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        };
        Self {
            proof: proof(&live, &policy),
            record: RunAuthorityRecord {
                run: live.run,
                workspace,
                generation: live.generation,
                grants: [workspace].into(),
                run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
                    if cfg!(windows) {
                        "D:/private-test"
                    } else {
                        "/private-test"
                    },
                ))
                .unwrap(),
                state: koru_artifact_core::AuthorityState::Active,
                created_at_ms: 0,
                revoked_at_ms: None,
            },
            provenance,
            plans: Mutex::new(Plans {
                verdict: if granted {
                    PrivateReadVerdict::Granted
                } else {
                    PrivateReadVerdict::Ask
                },
                item: Some((connection.id, "doc-1")),
                requests: 0,
                questions: Vec::new(),
                recorded: Vec::new(),
            }),
            captured: policy,
            connection: connection.id,
            item,
            delegate,
            world,
        }
    }
    fn call(
        &self,
        arguments: Value,
        gate: PrivateGate,
        waiting: bool,
    ) -> Result<Value, ToolFailure> {
        let call = parse(arguments)?;
        invoke(
            &Owners {
                planner: self,
                port: &self.delegate,
                service: &self.world.service,
            },
            &self.record,
            self.provenance.clone(),
            self.captured.clone(),
            call,
            gate,
            waiting,
            &PresentCaller,
            self.world.clock.now() + PRIVATE_READ_BUDGET,
        )
    }
    fn by_id(&self, operation_id: Uuid) -> Value {
        json!({"operation_id": operation_id, "item_id": self.item})
    }
    fn read(&self, operation_id: Uuid) -> Result<Value, ToolFailure> {
        self.call(self.by_id(operation_id), OPEN, false)
    }
    fn plans(&self) -> std::sync::MutexGuard<'_, Plans> {
        self.plans.lock().unwrap()
    }
    fn needs(&self, verdict: PrivateReadVerdict, item: bool) {
        let mut plans = self.plans();
        plans.verdict = verdict;
        if !item {
            plans.item = None;
        }
    }
}

fn refused(result: Result<Value, ToolFailure>) -> ToolFailure {
    result.expect_err("the read was refused")
}

/// A failure names the operation and nothing else.
fn only_the_operation(failure: &ToolFailure, operation_id: Uuid) {
    assert_eq!(
        failure.details,
        Some(json!({"operation_id": operation_id})),
        "{}",
        failure.code
    );
}

fn nothing_private(text: &str) {
    for mark in [
        "MARK-",
        "vendor.example",
        CONTENT_1,
        LABEL_A,
        TITLE_1,
        "Work files",
    ] {
        assert!(!text.contains(mark), "private text in an answer: {mark}");
    }
}

#[test]
fn m5_tool_output_the_private_read_is_closed_in_this_build() {
    // The private read's own constant is off. The edit decisions were opened
    // by the product owner on 2026-11-17; that alone asks no private-read
    // question, because the question also needs this gate's `tools`.
    assert!(!PRIVATE_OPERATIONS_AVAILABLE);
    assert!(EDIT_DECISIONS_AVAILABLE);
    let production = PrivateGate::production();
    assert!(!production.tools);
    assert!(production.questions.is_none());
    // Not listed: the Bot catalog is what it was.
    let listed: Vec<_> = super::super::definitions()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(!listed.iter().any(|name| name == READ_TOOL));
    assert_eq!(
        definitions_for(false, super::super::children::ChildTools::NONE)
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>(),
        listed
    );
    // A call under the closed gate asks no owner and reads nothing.
    let tool = Tool::new(true);
    let operation_id = Uuid::new_v4();
    let failure = refused(tool.call(tool.by_id(operation_id), CLOSED, false));
    assert_eq!(failure.code, "private_unavailable");
    only_the_operation(&failure, operation_id);
    assert!(tool.delegate.log().is_empty());
    assert_eq!(tool.plans().requests, 0);
    assert_eq!(tool.world.vendor.content_reads(), 0);
}

#[test]
fn m5_tool_listing_follows_the_gate_the_host_and_a_routines_grant() {
    let tool = Tool::new(false);
    let interactive = &tool.provenance;
    let mut routine = tool.provenance.clone();
    routine.routine = Some(koru_delegate_core::RoutineProvenance {
        schedule: koru_delegate_core::ScheduleId::from_uuid(Uuid::new_v4()),
        occurrence_ms: 900,
        definition: koru_delegate_core::RoutineDefinitionIdentity {
            revision: 2,
            digest: koru_delegate_core::RoutineDigest32::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    });
    let none = captured(tool.record.workspace, &[]);
    let one = captured(tool.record.workspace, &[tool.connection]);
    // Closed, or no connection service hosted: never listed.
    for policy in [&none, &one] {
        assert!(!listed(CLOSED, true, interactive, policy));
        assert!(!listed(OPEN, false, interactive, policy));
        assert!(!listed(OPEN, false, &routine, policy));
    }
    // An interactive Bot Run sees it without a grant: it can ask.
    assert!(listed(OPEN, true, interactive, &none));
    assert!(listed(NO_QUESTIONS, true, interactive, &one));
    // A Routine cannot ask: listed only when its captured grant holds one.
    assert!(!listed(OPEN, true, &routine, &none));
    assert!(listed(OPEN, true, &routine, &one));

    // When it is listed it is one tool with a closed schema.
    let tools = definitions_for(true, super::super::children::ChildTools::NONE);
    let read = tools
        .iter()
        .find(|tool| tool.name == READ_TOOL)
        .expect("the private read");
    let schema = serde_json::to_value(&read.input_schema).unwrap();
    assert_eq!(schema["required"], json!(["operation_id"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    let mut properties: Vec<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    properties.sort();
    assert_eq!(
        properties,
        ["connection_id", "item_id", "item_link", "operation_id"]
    );
    assert_eq!(
        tools.len(),
        definitions_for(false, super::super::children::ChildTools::NONE).len() + 1
    );
    assert!(recognizes(READ_TOOL));
    assert!(!recognizes("read_document"));
}

#[test]
fn m5_tool_granted_a_read_returns_the_content_once_and_its_retry_never_reads_again() {
    let tool = Tool::new(true);
    let operation_id = Uuid::new_v4();
    let answer = tool.read(operation_id).unwrap();
    assert_eq!(
        answer,
        json!({
            "operation_id": operation_id,
            "connection_id": tool.connection,
            "item_id": tool.item,
            "format": "markdown",
            "content": CONTENT_1,
            "truncated": false,
            "total_bytes": CONTENT_1.len(),
        })
    );
    // The retained lookup, the plan, the live fence, the fresh checks, the
    // boot, then the one intent.
    assert_eq!(
        tool.delegate.log(),
        [
            "read_effect",
            "reconcile",
            "sync",
            "effect_fence",
            "hello",
            "prepare_effect"
        ]
    );
    assert_eq!(tool.world.vendor.content_reads(), 1);
    assert_eq!(tool.plans().requests, 1);

    // The exact retry is answered from the retained effect, before any plan.
    let failure = refused(tool.read(operation_id));
    assert_eq!(failure.code, "private_result_unavailable");
    only_the_operation(&failure, operation_id);
    assert_eq!(tool.plans().requests, 1);
    assert_eq!(tool.delegate.count("prepare_effect"), 1);
    assert_eq!(tool.world.vendor.content_reads(), 1);

    // By the person's link the same item is read, under a new operation.
    let by_link = tool
        .call(
            json!({"operation_id": Uuid::new_v4(), "item_link": "https://vendor.example/item/doc-1"}),
            OPEN,
            false,
        )
        .unwrap();
    assert_eq!(by_link["item_id"], json!(tool.item));
    assert_eq!(tool.world.vendor.content_reads(), 2);
}

#[test]
fn m5_tool_output_a_long_file_is_cut_at_the_limit_and_says_so() {
    let tool = Tool::new(true);
    tool.plans().item = Some((tool.connection, "big"));
    let big = tool.world.item(tool.connection, "big").id;
    let answer = tool
        .call(
            json!({"operation_id": Uuid::new_v4(), "item_id": big}),
            OPEN,
            false,
        )
        .unwrap();
    let content = answer["content"].as_str().unwrap();
    assert!(content.len() <= READ_LIMIT_BYTES, "{}", content.len());
    assert!(content.len() > READ_LIMIT_BYTES - 16);
    assert_eq!(answer["truncated"], json!(true));
    assert!(answer["total_bytes"].as_u64().unwrap() > READ_LIMIT_BYTES as u64);
    assert_eq!(READ_LIMIT_BYTES, 262_144);
}

#[test]
fn m5_tool_question_an_ungranted_read_asks_once_and_the_resumed_retry_performs_it() {
    let mut tool = Tool::new(false);
    let operation_id = Uuid::new_v4();
    // Not granted: one question, positively pending.
    let pending = refused(tool.read(operation_id));
    assert_eq!(pending.code, "permission_pending");
    let details = pending.details.clone().unwrap();
    assert_eq!(details["operation_id"], json!(operation_id));
    assert!(details["decision_id"].is_string());
    assert_eq!(tool.delegate.script().decisions_created, 1);
    assert!(
        !tool.plans().recorded.is_empty(),
        "the receipt went to its card"
    );
    assert_eq!(tool.world.vendor.content_reads(), 0);
    assert_eq!(tool.delegate.count("prepare_effect"), 0);
    nothing_private(&json!(pending).to_string());

    // While it waits, the same call stays pending and asks nothing new.
    assert_eq!(
        refused(tool.call(tool.by_id(operation_id), OPEN, true)).code,
        "permission_pending"
    );
    assert_eq!(tool.delegate.script().decisions_created, 1);

    // The person allows, and the recorded successor takes the Run over.
    let origin = tool.delegate.script().fence.clone();
    let next = successor(&origin);
    tool.delegate.allow(next);
    // The resumed Bot's exact retry consumes the decision and reads, once.
    let answer = tool.call(tool.by_id(operation_id), OPEN, true).unwrap();
    assert_eq!(answer["content"], json!(CONTENT_1));
    assert_eq!(answer["operation_id"], json!(operation_id));
    assert_eq!(tool.delegate.count("consume"), 1);
    assert_eq!(tool.world.vendor.content_reads(), 1);
    // It can never read twice.
    let again = refused(tool.read(operation_id));
    assert_eq!(again.code, "private_result_unavailable");
    assert_eq!(tool.world.vendor.content_reads(), 1);
    assert_eq!(tool.delegate.count("consume"), 1);

    // Denied: the question is closed and nothing was read.
    tool = Tool::new(false);
    let operation_id = Uuid::new_v4();
    assert_eq!(refused(tool.read(operation_id)).code, "permission_pending");
    tool.delegate.deny();
    let denied = refused(tool.call(tool.by_id(operation_id), OPEN, true));
    assert_eq!(denied.code, "decision_denied");
    only_the_operation(&denied, operation_id);
    assert_eq!(tool.world.vendor.content_reads(), 0);
    assert_eq!(tool.delegate.count("consume"), 0);
}

#[test]
fn m5_tool_question_none_is_asked_while_the_decision_gate_is_closed() {
    // The tool gate alone does not open the question path.
    let tool = Tool::new(false);
    let operation_id = Uuid::new_v4();
    let failure = refused(tool.call(tool.by_id(operation_id), NO_QUESTIONS, false));
    assert_eq!(failure.code, "private_read_needs_person");
    only_the_operation(&failure, operation_id);
    assert_eq!(tool.plans().questions, [false]);
    assert_eq!(tool.delegate.count("prepare_decision"), 0);
    assert_eq!(tool.delegate.script().decisions_created, 0);
    // A granted Run still reads.
    let tool = Tool::new(true);
    assert!(
        tool.call(tool.by_id(Uuid::new_v4()), NO_QUESTIONS, false)
            .is_ok()
    );
}

/// M6 contract 3.3: a child Run never asks a private-operation question,
/// even with both gates open; its ungranted read gets M5's ungranted
/// refusal. The ledger refuses such a decision for a child Run as well
/// (`koru-delegate-sqlite`, `m6_child_resume_rows_a_child_run_never_asks_a_private_question`).
#[test]
fn m6_tool_fence_a_child_runs_private_read_never_becomes_a_question() {
    let mut tool = Tool::new(false);
    tool.provenance.parent = Some(koru_delegate_core::ChildParent {
        run: koru_delegate_core::RunId::mint(),
        bot: koru_delegate_core::BotId::mint(),
        chat_id: Uuid::new_v4(),
        child_request: koru_delegate_core::ChildRequestId::mint(),
    });
    let operation_id = Uuid::new_v4();
    let failure = refused(tool.call(tool.by_id(operation_id), OPEN, false));
    assert_eq!(failure.code, "private_read_needs_person");
    only_the_operation(&failure, operation_id);
    // The plan was asked without questions, and none was prepared.
    assert_eq!(tool.plans().questions, [false]);
    assert_eq!(tool.delegate.count("prepare_decision"), 0);
    assert_eq!(tool.delegate.script().decisions_created, 0);
    assert_eq!(tool.world.vendor.content_reads(), 0);
    nothing_private(&json!(failure).to_string());
    // The same Run that is not a child asks, as before.
    tool.provenance.parent = None;
    assert_eq!(
        refused(tool.call(tool.by_id(Uuid::new_v4()), OPEN, false)).code,
        "permission_pending"
    );
}

#[test]
fn m5_tool_ungranted_codes_every_setup_situation_is_one_code_with_the_same_details() {
    use PrivateSetupReason as S;
    let operation_id = Uuid::new_v4();
    let mut answers = Vec::new();
    for reason in [
        S::NoConnection,
        S::NotPicked,
        S::ChooseAccount,
        S::NeedsSignIn,
        S::Disconnected,
        S::ItemChanged,
        S::GiveAccess,
    ] {
        // With and without a resolved item, by id and by a link.
        for (item, arguments) in [
            (
                false,
                json!({"operation_id": operation_id, "item_link": "https://vendor.example/item/MARK-LINK"}),
            ),
            (
                true,
                json!({"operation_id": operation_id, "item_id": Uuid::new_v4()}),
            ),
        ] {
            let tool = Tool::new(false);
            tool.needs(PrivateReadVerdict::NeedsPerson(reason), item);
            let failure = refused(tool.call(arguments, NO_QUESTIONS, false));
            assert_eq!(failure.code, "private_read_needs_person", "{reason:?}");
            only_the_operation(&failure, operation_id);
            // Nothing was asked, recorded or read for it.
            assert_eq!(tool.delegate.count("prepare_decision"), 0);
            assert_eq!(tool.delegate.count("prepare_effect"), 0);
            assert_eq!(tool.world.vendor.content_reads(), 0);
            let text = json!(failure).to_string();
            nothing_private(&text);
            answers.push(text);
        }
    }
    answers.dedup();
    assert_eq!(answers.len(), 1, "one answer, whatever the reason");

    // The delegate's own predicate finds no grant after the plan did: the
    // same code, and nothing was committed.
    let tool = Tool::new(true);
    tool.delegate.script().granted = false;
    let failure = refused(tool.call(tool.by_id(operation_id), NO_QUESTIONS, false));
    assert_eq!(json!(failure).to_string(), answers[0]);
    assert_eq!(tool.world.vendor.content_reads(), 0);
}

#[test]
fn m5_tool_granted_a_typed_refusal_of_the_plan_or_the_read_is_its_own_code() {
    // The plan's typed refusals pass through with the operation id only.
    for code in [
        "connection_needs_sign_in",
        "connection_revoked",
        "item_changed",
        "routine_connection_not_granted",
        "native_policy_pending",
    ] {
        let tool = Tool::new(true);
        tool.needs(PrivateReadVerdict::Refused(code), true);
        let operation_id = Uuid::new_v4();
        let failure = refused(tool.read(operation_id));
        assert_eq!(failure.code, code);
        only_the_operation(&failure, operation_id);
        nothing_private(&json!(failure).to_string());
        assert_eq!(tool.delegate.count("prepare_effect"), 0);
    }
    // The credential store is locked: recorded as a refused effect, told by
    // its code, and told again by the retry.
    let tool = Tool::new(true);
    tool.world.credentials.set_locked(true);
    tool.world.clock.advance(Duration::from_secs(2 * 3600));
    let operation_id = Uuid::new_v4();
    for _ in 0..2 {
        let failure = refused(tool.read(operation_id));
        assert_eq!(failure.code, "credential_locked");
        only_the_operation(&failure, operation_id);
    }
    assert_eq!(tool.delegate.count("prepare_effect"), 1);
    assert_eq!(tool.world.vendor.content_reads(), 0);
}

#[test]
fn m5_tool_replay_a_lost_intent_is_unknown_then_closed_and_an_id_names_one_thing() {
    let tool = Tool::new(true);
    let operation_id = Uuid::new_v4();
    tool.delegate.script().lose.insert("prepare_effect");
    let unknown = refused(tool.read(operation_id));
    assert_eq!(unknown.code, "private_outcome_unknown");
    only_the_operation(&unknown, operation_id);
    assert_eq!(tool.world.vendor.content_reads(), 0);
    // The retry finds the row this process opened and nobody is performing:
    // it is closed unread, never performed late.
    let closed = refused(tool.read(operation_id));
    assert_eq!(closed.code, "private_read_not_done");
    assert_eq!(
        refused(tool.read(operation_id)).code,
        "private_read_not_done"
    );
    assert_eq!(tool.world.vendor.content_reads(), 0);
    assert_eq!(tool.delegate.count("prepare_effect"), 1);

    // The same id for another item, or on another connection, is a conflict.
    let other = tool.world.item(tool.connection, "big").id;
    for arguments in [
        json!({"operation_id": operation_id, "item_id": other}),
        json!({"operation_id": operation_id, "item_id": tool.item, "connection_id": Uuid::new_v4()}),
    ] {
        assert_eq!(
            refused(tool.call(arguments, OPEN, false)).code,
            "native_operation_conflict"
        );
    }

    // The delegate cannot say whether an effect is retained: no fresh intent.
    let tool = Tool::new(true);
    tool.delegate.script().lose.insert("read_effect");
    assert_eq!(
        refused(tool.read(Uuid::new_v4())).code,
        "native_owner_unavailable"
    );
    assert_eq!(tool.delegate.count("prepare_effect"), 0);
}

#[test]
fn m5_tool_fence_while_a_decision_waits_only_its_exact_retry_passes() {
    // A Run that holds the grant, classified as waiting on another decision:
    // nothing new is read.
    let tool = Tool::new(true);
    for gate in [OPEN, NO_QUESTIONS] {
        let operation_id = Uuid::new_v4();
        let failure = refused(tool.call(tool.by_id(operation_id), gate, true));
        assert_eq!(failure.code, "decision_in_progress");
        only_the_operation(&failure, operation_id);
    }
    assert_eq!(tool.delegate.count("prepare_effect"), 0);
    assert_eq!(tool.world.vendor.content_reads(), 0);

    // A Run whose own question is open: another operation is refused, and
    // asks no second question.
    let tool = Tool::new(false);
    let asked = Uuid::new_v4();
    assert_eq!(refused(tool.read(asked)).code, "permission_pending");
    let other = Uuid::new_v4();
    let failure = refused(tool.call(tool.by_id(other), OPEN, true));
    assert_eq!(failure.code, "decision_in_progress");
    only_the_operation(&failure, other);
    assert_eq!(tool.delegate.script().decisions_created, 1);
    assert_eq!(tool.world.vendor.content_reads(), 0);
    // The question's own exact retry still passes the fence.
    assert_eq!(
        refused(tool.call(tool.by_id(asked), OPEN, true)).code,
        "permission_pending"
    );
}

#[test]
fn m5_tool_output_malformed_arguments_are_refused_before_any_owner_is_asked() {
    let tool = Tool::new(true);
    let id = Uuid::new_v4();
    for arguments in [
        json!({}),
        json!({"operation_id": id}),
        json!({"operation_id": id, "item_id": tool.item, "item_link": "doc-1"}),
        json!({"operation_id": id, "item_link": ""}),
        json!({"operation_id": id, "item_link": "x".repeat(2049)}),
        json!({"operation_id": id, "item_id": "not-an-id"}),
        json!({"operation_id": "not-an-id", "item_id": tool.item}),
        json!({"operation_id": id.to_string().to_uppercase(), "item_id": tool.item}),
        json!({"operation_id": Uuid::nil(), "item_id": tool.item}),
        json!({"operation_id": id, "item_id": tool.item, "title": "MARK-TITLE"}),
    ] {
        let failure = refused(tool.call(arguments, OPEN, false));
        assert_eq!(failure.code, "invalid_request");
        nothing_private(&json!(failure).to_string());
    }
    assert!(tool.delegate.log().is_empty());
    assert_eq!(tool.plans().requests, 0);
    // A Run whose authority is not exactly its own home is refused.
    let mut foreign = Tool::new(true);
    foreign.captured.workspace = WorkspaceId::new();
    assert_eq!(
        refused(foreign.read(Uuid::new_v4())).code,
        "permission_denied"
    );
    assert!(foreign.delegate.log().is_empty());
}
