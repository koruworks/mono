// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_core::decisions::{
    BotWaitId, DecisionAnswerChoice, DecisionAnswerId, DecisionDigest, DecisionEditAction,
    DecisionId, DecisionPayload, DecisionRevision, DecisionVersionVector,
};
use koru_delegate_core::{
    BotAccess, BotId, BotOwner, BotRequestId, DelegateBootId, FreshBotPolicyProof, ProfileId,
    SessionId,
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

fn edit() -> DecisionEditProposal {
    DecisionEditProposal {
        target: DocumentId::from_uuid(Uuid::new_v4()),
        action: DecisionEditAction::EditDocument,
        operation_id: Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        expected_revision: DecisionVersionVector::new(vec![2, 1, 2, 2, 4]).unwrap(),
        payload: DecisionPayload::new(b"PRIVATE-EDIT-SENTINEL".to_vec()).unwrap(),
    }
}

fn commands() -> Vec<(BotCommand, BotOwner)> {
    let owner = owner();
    let fence = fence(owner);
    let proof = proof(&fence);
    let boot = DelegateBootId::mint();
    vec![
        (
            BotCommand::PrepareNativeDecision {
                fence: fence.clone(),
                approval: ApprovalId::mint(),
                proof: proof.clone(),
                boot,
            },
            owner,
        ),
        (
            BotCommand::PrepareEditDecision {
                fence: fence.clone(),
                edit: edit(),
                proof: proof.clone(),
                boot,
            },
            owner,
        ),
        (
            BotCommand::AnswerDecision {
                answer: DecisionAnswerRequest {
                    owner,
                    run: fence.run,
                    decision: DecisionId::mint(),
                    expected_revision: DecisionRevision::new(2).unwrap(),
                    binding_digest: DecisionDigest::from_bytes([1; 32]),
                    request: DecisionAnswerId::mint(),
                    human: ProfileId::new(),
                    choice: DecisionAnswerChoice::AllowOnce {},
                    authority_digest: DecisionDigest::from_bytes([2; 32]),
                },
                proof: proof.clone(),
                boot,
            },
            owner,
        ),
        (
            BotCommand::ConsumeDecisionEdit {
                fence: fence.clone(),
                request: prepare(),
                payload: DecisionPayload::new(b"PRIVATE-EDIT-SENTINEL".to_vec()).unwrap(),
                proof,
                boot,
            },
            owner,
        ),
        (
            BotCommand::ReadBotDecisionWait {
                owner,
                bot: fence.bot,
                run: fence.run,
                wait_id: BotWaitId::mint(),
                decision_id: DecisionId::mint(),
            },
            owner,
        ),
    ]
}

#[test]
fn ipc16_decision_commands_roundtrip_strictly_and_bind_their_owner() {
    for (command, owner) in commands() {
        assert_eq!(command.owner(), Some(&owner));
        let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
        let (version, decoded): (u8, BotCommand) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, crate::ipc::WIRE_VERSION);
        assert_eq!(decoded, command);
        let mut json = serde_json::to_value(&command).unwrap();
        json["undeclared_authority"] = true.into();
        assert!(serde_json::from_value::<BotCommand>(json).is_err());
        assert_eq!(format!("{command:?}"), "BotCommand([REDACTED])");
    }
}

#[test]
fn ipc16_decision_nested_shapes_refuse_unknown_fields_and_keep_tagged_empty_variants() {
    let commands = commands();
    for (pointer, index) in [
        ("/edit", 1),
        ("/answer", 2),
        ("/answer/choice", 2),
        ("/proof", 0),
        ("/fence", 3),
    ] {
        let mut json = serde_json::to_value(&commands[index].0).unwrap();
        json.pointer_mut(pointer).unwrap()["undeclared_authority"] = true.into();
        assert!(
            serde_json::from_value::<BotCommand>(json).is_err(),
            "{pointer}"
        );
    }
    // The empty AllowOnce variant keeps its tag and refuses any body.
    let answer = serde_json::to_value(&commands[2].0).unwrap();
    assert_eq!(
        answer["answer"]["choice"],
        serde_json::json!({"choice":"allow_once"})
    );
    let mut deny = answer.clone();
    deny["answer"]["choice"] = serde_json::json!({"choice":"deny"});
    assert!(matches!(
        serde_json::from_value::<BotCommand>(deny).unwrap(),
        BotCommand::AnswerDecision { answer, .. } if answer.choice == DecisionAnswerChoice::Deny {}
    ));
    let mut untagged = answer.clone();
    untagged["answer"]["choice"] = serde_json::json!({});
    assert!(serde_json::from_value::<BotCommand>(untagged).is_err());
    let mut invented = answer;
    invented["answer"]["choice"] = serde_json::json!({"choice":"skip"});
    assert!(
        serde_json::from_value::<BotCommand>(invented).is_err(),
        "no invented native Skip token"
    );
}

