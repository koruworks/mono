// SPDX-License-Identifier: MPL-2.0

//! Piece A cohorts of M6: `m6_bytes_unchanged`, `m6_child_binding`,
//! `m6_child_enrollment`, `m6_child_waiting_target`, `m6_provenance_parent`.
//! Every golden is an authored literal, never produced by the serializer
//! under test.
use super::*;
use crate::decisions::*;
use crate::*;
use std::collections::BTreeSet;

fn uuid(n: u32) -> Uuid {
    format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
}
fn id<T: std::str::FromStr>(n: u32) -> T {
    uuid(n).to_string().parse().ok().unwrap()
}
fn owner() -> BotOwner {
    BotOwner {
        user_scope: koru_store_core::UserScopeId::from_uuid(uuid(1)),
        device: DeviceId::from_uuid(uuid(2)),
    }
}
fn reference(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(domain);
    hash.update(bytes);
    *hash.finalize().as_bytes()
}
fn hex(bytes: [u8; 32]) -> String {
    String::from(DecisionDigest::from_bytes(bytes))
}
fn policy() -> CapturedBotPolicy {
    let workspace = WorkspaceId::from_uuid(uuid(10));
    CapturedBotPolicy {
        workspace,
        access: BotAccess::Default,
        selected_pages: BTreeSet::new(),
        destination: workspace,
        connections: BTreeSet::new(),
    }
}
fn provenance() -> BotProvenance {
    BotProvenance {
        parent: None,
        owner: owner(),
        bot: id(3),
        submission: id(8),
        chat_id: uuid(20),
        ai_attempt_id: None,
        assistant_message_id: None,
        config_revision: 2,
        policy_revision: 3,
        continued_from: None,
        routine: None,
    }
}
fn parent() -> ChildParent {
    ChildParent {
        run: id(4),
        bot: id(13),
        chat_id: uuid(21),
        child_request: id(15),
    }
}
fn result() -> NativeResultReference {
    NativeResultReference {
        version_id: None,
        operation_id: uuid(12),
        workspace: WorkspaceId::from_uuid(uuid(10)),
        document: DocumentId::from_uuid(uuid(11)),
        version: vec![1, 2],
        title: "Result".into(),
        confirmed_at_ms: 7,
        kind: NativeResultKind::CreatedPage,
    }
}
fn routine_provenance() -> RoutineProvenance {
    RoutineProvenance {
        schedule: ScheduleId::from_uuid(uuid(70)),
        occurrence_ms: 60_000,
        definition: RoutineDefinitionIdentity {
            revision: 1,
            digest: DecisionDigest::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    }
}
fn fence() -> DecisionFence {
    DecisionFence {
        owner: owner(),
        bot: id(3),
        run: id(4),
        attempt: id(5),
        generation: AttemptGeneration::FIRST,
        session: id(6),
        turn: id(7),
    }
}
fn binding() -> ChildRequestBindingV1 {
    ChildRequestBindingV1 {
        version: DecisionVersionV1,
        owner: owner(),
        parent_bot: id(3),
        parent_run: id(4),
        wait: id(5),
        ask_key: uuid(6),
        ordinal: 0,
        parent_submission: INTERACTIVE_BOT_SUBMISSION_VERSION,
        parent_routine: None,
        human: ProfileId::from_uuid(uuid(9)),
        recipient_bot: id(13),
        child_run: id(14),
        child_request: id(15),
        workspace: WorkspaceId::from_uuid(uuid(10)),
        task_digest: DecisionDigest::from_bytes([6; 32]),
        documents: vec![
            DocumentId::from_uuid(uuid(11)),
            DocumentId::from_uuid(uuid(12)),
        ],
        parent_policy: ChildPolicyRef {
            config_revision: 2,
            policy_revision: 3,
            digest: DecisionDigest::from_bytes([7; 32]),
        },
        child_policy: ChildPolicyRef {
            config_revision: 4,
            policy_revision: 5,
            digest: DecisionDigest::from_bytes([8; 32]),
        },
    }
}
fn intent() -> BotChildWaitCleanupIntentV1 {
    BotChildWaitCleanupIntentV1::frozen(id(14), &fence()).unwrap()
}

// ---- m6_bytes_unchanged ----------------------------------------------------

// What these values serialized to before M6 added `parent` and `version_id`.
const PRE_M6_PROVENANCE: &str = r#"{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","submission":"00000000-0000-4000-8000-000000000008","chat_id":"00000000-0000-4000-8000-000000000014","ai_attempt_id":null,"assistant_message_id":null,"config_revision":2,"policy_revision":3,"continued_from":null}"#;
const PRE_M6_RESULT: &str = r#"{"operation_id":"00000000-0000-4000-8000-00000000000c","workspace":"00000000-0000-4000-8000-00000000000a","document":"00000000-0000-4000-8000-00000000000b","version":[1,2],"title":"Result","confirmed_at_ms":7,"kind":"created_page"}"#;
const PARENT_PROVENANCE: &str = r#"{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","submission":"00000000-0000-4000-8000-000000000008","chat_id":"00000000-0000-4000-8000-000000000014","ai_attempt_id":null,"assistant_message_id":null,"config_revision":2,"policy_revision":3,"continued_from":null,"parent":{"run":"00000000-0000-4000-8000-000000000004","bot":"00000000-0000-4000-8000-00000000000d","chat_id":"00000000-0000-4000-8000-000000000015","child_request":"00000000-0000-4000-8000-00000000000f"}}"#;
const PINNED_RESULT: &str = r#"{"operation_id":"00000000-0000-4000-8000-00000000000c","workspace":"00000000-0000-4000-8000-00000000000a","document":"00000000-0000-4000-8000-00000000000b","version":[1,2],"title":"Result","confirmed_at_ms":7,"kind":"created_page","version_id":"00000000-0000-4000-8000-000000000010"}"#;

#[test]
fn m6_bytes_unchanged_for_a_provenance_without_a_parent() {
    let encoded = serde_json::to_string(&provenance()).unwrap();
    assert_eq!(encoded, PRE_M6_PROVENANCE);
    // An encoding written before the field existed still decodes, to the
    // same value.
    assert_eq!(
        serde_json::from_str::<BotProvenance>(PRE_M6_PROVENANCE).unwrap(),
        provenance()
    );
    // A null is not the canonical absent form and is never written, but a
    // reader accepts it as absent.
    let mut raw: serde_json::Value = serde_json::from_str(PRE_M6_PROVENANCE).unwrap();
    raw["parent"] = serde_json::Value::Null;
    assert_eq!(
        serde_json::from_value::<BotProvenance>(raw).unwrap(),
        provenance()
    );
}

#[test]
fn m6_bytes_unchanged_for_a_result_reference_without_a_version_id() {
    let encoded = serde_json::to_string(&result()).unwrap();
    assert_eq!(encoded, PRE_M6_RESULT);
    assert_eq!(
        serde_json::from_str::<NativeResultReference>(PRE_M6_RESULT).unwrap(),
        result()
    );
    let mut pinned = result();
    pinned.version_id = Some(DocumentVersionId::from_uuid(uuid(16)));
    assert_eq!(serde_json::to_string(&pinned).unwrap(), PINNED_RESULT);
    assert_eq!(
        serde_json::from_str::<NativeResultReference>(PINNED_RESULT).unwrap(),
        pinned
    );
    assert!(pinned.valid());
    // The Debug of a reference names neither its title nor its version.
    let debug = format!("{pinned:?}");
    assert!(!debug.contains("title") && !debug.contains("version"));
}

#[test]
fn m6_bytes_unchanged_for_the_wait_vocabulary_and_the_decision_intent() {
    // Tokens that are persisted: every existing one keeps its spelling.
    for (reason, token) in [
        (BotWaitReason::Capacity {}, r#"{"reason":"capacity"}"#),
        (
            BotWaitReason::NativeExpired {},
            r#"{"reason":"native_expired"}"#,
        ),
        (
            BotWaitReason::OverlappingPrompt {},
            r#"{"reason":"overlapping_prompt"}"#,
        ),
        (BotWaitReason::Child {}, r#"{"reason":"child"}"#),
    ] {
        assert_eq!(serde_json::to_string(&reason).unwrap(), token);
        assert_eq!(
            serde_json::from_str::<BotWaitReason>(token).unwrap(),
            reason
        );
    }
    assert_eq!(
        serde_json::to_string(&BotWaitCleanupCause::DecisionWaitCleanup).unwrap(),
        r#""decision_wait_cleanup""#
    );
    assert_eq!(
        serde_json::to_string(&BotWaitCleanupCause::ChildWaitCleanup).unwrap(),
        r#""child_wait_cleanup""#
    );
    // A decision's intent cannot carry the child cause, and the reverse.
    let child = intent();
    let mut raw = serde_json::to_value(&child).unwrap();
    raw["cause"] = "decision_wait_cleanup".into();
    let wrong: BotChildWaitCleanupIntentV1 = serde_json::from_value(raw).unwrap();
    assert_eq!(wrong.validate(), Err(DecisionError::Invalid));
    let decision = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: owner(),
        bot: id(3),
        run: id(4),
        wait_id: id(14),
        decision_id: id(15),
        decision_revision: DecisionRevision::new(2).unwrap(),
        wait_revision: DecisionRevision::new(3).unwrap(),
        origin_attempt: id(5),
        origin_generation: AttemptGeneration::FIRST,
        origin_session: id(6),
        origin_turn: id(7),
        cause: BotWaitCleanupCause::ChildWaitCleanup,
    };
    assert_eq!(decision.validate(), Err(DecisionError::Invalid));
    assert!(decision.digest().is_err());
}

#[test]
fn m6_bytes_unchanged_for_a_submission_and_a_policy_without_m6_fields() {
    // A policy has no M6 field at all; a submission's only one is the
    // provenance's parent, which is absent here.
    let policy_bytes = serde_json::to_string(&policy()).unwrap();
    assert_eq!(
        policy_bytes,
        r#"{"workspace":"00000000-0000-4000-8000-00000000000a","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000a"}"#
    );
    let value = serde_json::to_value(provenance()).unwrap();
    assert!(value.get("parent").is_none() && value.get("routine").is_none());
}

// ---- m6_provenance_parent --------------------------------------------------

#[test]
fn m6_provenance_parent_encodes_only_when_present_and_roundtrips() {
    let mut value = provenance();
    assert!(value.valid());
    value.parent = Some(parent());
    assert!(value.valid());
    let encoded = serde_json::to_string(&value).unwrap();
    assert_eq!(encoded, PARENT_PROVENANCE);
    assert_eq!(
        serde_json::from_str::<BotProvenance>(&encoded).unwrap(),
        value
    );
    // Unknown fields inside the parent are refused.
    let mut raw = serde_json::to_value(&value).unwrap();
    raw["parent"]["authority"] = true.into();
    assert!(serde_json::from_value::<BotProvenance>(raw).is_err());
}

#[test]
fn m6_provenance_parent_excludes_a_routine_a_continuation_and_itself() {
    let mut continued = provenance();
    continued.parent = Some(parent());
    continued.continued_from = Some(id(30));
    assert!(!continued.valid());

    let mut routine: BotProvenance = provenance();
    routine.ai_attempt_id = Some(uuid(31));
    routine.assistant_message_id = Some(uuid(32));
    routine.routine = Some(routine_provenance());
    assert!(routine.valid(), "the Routine fixture is valid by itself");
    routine.parent = Some(parent());
    assert!(!routine.valid());

    // A Bot is never its own parent.
    let mut own = provenance();
    own.parent = Some(ChildParent {
        bot: own.bot,
        ..parent()
    });
    assert!(!own.valid());

    // The chat is an existing AI identity: a nil id is refused.
    let mut nil = provenance();
    nil.parent = Some(ChildParent {
        chat_id: Uuid::nil(),
        ..parent()
    });
    assert!(!nil.valid());
}

// ---- m6_child_binding ------------------------------------------------------

const CHILD_BINDING: &str = r#"{"version":1,"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"parent_bot":"00000000-0000-4000-8000-000000000003","parent_run":"00000000-0000-4000-8000-000000000004","wait":"00000000-0000-4000-8000-000000000005","ask_key":"00000000-0000-4000-8000-000000000006","ordinal":0,"parent_submission":1,"parent_routine":null,"human":"00000000-0000-4000-8000-000000000009","recipient_bot":"00000000-0000-4000-8000-00000000000d","child_run":"00000000-0000-4000-8000-00000000000e","child_request":"00000000-0000-4000-8000-00000000000f","workspace":"00000000-0000-4000-8000-00000000000a","task_digest":"0606060606060606060606060606060606060606060606060606060606060606","documents":["00000000-0000-4000-8000-00000000000b","00000000-0000-4000-8000-00000000000c"],"parent_policy":{"config_revision":2,"policy_revision":3,"digest":"0707070707070707070707070707070707070707070707070707070707070707"},"child_policy":{"config_revision":4,"policy_revision":5,"digest":"0808080808080808080808080808080808080808080808080808080808080808"}}"#;

#[test]
fn m6_child_binding_golden_bytes_and_domain_digest() {
    let value = binding();
    let expected = CHILD_BINDING.as_bytes();
    assert_eq!(value.canonical_bytes().unwrap(), expected);
    let digest = reference(b"koru.bot.child.request.v1", expected);
    assert_eq!(*value.digest().unwrap().as_bytes(), digest);
    // The domain is its own: the same bytes under a decision binding's
    // domain are a different digest.
    assert_ne!(digest, reference(b"koru.bot.decision.binding.v1", expected));
    assert_eq!(
        hex(digest),
        "c93d130d555033ed67eb5397a28ef7f2c4d7792c1256cfbb109bf9de66759bc3"
    );
    assert_eq!(ChildRequestBindingV1::decode(expected).unwrap(), value);
}

#[test]
fn m6_child_binding_decodes_strictly() {
    let raw: serde_json::Value = serde_json::from_str(CHILD_BINDING).unwrap();
    for field in [
        "version",
        "owner",
        "parent_bot",
        "parent_run",
        "wait",
        "ask_key",
        "ordinal",
        "parent_submission",
        "human",
        "recipient_bot",
        "child_run",
        "child_request",
        "workspace",
        "task_digest",
        "documents",
        "parent_policy",
        "child_policy",
    ] {
        let mut missing = raw.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            ChildRequestBindingV1::decode(missing.to_string().as_bytes()).is_err(),
            "{field} is required"
        );
    }
    let mut extra = raw.clone();
    extra["task"] = "text".into();
    assert!(ChildRequestBindingV1::decode(extra.to_string().as_bytes()).is_err());
    let mut version = raw.clone();
    version["version"] = 2.into();
    assert!(ChildRequestBindingV1::decode(version.to_string().as_bytes()).is_err());
    let mut ordinal = raw.clone();
    ordinal["ordinal"] = 3.into();
    assert!(ChildRequestBindingV1::decode(ordinal.to_string().as_bytes()).is_err());
    let mut nil = raw.clone();
    nil["ask_key"] = Uuid::nil().to_string().into();
    assert!(ChildRequestBindingV1::decode(nil.to_string().as_bytes()).is_err());
    // Too many bytes are refused before they are parsed.
    let oversized = vec![b' '; CHILD_BINDING_BYTES + 1];
    assert_eq!(
        ChildRequestBindingV1::decode(&oversized),
        Err(DecisionError::TooLarge)
    );
}

#[test]
fn m6_child_binding_bounds_and_orders_its_documents_and_subject() {
    let document = |n: u32| DocumentId::from_uuid(uuid(100 + n));
    let mut sixteen = binding();
    sixteen.documents = (0..16).map(document).collect();
    assert!(sixteen.validate().is_ok());
    let mut seventeen = binding();
    seventeen.documents = (0..17).map(document).collect();
    assert_eq!(seventeen.validate(), Err(DecisionError::Invalid));
    assert!(seventeen.digest().is_err() && seventeen.canonical_bytes().is_err());

    let mut unsorted = binding();
    unsorted.documents.reverse();
    assert_eq!(unsorted.validate(), Err(DecisionError::Invalid));
    let mut twice = binding();
    twice.documents = vec![document(1), document(1)];
    assert_eq!(twice.validate(), Err(DecisionError::Invalid));
    let mut none = binding();
    none.documents.clear();
    assert!(none.validate().is_ok());

    let mut own_bot = binding();
    own_bot.recipient_bot = own_bot.parent_bot;
    assert_eq!(own_bot.validate(), Err(DecisionError::Invalid));
    let mut own_run = binding();
    own_run.child_run = own_run.parent_run;
    assert_eq!(own_run.validate(), Err(DecisionError::Invalid));
    // The submission kind and the Routine identity go together.
    let mut routine_without = binding();
    routine_without.parent_submission = ROUTINE_BOT_SUBMISSION_VERSION;
    assert_eq!(routine_without.validate(), Err(DecisionError::Invalid));
    let mut routine = binding();
    routine.parent_submission = ROUTINE_BOT_SUBMISSION_VERSION;
    routine.parent_routine = Some(routine_provenance());
    assert!(routine.validate().is_ok());
    let mut interactive_with = binding();
    interactive_with.parent_routine = Some(routine_provenance());
    assert_eq!(interactive_with.validate(), Err(DecisionError::Invalid));
    let mut zero = binding();
    zero.child_policy.policy_revision = 0;
    assert_eq!(zero.validate(), Err(DecisionError::Invalid));
    // Every field is in the digest.
    let original = binding().digest().unwrap();
    let mut other = binding();
    other.task_digest = DecisionDigest::from_bytes([9; 32]);
    assert_ne!(other.digest().unwrap(), original);
    let mut other = binding();
    other.child_policy.digest = DecisionDigest::from_bytes([9; 32]);
    assert_ne!(other.digest().unwrap(), original);
}

#[test]
fn m6_child_binding_matches_only_its_own_child_submission() {
    let (request, item) = ask();
    assert!(request.validate().is_ok());
    assert!(item.binding.matches_submission(&item.submission));
    // The Debug of an ask, an item and a binding names no task text.
    let rendered = format!("{request:?}{item:?}{:?}", item.binding);
    assert!(!rendered.contains("TASK-SENTINEL"));

    // The policy the binding names is the one submitted.
    let mut wider = item.clone();
    wider
        .submission
        .policy
        .connections
        .insert(ConnectionGrantRef {
            connection: ConnectionId::from_uuid(uuid(40)),
            operation: PrivateOperationKind::ReadItem,
        });
    assert!(!wider.binding.matches_submission(&wider.submission));
    // The person is the parent's, never a Bot or another person.
    let mut person = item.clone();
    person.submission.request.requested_by = ActorRef::Person {
        profile: ProfileId::from_uuid(uuid(77)),
    };
    assert!(!person.binding.matches_submission(&person.submission));
    // The submission must say who asked, with this request's id.
    let mut orphan = item.clone();
    orphan.submission.provenance.parent = None;
    assert!(!orphan.binding.matches_submission(&orphan.submission));
    let mut other = item.clone();
    other.submission.provenance.parent = Some(ChildParent {
        child_request: id(99),
        ..other.submission.provenance.parent.unwrap()
    });
    assert!(!other.binding.matches_submission(&other.submission));
    // The task is bound by digest.
    let mut task = item.clone();
    task.binding.task_digest = child_task_digest("another task").unwrap();
    assert!(!task.binding.matches_submission(&task.submission));

    // An ask is one to three items, in order, of one wait and one person.
    let mut empty = request.clone();
    empty.requests.clear();
    assert_eq!(empty.validate(), Err(DecisionError::Invalid));
    let mut turnless = request.clone();
    turnless.parent.turn = None;
    assert_eq!(turnless.validate(), Err(DecisionError::Invalid));
    let mut human = request.clone();
    human.human = ProfileId::from_uuid(uuid(78));
    assert_eq!(human.validate(), Err(DecisionError::Invalid));
    let mut twice = request.clone();
    twice.requests.push(item.clone());
    assert_eq!(twice.validate(), Err(DecisionError::Invalid));
}

/// One valid ask of one item, built the way the AI owner builds it.
fn ask() -> (ChildRequest, ChildRequestItem) {
    let workspace = WorkspaceId::from_uuid(uuid(10));
    let human = ProfileId::from_uuid(uuid(9));
    let mut request = crate::tests::request();
    request.run = id(14);
    request.requested_by = ActorRef::Person { profile: human };
    request.objective = Objective::new("TASK-SENTINEL").unwrap();
    request.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("/recipient".into()).unwrap(),
    };
    request.grants = BTreeSet::from([workspace]);
    request.origin = RunOrigin::Manual;
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::from_uuid(uuid(1)),
        device: request.executing_device,
    };
    let submission = BotSubmission {
        provenance: BotProvenance {
            parent: Some(ChildParent {
                run: id(4),
                bot: id(3),
                chat_id: uuid(21),
                child_request: id(15),
            }),
            owner,
            bot: id(13),
            submission: id(15),
            chat_id: uuid(22),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 4,
            policy_revision: 5,
            continued_from: None,
            routine: None,
        },
        policy: policy(),
        request,
        workspace,
        input: koru_session_protocol::PendingInput {
            idempotency_key: IdempotencyKey::mint(),
            text: "TASK-SENTINEL".into(),
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
    let mut binding = binding();
    binding.owner = owner;
    binding.task_digest = child_task_digest("TASK-SENTINEL").unwrap();
    binding.child_policy = ChildPolicyRef::of(4, 5, &submission.policy).unwrap();
    let item = ChildRequestItem {
        child_request: binding.child_request,
        binding,
        submission,
    };
    let request = ChildRequest {
        parent: BotExecutionFence {
            owner,
            bot: id(3),
            run: id(4),
            attempt: id(5),
            generation: AttemptGeneration::FIRST,
            session: id(6),
            turn: Some(id(7)),
        },
        ask_key: uuid(6),
        human,
        requests: vec![item.clone()],
    };
    (request, item)
}

#[test]
fn m6_child_binding_refusals_have_final_bot_codes() {
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
    let codes: BTreeSet<_> = all.iter().map(|refusal| refusal.code()).collect();
    assert_eq!(codes.len(), all.len());
    assert!(codes.iter().all(|code| code.starts_with("bot_child_")));
    assert_eq!(
        all.iter()
            .filter(|refusal| refusal.retryable())
            .collect::<Vec<_>>(),
        [&ChildRefusal::WaitClosing]
    );
}

// ---- m6_child_enrollment ---------------------------------------------------

const CHILD_INTENT: &str = r#"{"version":1,"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","run":"00000000-0000-4000-8000-000000000004","wait_id":"00000000-0000-4000-8000-00000000000e","wait_revision":1,"origin_attempt":"00000000-0000-4000-8000-000000000005","origin_generation":1,"origin_session":"00000000-0000-4000-8000-000000000006","origin_turn":"00000000-0000-4000-8000-000000000007","cause":"child_wait_cleanup"}"#;

#[test]
fn m6_child_enrollment_intent_golden_bytes_and_domain_digest() {
    let value = intent();
    let expected = CHILD_INTENT.as_bytes();
    assert_eq!(value.canonical_bytes().unwrap(), expected);
    let digest = reference(b"koru.bot.child.wait-enrollment.v1", expected);
    assert_eq!(*value.digest().unwrap().as_bytes(), digest);
    // Not the decision enrollment's domain.
    assert_ne!(
        digest,
        reference(b"koru.bot.decision.wait-enrollment.v1", expected)
    );
    assert_eq!(
        hex(digest),
        "61e578922acc0ce09aa685599c59cb7a2c387636bc5ea70f91ba89142500eebb"
    );
    assert_eq!(value.origin(), fence());
}

const CHILD_KEY_PREFIX: &str = r#"{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","run":"00000000-0000-4000-8000-000000000004","wait_id":"00000000-0000-4000-8000-00000000000e","wait_revision":1,"origin_attempt":"00000000-0000-4000-8000-000000000005","origin_generation":1,"origin_session":"00000000-0000-4000-8000-000000000006","origin_turn":"00000000-0000-4000-8000-000000000007","receipt_digest":""#;

#[test]
fn m6_child_enrollment_key_is_the_intent_without_its_cause_plus_the_digest() {
    let value = intent();
    let key = value.key().unwrap();
    let digest = hex(reference(
        b"koru.bot.child.wait-enrollment.v1",
        CHILD_INTENT.as_bytes(),
    ));
    assert_eq!(
        serde_json::to_string(&key).unwrap(),
        format!("{CHILD_KEY_PREFIX}{digest}\"}}")
    );
    assert_eq!(key.origin(), fence());
    // The digest binds the frozen revision and the origin.
    let mut later = value.clone();
    later.wait_revision = DecisionRevision::new(2).unwrap();
    assert_ne!(later.digest().unwrap(), value.digest().unwrap());
    let mut turn = value.clone();
    turn.origin_turn = id(99);
    assert_ne!(turn.digest().unwrap(), value.digest().unwrap());
    // Strict: a decision id has no place in a child intent or key.
    let mut raw = serde_json::to_value(&value).unwrap();
    raw["decision_id"] = uuid(15).to_string().into();
    assert!(serde_json::from_value::<BotChildWaitCleanupIntentV1>(raw).is_err());
    let mut raw = serde_json::to_value(&key).unwrap();
    raw["decision_id"] = uuid(15).to_string().into();
    assert!(serde_json::from_value::<BotChildWaitEnrollmentKey>(raw).is_err());
    let mut raw = serde_json::to_value(&value).unwrap();
    raw["origin_generation"] = 0.into();
    assert!(serde_json::from_value::<BotChildWaitCleanupIntentV1>(raw).is_err());
}

#[test]
fn m6_child_enrollment_request_and_reply_correlate_by_key() {
    let value = intent();
    let key = value.key().unwrap();
    let enroll = EnrollBotChildWaitCleanup {
        key: key.clone(),
        cleanup_intent: value.clone(),
    };
    assert!(enroll.validate().is_ok());
    let mut other = enroll.clone();
    other.key.wait_id = id(98);
    assert_eq!(other.validate(), Err(DecisionError::Invalid));

    let enrolled = BotChildWaitEnrollmentReply::Enrolled {
        key: key.clone(),
        ai_attempt: uuid(50),
        receipt_digest: key.receipt_digest,
    };
    assert!(enrolled.matches_request(&key, false) && enrolled.matches_request(&key, true));
    let missing = BotChildWaitEnrollmentReply::Missing { key: key.clone() };
    assert!(missing.matches_request(&key, true));
    assert!(
        !missing.matches_request(&key, false),
        "an enroll call is never answered Missing"
    );
    let wrong = BotChildWaitEnrollmentReply::Enrolled {
        key: key.clone(),
        ai_attempt: uuid(50),
        receipt_digest: DecisionDigest::from_bytes([1; 32]),
    };
    assert!(!wrong.matches_request(&key, false));
    assert_eq!(
        serde_json::to_value(BotChildWaitEnrollmentReply::Refused {
            code: BotWaitRefusal::Stale
        })
        .unwrap(),
        serde_json::json!({"result":"refused","code":"stale"})
    );
}

#[test]
fn m6_child_enrollment_resume_is_always_a_recorded_successor() {
    let origin = fence();
    let successor = ChildResume {
        wait: id(14),
        origin,
        consumer: DecisionConsumer::RecordedSuccessor {
            attempt: id(60),
            generation: origin.generation.next(),
            creation_key: id(61),
            session: None,
        },
        input_key: id(62),
        input_fingerprint: [3; 32],
        state: DecisionResumeState::Prepared {},
    };
    assert!(successor.validate().is_ok());
    assert!(matches!(
        successor.cleanup_target().unwrap(),
        DecisionCleanupTarget::Consumer {
            accepted_turn: None,
            ..
        }
    ));
    let mut same = successor.clone();
    same.consumer = DecisionConsumer::SameSession {
        creation_key: id(61),
    };
    assert_eq!(same.validate(), Err(DecisionError::Invalid));
    let mut skipped = successor.clone();
    skipped.consumer = DecisionConsumer::RecordedSuccessor {
        attempt: id(60),
        generation: origin.generation.next().next(),
        creation_key: id(61),
        session: None,
    };
    assert_eq!(skipped.validate(), Err(DecisionError::Invalid));
    // A bound state needs a session.
    let mut unbound = successor.clone();
    unbound.state = DecisionResumeState::SessionBound {};
    assert_eq!(unbound.validate(), Err(DecisionError::Invalid));
    // A closed disposition names only the two causes a child wait can have.
    assert!(ChildWaitDisposition::Open {}.validate().is_ok());
    for (cause, ok) in [
        (DecisionClosureReason::Canceled, true),
        (DecisionClosureReason::AiTerminal, true),
        (DecisionClosureReason::Denied, false),
        (DecisionClosureReason::Expired, false),
        (DecisionClosureReason::Completed, false),
    ] {
        assert_eq!(
            ChildWaitDisposition::Closed { cause }.validate().is_ok(),
            ok
        );
    }
}

// ---- m6_child_waiting_target -----------------------------------------------

fn waiting_target() -> RunProjectionTargetV1 {
    let step = TimelineEntry {
        id: id(90),
        kind: StepKind::WaitingForBot,
        refs: TimelineRefs::new([TimelineRef::Attempt { attempt: id(5) }]).unwrap(),
        at_ms: 8,
    };
    RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            observed_created_at_ms: 1,
            timeline_start: 0,
            projection: None,
            status: RunStatus::running(),
            failure_reason: None,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::WaitingForBot),
            status: transition_bot(&RunStatus::running(), &RunEvent::WaitingForBot).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::new(vec![step]).unwrap(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: fence(),
        disposition: None,
        wait_phase: BotWaitPhase::HoldRequested,
        reason: BotWaitReason::Child {},
        action: DecisionControlAction::ChildWaiting {},
        retained_bookkeeping: None,
        flush: DecisionList::default(),
        delivery: None,
    }
}

