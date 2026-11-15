// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn decision_source_preserves_ordinary_subject_refs_without_fabricated_attempt_refs() {
    let fence = edit().origin();
    let cases = [
        (
            StepKind::ApprovalRequired,
            TimelineRef::Approval { approval: id(91) },
            RunFact::ApprovalRequired {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
                approval: id(91),
            },
            None,
        ),
        (
            StepKind::ApprovalResolved {
                outcome: ApprovalOutcome::Approved,
            },
            TimelineRef::Approval { approval: id(91) },
            RunFact::ApprovalResolved {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
                approval: id(91),
                outcome: ApprovalOutcome::Approved,
            },
            None,
        ),
        (
            StepKind::AttachmentAccepted,
            TimelineRef::Attachment { attachment: id(91) },
            RunFact::AttachmentAccepted {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
            },
            None,
        ),
        (
            StepKind::AttachmentRejected,
            TimelineRef::Attachment { attachment: id(91) },
            RunFact::AttachmentRejected {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
                attachment: id(91),
                code: koru_session_protocol::ErrorCode::AttachmentNotFound,
            },
            None,
        ),
        (
            StepKind::ChildSessionDetected,
            TimelineRef::Session { session: id(91) },
            RunFact::ChildSessionDetected {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
                child: id(91),
                terminated: false,
            },
            Some(RunEvent::ChildSessionDetected),
        ),
    ];
    for (kind, subject, fact, transition) in cases {
        let prior = target().predecessor().clone();
        let status = transition.map_or_else(
            || prior.status.clone(),
            |event| transition_bot(&prior.status, &event).unwrap(),
        );
        let entry = TimelineEntry {
            id: id(90),
            kind,
            refs: TimelineRefs::new([TimelineRef::Turn { turn: fence.turn }, subject.clone()])
                .unwrap(),
            at_ms: 8,
        };
        let source = RunProjectionTargetV1::Source {
            version: DecisionVersionV1,
            predecessor: prior,
            transitions: DecisionList::new(transition.into_iter().collect()).unwrap(),
            effects: DecisionProjectionEffects {
                event: None,
                status,
                failure_reason: None,
                economics_at_ms: None,
                steps: DecisionList::new(vec![entry]).unwrap(),
                models: DecisionList::default(),
                publications: DecisionList::new(vec![DecisionProjectionPublication {
                    event: id(93),
                    step: Some(id(90)),
                    fact,
                }])
                .unwrap(),
            },
            bookkeeping: DecisionSourceBookkeeping {
                boundary: DecisionSourceBoundary {
                    fence,
                    producer_epoch: uuid(92),
                    stream: DecisionSourceStream::Agent,
                    sequence: StreamSequence::new(8).unwrap(),
                    resulting: DecisionSourceFrontiers {
                        agent_after: Some(StreamSequence::new(8).unwrap()),
                        session_after: None,
                    },
                },
                unflushed: DecisionList::default(),
                usage: None,
                allocated_steps: DecisionList::new(vec![id(90)]).unwrap(),
            },
        };
        assert!(
            source.validate().is_ok(),
            "ordinary refs are valid: {:?}",
            source.effects().steps.as_slice()[0].kind
        );
        let bytes = source.canonical_bytes().unwrap();
        assert_eq!(
            RunProjectionTargetV1::decode(bytes.as_slice()).unwrap(),
            source
        );

        for refs in [
            vec![TimelineRef::Turn { turn: fence.turn }],
            vec![TimelineRef::Turn { turn: id(94) }, subject.clone()],
            vec![
                TimelineRef::Turn { turn: fence.turn },
                subject,
                TimelineRef::Attempt { attempt: id(94) },
            ],
        ] {
            let mut wrong = source.clone();
            if let RunProjectionTargetV1::Source { effects, .. } = &mut wrong {
                let mut entry = effects.steps.as_slice()[0].clone();
                entry.refs = TimelineRefs::new(refs).unwrap();
                effects.steps = DecisionList::new(vec![entry]).unwrap();
            }
            assert!(
                wrong.validate().is_err(),
                "missing subject, wrong turn or explicit wrong attempt must fail"
            );
        }
    }
}

