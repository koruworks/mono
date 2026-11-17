// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_tool_listing`, `m6_tool_ask`, `m6_tool_answer_held`,
//! `m6_tool_fence` (the parts this module owns) and `m6_tool_result`.
//!
//! **Doubles.** The AI worker is a scripted [`ChildOwner`]; its own ask is
//! held by piece W's cohorts. The delegate is a scripted port that answers
//! `ReadChildRequests` and the child Run's `Read`; the ledger and the actor
//! are piece B's and R's. The pinned read is the real one, over a real Store
//! kept in memory. No socket, no disk, no provider, no browser.

use super::*;
use koru_ai_server::native_store_backend;
use koru_delegate_core::children::ChildClock;
use koru_delegate_core::decisions::DecisionRevision;
use koru_delegate_core::{
    ActorRef, BotAccess, BotOwner, BotPolicyBarrier, BotPolicyBarrierKey, BotRequestId,
    BotWorkingFolder, ChildParent, DelegationRequest, DeviceId, DocumentVersionId,
    FreshBotPolicyProof, NativeResultKind, ProfileId, RoutineDigest32, RunExecution, RunId,
    RunView, SessionProfileKey, TimelineWindow, WorkspaceScope,
};
use koru_mcp_server::Backend;
use koru_mcp_server::native::NativeStoreIdentity;
use koru_store_core::{Locale, Timestamp};
use koru_store_server::memory::MemoryStore;
use std::collections::VecDeque;
use std::sync::Mutex;

/// What the asked Bot wrote, and what a person wrote after it.
const PINNED: &str = "PINNED-SENTINEL the asked Bot's sentence";
const LIVE: &str = "LIVE-SENTINEL a person's later sentence";
const TITLE: &str = "TITLE-SENTINEL Findings";

const QUICK: HoldPacing = HoldPacing {
    budget: Duration::from_millis(300),
    first: Duration::from_millis(1),
    ceiling: Duration::from_millis(5),
};
const OPEN: ChildGate = ChildGate {
    tools: true,
    hold: QUICK,
};
const CLOSED: ChildGate = ChildGate {
    tools: false,
    hold: QUICK,
};

fn r(value: u64) -> DecisionRevision {
    DecisionRevision::new(value).unwrap()
}

fn owner_failure(code: &str, details: &[(&str, Value)]) -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: code.into(),
        message: "MESSAGE-SENTINEL a worker text".into(),
        retryable: false,
        details: details
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    }
}

// ---- the AI worker --------------------------------------------------------------

struct Owner {
    askable: Mutex<Result<Vec<RunAskableBot>, koru_ai_wire::Failure>>,
    /// `None`: admit the ask in input order under [`Owner::wait`].
    answer: Mutex<Option<Result<RunChildAskPending, koru_ai_wire::Failure>>>,
    wait: BotWaitId,
    asks: Mutex<Vec<(Uuid, Vec<BotId>)>>,
    lists: Mutex<usize>,
    evidence: NativePolicyEvidence,
    policies: Mutex<usize>,
}
impl ChildOwner for Owner {
    fn askable(&self, _: RunAskableRequest) -> Result<Vec<RunAskableBot>, koru_ai_wire::Failure> {
        *self.lists.lock().unwrap() += 1;
        self.askable.lock().unwrap().clone()
    }
    fn ask(
        &self,
        request: RunChildAskRequest,
    ) -> Result<RunChildAskPending, koru_ai_wire::Failure> {
        let bots: Vec<BotId> = request.requests.iter().map(|item| item.bot_id).collect();
        self.asks
            .lock()
            .unwrap()
            .push((request.ask_key, bots.clone()));
        match self.answer.lock().unwrap().clone() {
            Some(answer) => answer,
            None => Ok(RunChildAskPending {
                wait: self.wait,
                requests: bots
                    .into_iter()
                    .map(|bot| (ChildRequestId::mint(), bot))
                    .collect(),
            }),
        }
    }
    fn policy(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        assert!(request.target.is_some(), "a page read names its page");
        *self.policies.lock().unwrap() += 1;
        Ok(self.evidence.clone())
    }
}

// ---- the delegate ---------------------------------------------------------------

#[derive(Default)]
struct Script {
    /// The open wait's phase at each read, in order; `None` once it is gone.
    phases: VecDeque<Option<BotWaitPhase>>,
    /// Repeated once `phases` is used up.
    last: Option<BotWaitPhase>,
    requests: Vec<ChildRequestReceipt>,
    header: Option<RunHeader>,
    lose_reads: bool,
    log: Vec<&'static str>,
}
struct Port {
    key: BotRunKey,
    wait: BotWaitId,
    script: Mutex<Script>,
}
impl Port {
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script.lock().unwrap()
    }
    fn count(&self, entry: &str) -> usize {
        self.script().log.iter().filter(|e| **e == entry).count()
    }
}
impl BotDelegatePort for Port {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut s = self.script();
        match command {
            BotCommand::ReadChildRequests { subject } => {
                assert_eq!(subject, self.key);
                s.log.push("read_requests");
                if s.lose_reads {
                    return Err(koru_delegate_api::Failure::unavailable());
                }
                let phase = match s.phases.pop_front() {
                    Some(phase) => phase,
                    None => s.last,
                };
                let open = s
                    .requests
                    .iter()
                    .filter(|receipt| receipt.state == ChildRequestState::Open)
                    .count() as u8;
                Ok(BotReply::ChildRequests {
                    wait: phase.map(|phase| ChildWaitView {
                        wait: self.wait,
                        phase,
                        revision: r(1),
                        open_requests: open,
                    }),
                    requests: s.requests.clone(),
                })
            }
            BotCommand::Read { key, window } => {
                s.log.push("read_header");
                assert_eq!(window.len, 1);
                match &s.header {
                    Some(header) if header.run == key.run => Ok(BotReply::Run {
                        view: RunView {
                            document: DocumentId::new(),
                            header: header.clone(),
                            execution: RunExecution {
                                device: key.owner.device,
                                session: None,
                                run_workspace: None,
                            },
                            context: Vec::new(),
                            timeline: TimelineWindow {
                                total: 0,
                                from: 0,
                                items: Vec::new(),
                            },
                        },
                    }),
                    _ => Err(koru_delegate_api::Failure::new(
                        "unknown_run",
                        "No run by that id exists on this device.",
                        false,
                    )),
                }
            }
            _ => panic!("unexpected command"),
        }
    }
}

