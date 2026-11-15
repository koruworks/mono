// SPDX-License-Identifier: MPL-2.0
use super::*;

fn target() -> RunProjectionTargetV1 {
    RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            projection: None,
            status: RunStatus::running(),
            failure_reason: None,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::ApprovalRequired),
            status: transition_bot(&RunStatus::running(), &RunEvent::ApprovalRequired).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
        },
        fence: edit().origin(),
        disposition: None,
        wait_phase: BotWaitPhase::Fenced,
        reason: BotWaitReason::Capacity {},
        action: DecisionControlAction::DecisionWaiting {},
        retained_bookkeeping: None,
    }
}

#[test]
fn decision_projection_target_strict_closed_class_and_nested_leaves() {
    let t = target();
    let bytes = t.canonical_bytes().unwrap();
    assert_eq!(RunProjectionTargetV1::decode(bytes.as_slice()).unwrap(), t);
    for key in ["extra", "bookkeeping"] {
        let mut raw = json(&t);
        raw[key] = serde_json::json!({});
        assert!(RunProjectionTargetV1::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
    let mut raw = json(&t);
    raw["kind"] = "source".into();
    assert!(RunProjectionTargetV1::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    let mut raw = json(&t);
    raw["effects"]["event"]["SECRET"] = true.into();
    assert!(RunProjectionTargetV1::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    let mut raw = json(&t);
    raw["effects"]["event"]["event"] = "SECRET".into();
    let error = serde_json::from_value::<RunProjectionTargetV1>(raw).unwrap_err();
    assert!(!error.to_string().contains("SECRET"));
}

#[test]
fn decision_projection_terminal_control_cannot_invent_cancelled_step() {
    let mut t = target();
    if let RunProjectionTargetV1::Control {
        predecessor,
        effects,
        ..
    } = &mut t
    {
        predecessor.status = RunStatus::Completed;
        effects.status = RunStatus::Completed;
        effects.event = None;
    }
    assert!(t.validate().is_ok());
    if let RunProjectionTargetV1::Control { effects, .. } = &mut t {
        effects.event = Some(RunEvent::Cancel);
    }
    assert!(t.validate().is_err());
    if let RunProjectionTargetV1::Control { effects, .. } = &mut t {
        effects.event = None;
        effects.steps = DecisionList::new(vec![TimelineEntry {
            id: id(90),
            kind: StepKind::Completed,
            refs: TimelineRefs::default(),
            at_ms: 0,
        }])
        .unwrap();
    }
    assert!(t.validate().is_err());
}

#[test]
fn decision_projection_identity_and_control_revision_are_exact() {
    let t = target();
    let mut identity = DecisionProjectionIdentity {
        transition: id(70),
        run: edit().run,
        wait: id(71),
        decision: None,
        sequence: rev(2),
        workspace: edit().workspace,
        document: edit().target,
        source_schema: 3u8.try_into().unwrap(),
        subject_digest: digest(0),
        kind: DecisionProjectionKind::Control,
        target_digest: t.digest().unwrap(),
    };
    assert!(t.matches_identity(&identity).is_ok());
    identity.kind = DecisionProjectionKind::Source;
    assert!(t.matches_identity(&identity).is_err());
    identity.kind = DecisionProjectionKind::Control;
    identity.decision = Some(DecisionProjectionRevision {
        decision: id(72),
        revision: rev(2),
    });
    assert!(t.matches_identity(&identity).is_err());
}

#[test]
fn decision_projection_source_requires_exact_allocated_steps_and_bounded_target() {
    let base = target();
    let bookkeeping = DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: edit().origin(),
            producer_epoch: uuid(80),
            stream: DecisionSourceStream::Agent,
            sequence: StreamSequence::new(2).unwrap(),
            resulting: DecisionSourceFrontiers {
                agent_after: Some(StreamSequence::new(2).unwrap()),
                session_after: None,
            },
        },
        unflushed: DecisionList::default(),
        usage: None,
        allocated_steps: DecisionList::default(),
    };
    let mut t = RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: base.predecessor().clone(),
        effects: base.effects().clone(),
        bookkeeping,
    };
    assert!(t.validate().is_ok());
    if let RunProjectionTargetV1::Source { bookkeeping, .. } = &mut t {
        bookkeeping.allocated_steps = DecisionList::new(vec![id(99)]).unwrap();
    }
    assert!(t.validate().is_err());
    assert_eq!(
        RunProjectionTargetV1::decode(&vec![b' '; 65_537]),
        Err(DecisionError::TooLarge)
    );
}

#[test]
fn decision_projection_target_digest_is_domain_separated_and_redacted() {
    let t = target();
    let bytes = t.canonical_bytes().unwrap();
    let mut reference = blake3::Hasher::new();
    reference.update(b"koru.bot.decision.projection.target.v1");
    reference.update(bytes.as_slice());
    assert_eq!(
        t.digest().unwrap().as_bytes(),
        reference.finalize().as_bytes()
    );
    let mut changed = t.clone();
    if let RunProjectionTargetV1::Control { wait_phase, .. } = &mut changed {
        *wait_phase = BotWaitPhase::CleanupPending;
    }
    assert_ne!(t.digest().unwrap(), changed.digest().unwrap());
    assert_eq!(format!("{t:?}"), "RunProjectionTargetV1([REDACTED])");
}

#[test]
fn decision_projection_control_cannot_coalesce_source_only_or_delivery_effects() {
    for kind in [
        StepKind::ToolsCompleted {
            calls: 1,
            failed: 0,
            failures: ToolFailures::default(),
        },
        StepKind::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
        StepKind::Gap {
            events_lost: Some(1),
        },
    ] {
        let mut t = target();
        if let RunProjectionTargetV1::Control { effects, .. } = &mut t {
            effects.steps = DecisionList::new(vec![TimelineEntry {
                id: id(90),
                kind,
                refs: TimelineRefs::default(),
                at_ms: 0,
            }])
            .unwrap();
        }
        assert!(t.validate().is_err());
    }
    let mut t = target();
    if let RunProjectionTargetV1::Control { effects, .. } = &mut t {
        effects.event = Some(RunEvent::BotResponseDelivered);
        effects.status =
            transition_bot(&RunStatus::running(), &RunEvent::BotResponseDelivered).unwrap();
    }
    assert!(t.validate().is_err());
}