const WAITING_TARGET: &str = r#"{"kind":"control","version":1,"predecessor":{"observed_created_at_ms":1,"timeline_start":0,"projection":null,"status":{"state":"running","attention":[],"control":{"holder":"orchestrator"}},"failure_reason":null},"effects":{"event":{"event":"waiting_for_bot"},"status":{"state":"running","attention":["waiting_for_bot"],"control":{"holder":"orchestrator"}},"failure_reason":null,"economics_at_ms":null,"steps":[{"id":"00000000-0000-4000-8000-00000000005a","kind":{"step":"waiting_for_bot"},"refs":[{"ref":"attempt","attempt":"00000000-0000-4000-8000-000000000005"}],"at_ms":8}],"models":[],"publications":[]},"fence":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","run":"00000000-0000-4000-8000-000000000004","attempt":"00000000-0000-4000-8000-000000000005","generation":1,"session":"00000000-0000-4000-8000-000000000006","turn":"00000000-0000-4000-8000-000000000007"},"disposition":null,"wait_phase":"hold_requested","reason":{"reason":"child"},"action":{"action":"child_waiting"},"retained_bookkeeping":null,"flush":[],"delivery":null}"#;

#[test]
fn m6_child_waiting_target_golden_bytes_and_digest() {
    let target = waiting_target();
    assert!(target.validate().is_ok());
    let bytes = target.canonical_bytes().unwrap();
    assert_eq!(
        std::str::from_utf8(bytes.as_slice()).unwrap(),
        WAITING_TARGET
    );
    let digest = reference(
        b"koru.bot.decision.projection.target.v1",
        WAITING_TARGET.as_bytes(),
    );
    assert_eq!(*target.digest().unwrap().as_bytes(), digest);
    assert_eq!(
        hex(digest),
        "e328940601d22ad352f4773f2006b3a930223f49a6f85bc2be30dc753bc84393"
    );
    assert_eq!(
        RunProjectionTargetV1::decode(WAITING_TARGET.as_bytes()).unwrap(),
        target
    );
    assert_eq!(target.timeline_end().unwrap(), 1);
}