// ---- the fixture -----------------------------------------------------------------

struct Fixture {
    memory: MemoryStore,
    record: RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    owner: Owner,
    port: Port,
    recipient: BotId,
}

impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("child-tools-test").unwrap();
        let (scope, profile, workspace) = memory
            .with_store(|store| {
                let profile = store
                    .create_profile("Test", None, None, Locale::default(), Timestamp::now())
                    .unwrap()
                    .id;
                let workspace = store
                    .create_workspace(profile, "Work", Timestamp::now())
                    .unwrap()
                    .id;
                (store.local_user_scope().unwrap(), profile, workspace)
            })
            .unwrap();
        let owner = BotOwner {
            user_scope: scope,
            device: DeviceId::new(),
        };
        let bot = BotId::mint();
        let run = RunId::mint();
        let captured = CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        };
        let provenance = BotProvenance {
            parent: None,
            routine: None,
            owner,
            bot,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        };
        let wait = BotWaitId::mint();
        Self {
            memory,
            record: RunAuthorityRecord {
                run,
                workspace,
                generation: koru_delegate_core::AttemptGeneration::FIRST,
                grants: [workspace].into(),
                run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
                    if cfg!(windows) {
                        "D:/child-tools-test"
                    } else {
                        "/child-tools-test"
                    },
                ))
                .unwrap(),
                state: koru_artifact_core::AuthorityState::Active,
                created_at_ms: 0,
                revoked_at_ms: None,
            },
            owner: Owner {
                askable: Mutex::new(Ok(Vec::new())),
                answer: Mutex::new(None),
                wait,
                asks: Mutex::new(Vec::new()),
                lists: Mutex::new(0),
                evidence: NativePolicyEvidence {
                    profile,
                    proof: FreshBotPolicyProof {
                        routine: None,
                        global: BotPolicyBarrier {
                            key: BotPolicyBarrierKey {
                                owner,
                                bot,
                                request: Uuid::new_v4().try_into().unwrap(),
                            },
                            config_revision: 1,
                            policy_revision: 1,
                            policy: captured.clone(),
                            effects_allowed: true,
                        },
                    },
                },
                policies: Mutex::new(0),
            },
            port: Port {
                key: BotRunKey { owner, bot, run },
                wait,
                script: Mutex::new(Script::default()),
            },
            provenance,
            captured,
            recipient: BotId::mint(),
        }
    }

    fn call_as(
        &self,
        provenance: &BotProvenance,
        tool: &str,
        arguments: Value,
        wait: RunWait,
        gate: ChildGate,
    ) -> Result<Value, ToolFailure> {
        invoke(
            &Owners {
                owner: &self.owner,
                port: &self.port,
                store: &self.memory.client(),
            },
            &self.record,
            provenance.clone(),
            self.captured.clone(),
            wait,
            gate,
            tool,
            arguments,
            Cancellation::default(),
        )
    }

    fn call(&self, tool: &str, arguments: Value) -> Result<Value, ToolFailure> {
        self.call_as(&self.provenance, tool, arguments, RunWait::None, OPEN)
    }

    fn child_provenance(&self) -> BotProvenance {
        let mut child = self.provenance.clone();
        child.parent = Some(ChildParent {
            run: RunId::mint(),
            bot: BotId::mint(),
            chat_id: Uuid::new_v4(),
            child_request: ChildRequestId::mint(),
        });
        child
    }

    fn authority(&self) -> NativeStoreAuthority {
        NativeStoreAuthority {
            owner_scope: self.provenance.owner.user_scope,
            expected_profile: self.owner.evidence.profile,
            workspace: self.record.workspace,
        }
    }

    /// The asked Bot's save of a new page, through the native effect path.
    fn save_page(&self) -> (DocumentId, DocumentVersionId) {
        let backend = native_store_backend(self.memory.client(), self.authority());
        let prepared = backend
            .prepare_native_write(
                "create_document",
                json!({
                    "workspace_id": self.record.workspace,
                    "title": TITLE,
                    "initial_operations": [{
                        "op": "insert", "node_id": "body", "parent_id": "root",
                        "after_node_id": null, "kind": "paragraph", "text": PINNED
                    }]
                }),
            )
            .unwrap();
        let evidence = backend
            .execute_native_write(
                &prepared,
                NativeStoreIdentity {
                    candidate: DocumentId::new(),
                    scope: "child-tools-test".into(),
                    request_key: Uuid::new_v4().to_string(),
                },
            )
            .unwrap_or_else(|error| panic!("create refused: {}", error.error().code));
        (
            evidence.document,
            evidence.version_id.expect("a pinned save"),
        )
    }

    /// A person's edit of the page after the asked Bot saved it.
    fn person_edits(&self, document: DocumentId) {
        let backend = native_store_backend(self.memory.client(), self.authority());
        let read = backend
            .invoke(
                &koru_mcp_server::RequestContext::local(),
                "read_document",
                json!({"workspace_id": self.record.workspace, "document_id": document}),
            )
            .unwrap();
        let prepared = backend
            .prepare_native_write(
                "edit_document",
                json!({
                    "workspace_id": self.record.workspace,
                    "document_id": document,
                    "base_revision": read["revision"],
                    "operations": [{
                        "op": "insert", "parent_id": read["root_node_id"],
                        "after_node_id": null, "kind": "paragraph", "text": LIVE
                    }]
                }),
            )
            .unwrap();
        backend
            .execute_native_write(
                &prepared,
                NativeStoreIdentity {
                    candidate: document,
                    scope: "child-tools-test".into(),
                    request_key: Uuid::new_v4().to_string(),
                },
            )
            .unwrap_or_else(|error| panic!("edit refused: {}", error.error().code));
    }

    fn receipt(
        &self,
        ordinal: u8,
        state: ChildRequestState,
        settled_by: Option<ChildSettledBy>,
        child: BotAdmissionState,
    ) -> ChildRequestReceipt {
        ChildRequestReceipt {
            child_request: ChildRequestId::mint(),
            parent: self.port.key.clone(),
            wait: self.port.wait,
            ordinal,
            recipient: self.recipient,
            child_run: RunId::mint(),
            state,
            settled_by,
            child,
            clock: if state == ChildRequestState::Open {
                ChildClock::Running
            } else {
                ChildClock::Stopped
            },
            elapsed_ms: 1_000,
            budget_ms: BOT_CHILD_WAIT_MS as u32,
            revision: r(2),
            binding_digest: RoutineDigest32::from_bytes([7; 32]),
        }
    }

    /// A completed request, its child Run's header holding `entries`.
    fn completed(&self, entries: Vec<NativeResultReference>) -> ChildRequestReceipt {
        let receipt = self.receipt(
            0,
            ChildRequestState::Settled,
            Some(ChildSettledBy::ChildEnded),
            BotAdmissionState::Settled,
        );
        let header = self.header(&receipt, RunStatus::Completed, entries);
        let mut s = self.port.script();
        s.requests.push(receipt.clone());
        s.header = Some(header);
        receipt
    }

    fn header(
        &self,
        receipt: &ChildRequestReceipt,
        status: RunStatus,
        entries: Vec<NativeResultReference>,
    ) -> RunHeader {
        let mut header = RunHeader::from_request(
            DelegationRequest {
                run: receipt.child_run,
                requested_by: ActorRef::Person {
                    profile: ProfileId::from_uuid(Uuid::new_v4()),
                },
                objective: Objective::new("OBJECTIVE-SENTINEL").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::BotFolder {
                    folder: BotWorkingFolder::new("D:/isolated".into()).unwrap(),
                },
                context: Vec::new(),
                grants: Default::default(),
                executing_device: self.provenance.owner.device,
                origin: Default::default(),
            },
            1,
        );
        header.bot = Some(BotProvenance {
            parent: Some(ChildParent {
                run: self.record.run,
                bot: self.provenance.bot,
                chat_id: self.provenance.chat_id,
                child_request: receipt.child_request,
            }),
            routine: None,
            owner: self.provenance.owner,
            bot: receipt.recipient,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        });
        header.status = status;
        header.native_results = entries;
        header
    }

    fn entry(
        &self,
        document: DocumentId,
        version: Option<DocumentVersionId>,
        confirmed_at_ms: i64,
    ) -> NativeResultReference {
        NativeResultReference {
            operation_id: Uuid::new_v4(),
            workspace: self.record.workspace,
            document,
            version: vec![1, 2, 3],
            title: TITLE.into(),
            confirmed_at_ms,
            kind: NativeResultKind::CreatedPage,
            version_id: version,
        }
    }
}

