// SPDX-License-Identifier: MPL-2.0
//! Delegate IPC 18: private operations. Their decision, their one-use
//! consume, their effect intent and receipt, and the dispatcher's announce.
use super::*;
use koru_delegate_core::decisions::{DecisionDigest, DecisionId, DecisionRevision};
use koru_delegate_core::{
    BotAccess, BotId, BotOwner, BotRequestId, DelegateBootId, FreshBotPolicyProof, SessionId,
};

fn owner() -> BotOwner {
    serde_json::from_value(serde_json::json!({"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()}))
        .unwrap()
}

fn fence(owner: BotOwner) -> BotExecutionFence {
    BotExecutionFence {
        owner,
        bot: BotId::mint(),
        run: RunId::mint(),
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
        turn: Some(Uuid::new_v4().to_string().parse().unwrap()),
    }
}

fn proof(fence: &BotExecutionFence) -> FreshBotPolicyProof {
    FreshBotPolicyProof {
        global: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner: fence.owner,
                bot: fence.bot,
                request: BotRequestId::mint().into(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: CapturedBotPolicy {
                workspace: WorkspaceId::from_uuid(Uuid::new_v4()),
                destination: WorkspaceId::from_uuid(Uuid::new_v4()),
                access: BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            effects_allowed: true,
        },
        routine: None,
    }
}

fn proposal() -> PrivateOperationProposal {
    let connection = ConnectionId::from_uuid(Uuid::new_v4());
    let item = ConnectionItemId::from_uuid(Uuid::new_v4());
    let item_ref = DecisionDigest::from_bytes([7; 32]);
    PrivateOperationProposal {
        operation_id: Uuid::new_v4(),
        connection,
        connection_revision: DecisionRevision::new(3).unwrap(),
        account: DecisionDigest::from_bytes([8; 32]),
        item,
        item_ref,
        operation: PrivateOperationKind::ReadItem,
        request_fingerprint: koru_delegate_core::private_request_fingerprint(
            PrivateOperationKind::ReadItem,
            connection,
            item,
            item_ref,
        ),
    }
}

fn key(fence: &BotExecutionFence, operation: &PrivateOperationProposal) -> PrivateEffectKey {
    PrivateEffectKey {
        run: fence.run,
        operation_id: operation.operation_id,
        attempt: fence.attempt,
        generation: fence.generation,
        turn: fence.turn.unwrap(),
    }
}

/// Every private command, with the tag it travels under and the owner it
/// asserts (`None`: the actor resolves the Run's binding itself).
fn commands() -> Vec<(BotCommand, &'static str, Option<BotOwner>)> {
    let owner = owner();
    let fence = fence(owner);
    let proof = proof(&fence);
    let boot = DelegateBootId::mint();
    let operation = proposal();
    let dispatcher = PrivateDispatcherId::mint();
    vec![
        (
            BotCommand::AnnouncePrivateDispatcher { owner, dispatcher },
            "announce_private_dispatcher",
            Some(owner),
        ),
        (
            BotCommand::PreparePrivateDecision {
                fence: fence.clone(),
                operation: operation.clone(),
                proof: proof.clone(),
                boot,
            },
            "prepare_private_decision",
            Some(owner),
        ),
        (
            BotCommand::ConsumePrivateDecision {
                fence: fence.clone(),
                dispatcher,
                operation: operation.clone(),
                proof: proof.clone(),
                boot,
            },
            "consume_private_decision",
            Some(owner),
        ),
        (
            BotCommand::PreparePrivateEffect {
                fence: fence.clone(),
                dispatcher,
                operation: operation.clone(),
                proof,
                boot,
            },
            "prepare_private_effect",
            Some(owner),
        ),
        (
            BotCommand::AdvancePrivateEffect {
                key: key(&fence, &operation),
                dispatcher,
                transition: PrivateEffectTransition::Confirmed {
                    outcome_bytes: 4096,
                    withheld: false,
                },
            },
            "advance_private_effect",
            None,
        ),
        (
            BotCommand::ReadPrivateEffect {
                run: fence.run,
                operation_id: operation.operation_id,
                dispatcher,
            },
            "read_private_effect",
            None,
        ),
    ]
}

fn view(stage: PrivateEffectStage) -> PrivateEffectView {
    let fence = fence(owner());
    let operation = proposal();
    PrivateEffectView {
        key: key(&fence, &operation),
        dispatcher: PrivateDispatcherId::mint(),
        decision: Some(DecisionId::mint()),
        operation,
        config_revision: 2,
        policy_revision: 3,
        routine: None,
        stage,
        refusal: (stage == PrivateEffectStage::Refused)
            .then_some(PrivateEffectRefusal::CredentialLocked),
        outcome_bytes: (stage == PrivateEffectStage::Confirmed).then_some(512),
        withheld: (stage == PrivateEffectStage::Confirmed).then_some(false),
        disposition: PrivateEffectDisposition::ReconcileOnly,
        deliver: false,
    }
}

#[test]
fn m5_ipc18_codec_commands_roundtrip_strictly_and_bind_their_owner() {
    assert_eq!(crate::ipc::WIRE_VERSION, 19);
    for (command, tag, owner) in commands() {
        assert_eq!(command.owner(), owner.as_ref(), "{tag}");
        assert_eq!(command.private_valid(), Some(true), "{tag}");
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, crate::ipc::WIRE_VERSION);
        assert_eq!(decoded, command, "{tag}");
        let json = serde_json::to_value(&command).unwrap();
        assert_eq!(json["command"], tag);
        // Unknown fields are refused at the command and inside every nested
        // private record.
        let mut unknown = json.clone();
        unknown["undeclared_authority"] = true.into();
        assert!(
            serde_json::from_value::<BotCommand>(unknown).is_err(),
            "{tag}"
        );
        for nested in ["operation", "key", "transition"] {
            if json.get(nested).is_some() {
                let mut unknown = json.clone();
                unknown[nested]["undeclared"] = true.into();
                assert!(
                    serde_json::from_value::<BotCommand>(unknown).is_err(),
                    "{tag}.{nested}"
                );
            }
        }
        // Redacted: a transport's derived diagnostics never print an id, a
        // digest or a proof.
        assert_eq!(format!("{command:?}"), "BotCommand([REDACTED])");
    }
    // An ordinary command is not a private one.
    assert_eq!(
        BotCommand::Hello {
            owner: owner(),
            scan: BotBootScan::Probe {}
        }
        .private_valid(),
        None
    );
}

#[test]
fn m5_ipc18_codec_operation_ids_are_canonical_v4_text_and_the_fingerprint_recomputes() {
    let (command, _, _) = commands().remove(2);
    let json = serde_json::to_value(&command).unwrap();
    let id = json["operation"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for bad in [
        id.replace('-', ""),
        id.to_uppercase(),
        format!("{{{id}}}"),
        // A version-1 id with the same variant.
        "6ba7b810-9dad-11d1-80b4-00c04fd430c8".to_owned(),
    ] {
        if bad == id {
            continue;
        }
        let mut forged = json.clone();
        forged["operation"]["operation_id"] = bad.clone().into();
        assert!(
            serde_json::from_value::<BotCommand>(forged).is_err(),
            "{bad}"
        );
    }
    // The same rule on the effect key and on the read.
    for index in [4, 5] {
        let (command, tag, _) = commands().remove(index);
        let mut forged = serde_json::to_value(&command).unwrap();
        let slot = if index == 4 {
            &mut forged["key"]["operation_id"]
        } else {
            &mut forged["operation_id"]
        };
        *slot = slot.as_str().unwrap().replace('-', "").into();
        assert!(
            serde_json::from_value::<BotCommand>(forged).is_err(),
            "{tag}"
        );
    }
    // A fingerprint is recomputed, never a caller's assertion: a proposal
    // whose fingerprint does not recompute decodes, and is then not valid.
    let mut forged = json.clone();
    forged["operation"]["request_fingerprint"] = serde_json::to_value([9u8; 32]).unwrap();
    let forged: BotCommand = serde_json::from_value(forged).unwrap();
    assert_eq!(forged.private_valid(), Some(false));
    // A zero connection revision is refused while decoding.
    let mut zero = json.clone();
    zero["operation"]["connection_revision"] = 0.into();
    assert!(serde_json::from_value::<BotCommand>(zero).is_err());
    // A refusal code and a transition outside the closed lists are refused.
    let (advance, _, _) = commands().remove(4);
    let mut unknown = serde_json::to_value(&advance).unwrap();
    unknown["transition"] = serde_json::json!({"transition":"refused","code":"vendor_said_no"});
    assert!(serde_json::from_value::<BotCommand>(unknown.clone()).is_err());
    unknown["transition"] = serde_json::json!({"transition":"prepared"});
    assert!(serde_json::from_value::<BotCommand>(unknown).is_err());
}

#[test]
fn m5_ipc18_codec_replies_roundtrip_with_every_field_present_and_closed_shapes() {
    for stage in [
        PrivateEffectStage::Prepared,
        PrivateEffectStage::Dispatched,
        PrivateEffectStage::Confirmed,
        PrivateEffectStage::Refused,
        PrivateEffectStage::Unknown,
    ] {
        let reply = BotReply::PrivateEffect {
            effect: view(stage),
        };
        assert_eq!(reply.private_valid(), Some(true), "{stage:?}");
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json["result"], "private_effect");
        // Every field is always present; an absent value is `null`.
        let effect = json["effect"].as_object().unwrap();
        let mut fields: Vec<&str> = effect.keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "config_revision",
                "decision",
                "deliver",
                "dispatcher",
                "disposition",
                "key",
                "operation",
                "outcome_bytes",
                "policy_revision",
                "refusal",
                "routine",
                "stage",
                "withheld"
            ]
        );
        assert!(effect["routine"].is_null());
        assert_eq!(
            effect["refusal"].is_null(),
            stage != PrivateEffectStage::Refused
        );
        assert_eq!(
            effect["outcome_bytes"].is_null(),
            stage != PrivateEffectStage::Confirmed
        );
        let decoded: BotReply = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(decoded, reply);
        let mut unknown = json;
        unknown["effect"]["content"] = "PRIVATE-CONTENT-SENTINEL".into();
        assert!(serde_json::from_value::<BotReply>(unknown).is_err());
        assert_eq!(format!("{reply:?}"), "BotReply([REDACTED])");
    }
    let announced = BotReply::PrivateDispatcher {
        dispatcher: PrivateDispatcherId::mint(),
        resolved: 2,
    };
    assert_eq!(announced.private_valid(), Some(true));
    let json = serde_json::to_value(&announced).unwrap();
    assert_eq!(json["result"], "private_dispatcher");
    assert_eq!(
        serde_json::from_value::<BotReply>(json.clone()).unwrap(),
        announced
    );
    let mut unknown = json;
    unknown["owner"] = "x".into();
    assert!(serde_json::from_value::<BotReply>(unknown).is_err());
    // More rows than one resolution can close is not a reply of this owner.
    assert_eq!(
        BotReply::PrivateDispatcher {
            dispatcher: PrivateDispatcherId::mint(),
            resolved: PRIVATE_RESOLVED_LIMIT + 1,
        }
        .private_valid(),
        Some(false)
    );
    assert_eq!(BotReply::Applied.private_valid(), None);
}

#[test]
fn m5_ipc18_codec_a_reply_names_only_what_its_stage_records() {
    // The one license goes with a fresh `prepared` row only.
    let mut execute = view(PrivateEffectStage::Prepared);
    execute.disposition = PrivateEffectDisposition::Execute;
    assert_eq!(
        BotReply::PrivateEffect {
            effect: execute.clone()
        }
        .private_valid(),
        Some(true)
    );
    execute.stage = PrivateEffectStage::Dispatched;
    assert_eq!(
        BotReply::PrivateEffect { effect: execute }.private_valid(),
        Some(false)
    );
    // Content is delivered only for a confirmed read that was not withheld.
    let mut deliver = view(PrivateEffectStage::Confirmed);
    deliver.deliver = true;
    assert_eq!(
        BotReply::PrivateEffect {
            effect: deliver.clone()
        }
        .private_valid(),
        Some(true)
    );
    deliver.withheld = Some(true);
    assert_eq!(
        BotReply::PrivateEffect {
            effect: deliver.clone()
        }
        .private_valid(),
        Some(false)
    );
    for stage in [
        PrivateEffectStage::Prepared,
        PrivateEffectStage::Dispatched,
        PrivateEffectStage::Refused,
        PrivateEffectStage::Unknown,
    ] {
        let mut wrong = view(stage);
        wrong.deliver = true;
        assert_eq!(
            BotReply::PrivateEffect { effect: wrong }.private_valid(),
            Some(false),
            "{stage:?}"
        );
    }
    // A stage carries its own columns and no others.
    let mut refused = view(PrivateEffectStage::Refused);
    refused.refusal = None;
    assert_eq!(
        BotReply::PrivateEffect { effect: refused }.private_valid(),
        Some(false)
    );
    let mut unknown = view(PrivateEffectStage::Unknown);
    unknown.outcome_bytes = Some(1);
    assert_eq!(
        BotReply::PrivateEffect { effect: unknown }.private_valid(),
        Some(false)
    );
    // D6: a Routine Run never has a private decision.
    let mut routine = view(PrivateEffectStage::Prepared);
    routine.routine = Some(PrivateRoutineBinding {
        schedule: koru_delegate_core::ScheduleId::mint(),
        grant_revision: 1,
    });
    assert_eq!(
        BotReply::PrivateEffect {
            effect: routine.clone()
        }
        .private_valid(),
        Some(false)
    );
    routine.decision = None;
    assert_eq!(
        BotReply::PrivateEffect { effect: routine }.private_valid(),
        Some(true)
    );
}

fn frame(command: &BotCommand) -> Vec<u8> {
    koru_ipc::frame::encode(
        crate::ipc::WIRE_VERSION,
        &crate::ipc::Request::Bot {
            authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
            command: command.clone(),
        },
    )
    .unwrap()
}

#[test]
fn m5_ipc18_codec_bounded_commands_stay_within_16_kib_and_an_oversized_frame_is_named_before_decoding()
 {
    assert_eq!(PRIVATE_WIRE_LIMIT, 16 * 1024);
    for (command, tag, _) in commands() {
        let bytes = frame(&command);
        let body = &bytes[koru_ipc::frame::HEADER_LEN..];
        assert!(!private_frame_oversized(body), "{tag}");
        let bounded = matches!(
            tag,
            "announce_private_dispatcher" | "advance_private_effect" | "read_private_effect"
        );
        if bounded {
            assert!(
                serde_json::to_vec(&command).unwrap().len() < PRIVATE_WIRE_LIMIT,
                "{tag}"
            );
        }
        // The same command padded past its frame bound: the probe reads one
        // tag and nothing else. Only the three bounded commands are named;
        // the three that carry a policy proof keep the existing frame bound.
        let mut padded = serde_json::to_value(crate::ipc::Request::Bot {
            authority: BotDelegateAuthority::new("a".repeat(48)).unwrap(),
            command: command.clone(),
        })
        .unwrap();
        padded["command"]["padding"] = "x".repeat(PRIVATE_FRAME_LIMIT).into();
        let body = serde_json::to_vec(&padded).unwrap();
        assert_eq!(private_frame_oversized(&body), bounded, "{tag}");
    }
    // An ordinary oversized frame and an undecodable body are not named.
    assert!(!private_frame_oversized(
        "x".repeat(PRIVATE_FRAME_LIMIT + 1).as_bytes()
    ));
    let other = serde_json::json!({"frame":"bot","command":{"command":"hello","padding":"x".repeat(PRIVATE_FRAME_LIMIT)}});
    assert!(!private_frame_oversized(
        &serde_json::to_vec(&other).unwrap()
    ));
}

#[test]
fn m5_ipc18_codec_the_new_failures_are_fixed_text_and_never_retryable() {
    let codes: Vec<&str> = PRIVATE_FAILURES.iter().map(|(code, _)| *code).collect();
    assert_eq!(
        codes,
        [
            "bot_private_unavailable",
            "bot_connection_granted",
            "bot_connection_not_granted",
            "bot_private_effect_open",
            "bot_private_effect_stale"
        ]
    );
    for (code, message) in PRIVATE_FAILURES {
        let failure = private_failure(code).expect("a known code");
        assert_eq!(failure.code, *code);
        assert_eq!(failure.message, *message);
        assert!(!failure.retryable, "{code}");
        assert!(failure.details.is_empty(), "{code}");
        assert!(code.starts_with("bot_"));
        // Fixed English text: no placeholder, id, digest or path.
        assert!(!message.contains(['{', '}', '`', '/', '\\']), "{code}");
        assert!(!message.chars().any(|c| c.is_ascii_digit()), "{code}");
    }
    // `bot_private_revoked` was replaced by a reply and does not exist; an
    // existing code keeps its existing text.
    assert!(private_failure("bot_private_revoked").is_none());
    assert!(private_failure("bot_fenced").is_none());
}

/// The build has one `WIRE_VERSION`, so this is the frame-level half: a
/// frame stamped 17 is not read as an 18 frame, whatever its body. The
/// worker's half (the frame is dropped unanswered before its body is
/// decoded) is pinned in `koru-delegate-host`'s server tests.
#[test]
fn m5_ipc18_frame_version_a_frame_stamped_17_is_refused_by_the_18_reader() {
    assert_eq!(crate::ipc::WIRE_VERSION, 19);
    for (command, tag, _) in commands() {
        let reply = crate::ipc::Response::Bot(BotReply::Missing);
        // A reply stamped 17 never becomes an answer to a private command.
        let old = koru_ipc::frame::encode(17, &reply).unwrap();
        let failure = crate::local::read_current_response(&mut old.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible", "{tag}");
        assert!(!failure.retryable);
        // The version is checked before the body: an undecodable 17 body is
        // the same refusal, never a decode error.
        let undecodable = koru_ipc::frame::encode(17, &serde_json::json!({"old":true})).unwrap();
        let failure = crate::local::read_current_response(&mut undecodable.as_slice()).unwrap_err();
        assert_eq!(failure.code, "delegate_incompatible", "{tag}");
        // A private command stamped 17 carries its stamp, so a reader that
        // compares it never mistakes it for a current frame.
        let stamped = koru_ipc::frame::encode(17, &command).unwrap();
        let (version, _) = koru_ipc::frame::read_raw(&mut stamped.as_slice()).unwrap();
        assert_eq!(version, 17);
        assert_ne!(version, crate::ipc::WIRE_VERSION);
    }
    let current = koru_ipc::frame::encode(
        crate::ipc::WIRE_VERSION,
        &crate::ipc::Response::Bot(BotReply::Missing),
    )
    .unwrap();
    assert_eq!(
        crate::local::read_current_response(&mut current.as_slice()).unwrap(),
        crate::ipc::Response::Bot(BotReply::Missing)
    );
}

/// The credentialed adapter refuses a malformed or oversized private command
/// before a frame is written, and a malformed private reply before it is
/// handed to the caller.
#[test]
fn m5_ipc18_codec_the_local_adapter_refuses_a_malformed_private_frame_on_both_sides() {
    let (command, _, _) = commands().remove(3);
    assert!(LocalBotDelegate::encodable(&command).is_ok());
    let mut forged = serde_json::to_value(&command).unwrap();
    forged["operation"]["request_fingerprint"] = serde_json::to_value([1u8; 32]).unwrap();
    let forged: BotCommand = serde_json::from_value(forged).unwrap();
    let refused = LocalBotDelegate::encodable(&forged).unwrap_err();
    assert_eq!(refused.code, "invalid_request");
    assert!(!refused.retryable);
    let mut wrong = view(PrivateEffectStage::Refused);
    wrong.deliver = true;
    let failure = LocalBotDelegate::answer(crate::ipc::Response::Bot(BotReply::PrivateEffect {
        effect: wrong,
    }))
    .unwrap_err();
    assert_eq!(failure.code, "delegate_unexpected_response");
    assert!(
        LocalBotDelegate::answer(crate::ipc::Response::Bot(BotReply::PrivateEffect {
            effect: view(PrivateEffectStage::Unknown),
        }))
        .is_ok()
    );
}