#[test]
fn ipc16_edit_proposal_bounds_and_redacts_its_content() {
    let proposal = edit();
    assert_eq!(format!("{proposal:?}"), "DecisionEditProposal([REDACTED])");
    let wire = serde_json::to_value(&proposal).unwrap();
    for (field, value) in [
        ("operation_id", serde_json::json!(Uuid::nil())),
        (
            "operation_id",
            serde_json::json!("00000000-0000-1000-8000-000000000001"),
        ),
        (
            "operation_id",
            serde_json::json!(proposal.operation_id.to_string().to_uppercase()),
        ),
        ("payload", serde_json::json!([])),
        (
            "payload",
            serde_json::json!(vec![
                1u8;
                koru_delegate_core::decisions::DECISION_PAYLOAD_BYTES
                    + 1
            ]),
        ),
        ("expected_revision", serde_json::json!(vec![1u8; 65_537])),
        ("action", serde_json::json!("create_document")),
    ] {
        let mut changed = wire.clone();
        changed[field] = value;
        assert!(
            serde_json::from_value::<DecisionEditProposal>(changed).is_err(),
            "{field}"
        );
    }
    assert_eq!(
        serde_json::from_value::<DecisionEditProposal>(wire).unwrap(),
        proposal
    );
}

#[test]
fn ipc16_decision_wait_reply_is_closed_and_redacted() {
    let reply = BotReply::DecisionWait {
        observation: observation(),
        resume: None,
    };
    let json = serde_json::to_value(&reply).unwrap();
    assert_eq!(json["result"], "decision_wait");
    assert_eq!(
        serde_json::from_value::<BotReply>(json.clone()).unwrap(),
        reply
    );
    for pointer in ["", "/observation", "/observation/key"] {
        let mut changed = json.clone();
        changed.pointer_mut(pointer).unwrap()["undeclared_authority"] = true.into();
        assert!(
            serde_json::from_value::<BotReply>(changed).is_err(),
            "{pointer}"
        );
    }
    assert_eq!(format!("{reply:?}"), "BotReply([REDACTED])");
}

struct OnePort(std::sync::Mutex<Option<Result<BotReply, crate::Failure>>>);
impl BotDelegatePort for OnePort {
    fn execute(&self, command: BotCommand) -> Result<BotReply, crate::Failure> {
        assert!(matches!(command, BotCommand::ReadBotDecisionWait { .. }));
        self.0.lock().unwrap().take().expect("one call")
    }
}
fn port(reply: Result<BotReply, crate::Failure>) -> OnePort {
    OnePort(std::sync::Mutex::new(Some(reply)))
}

fn observation() -> koru_delegate_core::decisions::BotDecisionWaitObservation {
    use koru_delegate_core::decisions::*;
    let request = crate::decision_bridge::tests::enroll();
    BotDecisionWaitObservation {
        current_revision: request.key.wait_revision,
        fresh_stop_eligibility: FreshWaitStopEligibility::EligibleOriginWaitStop,
        origin_stop: OriginWaitStop::Pending {},
        consumer_cleanup: ConsumerCleanup::NotRequested {},
        disposition: DecisionDisposition::Open {
            state: DecisionState {
                kind: DecisionKind::KoruEdit,
                authorization: DecisionAuthorization::Pending,
                phase: DecisionPhase::Holding,
                revision: request.key.decision_revision,
            },
        },
        publication: DecisionPublication::Pending {
            latest_sequence: DecisionRevision::new(1).unwrap(),
            target_digest: DecisionDigest::from_bytes([4; 32]),
        },
        key: request.key,
        cleanup_intent: request.cleanup_intent,
    }
}

#[test]
fn ipc16_decision_wait_read_helper_never_turns_unknown_or_foreign_into_missing() {
    let observation = observation();
    let key = observation.key.clone();
    let within = std::time::Duration::from_secs(5);
    let read = |reply| {
        port(reply).read_bot_decision_wait(
            key.owner,
            key.bot,
            key.run,
            key.wait_id,
            key.decision_id,
            within,
        )
    };
    assert_eq!(read(Ok(BotReply::Missing)).unwrap(), None);
    let found = read(Ok(BotReply::DecisionWait {
        observation: observation.clone(),
        resume: None,
    }))
    .unwrap()
    .unwrap();
    assert_eq!(found.0, observation);
    // A foreign subject, another reply shape or a transport failure is
    // unavailable (retryable), never Missing.
    let mut foreign = observation.clone();
    foreign.current_revision = DecisionRevision::new(1).unwrap();
    for reply in [
        Ok(BotReply::DecisionWait {
            observation: foreign,
            resume: None,
        }),
        Ok(BotReply::Applied),
        Err(crate::Failure::unavailable()),
    ] {
        let failure = read(reply).unwrap_err();
        assert_eq!(failure.code, crate::Failure::unavailable().code);
    }
    let other = port(Ok(BotReply::DecisionWait {
        observation: observation.clone(),
        resume: None,
    }))
    .read_bot_decision_wait(
        key.owner,
        key.bot,
        key.run,
        key.wait_id,
        DecisionId::mint(),
        within,
    );
    assert!(
        other.is_err(),
        "an answer for another decision is not this one"
    );
    assert!(
        OnePort(std::sync::Mutex::new(None))
            .read_bot_decision_wait(
                key.owner,
                key.bot,
                key.run,
                key.wait_id,
                key.decision_id,
                std::time::Duration::ZERO
            )
            .is_err(),
        "no budget, no call"
    );
}