fn ask_arguments(bots: &[BotId]) -> Value {
    json!({
        "ask_key": Uuid::new_v4(),
        "requests": bots
            .iter()
            .map(|bot| json!({"bot_id": bot, "task": "TASK-SENTINEL summarize", "documents": []}))
            .collect::<Vec<_>>(),
    })
}

fn names(tools: &[Tool]) -> Vec<String> {
    tools.iter().map(|tool| tool.name.to_string()).collect()
}

/// No answer carries a task, a worker message or (outside a page read) a
/// title or page text.
fn nothing_private(text: &str) {
    for mark in [
        "TASK-SENTINEL",
        "MESSAGE-SENTINEL",
        "OBJECTIVE-SENTINEL",
        PINNED,
        LIVE,
    ] {
        assert!(!text.contains(mark), "private text in an answer: {mark}");
    }
}

// ---- m6_tool_listing -------------------------------------------------------------

#[test]
fn m6_tool_listing_the_gate_is_closed_in_this_build_and_nothing_is_listed() {
    assert!(!CHILD_REQUESTS_AVAILABLE);
    assert!(!ChildGate::production().tools);
    // Neither neighboring gate moved.
    assert!(!super::super::decision::EDIT_DECISIONS_AVAILABLE);
    assert!(!super::super::private::PRIVATE_OPERATIONS_AVAILABLE);
    let f = Fixture::new();
    for may_ask in [false, true] {
        assert_eq!(
            listed(ChildGate::production().tools, &f.provenance, may_ask),
            ChildTools::NONE
        );
    }
    let catalog = super::super::definitions();
    assert_eq!(catalog.len(), 13);
    for name in [LIST_TOOL, ASK_TOOL, READ_TOOL] {
        assert!(!names(&catalog).iter().any(|listed| listed == name));
    }
    // A call under the closed gate asks no owner.
    for tool in [LIST_TOOL, ASK_TOOL, READ_TOOL] {
        let refused = f
            .call_as(&f.provenance, tool, json!({}), RunWait::None, CLOSED)
            .unwrap_err();
        assert_eq!(refused.code, "unknown_tool");
    }
    assert!(f.port.script().log.is_empty());
    assert!(f.owner.asks.lock().unwrap().is_empty());
    assert_eq!(*f.owner.lists.lock().unwrap(), 0);
}

#[test]
fn m6_tool_listing_sixteen_with_the_gate_open_and_thirteen_for_a_child_or_without_may_ask() {
    let f = Fixture::new();
    let child = f.child_provenance();
    let full = definitions_for(false, listed(true, &f.provenance, true));
    assert_eq!(full.len(), 16);
    let mut added: Vec<_> = names(&full)
        .into_iter()
        .filter(|name| recognizes(name))
        .collect();
    added.sort();
    assert_eq!(added, [ASK_TOOL, LIST_TOOL, READ_TOOL]);
    // A child Run is offered none of them (depth one).
    assert_eq!(listed(true, &child, true), ChildTools::NONE);
    assert_eq!(definitions_for(false, listed(true, &child, true)).len(), 13);
    // `may_ask` off: the two ask tools go; the read stays.
    let without = definitions_for(false, listed(true, &f.provenance, false));
    assert_eq!(without.len(), 14);
    assert!(names(&without).iter().any(|name| name == READ_TOOL));
    assert!(
        !names(&without)
            .iter()
            .any(|name| name == ASK_TOOL || name == LIST_TOOL)
    );
    // The M2 external tools are never listed to a Bot Run.
    let external = super::super::super::tools::definitions();
    for tools in [&full, &without] {
        for tool in &external {
            assert!(
                !names(tools).contains(&tool.name.to_string()),
                "{}",
                tool.name
            );
        }
        for name in [
            "list_bots",
            "start_bot_task",
            "read_bot_task",
            "stop_bot_task",
        ] {
            assert!(!names(tools).iter().any(|listed| listed == name));
        }
    }
}