#[test]
fn m6_child_waiting_target_belongs_to_a_child_wait_and_to_nothing_else() {
    let set = |edit: &dyn Fn(&mut RunProjectionTargetV1)| {
        let mut target = waiting_target();
        edit(&mut target);
        target
    };
    // The marker under any other reason is refused.
    for reason in [
        BotWaitReason::Capacity {},
        BotWaitReason::NativeExpired {},
        BotWaitReason::OverlappingPrompt {},
        BotWaitReason::Decision { decision: id(15) },
    ] {
        let target = set(&|target| {
            if let RunProjectionTargetV1::Control { reason: slot, .. } = target {
                *slot = reason;
            }
        });
        assert_eq!(target.validate(), Err(DecisionError::Invalid));
    }
    // A child wait never carries a decision's waiting action.
    let decision_action = set(&|target| {
        if let RunProjectionTargetV1::Control {
            action, effects, ..
        } = target
        {
            *action = DecisionControlAction::DecisionWaiting {};
            effects.event = Some(RunEvent::ApprovalRequired);
            effects.status =
                transition_bot(&RunStatus::running(), &RunEvent::ApprovalRequired).unwrap();
            effects.steps = DecisionList::default();
        }
    });
    assert_eq!(decision_action.validate(), Err(DecisionError::Invalid));
    // The waiting step is the only step the action admits.
    let wrong_step = set(&|target| {
        if let RunProjectionTargetV1::Control { effects, .. } = target {
            let mut step = effects.steps.as_slice()[0].clone();
            step.kind = StepKind::WaitingForInput;
            effects.steps = DecisionList::new(vec![step]).unwrap();
        }
    });
    assert_eq!(wrong_step.validate(), Err(DecisionError::Invalid));
    // A cleanup request for a child wait has no decision and is valid.
    let cleanup = set(&|target| {
        if let RunProjectionTargetV1::Control {
            action,
            effects,
            wait_phase,
            predecessor,
            ..
        } = target
        {
            *action = DecisionControlAction::CleanupRequested {
                cause: DecisionClosureReason::Canceled,
                expected_wait_revision: DecisionRevision::new(2).unwrap(),
            };
            *wait_phase = BotWaitPhase::CleanupPending;
            effects.event = None;
            effects.status = predecessor.status.clone();
            effects.steps = DecisionList::default();
        }
    });
    assert!(cleanup.validate().is_ok());
}