fn target() -> RunProjectionTargetV1 {
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
            event: Some(RunEvent::ApprovalRequired),
            status: transition_bot(&RunStatus::running(), &RunEvent::ApprovalRequired).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: edit().origin(),
        disposition: None,
        wait_phase: BotWaitPhase::Fenced,
        reason: BotWaitReason::Capacity {},
        action: DecisionControlAction::DecisionWaiting {},
        retained_bookkeeping: None,
        flush: DecisionList::default(),
        delivery: None,
    }
}

fn cleanup_target(cause: DecisionClosureReason) -> RunProjectionTargetV1 {
    let mut target = target();
    if let RunProjectionTargetV1::Control {
        predecessor,
        effects,
        action,
        wait_phase,
        ..
    } = &mut target
    {
        *action = DecisionControlAction::CleanupRequested {
            cause,
            expected_wait_revision: rev(2),
        };
        *wait_phase = BotWaitPhase::CleanupPending;
        effects.event = None;
        effects.status = predecessor.status.clone();
    }
    if let RunProjectionTargetV1::Control {
        retained_bookkeeping,
        ..
    } = &mut target
    {
        *retained_bookkeeping = Some(DecisionSourceBookkeeping {
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
        });
    }
    target
}

#[test]
fn decision_cleanup_request_retains_cause_without_claiming_terminal_status() {
    use DecisionClosureReason as C;
    for (cause, authorization) in [
        (C::Canceled, DecisionAuthorization::Canceled),
        (C::AuthorityChanged, DecisionAuthorization::Canceled),
        (C::Expired, DecisionAuthorization::Expired),
        (C::ClockUncertain, DecisionAuthorization::Expired),
    ] {
        let mut target = cleanup_target(cause);
        for reason in [
            BotWaitReason::Capacity {},
            BotWaitReason::NativeExpired {},
            BotWaitReason::OverlappingPrompt {},
        ] {
            if let RunProjectionTargetV1::Control {
                reason: offered, ..
            } = &mut target
            {
                *offered = reason;
            }
            let bytes = target.canonical_bytes().unwrap();
            assert_eq!(
                RunProjectionTargetV1::decode(bytes.as_slice()).unwrap(),
                target
            );
            assert_eq!(target.effects().status, RunStatus::running());
            assert!(target.projected_steps().unwrap().is_empty());
            assert!(target.publications().is_empty());
            assert_eq!(
                target.resulting_bookkeeping().unwrap().as_ref(),
                target.bookkeeping()
            );
        }
        for authorization in [authorization, DecisionAuthorization::Consumed] {
            if let RunProjectionTargetV1::Control {
                disposition,
                reason,
                ..
            } = &mut target
            {
                *reason = BotWaitReason::Decision { decision: id(72) };
                *disposition = Some(DecisionDisposition::Open {
                    state: DecisionState {
                        kind: DecisionKind::KoruEdit,
                        authorization,
                        phase: DecisionPhase::StopPending,
                        revision: rev(3),
                    },
                });
            }
            assert!(target.validate().is_ok());
        }
    }
    assert_ne!(
        cleanup_target(C::Canceled).digest().unwrap(),
        cleanup_target(C::AuthorityChanged).digest().unwrap()
    );
    assert_ne!(
        cleanup_target(C::Expired).digest().unwrap(),
        cleanup_target(C::ClockUncertain).digest().unwrap()
    );
    let mut revision = cleanup_target(C::Canceled);
    if let RunProjectionTargetV1::Control { action, .. } = &mut revision {
        *action = DecisionControlAction::CleanupRequested {
            cause: C::Canceled,
            expected_wait_revision: rev(3),
        };
    }
    assert_ne!(
        revision.digest().unwrap(),
        cleanup_target(C::Canceled).digest().unwrap()
    );
}