#[test]
fn m6_tool_listing_the_schemas_are_closed_and_bounded() {
    let tools = definitions(ChildTools {
        ask: true,
        read: true,
    });
    let schema = |name: &str| {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        serde_json::to_value(&tool.input_schema).unwrap()
    };
    let ask = schema(ASK_TOOL);
    assert_eq!(ask["additionalProperties"], json!(false));
    assert_eq!(ask["properties"]["requests"]["minItems"], json!(1));
    assert_eq!(ask["properties"]["requests"]["maxItems"], json!(3));
    assert_eq!(ask["properties"]["ask_key"]["format"], json!("uuid"));
    let mut required: Vec<_> = ask["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    required.sort();
    assert_eq!(required, ["ask_key", "requests"]);
    let item = ask.to_string();
    assert!(item.contains("\"maxItems\":16"), "documents are bounded");
    let read = schema(READ_TOOL);
    assert_eq!(read["additionalProperties"], json!(false));
    assert!(read["properties"].get("version_id").is_none());
    assert_eq!(read["required"], json!(["child_request_id"]));
    assert_eq!(schema(LIST_TOOL)["additionalProperties"], json!(false));
    // Read-only hints: only the ask mutates.
    for tool in &tools {
        let encoded = serde_json::to_value(tool).unwrap();
        assert_eq!(
            encoded["annotations"]["readOnlyHint"],
            json!(tool.name != ASK_TOOL),
            "{}",
            tool.name
        );
    }
}

#[test]
fn m6_tool_listing_the_instructions_name_the_tools_only_when_listed() {
    let base = "This Bot endpoint exposes thirteen scoped tools. More.";
    assert_eq!(instructions(base, ChildTools::NONE), base);
    let all = instructions(
        base,
        ChildTools {
            ask: true,
            read: true,
        },
    );
    assert!(all.starts_with("This Bot endpoint exposes sixteen scoped tools."));
    for name in [LIST_TOOL, ASK_TOOL, READ_TOOL, "stop"] {
        assert!(all.contains(name), "{name}");
    }
    let read = instructions(
        base,
        ChildTools {
            ask: false,
            read: true,
        },
    );
    assert!(read.starts_with("This Bot endpoint exposes fourteen scoped tools."));
    assert!(!read.contains(ASK_TOOL));
}

#[test]
fn m6_tool_listing_list_askable_bots_answers_the_workers_list_or_ask_not_allowed() {
    let f = Fixture::new();
    *f.owner.askable.lock().unwrap() = Ok(vec![RunAskableBot {
        bot_id: f.recipient,
        name: "Researcher".into(),
    }]);
    assert_eq!(
        f.call(LIST_TOOL, json!({})).unwrap(),
        json!({"bots": [{"bot_id": f.recipient, "name": "Researcher"}]})
    );
    *f.owner.askable.lock().unwrap() = Err(owner_failure(codes::ASK_NOT_ALLOWED, &[]));
    assert_eq!(
        f.call(LIST_TOOL, json!({})).unwrap_err().code,
        codes::ASK_NOT_ALLOWED
    );
    // A child Run is refused before the worker; unknown fields are invalid.
    let lists = *f.owner.lists.lock().unwrap();
    let child = f.child_provenance();
    assert_eq!(
        f.call_as(&child, LIST_TOOL, json!({}), RunWait::None, OPEN)
            .unwrap_err()
            .code,
        codes::ASK_NOT_ALLOWED
    );
    assert_eq!(
        f.call(LIST_TOOL, json!({"bot_id": f.recipient}))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    assert_eq!(*f.owner.lists.lock().unwrap(), lists);
}

// ---- m6_tool_ask -----------------------------------------------------------------

#[test]
fn m6_tool_ask_one_to_three_items_are_sent_in_input_order() {
    for count in 1..=3 {
        let f = Fixture::new();
        f.port.script().last = Some(BotWaitPhase::Retired);
        let bots: Vec<BotId> = (0..count).map(|_| BotId::mint()).collect();
        let arguments = ask_arguments(&bots);
        let answer = f.call(ASK_TOOL, arguments.clone()).unwrap();
        assert_eq!(answer["status"], "child_pending");
        assert_eq!(answer["message"], ASK_MESSAGE);
        assert_eq!(answer["budget_ms"], json!(900_000));
        let returned: Vec<_> = answer["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|request| request["bot_id"].clone())
            .collect();
        assert_eq!(
            returned,
            bots.iter().map(|bot| json!(bot)).collect::<Vec<_>>()
        );
        assert!(
            answer["requests"]
                .as_array()
                .unwrap()
                .iter()
                .all(|request| request["child_request_id"].is_string())
        );
        assert_eq!(answer["requests_used"], json!(count));
        let asks = f.owner.asks.lock().unwrap();
        assert_eq!(asks.len(), 1);
        assert_eq!(json!(asks[0].0), arguments["ask_key"]);
        assert_eq!(asks[0].1, bots);
        nothing_private(&answer.to_string());
    }
}

#[test]
fn m6_tool_ask_malformed_calls_are_invalid_before_the_worker_is_asked() {
    let f = Fixture::new();
    let bot = f.recipient;
    let key = Uuid::new_v4();
    let item = |documents: usize| {
        json!({"bot_id": BotId::mint(), "task": "TASK-SENTINEL x",
            "documents": (0..documents).map(|_| DocumentId::new()).collect::<Vec<_>>()})
    };
    for arguments in [
        json!({}),
        json!({"ask_key": key, "requests": []}),
        // Four items is one too many.
        json!({"ask_key": key, "requests": [item(0), item(0), item(0), item(0)]}),
        // The same Bot twice in one call.
        json!({"ask_key": key, "requests": [
            {"bot_id": bot, "task": "TASK-SENTINEL a", "documents": []},
            {"bot_id": bot, "task": "TASK-SENTINEL b", "documents": []}]}),
        // Seventeen pages.
        json!({"ask_key": key, "requests": [item(17)]}),
        json!({"ask_key": "not-a-key", "requests": [item(0)]}),
        json!({"ask_key": key.to_string().to_uppercase(), "requests": [item(0)]}),
        json!({"ask_key": Uuid::nil(), "requests": [item(0)]}),
        json!({"ask_key": key, "requests": [item(0)], "parent": "MARK"}),
        json!({"ask_key": key, "requests": [{"bot_id": bot, "task": "", "documents": []}]}),
        json!({"ask_key": key, "requests": [{"bot_id": bot, "task": "TASK-SENTINEL", "documents": [], "version_id": "x"}]}),
    ] {
        let refused = f.call(ASK_TOOL, arguments).unwrap_err();
        assert_eq!(refused.code, "invalid_request");
        nothing_private(&json!(refused).to_string());
    }
    // Sixteen pages and three items are within the bounds.
    f.port.script().last = Some(BotWaitPhase::Retired);
    assert!(
        f.call(
            ASK_TOOL,
            json!({"ask_key": key, "requests": [item(16), item(0), item(0)]})
        )
        .is_ok()
    );
    assert_eq!(f.owner.asks.lock().unwrap().len(), 1);
}

#[test]
fn m6_tool_ask_each_refusal_is_returned_with_its_code_and_index_and_nothing_else() {
    let per_item = [
        codes::RECIPIENT_UNAVAILABLE,
        codes::RECIPIENT_PAUSED,
        codes::RECIPIENT_FULL,
        codes::RECIPIENT_FOLDER_OVERLAP,
        codes::SELF_ASK,
        codes::WOULD_WAIT_ON_ITSELF,
    ];
    let whole = [
        codes::ASK_NOT_ALLOWED,
        codes::CHILD_CAPACITY,
        codes::CHILD_BUDGET_LOW,
        codes::ASK_KEY_CONFLICT,
        "decision_in_progress",
        "invalid_request",
        "permission_denied",
    ];
    for code in per_item {
        let f = Fixture::new();
        *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(
            code,
            &[
                ("index", json!(2)),
                ("ordinal", json!(2)),
                ("secret", json!("MARK")),
            ],
        )));
        let refused = f
            .call(
                ASK_TOOL,
                ask_arguments(&[BotId::mint(), BotId::mint(), BotId::mint()]),
            )
            .unwrap_err();
        assert_eq!(refused.code, code);
        assert_eq!(refused.details, Some(json!({"index": 2})), "{code}");
        nothing_private(&json!(refused).to_string());
        // All or nothing: a refusal is answered at once, and nothing is held.
        assert_eq!(f.port.count("read_requests"), 0, "{code}");
    }
    for code in whole {
        let f = Fixture::new();
        *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(code, &[])));
        let refused = f
            .call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
            .unwrap_err();
        assert_eq!(refused.code, code);
        assert_eq!(refused.details, None, "{code}");
        assert_eq!(f.port.count("read_requests"), 0);
    }
    // `page_unavailable` names the page too.
    let f = Fixture::new();
    let page = DocumentId::new();
    *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(
        codes::PAGE_UNAVAILABLE,
        &[("index", json!(0)), ("document_id", json!(page))],
    )));
    let refused = f
        .call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
        .unwrap_err();
    assert_eq!(refused.code, codes::PAGE_UNAVAILABLE);
    assert_eq!(
        refused.details,
        Some(json!({"index": 0, "document_id": page}))
    );
}

