// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_closed_kind_authorization_phase_matrix() {
    use DecisionAuthorization as A;
    use DecisionPhase as P;
    let authorizations = [
        A::Pending,
        A::Granted,
        A::Denied,
        A::Canceled,
        A::Expired,
        A::Consumed,
    ];
    let phases = [
        P::Publishing,
        P::Holding,
        P::Held,
        P::NativeWait,
        P::ResumePrepared,
        P::AwaitRetry,
        P::Effect,
        P::NativeDispatch,
        P::NativeUnknown,
        P::StopPending,
        P::Settled,
    ];
    let koru = [
        vec![P::Publishing, P::Holding, P::Held],
        vec![P::Held, P::ResumePrepared, P::AwaitRetry],
        vec![P::StopPending, P::Settled],
        vec![P::StopPending, P::Settled],
        vec![P::StopPending, P::Settled],
        vec![P::Effect, P::StopPending, P::Settled],
    ];
    let native = [
        vec![P::Publishing, P::Holding, P::Held, P::NativeWait],
        vec![P::Held, P::NativeWait, P::NativeDispatch, P::NativeUnknown],
        vec![
            P::NativeDispatch,
            P::NativeUnknown,
            P::StopPending,
            P::Settled,
        ],
        vec![P::StopPending, P::Settled],
        vec![P::StopPending, P::Settled],
        vec![
            P::NativeDispatch,
            P::NativeUnknown,
            P::StopPending,
            P::Settled,
        ],
    ];
    for (kind, table) in [
        (DecisionKind::KoruEdit, koru),
        (DecisionKind::NativeApproval, native),
    ] {
        for (index, authorization) in authorizations.iter().enumerate() {
            for phase in phases {
                let s = DecisionState {
                    kind,
                    authorization: *authorization,
                    phase,
                    revision: rev(1),
                };
                assert_eq!(s.validate().is_ok(), table[index].contains(&phase), "{s:?}");
            }
        }
    }
}
#[test]
fn decision_deadline_is_exact_signed_min_and_rejects_overflow() {
    let mut d = DecisionDeadline {
        created_boot: id(35),
        created_at_ms: -700_000,
        deadline_at_ms: -100_000,
        native_deadline_at_ms: None,
    };
    assert!(d.validate().is_ok());
    d.deadline_at_ms -= 1;
    assert!(d.validate().is_err());
    d.native_deadline_at_ms = Some(-600_000);
    d.deadline_at_ms = -600_000;
    assert!(d.validate().is_ok());
    d.native_deadline_at_ms = Some(-700_000);
    d.deadline_at_ms = -700_000;
    assert!(d.validate().is_err());
    d.created_at_ms = i64::MAX - 1;
    d.deadline_at_ms = i64::MAX;
    d.native_deadline_at_ms = Some(i64::MAX);
    assert!(d.validate().is_err());
    d.created_at_ms = i64::MIN;
    d.deadline_at_ms = i64::MIN + 600_000;
    d.native_deadline_at_ms = None;
    assert!(d.validate().is_ok());
}
#[test]
fn decision_runtime_phase_cannot_reuse_historical_release_after_resume() {
    use BotWaitPhase::*;
    assert!(Held.permits(ResumeReserved, false));
    assert!(!ResumeReserved.permits(Held, true));
    assert!(!Retired.permits(Retired, true));
    assert!(Fenced.permits(Settled, true));
    assert!(ResumeReserved.permits(CleanupPending, true));
    assert!(CleanupPending.permits(Settled, true));
    assert!(!Settled.permits(Fenced, false));
}
#[test]
fn decision_resume_checks_same_session_and_exact_successor() {
    for r in [resume(false), resume(true)] {
        assert_eq!(
            decode_decision_frame::<DecisionResume>(&encode_decision_frame(&r).unwrap()).unwrap(),
            r
        );
    }
    let mut r = resume(true);
    if let DecisionConsumer::RecordedSuccessor { generation, .. } = &mut r.consumer {
        *generation = AttemptGeneration::FIRST;
    }
    assert!(r.validate().is_err());
    let mut r = resume(true);
    if let DecisionConsumer::RecordedSuccessor { attempt, .. } = &mut r.consumer {
        *attempt = r.origin.attempt;
    }
    assert!(r.validate().is_err());
    let mut r = resume(true);
    r.origin.generation = AttemptGeneration::from_raw(u32::MAX).unwrap();
    if let DecisionConsumer::RecordedSuccessor { generation, .. } = &mut r.consumer {
        *generation = AttemptGeneration::from_raw(u32::MAX).unwrap();
    }
    assert!(r.validate().is_err());
}
#[test]
fn decision_uncreated_successor_keeps_key_and_cannot_consume() {
    let mut r = resume(true);
    if let DecisionConsumer::RecordedSuccessor { session, .. } = &mut r.consumer {
        *session = None;
    }
    assert!(r.validate().is_err());
    r.state = DecisionResumeState::Prepared {};
    assert!(r.validate().is_ok());
    assert!(r.candidate_native_key(&binding()).is_err());
    let target = DecisionCleanupTarget::from_resume(&r).unwrap();
    assert_eq!(target.session(), None);
    assert_eq!(target.creation_key(), id(31));
    assert_eq!(target.attempt().0, id(30));
    assert_ne!(target.attempt().0, r.origin.attempt);
}
#[test]
fn decision_successor_cannot_reuse_origin_session() {
    let mut r = resume(true);
    if let DecisionConsumer::RecordedSuccessor { session, .. } = &mut r.consumer {
        *session = Some(r.origin.session);
    }
    assert!(r.validate().is_err());
    let mut target = DecisionCleanupTarget::from_resume(&resume(true)).unwrap();
    if let DecisionCleanupTarget::Consumer {
        origin, session, ..
    } = &mut target
    {
        *session = Some(origin.session);
    }
    assert!(target.validate().is_err());
    assert!(resume(false).validate().is_ok());
}
#[test]
fn decision_closed_resume_and_foreign_origin_never_yield_fresh_key() {
    let mut r = resume(true);
    r.state = DecisionResumeState::Closed {
        acknowledgment: Some(DecisionInputAcknowledgment {
            turn: id(34),
            receipt_digest: digest(5),
        }),
    };
    assert!(r.validate().is_ok());
    assert!(r.candidate_native_key(&binding()).is_err());
    let mut r = resume(false);
    r.origin.turn = id(100);
    assert!(r.candidate_native_key(&binding()).is_err());
}
#[test]
fn decision_enrollment_exact_key_digest_and_retained_ai_uuid_domain() {
    let request = EnrollBotWaitCleanup {
        key: intent().key().unwrap(),
        cleanup_intent: intent(),
    };
    assert_eq!(
        decode_decision_frame::<EnrollBotWaitCleanup>(&encode_decision_frame(&request).unwrap())
            .unwrap(),
        request
    );
    let mut wrong = request.clone();
    wrong.key.wait_revision = rev(4);
    assert!(wrong.validate().is_err());
    let key = request.key;
    let reply = BotWaitEnrollmentReply::Enrolled {
        receipt_digest: key.receipt_digest,
        key: key.clone(),
        ai_attempt: "00000000-0000-1000-8000-000000000001".parse().unwrap(),
    };
    assert!(reply.matches_request(&key, false));
    assert!(
        decode_decision_frame::<BotWaitEnrollmentReply>(&encode_decision_frame(&reply).unwrap())
            .is_ok()
    );
    let missing = BotWaitEnrollmentReply::Missing { key: key.clone() };
    assert!(missing.matches_request(&key, true));
    assert!(!missing.matches_request(&key, false));
    let mut foreign = key;
    foreign.decision_id = id(99);
    assert!(!reply.matches_request(&foreign, true));
}
#[test]
fn decision_native_ack_requires_exact_offered_key_and_reject_can_continue() {
    let b = DecisionBindingV1::NativeApproval(native());
    for choice in native().choices.as_slice() {
        let key = DecisionNativeResponseKey {
            decision: id(15),
            session: id(6),
            turn: id(7),
            prompt: id(20),
            response_key: id(50),
            choice: choice.clone(),
        };
        assert!(key.matches_binding(id(15), &b));
        let reply = DecisionNativeResponse::Acknowledged {
            key: key.clone(),
            acknowledgment: key.clone(),
            observed_at_ms: -1,
        };
        assert!(reply.validate().is_ok());
        let state = DecisionState {
            kind: DecisionKind::NativeApproval,
            authorization: if choice.intent.grants() {
                DecisionAuthorization::Consumed
            } else {
                DecisionAuthorization::Denied
            },
            phase: DecisionPhase::Settled,
            revision: rev(3),
        };
        assert!(state.validate().is_ok());
        let mut different = key.clone();
        different.response_key = id(51);
        assert!(
            DecisionNativeResponse::Acknowledged {
                key,
                acknowledgment: different,
                observed_at_ms: 0
            }
            .validate()
            .is_err()
        );
    }
    // No consumer cleanup is invented for an acknowledged Native Reject.
    assert!(ConsumerCleanup::NotRequested {}.validate().is_ok());
}
#[test]
fn decision_receipt_requires_matching_answer_consumption_and_closure() {
    let mut r = receipt();
    assert!(r.validate().is_ok());
    r.state.authorization = DecisionAuthorization::Granted;
    r.state.phase = DecisionPhase::Held;
    assert!(r.validate().is_err());
    r.answer = Some(DecisionAnswerReceipt {
        decision: r.decision,
        request: id(40),
        expected_revision: rev(1),
        committed_revision: rev(2),
        binding_digest: r.binding_digest,
        human_profile: edit().human_profile,
        choice: DecisionAnswerChoice::AllowOnce {},
        answered_at_ms: 0,
        authority_digest: digest(4),
    });
    r.state.revision = rev(2);
    assert!(r.validate().is_ok());
    r.state.authorization = DecisionAuthorization::Consumed;
    r.state.phase = DecisionPhase::Effect;
    assert!(r.validate().is_err());
    r.consumed_at_ms = Some(1);
    assert!(r.validate().is_ok());
    r.state.phase = DecisionPhase::Settled;
    assert!(r.validate().is_err());
    r.closure = Some(DecisionClosure {
        reason: DecisionClosureReason::Completed,
        settled_at_ms: 2,
    });
    assert!(r.validate().is_ok());
    r.answer.as_mut().unwrap().human_profile = ProfileId::from_uuid(uuid(99));
    assert!(r.validate().is_err());
}
#[test]
fn decision_cleanup_cannot_use_origin_retirement_for_live_successor() {
    let r = resume(true);
    let target = DecisionCleanupTarget::from_resume(&r).unwrap();
    let (attempt, generation) = target.attempt();
    let mut c = ConsumerCleanup::Confirmed {
        cleanup_revision: rev(8),
        target: target.clone(),
        creation_retirement: ObservedCreationRetirement {
            creation_key: id(31),
            host: HostInstanceId::from_uuid(uuid(60)),
            result: ObservedRetirementResult::KnownExited {
                session: id(32),
                code: Some(0),
            },
            observed_at_ms: 8,
        },
        attempt_retirement: DecisionAttemptRetirement {
            run: r.origin.run,
            attempt,
            generation,
            retired_at_ms: 8,
        },
        native_drain: DecisionNativeDrain {
            run: r.origin.run,
            attempt,
            generation,
            observed_at_ms: 8,
            effects: DecisionList::default(),
        },
        retained_effect_outcome: None,
    };
    assert!(c.validate().is_ok());
    if let ConsumerCleanup::Confirmed {
        attempt_retirement, ..
    } = &mut c
    {
        attempt_retirement.attempt = r.origin.attempt;
        attempt_retirement.generation = r.origin.generation;
    }
    assert!(c.validate().is_err());
    let mut false_target = target;
    if let DecisionCleanupTarget::Consumer { session, .. } = &mut false_target {
        *session = None;
    }
    assert!(false_target.validate().is_err());
}
#[test]
fn decision_retirement_observation_is_closed_and_exact_session() {
    for result in [
        ObservedRetirementResult::NoEvidence {},
        ObservedRetirementResult::KnownEnded { session: id(6) },
        ObservedRetirementResult::KnownExited {
            session: id(6),
            code: Some(2),
        },
        ObservedRetirementResult::KnownFailed {
            session: id(6),
            error: koru_session_protocol::ErrorCode::HostUnavailable,
        },
    ] {
        let observation = ObservedCreationRetirement {
            creation_key: id(31),
            host: HostInstanceId::from_uuid(uuid(60)),
            result,
            observed_at_ms: -9,
        };
        assert!(observation.matches_target(&DecisionCleanupTarget::Origin {
            fence: edit().origin(),
            creation_key: id(31)
        }));
        assert!(!observation.matches_target(&DecisionCleanupTarget::Origin {
            fence: edit().origin(),
            creation_key: id(32)
        }));
    }
    assert!(serde_json::from_str::<ObservedRetirementResult>(r#"{"result":"creating"}"#).is_err());
    assert!(
        serde_json::from_str::<ObservedRetirementResult>(
            r#"{"result":"known_running","session":"00000000-0000-4000-8000-000000000006"}"#
        )
        .is_err()
    );
}

#[test]
fn decision_wait_observation_keeps_historical_origin_and_final_consumer_distinct() {
    let i = intent();
    let key = i.key().unwrap();
    let origin = i.origin();
    let mut read = BotDecisionWaitObservation {
        key: key.clone(),
        cleanup_intent: i,
        current_revision: rev(5),
        fresh_stop_eligibility: FreshWaitStopEligibility::Superseded,
        origin_stop: OriginWaitStop::Confirmed {
            origin_fence: origin,
            wait_id: key.wait_id,
            intent_digest: key.receipt_digest,
            enrollment_digest: key.receipt_digest,
            cause: BotWaitCleanupCause::DecisionWaitCleanup,
            creation_retirement: ObservedCreationRetirement {
                creation_key: id(60),
                host: HostInstanceId::from_uuid(uuid(61)),
                result: ObservedRetirementResult::KnownEnded {
                    session: origin.session,
                },
                observed_at_ms: 3,
            },
            native_drain: DecisionNativeDrain {
                run: origin.run,
                attempt: origin.attempt,
                generation: origin.generation,
                observed_at_ms: 3,
                effects: DecisionList::default(),
            },
        },
        consumer_cleanup: ConsumerCleanup::Owed {
            cleanup_revision: rev(5),
            target: DecisionCleanupTarget::from_resume(&resume(true)).unwrap(),
        },
        disposition: DecisionDisposition::Open {
            state: DecisionState {
                kind: DecisionKind::KoruEdit,
                authorization: DecisionAuthorization::Expired,
                phase: DecisionPhase::StopPending,
                revision: rev(5),
            },
        },
        publication: DecisionPublication::Pending {
            latest_sequence: rev(4),
            target_digest: digest(7),
        },
    };
    assert_eq!(
        decode_decision_frame::<BotDecisionWaitObservation>(&encode_decision_frame(&read).unwrap())
            .unwrap(),
        read
    );
    // Origin stop is historical; it neither confirms nor hides the live consumer.
    assert!(matches!(
        read.consumer_cleanup,
        ConsumerCleanup::Owed { .. }
    ));
    if let OriginWaitStop::Confirmed { origin_fence, .. } = &mut read.origin_stop {
        origin_fence.turn = id(100);
    }
    assert!(read.validate().is_err());
}