#[test]
fn decision_cleanup_request_preserves_settled_answer_only_for_stop_or_authority_loss() {
    for cause in [
        DecisionClosureReason::Canceled,
        DecisionClosureReason::AuthorityChanged,
        DecisionClosureReason::Expired,
        DecisionClosureReason::ClockUncertain,
    ] {
        for authorization in [
            DecisionAuthorization::Consumed,
            DecisionAuthorization::Denied,
        ] {
            let mut target = cleanup_target(cause);
            let retained = DecisionDisposition::Settled {
                state: DecisionState {
                    kind: DecisionKind::NativeApproval,
                    authorization,
                    phase: DecisionPhase::Settled,
                    revision: rev(4),
                },
                reason: DecisionClosureReason::Completed,
                outcome_digest: digest(7),
            };
            if let RunProjectionTargetV1::Control {
                reason,
                disposition,
                ..
            } = &mut target
            {
                *reason = BotWaitReason::Decision { decision: id(72) };
                *disposition = Some(retained.clone());
            }
            let allowed = matches!(
                cause,
                DecisionClosureReason::Canceled | DecisionClosureReason::AuthorityChanged
            );
            assert_eq!(target.validate().is_ok(), allowed);
            if allowed {
                let decoded =
                    RunProjectionTargetV1::decode(target.canonical_bytes().unwrap().as_slice())
                        .unwrap();
                let RunProjectionTargetV1::Control { disposition, .. } = decoded else {
                    panic!("Control")
                };
                assert_eq!(disposition, Some(retained));
            }
        }
    }
}