#[test]
fn m6_tool_ask_the_limit_says_how_many_were_used_and_a_second_wait_names_its_requests() {
    let f = Fixture::new();
    let earlier = [
        f.receipt(
            0,
            ChildRequestState::Settled,
            Some(ChildSettledBy::ChildEnded),
            BotAdmissionState::Settled,
        ),
        f.receipt(
            1,
            ChildRequestState::Settled,
            Some(ChildSettledBy::TimedOut),
            BotAdmissionState::Queued,
        ),
    ];
    f.port.script().requests = earlier.to_vec();
    *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(codes::RECIPIENT_LIMIT, &[])));
    let refused = f
        .call(ASK_TOOL, ask_arguments(&[BotId::mint(), BotId::mint()]))
        .unwrap_err();
    assert_eq!(refused.code, codes::RECIPIENT_LIMIT);
    assert_eq!(refused.details, Some(json!({"used": 2})));

    // Another key while this Run's wait is open.
    let f = Fixture::new();
    let open = f.receipt(
        0,
        ChildRequestState::Open,
        None,
        BotAdmissionState::Executing,
    );
    {
        let mut s = f.port.script();
        s.requests = vec![open.clone()];
        s.last = Some(BotWaitPhase::Retired);
    }
    *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(codes::CHILD_WAIT_IN_PROGRESS, &[])));
    let refused = f
        .call_as(
            &f.provenance,
            ASK_TOOL,
            ask_arguments(&[BotId::mint()]),
            RunWait::Child,
            OPEN,
        )
        .unwrap_err();
    assert_eq!(refused.code, codes::CHILD_WAIT_IN_PROGRESS);
    assert_eq!(
        refused.details,
        Some(json!({"child_request_ids": [open.child_request]}))
    );
}

#[test]
fn m6_tool_ask_an_unknown_outcome_is_child_status_unknown_with_the_same_key() {
    for answer in [
        owner_failure(codes::CHILD_STATUS_UNKNOWN, &[]),
        // A timeout, a full queue or any code this tool does not know.
        owner_failure("ai_worker_timeout", &[]),
        owner_failure("ai_worker_busy", &[("index", json!(1))]),
    ] {
        let f = Fixture::new();
        *f.owner.answer.lock().unwrap() = Some(Err(answer));
        let refused = f
            .call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
            .unwrap_err();
        assert_eq!(refused.code, codes::CHILD_STATUS_UNKNOWN);
        assert!(refused.message.contains("same ask_key"));
        assert!(refused.message.contains("never a new key"));
    }
    // An admitted answer that is not this call's (another Bot, another
    // count) is not taken as an answer either.
    let f = Fixture::new();
    *f.owner.answer.lock().unwrap() = Some(Ok(RunChildAskPending {
        wait: f.owner.wait,
        requests: vec![(ChildRequestId::mint(), BotId::mint())],
    }));
    assert_eq!(
        f.call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
            .unwrap_err()
            .code,
        codes::CHILD_STATUS_UNKNOWN
    );
}

#[test]
fn m6_tool_ask_a_child_run_and_a_run_waiting_on_a_decision_never_reach_the_worker() {
    let f = Fixture::new();
    let child = f.child_provenance();
    assert_eq!(
        f.call_as(
            &child,
            ASK_TOOL,
            ask_arguments(&[BotId::mint()]),
            RunWait::None,
            OPEN
        )
        .unwrap_err()
        .code,
        codes::ASK_NOT_ALLOWED
    );
    let refused = f
        .call_as(
            &f.provenance,
            ASK_TOOL,
            ask_arguments(&[BotId::mint()]),
            RunWait::Decision,
            OPEN,
        )
        .unwrap_err();
    assert_eq!(refused.code, "decision_in_progress");
    assert!(f.owner.asks.lock().unwrap().is_empty());
    assert!(f.port.script().log.is_empty());
    // A Run whose authority is not exactly its own home is refused.
    let mut foreign = Fixture::new();
    foreign.captured.workspace = koru_delegate_core::WorkspaceId::new();
    assert_eq!(
        foreign
            .call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
            .unwrap_err()
            .code,
        "permission_denied"
    );
}