#[test]
fn m6_child_waiting_target_source_schema_admits_six_and_refuses_seven() {
    for accepted in [3u8, 4, 5, 6] {
        assert_eq!(
            u8::from(DecisionSourceSchema::try_from(accepted).unwrap()),
            accepted
        );
        assert_eq!(
            serde_json::from_str::<DecisionSourceSchema>(&accepted.to_string())
                .map(u8::from)
                .unwrap(),
            accepted
        );
    }
    for refused in [0u8, 1, 2, 7, 255] {
        assert!(DecisionSourceSchema::try_from(refused).is_err());
        assert!(serde_json::from_str::<DecisionSourceSchema>(&refused.to_string()).is_err());
    }
}

#[test]
fn m6_child_waiting_target_status_adds_the_marker_and_resume_removes_it() {
    let running = RunStatus::running();
    let waiting = transition_bot(&running, &RunEvent::WaitingForBot).unwrap();
    assert!(
        waiting
            .attention()
            .unwrap()
            .contains(Attention::WaitingForBot)
    );
    assert_eq!(
        serde_json::to_value(&waiting).unwrap()["attention"],
        serde_json::json!(["waiting_for_bot"])
    );
    // Only the Bot owner sets or clears it; an ordinary Run refuses both.
    assert!(transition(&running, &RunEvent::WaitingForBot).is_err());
    assert!(transition(&waiting, &RunEvent::Resume).is_err());
    // Resume clears exactly this marker and is refused without it.
    assert_eq!(
        transition_bot(&waiting, &RunEvent::Resume).unwrap(),
        running
    );
    assert!(transition_bot(&running, &RunEvent::Resume).is_err());
    // The marker survives a turn starting; another marker survives Resume.
    let started = transition_bot(
        &waiting,
        &RunEvent::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
    )
    .unwrap();
    assert_eq!(started, waiting);
    let both = transition_bot(&waiting, &RunEvent::WaitingForInput).unwrap();
    let resumed = transition_bot(&both, &RunEvent::Resume).unwrap();
    assert_eq!(
        resumed.attention().unwrap().iter().collect::<Vec<_>>(),
        [Attention::WaitingForInput]
    );
    // A waiting Run can still be stopped, interrupted and delivered.
    assert_eq!(
        transition_bot(&waiting, &RunEvent::Cancel).unwrap(),
        RunStatus::Cancelled
    );
    assert_eq!(
        transition_bot(&waiting, &RunEvent::BotResponseDelivered).unwrap(),
        RunStatus::Completed
    );
    // The Resume control maps to the existing Resumed step and clears it.
    let resume = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            observed_created_at_ms: 1,
            timeline_start: 1,
            projection: None,
            status: waiting.clone(),
            failure_reason: None,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::Resume),
            status: running,
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::new(vec![TimelineEntry {
                id: id(91),
                kind: StepKind::Resumed {
                    via: ResumeSeam::RecordedContext,
                },
                refs: TimelineRefs::new([TimelineRef::Attempt { attempt: id(5) }]).unwrap(),
                at_ms: 9,
            }])
            .unwrap(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: fence(),
        disposition: None,
        wait_phase: BotWaitPhase::ResumeReserved,
        reason: BotWaitReason::Child {},
        action: DecisionControlAction::Resume {
            via: ResumeSeam::RecordedContext,
        },
        retained_bookkeeping: None,
        flush: DecisionList::default(),
        delivery: None,
    };
    assert!(resume.validate().is_ok());
    // The step and the event have stable tokens.
    assert_eq!(
        serde_json::to_value(StepKind::WaitingForBot).unwrap(),
        serde_json::json!({"step":"waiting_for_bot"})
    );
    assert_eq!(RunEvent::WaitingForBot.name(), "waiting_for_bot");
    assert_eq!(
        serde_json::to_value(DecisionControlAction::ChildWaiting {}).unwrap(),
        serde_json::json!({"action":"child_waiting"})
    );
}