#[test]
fn ipc16_native_answer_carries_a_stable_request_key_and_refuses_the_ipc15_shape() {
    let owner = owner();
    let command = BotCommand::AnswerApproval {
        fence: fence(owner),
        request: koru_delegate_core::IdempotencyKey::mint(),
        approval: ApprovalId::mint(),
        decision: ApprovalDecision::Approve,
        choice: None,
    };
    assert_eq!(command.owner(), Some(&owner));
    let bytes = koru_ipc::frame::encode(crate::ipc::WIRE_VERSION, &command).unwrap();
    let (_, decoded): (u8, BotCommand) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(decoded, command);
    let json = serde_json::to_value(&command).unwrap();
    // The IPC15 shape had no key: it no longer decodes, so an older caller
    // cannot have its answer re-keyed per call silently.
    let mut ipc15 = json.clone();
    ipc15.as_object_mut().unwrap().remove("request");
    assert!(serde_json::from_value::<BotCommand>(ipc15).is_err());
    let mut forged = json.clone();
    forged["undeclared_authority"] = true.into();
    assert!(serde_json::from_value::<BotCommand>(forged).is_err());
    let mut malformed = json;
    malformed["request"] = serde_json::json!("not-a-key");
    assert!(serde_json::from_value::<BotCommand>(malformed).is_err());
}

/// O147: the waiting classification inside IPC16 (decision contract §3).
#[test]
fn ipc16_run_authority_decision_wait_roundtrips_strictly_and_old_or_unknown_values_refuse() {
    use koru_delegate_core::RunToolAuthority;
    use koru_delegate_core::decisions::DecisionPhase;
    let f = fence(owner());
    let provenance = koru_delegate_core::BotProvenance {
        routine: None,
        owner: f.owner,
        bot: f.bot,
        submission: BotRequestId::mint(),
        chat_id: Uuid::new_v4(),
        ai_attempt_id: None,
        assistant_message_id: None,
        config_revision: 1,
        policy_revision: 1,
        continued_from: None,
    };
    let policy = proof(&f).global.policy;
    let decision = DecisionId::mint();
    let waiting = RunToolAuthority::BotDecisionWait {
        provenance: provenance.clone(),
        policy: policy.clone(),
        decision,
        phase: DecisionPhase::Holding,
    };
    let reply = BotReply::RunAuthority {
        authority: waiting.clone(),
    };
    let json = serde_json::to_value(&reply).unwrap();
    assert_eq!(json["authority"]["kind"], "bot_decision_wait");
    assert_eq!(json["authority"]["phase"], "holding");
    assert_eq!(json["authority"]["decision"], serde_json::json!(decision));
    assert!(serde_json::from_value::<BotReply>(json.clone()).unwrap() == reply);
    // The same subject as the Bot value, and no content in diagnostics.
    assert_eq!(waiting.bot_subject(), Some((&provenance, &policy)));
    assert_eq!(format!("{waiting:?}"), "RunToolAuthority([REDACTED])");
    // Strict: an extra field, a missing field, an unknown phase or an
    // unknown kind never decodes.
    let mut extra = json.clone();
    extra["authority"]["lease"] = serde_json::json!(true);
    let mut missing = json.clone();
    missing["authority"]
        .as_object_mut()
        .unwrap()
        .remove("decision");
    let mut phase = json.clone();
    phase["authority"]["phase"] = serde_json::json!("paused");
    let mut kind = json.clone();
    kind["authority"]["kind"] = serde_json::json!("bot_waiting");
    for bad in [extra, missing, phase, kind] {
        assert!(serde_json::from_value::<BotReply>(bad).is_err());
    }
    // A peer that predates the value refuses it rather than reading Bot.
    #[derive(serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    #[allow(dead_code)]
    enum OlderAuthority {
        Ordinary {},
        Bot {
            provenance: koru_delegate_core::BotProvenance,
            policy: CapturedBotPolicy,
        },
    }
    assert!(serde_json::from_value::<OlderAuthority>(json["authority"].clone()).is_err());
    let bot = serde_json::to_value(RunToolAuthority::Bot { provenance, policy }).unwrap();
    assert!(serde_json::from_value::<OlderAuthority>(bot).is_ok());
}