// ---- m6_tool_answer_held ----------------------------------------------------------

#[test]
fn m6_tool_answer_held_the_production_hold_is_ten_seconds() {
    assert_eq!(HoldPacing::PRODUCTION.budget, Duration::from_secs(10));
    assert!(HoldPacing::PRODUCTION.ceiling <= Duration::from_secs(1));
    assert_eq!(ChildGate::production().hold.budget, Duration::from_secs(10));
}

#[test]
fn m6_tool_answer_held_not_returned_while_hold_requested_and_returned_as_soon_as_it_changes() {
    let f = Fixture::new();
    {
        let mut s = f.port.script();
        s.phases = VecDeque::from([
            Some(BotWaitPhase::HoldRequested),
            Some(BotWaitPhase::HoldRequested),
            Some(BotWaitPhase::HoldRequested),
        ]);
        s.last = Some(BotWaitPhase::Retired);
    }
    let started = Instant::now();
    let answer = f.call(ASK_TOOL, ask_arguments(&[BotId::mint()])).unwrap();
    assert_eq!(answer["status"], "child_pending");
    // Three reads saw the hold; the fourth saw it left, and answered.
    assert_eq!(f.port.count("read_requests"), 4);
    assert!(started.elapsed() < QUICK.budget, "{:?}", started.elapsed());
    // A wait that is already gone (settled, or never seen) answers at once.
    let f = Fixture::new();
    f.port.script().last = None;
    f.call(ASK_TOOL, ask_arguments(&[BotId::mint()])).unwrap();
    assert_eq!(f.port.count("read_requests"), 1);
    // So does a wait that is no longer this ask's.
    let other = Fixture::new();
    other.port.script().last = Some(BotWaitPhase::HoldRequested);
    *other.owner.answer.lock().unwrap() = Some(Ok(RunChildAskPending {
        wait: BotWaitId::mint(),
        requests: vec![],
    }));
    assert_eq!(
        hold_answer(&other.port, &other.port.key, BotWaitId::mint(), QUICK),
        Some(0)
    );
    assert_eq!(other.port.count("read_requests"), 1);
}

#[test]
fn m6_tool_answer_held_returned_after_the_bound_with_the_wait_still_hold_requested() {
    let f = Fixture::new();
    f.port.script().last = Some(BotWaitPhase::HoldRequested);
    let started = Instant::now();
    let answer = f.call(ASK_TOOL, ask_arguments(&[BotId::mint()])).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(answer["status"], "child_pending");
    assert!(elapsed >= QUICK.budget, "{elapsed:?}");
    assert!(
        elapsed < QUICK.budget + Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert!(f.port.count("read_requests") > 3);
    // An owner that never answers holds the answer for the bound too, and the
    // answer is still given.
    let f = Fixture::new();
    f.port.script().lose_reads = true;
    let started = Instant::now();
    let answer = f.call(ASK_TOOL, ask_arguments(&[BotId::mint()])).unwrap();
    assert!(started.elapsed() >= QUICK.budget);
    assert_eq!(answer["requests_used"], json!(1));
}

#[test]
fn m6_tool_answer_held_a_refusal_is_returned_at_once() {
    let f = Fixture::new();
    f.port.script().last = Some(BotWaitPhase::HoldRequested);
    *f.owner.answer.lock().unwrap() = Some(Err(owner_failure(
        codes::RECIPIENT_PAUSED,
        &[("index", json!(0))],
    )));
    let started = Instant::now();
    assert_eq!(
        f.call(ASK_TOOL, ask_arguments(&[BotId::mint()]))
            .unwrap_err()
            .code,
        codes::RECIPIENT_PAUSED
    );
    assert!(started.elapsed() < QUICK.budget);
    assert_eq!(f.port.count("read_requests"), 0);
}

// ---- m6_tool_fence (this module's part) -------------------------------------------

#[test]
fn m6_tool_fence_during_a_child_wait_the_reads_pass_and_the_ask_retry_reaches_the_worker() {
    let f = Fixture::new();
    let open = f.receipt(0, ChildRequestState::Open, None, BotAdmissionState::Queued);
    {
        let mut s = f.port.script();
        s.requests = vec![open.clone()];
        s.last = Some(BotWaitPhase::Retired);
    }
    // read_bot_request is a read: it passes, and says the request is open.
    let read = f
        .call_as(
            &f.provenance,
            READ_TOOL,
            json!({"child_request_id": open.child_request}),
            RunWait::Child,
            OPEN,
        )
        .unwrap();
    assert_eq!(read["state"], "open");
    assert_eq!(read["child"], "queued");
    assert!(read.get("outcome").is_none());
    // list_askable_bots too.
    assert!(
        f.call_as(&f.provenance, LIST_TOOL, json!({}), RunWait::Child, OPEN)
            .is_ok()
    );
    // ask_bot under the same key reaches the worker, which answers it from
    // its rows; the gateway does not judge the key.
    let answer = f
        .call_as(
            &f.provenance,
            ASK_TOOL,
            ask_arguments(&[BotId::mint()]),
            RunWait::Child,
            OPEN,
        )
        .unwrap();
    assert_eq!(answer["status"], "child_pending");
    assert_eq!(f.owner.asks.lock().unwrap().len(), 1);
}

#[test]
fn m6_tool_fence_the_dispatcher_refuses_memory_writes_and_routine_mutations_by_name() {
    for name in [
        "remember_memory",
        "correct_memory",
        "forget_memory",
        "create_routine",
        "retry_routine_creation",
    ] {
        assert!(super::super::dispatch::child_fenced_here(name), "{name}");
    }
    for name in [
        "list_memory",
        "read_memory",
        "list_routines",
        "read_routine_creation",
        "search_documents",
        "read_document",
        // The document mutations settle a retained save first, in dispatch;
        // the ask and the private read answer the fence in their own modules.
        "create_document",
        "edit_document",
        ASK_TOOL,
        READ_TOOL,
        LIST_TOOL,
        "read_connection_item",
    ] {
        assert!(!super::super::dispatch::child_fenced_here(name), "{name}");
    }
    // The fence's answer names the open wait's requests, and nothing private.
    let f = Fixture::new();
    let first = f.receipt(
        0,
        ChildRequestState::Open,
        None,
        BotAdmissionState::Executing,
    );
    let second = f.receipt(
        1,
        ChildRequestState::Settled,
        Some(ChildSettledBy::ChildEnded),
        BotAdmissionState::Settled,
    );
    {
        let mut s = f.port.script();
        s.requests = vec![first.clone(), second.clone()];
        s.last = Some(BotWaitPhase::Retired);
    }
    let operation = Uuid::new_v4();
    let refused = in_progress(&f.port, &f.provenance, &f.record, Some(operation));
    assert_eq!(refused.code, codes::CHILD_WAIT_IN_PROGRESS);
    assert_eq!(
        refused.details,
        Some(json!({
            "operation_id": operation,
            "child_request_ids": [first.child_request, second.child_request],
        }))
    );
    // An owner that cannot say still refuses, with what it knows.
    f.port.script().lose_reads = true;
    let refused = in_progress(&f.port, &f.provenance, &f.record, None);
    assert_eq!(refused.code, codes::CHILD_WAIT_IN_PROGRESS);
    assert_eq!(refused.details, None);
}

// ---- m6_tool_result ---------------------------------------------------------------

#[test]
fn m6_tool_result_a_completed_request_lists_its_pages_and_reads_the_pinned_version() {
    let f = Fixture::new();
    let (document, version) = f.save_page();
    let receipt = f.completed(vec![f.entry(document, Some(version), 10)]);
    let listing = f
        .call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request}),
        )
        .unwrap();
    assert_eq!(
        listing,
        json!({
            "child_request_id": receipt.child_request,
            "bot_id": f.recipient,
            "state": "settled",
            "child": "finished",
            "outcome": "completed",
            "pages": [{
                "document_id": document,
                "title": TITLE,
                "kind": "created_page",
                "availability": "pinned",
            }],
            "pages_more": false,
            "no_page_saved": false,
        })
    );
    nothing_private(&listing.to_string());

    // A person edits the page; the pinned version is what is read.
    f.person_edits(document);
    let page = f
        .call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request, "document_id": document}),
        )
        .unwrap();
    assert_eq!(page["child_request_id"], json!(receipt.child_request));
    assert_eq!(page["document_id"], json!(document));
    assert_eq!(page["version_id"], json!(version));
    assert_eq!(page["title"], json!(TITLE));
    assert_eq!(page["truncated"], json!(false));
    let content = page["content"].to_string();
    assert!(content.contains(PINNED));
    assert!(!content.contains(LIVE), "the live page is never returned");
    // The parent's own authority was asked, once, for that page.
    assert_eq!(*f.owner.policies.lock().unwrap(), 1);
}