#[test]
fn decision_cleanup_request_refuses_wrong_phase_authorization_and_effects() {
    for cause in [
        DecisionClosureReason::Completed,
        DecisionClosureReason::Denied,
        DecisionClosureReason::Unsupported,
        DecisionClosureReason::EffectNotApplied,
        DecisionClosureReason::EffectUnknown,
        DecisionClosureReason::NativeUnknown,
        DecisionClosureReason::AiTerminal,
    ] {
        assert!(cleanup_target(cause).validate().is_err());
    }
    let good = cleanup_target(DecisionClosureReason::Canceled);
    let mut raw = json(&good);
    raw["wait_phase"] = serde_json::json!(BotWaitPhase::Fenced);
    assert!(RunProjectionTargetV1::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    let mut wrong = good.clone();
    if let RunProjectionTargetV1::Control { reason, .. } = &mut wrong {
        *reason = BotWaitReason::Decision { decision: id(72) };
    }
    assert!(
        wrong.validate().is_err(),
        "decision wait needs its exact disposition"
    );
    if let RunProjectionTargetV1::Control { disposition, .. } = &mut wrong {
        *disposition = Some(DecisionDisposition::Open {
            state: DecisionState {
                kind: DecisionKind::KoruEdit,
                authorization: DecisionAuthorization::Expired,
                phase: DecisionPhase::StopPending,
                revision: rev(3),
            },
        });
    }
    assert!(
        wrong.validate().is_err(),
        "cause and authorization must agree"
    );
    let mut wrong = good.clone();
    if let RunProjectionTargetV1::Control { effects, .. } = &mut wrong {
        effects.event = Some(RunEvent::Cancel);
        effects.status = RunStatus::Cancelled;
    }
    assert!(
        wrong.validate().is_err(),
        "request is not confirmed cancellation"
    );
    let mut wrong = good.clone();
    if let RunProjectionTargetV1::Control { flush, .. } = &mut wrong {
        *flush = DecisionList::new(vec![DecisionControlFlush {
            turn: id(91),
            step: id(92),
            event: id(93),
            at_ms: 4,
        }])
        .unwrap();
    }
    assert!(
        wrong.validate().is_err(),
        "cleanup request does not flush source progress"
    );
    let mut terminal = good;
    if let RunProjectionTargetV1::Control {
        predecessor,
        effects,
        ..
    } = &mut terminal
    {
        predecessor.status = RunStatus::Completed;
        effects.status = RunStatus::Completed;
    }
    assert!(
        terminal.validate().is_ok(),
        "retained successful status remains absorbing"
    );
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
    let mut effects = base.effects().clone();
    effects.event = None;
    let mut t = RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: base.predecessor().clone(),
        transitions: DecisionList::new(vec![RunEvent::ApprovalRequired]).unwrap(),
        effects,
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

#[test]
fn decision_source_manifest_refuses_omission_and_cross_kind_facts() {
    let fence = edit().origin();
    let entry = TimelineEntry {
        id: id(90),
        kind: StepKind::AttachmentRejected,
        refs: TimelineRefs::new([
            TimelineRef::Attempt {
                attempt: fence.attempt,
            },
            TimelineRef::Turn { turn: fence.turn },
            TimelineRef::Attachment { attachment: id(91) },
        ])
        .unwrap(),
        at_ms: 8,
    };
    let base = target();
    let mut effects = base.effects().clone();
    effects.event = None;
    effects.status = base.predecessor().status.clone();
    effects.steps = DecisionList::new(vec![entry]).unwrap();
    let mut source = RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: base.predecessor().clone(),
        transitions: DecisionList::default(),
        effects,
        bookkeeping: DecisionSourceBookkeeping {
            boundary: DecisionSourceBoundary {
                fence,
                producer_epoch: uuid(92),
                stream: DecisionSourceStream::Agent,
                sequence: StreamSequence::new(8).unwrap(),
                resulting: DecisionSourceFrontiers {
                    agent_after: Some(StreamSequence::new(8).unwrap()),
                    session_after: None,
                },
            },
            unflushed: DecisionList::default(),
            usage: None,
            allocated_steps: DecisionList::new(vec![id(90)]).unwrap(),
        },
    };
    assert!(
        source.validate().is_err(),
        "the step cannot lose its bus fact"
    );
    let fact = RunFact::AttachmentRejected {
        run: fence.run,
        attempt: fence.attempt,
        turn: fence.turn,
        attachment: id(91),
        code: koru_session_protocol::ErrorCode::AttachmentNotFound,
    };
    if let RunProjectionTargetV1::Source { effects, .. } = &mut source {
        effects.publications = DecisionList::new(vec![DecisionProjectionPublication {
            event: id(93),
            step: Some(id(90)),
            fact,
        }])
        .unwrap();
    }
    assert!(source.validate().is_ok());
    let bytes = source.canonical_bytes().unwrap();
    assert_eq!(
        RunProjectionTargetV1::decode(bytes.as_slice()).unwrap(),
        source
    );
    if let RunProjectionTargetV1::Source { effects, .. } = &mut source {
        effects.publications = DecisionList::new(vec![DecisionProjectionPublication {
            event: id(93),
            step: Some(id(90)),
            fact: RunFact::WaitingForInput {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
            },
        }])
        .unwrap();
    }
    assert!(
        source.validate().is_err(),
        "same Run and step do not permit a different fact kind"
    );
}

#[test]
fn decision_source_transitions_are_ordered_bounded_and_never_delivery_authority() {
    let base = target();
    let mut effects = base.effects().clone();
    effects.event = None;
    effects.status = RunStatus::running();
    let mut source = RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: base.predecessor().clone(),
        transitions: DecisionList::new(vec![
            RunEvent::ApprovalRequired,
            RunEvent::ApprovalResolved,
        ])
        .unwrap(),
        effects,
        bookkeeping: DecisionSourceBookkeeping {
            boundary: DecisionSourceBoundary {
                fence: edit().origin(),
                producer_epoch: uuid(92),
                stream: DecisionSourceStream::Agent,
                sequence: StreamSequence::new(8).unwrap(),
                resulting: DecisionSourceFrontiers {
                    agent_after: Some(StreamSequence::new(8).unwrap()),
                    session_after: None,
                },
            },
            unflushed: DecisionList::default(),
            usage: None,
            allocated_steps: DecisionList::default(),
        },
    };
    assert!(source.validate().is_ok());
    let encoded = source.canonical_bytes().unwrap();
    let mut raw: serde_json::Value = serde_json::from_slice(encoded.as_slice()).unwrap();
    raw["transitions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(RunEvent::ApprovalRequired).unwrap());
    assert!(RunProjectionTargetV1::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    if let RunProjectionTargetV1::Source {
        transitions,
        effects,
        ..
    } = &mut source
    {
        *transitions = DecisionList::new(vec![RunEvent::BotResponseDelivered]).unwrap();
        effects.status = RunStatus::Completed;
    }
    assert!(source.validate().is_err());
    if let RunProjectionTargetV1::Source {
        transitions,
        effects,
        ..
    } = &mut source
    {
        *transitions = DecisionList::default();
        effects.event = Some(RunEvent::ApprovalRequired);
    }
    assert!(
        source.validate().is_err(),
        "there is only one Source transition authority"
    );
}

#[test]
fn decision_source_child_fact_matches_accepted_termination_request_only() {
    let fence = edit().origin();
    let base = target();
    for accepted in [false, true] {
        let transitions = if accepted {
            vec![
                RunEvent::ChildSessionDetected,
                RunEvent::ChildSessionTerminated,
            ]
        } else {
            vec![RunEvent::ChildSessionDetected]
        };
        let mut effects = base.effects().clone();
        effects.event = None;
        effects.status = transitions
            .iter()
            .fold(base.predecessor().status.clone(), |status, event| {
                transition_bot(&status, event).unwrap()
            });
        effects.steps = DecisionList::new(vec![TimelineEntry {
            id: id(90),
            kind: StepKind::ChildSessionDetected,
            at_ms: 8,
            refs: TimelineRefs::new([
                TimelineRef::Attempt {
                    attempt: fence.attempt,
                },
                TimelineRef::Turn { turn: fence.turn },
                TimelineRef::Session { session: id(91) },
            ])
            .unwrap(),
        }])
        .unwrap();
        let publication = DecisionProjectionPublication {
            event: id(93),
            step: Some(id(90)),
            fact: RunFact::ChildSessionDetected {
                run: fence.run,
                attempt: fence.attempt,
                turn: fence.turn,
                child: id(91),
                terminated: accepted,
            },
        };
        effects.publications = DecisionList::new(vec![publication.clone()]).unwrap();
        let mut source = RunProjectionTargetV1::Source {
            version: DecisionVersionV1,
            predecessor: base.predecessor().clone(),
            transitions: DecisionList::new(transitions).unwrap(),
            effects,
            bookkeeping: DecisionSourceBookkeeping {
                boundary: DecisionSourceBoundary {
                    fence,
                    producer_epoch: uuid(92),
                    stream: DecisionSourceStream::Agent,
                    sequence: StreamSequence::new(8).unwrap(),
                    resulting: DecisionSourceFrontiers {
                        agent_after: Some(StreamSequence::new(8).unwrap()),
                        session_after: None,
                    },
                },
                unflushed: DecisionList::default(),
                usage: None,
                allocated_steps: DecisionList::new(vec![id(90)]).unwrap(),
            },
        };
        assert!(source.validate().is_ok());
        let mut wrong = publication;
        if let RunFact::ChildSessionDetected { terminated, .. } = &mut wrong.fact {
            *terminated = !accepted;
        }
        if let RunProjectionTargetV1::Source { effects, .. } = &mut source {
            effects.publications = DecisionList::new(vec![wrong]).unwrap();
        }
        assert!(source.validate().is_err());
    }
}
