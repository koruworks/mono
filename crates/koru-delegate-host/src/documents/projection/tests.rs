// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::decisions::*;
use koru_delegate_core::*;

fn id<T: std::str::FromStr>() -> T {
    uuid::Uuid::new_v4().to_string().parse().ok().unwrap()
}
fn fixture() -> (RunDocument, RunProjectionIntent) {
    let device = DeviceId::from_uuid(uuid::Uuid::new_v4());
    let mut header = RunHeader::from_request(
        DelegationRequest {
            run: id(),
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(uuid::Uuid::new_v4()),
            },
            objective: Objective::new("projection fixture").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::BotFolder {
                folder: BotWorkingFolder::new("D:/fixture".into()).unwrap(),
            },
            context: vec![],
            grants: Default::default(),
            executing_device: device,
            origin: RunOrigin::Manual,
        },
        1,
    );
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device,
    };
    let bot: BotId = id();
    header.bot = Some(BotProvenance {
        owner,
        bot,
        submission: id(),
        chat_id: uuid::Uuid::new_v4(),
        ai_attempt_id: Some(uuid::Uuid::new_v4()),
        assistant_message_id: Some(uuid::Uuid::new_v4()),
        config_revision: 1,
        policy_revision: 1,
        continued_from: None,
        routine: None,
    });
    header.status = RunStatus::running();
    let doc = RunDocument::create(&header).unwrap();
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            projection: None,
            status: header.status.clone(),
            failure_reason: None,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::ApprovalRequired),
            status: transition_bot(&header.status, &RunEvent::ApprovalRequired).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: DecisionFence {
            owner,
            bot,
            run: header.run,
            attempt: id(),
            generation: AttemptGeneration::FIRST,
            session: id(),
            turn: id(),
        },
        disposition: None,
        wait_phase: BotWaitPhase::Fenced,
        reason: BotWaitReason::Capacity {},
        action: DecisionControlAction::DecisionWaiting {},
        retained_bookkeeping: None,
        flush: DecisionList::default(),
        delivery: None,
    };
    let intent = RunProjectionIntent {
        identity: DecisionProjectionIdentity {
            transition: id(),
            run: header.run,
            wait: id(),
            decision: None,
            sequence: DecisionRevision::new(1).unwrap(),
            workspace: WorkspaceId::new(),
            document: DocumentId::new(),
            source_schema: 3u8.try_into().unwrap(),
            subject_digest: decision_projection_subject_digest(&header).unwrap(),
            kind: target.kind(),
            target_digest: target.digest().unwrap(),
        },
        target,
    };
    (doc, intent)
}

#[derive(Default)]
struct FakeIo {
    acked: VersionVector,
    acknowledge: bool,
    drift: bool,
    calls: Vec<&'static str>,
}
impl ProjectionIo for FakeIo {
    fn send(&mut self, _: &RunDocument) -> PortResult<()> {
        self.calls.push("send");
        Ok(())
    }
    fn settle(&mut self, doc: &RunDocument) -> PortResult<()> {
        self.calls.push("settle");
        if self.acknowledge {
            self.acked = doc.version();
        }
        if self.drift {
            // Model an incoming update during Link::settle, after the exact
            // result was acknowledged. Coverage alone must not pass it.
            doc.doc()
                .get_map("run")
                .delete("decision_projection")
                .unwrap();
            doc.doc().commit();
        }
        Ok(())
    }
    fn acknowledged(&self) -> &VersionVector {
        &self.acked
    }
}

#[test]
fn projection_handle_refuses_wrong_physical_workspace_or_document() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    assert!(location_matches(workspace, document, workspace, document).is_ok());
    for (w, d) in [
        (WorkspaceId::new(), document),
        (workspace, DocumentId::new()),
    ] {
        assert_eq!(
            location_matches(workspace, document, w, d)
                .unwrap_err()
                .code,
            "projection_subject"
        );
    }
}

#[test]
fn projection_link_sequence_requires_ack_and_retries_identical_prepared_bytes() {
    let (doc, intent) = fixture();
    let mut stage = RunProjectionStaging::default();
    let mut io = FakeIo::default();
    let before = doc.version();
    // Both I/O calls succeed (the queue is empty), but no batch acknowledged
    // the base. No staging or connected write may follow.
    assert_eq!(
        prepare_with_io(&mut io, &mut stage, &doc, &intent)
            .unwrap_err()
            .code,
        "projection_base_pending"
    );
    assert_eq!(doc.version(), before);
    assert_eq!(io.calls, ["send", "settle"]);
    io.acknowledge = true;
    let prepared = prepare_with_io(&mut io, &mut stage, &doc, &intent).unwrap();
    io.acknowledge = false;
    assert!(apply_with_io(&mut io, &mut stage, &doc, &prepared).is_err());
    let applied_version = doc.version();
    io.acknowledge = true;
    let result = apply_with_io(&mut io, &mut stage, &doc, &prepared).unwrap();
    assert_eq!(result.identity(), &prepared.identity);
    assert_eq!(
        doc.version(),
        applied_version,
        "retry imports the identical delta once"
    );
    assert_eq!(
        io.calls,
        [
            "send", "settle", "send", "settle", "send", "settle", "send", "settle"
        ]
    );
}

#[test]
fn projection_link_sequence_rechecks_marker_after_settle_import() {
    let (doc, intent) = fixture();
    let mut stage = RunProjectionStaging::default();
    let mut io = FakeIo {
        acknowledge: true,
        ..FakeIo::default()
    };
    let prepared = prepare_with_io(&mut io, &mut stage, &doc, &intent).unwrap();
    io.drift = true;
    assert!(apply_with_io(&mut io, &mut stage, &doc, &prepared).is_err());
    let result = VersionVector::decode(prepared.result_version.as_slice()).unwrap();
    assert!(
        covers(&io.acked, &result),
        "the exact result was acknowledged"
    );
    assert!(
        stage.discard(prepared.identity.transition).is_err(),
        "unknown canonical state keeps exact preparation owned"
    );
    assert_eq!(io.calls, ["send", "settle", "send", "settle"]);
}