#[test]
fn m6_tool_result_only_a_listed_page_is_read_and_the_model_never_supplies_a_version() {
    let f = Fixture::new();
    let (document, version) = f.save_page();
    let (other, _) = f.save_page();
    let receipt = f.completed(vec![f.entry(document, Some(version), 10)]);
    // A page of the workspace the request did not save.
    assert_eq!(
        f.call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request, "document_id": other}),
        )
        .unwrap_err()
        .code,
        codes::REQUEST_NOT_FOUND
    );
    // Another request id.
    assert_eq!(
        f.call(
            READ_TOOL,
            json!({"child_request_id": ChildRequestId::mint()})
        )
        .unwrap_err()
        .code,
        codes::REQUEST_NOT_FOUND
    );
    // A version id from the model is not an argument.
    assert_eq!(
        f.call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request, "document_id": document,
                "version_id": version}),
        )
        .unwrap_err()
        .code,
        "invalid_request"
    );
    assert_eq!(*f.owner.policies.lock().unwrap(), 0);
}

#[test]
fn m6_tool_result_each_unavailable_reason_and_never_the_live_page() {
    let f = Fixture::new();
    let (pinned, version) = f.save_page();
    let (unpinned, _) = f.save_page();
    let (missing, _) = f.save_page();
    let (trashed, trashed_version) = f.save_page();
    let receipt = f.completed(vec![
        f.entry(pinned, Some(version), 10),
        f.entry(unpinned, None, 11),
        f.entry(missing, Some(DocumentVersionId::new()), 12),
        f.entry(trashed, Some(trashed_version), 13),
    ]);
    let listing = f
        .call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request}),
        )
        .unwrap();
    let availability: Vec<_> = listing["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| page["availability"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(availability, ["pinned", "not_pinned", "pinned", "pinned"]);
    let read = |document: DocumentId| {
        f.call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request, "document_id": document}),
        )
    };
    let refused = read(unpinned).unwrap_err();
    assert_eq!(refused.code, codes::RESULT_VERSION_UNAVAILABLE);
    assert_eq!(
        refused.details,
        Some(json!({"document_id": unpinned, "reason": "not_pinned"}))
    );
    let refused = read(missing).unwrap_err();
    assert_eq!(refused.code, codes::RESULT_VERSION_UNAVAILABLE);
    assert_eq!(
        refused.details,
        Some(json!({"document_id": missing, "reason": "missing"}))
    );
    let workspace = f.record.workspace;
    f.memory
        .with_store(move |store| {
            store
                .trash_document_entry(workspace, trashed, Timestamp::now(), None)
                .unwrap();
        })
        .unwrap();
    let refused = read(trashed).unwrap_err();
    assert_eq!(refused.code, codes::RESULT_NOT_READABLE);
    assert_eq!(refused.details, Some(json!({"document_id": trashed})));
    for answer in [read(unpinned), read(missing), read(trashed)] {
        nothing_private(&json!(answer.unwrap_err()).to_string());
    }
    // The corrupt and too-large reasons come from the same read (piece V's
    // cohorts produce them on a real Store); every reason maps to its token
    // here, and an outage is never a missing version.
    use koru_ai_server::PinnedUnavailable as U;
    for (reason, token) in [
        (U::Missing, "missing"),
        (U::Corrupt, "corrupt"),
        (U::NotPinned, "not_pinned"),
        (U::TooLarge, "too_large"),
    ] {
        let refused = pinned_refusal(PinnedReadError::Unavailable(reason), pinned);
        assert_eq!(refused.code, codes::RESULT_VERSION_UNAVAILABLE);
        assert_eq!(
            refused.details,
            Some(json!({"document_id": pinned, "reason": token}))
        );
    }
    assert_eq!(
        pinned_refusal(PinnedReadError::StoreUnavailable, pinned).code,
        "native_owner_unavailable"
    );
    // A page saved into another workspace is not this Bot's to read.
    let elsewhere = Fixture::new();
    let mut entry = elsewhere.entry(DocumentId::new(), Some(DocumentVersionId::new()), 1);
    entry.workspace = koru_delegate_core::WorkspaceId::new();
    let document = entry.document;
    let receipt = elsewhere.completed(vec![entry]);
    assert_eq!(
        elsewhere
            .call(
                READ_TOOL,
                json!({"child_request_id": receipt.child_request, "document_id": document}),
            )
            .unwrap_err()
            .code,
        codes::RESULT_NOT_READABLE
    );
}

#[test]
fn m6_tool_result_only_a_completed_request_hands_anything_over() {
    let cases = [
        (
            ChildRequestState::Open,
            None,
            BotAdmissionState::Executing,
            None,
            None,
        ),
        (
            ChildRequestState::Settled,
            Some(ChildSettledBy::TimedOut),
            BotAdmissionState::Executing,
            None,
            Some("timed_out"),
        ),
        (
            ChildRequestState::Settled,
            Some(ChildSettledBy::ParentStopped),
            BotAdmissionState::Queued,
            None,
            Some("parent_stopped"),
        ),
        (
            ChildRequestState::Settled,
            Some(ChildSettledBy::ChildEnded),
            BotAdmissionState::Refused,
            None,
            Some("failed"),
        ),
        (
            ChildRequestState::Settled,
            Some(ChildSettledBy::ChildEnded),
            BotAdmissionState::Settled,
            Some(RunStatus::Cancelled),
            Some("canceled"),
        ),
        (
            ChildRequestState::Settled,
            Some(ChildSettledBy::ChildEnded),
            BotAdmissionState::Settled,
            Some(RunStatus::Failed {
                stage: koru_delegate_core::FailureStage::ProviderStart,
            }),
            Some("failed"),
        ),
    ];
    for (state, settled_by, child, status, outcome) in cases {
        let f = Fixture::new();
        let (document, version) = f.save_page();
        let receipt = f.receipt(0, state, settled_by, child);
        {
            let mut s = f.port.script();
            s.requests = vec![receipt.clone()];
            s.header = status.map(|status| {
                f.header(&receipt, status, vec![f.entry(document, Some(version), 1)])
            });
        }
        let listing = f
            .call(
                READ_TOOL,
                json!({"child_request_id": receipt.child_request}),
            )
            .unwrap();
        assert_eq!(listing.get("outcome").and_then(Value::as_str), outcome);
        assert_eq!(listing["pages"], json!([]), "{outcome:?}");
        assert_eq!(listing["no_page_saved"], json!(false));
        let refused = f
            .call(
                READ_TOOL,
                json!({"child_request_id": receipt.child_request, "document_id": document}),
            )
            .unwrap_err();
        assert_eq!(refused.code, codes::RESULT_NOT_READY, "{outcome:?}");
        assert_eq!(*f.owner.policies.lock().unwrap(), 0);
    }
    // A child that ended while its header is not terminal yet: try again.
    let f = Fixture::new();
    let receipt = f.receipt(
        0,
        ChildRequestState::Settled,
        Some(ChildSettledBy::ChildEnded),
        BotAdmissionState::Settled,
    );
    {
        let mut s = f.port.script();
        s.requests = vec![receipt.clone()];
        s.header = Some(f.header(&receipt, RunStatus::Starting, Vec::new()));
    }
    assert_eq!(
        f.call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request})
        )
        .unwrap_err()
        .code,
        "native_owner_unavailable"
    );
    // A completed request that saved nothing says so, and only that.
    let f = Fixture::new();
    let receipt = f.completed(Vec::new());
    let listing = f
        .call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request}),
        )
        .unwrap();
    assert_eq!(listing["outcome"], "completed");
    assert_eq!(listing["no_page_saved"], json!(true));
    assert_eq!(listing["pages"], json!([]));
}

#[test]
fn m6_tool_result_one_entry_per_page_the_latest_save_and_at_most_twenty() {
    let f = Fixture::new();
    let page = DocumentId::new();
    let (early, late) = (DocumentVersionId::new(), DocumentVersionId::new());
    let mut entries = vec![f.entry(page, Some(early), 5), f.entry(page, Some(late), 9)];
    for n in 0..21 {
        entries.push(f.entry(DocumentId::new(), None, 100 + n));
    }
    let header = f.header(
        &f.receipt(
            0,
            ChildRequestState::Settled,
            None,
            BotAdmissionState::Settled,
        ),
        RunStatus::Completed,
        entries,
    );
    let pages = result_pages(&header);
    assert_eq!(pages.len(), 22);
    assert_eq!(pages[0].document, page);
    assert_eq!(pages[0].version_id, Some(late));
    let receipt = f.completed(header.native_results.clone());
    let listing = f
        .call(
            READ_TOOL,
            json!({"child_request_id": receipt.child_request}),
        )
        .unwrap();
    assert_eq!(
        listing["pages"].as_array().unwrap().len(),
        RESULT_PAGE_LIMIT
    );
    assert_eq!(listing["pages_more"], json!(true));
}

#[test]
fn m6_tool_result_a_long_page_is_cut_at_a_whole_node_and_says_so() {
    let content = json!({
        "document_id": "d",
        "nodes": (0..50).map(|n| json!({"id": n, "text": "x".repeat(100)})).collect::<Vec<_>>(),
    });
    let whole = serde_json::to_vec(&content).unwrap().len();
    assert_eq!(bounded(content.clone(), whole), (content.clone(), false));
    let (cut, truncated) = bounded(content, whole / 2);
    assert!(truncated);
    assert!(serde_json::to_vec(&cut).unwrap().len() <= whole / 2);
    let kept = cut["nodes"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() < 50);
    assert_eq!(kept[0]["id"], json!(0), "a prefix, in document order");
    assert_eq!(cut["document_id"], json!("d"));
    assert_eq!(CONTENT_BYTES, 1024 * 1024);
}
