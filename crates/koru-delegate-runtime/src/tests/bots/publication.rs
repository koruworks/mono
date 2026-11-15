// SPDX-License-Identifier: MPL-2.0

//! Exact retained SQL controls with in-memory document/session ports. No Store
//! is opened by these cold cleanup and authority recovery scenarios.
use super::*;

fn waiting() -> (Harness, BotSubmission, BotExecutionFence) {
    let (mut h, submission, fence, policy, _) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    h.agent(
        fence.run,
        AgentEvent::TurnStarted {
            turn_id: fence.turn.unwrap(),
        },
    );
    let header = h.orchestrator.header(fence.run).unwrap();
    let state = h.orchestrator.open(fence.run).unwrap();
    let base = DecisionPublicationBase {
        workspace: state.workspace,
        document: state.document,
        source_schema: 3u8.try_into().unwrap(),
        observed_created_at_ms: header.created_at_ms,
        timeline_len: state.doc.document().unwrap().timeline_len() as u64,
        subject_digest: decision_projection_subject_digest(&header).unwrap(),
        predecessor: DecisionProjectionPredecessor {
            observed_created_at_ms: header.created_at_ms,
            timeline_start: state.doc.document().unwrap().timeline_len() as u64,
            projection: None,
            status: header.status,
            failure_reason: header.failure_reason,
        },
        bookkeeping: None,
    };
    let ActorRef::Person { profile } = submission.request.requested_by else {
        panic!("person")
    };
    let mut db = h.schedule_store.inner.lock().unwrap();
    let retained = db.native_policy(&policy.key).unwrap().unwrap();
    let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
        version: DecisionVersionV1,
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        origin_attempt: fence.attempt,
        origin_generation: fence.generation,
        origin_session: fence.session,
        origin_turn: fence.turn.unwrap(),
        submission: submission.provenance.submission,
        routine: None,
        human_profile: profile,
        workspace: submission.workspace,
        target: DocumentId::mint(),
        action: DecisionEditAction::EditDocument,
        operation_id: Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        expected_revision: DecisionBytes::new(vec![1]).unwrap(),
        payload_version: DecisionVersionV1,
        payload: DecisionBytes::new(b"private".to_vec()).unwrap(),
        captured_policy: submission.policy.clone(),
        global_policy: DecisionGlobalPolicy {
            key: policy.key.clone(),
            config_revision: DecisionRevision::new(policy.config_revision).unwrap(),
            policy_revision: DecisionRevision::new(policy.policy_revision).unwrap(),
            digest: DecisionDigest::from_bytes(retained.payload_digest),
        },
        routine_policy: None,
    });
    db.prepare_bot_decision(
        &binding,
        &FreshBotPolicyProof {
            global: policy,
            routine: None,
        },
        DelegateBootId::mint(),
        &base,
    )
    .unwrap();
    drop(db);
    // Simulate a cold actor without opening a replacement handle.
    h.orchestrator.runs.remove(&fence.run);
    (h, submission, fence)
}

fn latest(h: &Harness, fence: &BotExecutionFence) -> DecisionProjectionRecord {
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .latest_decision_projection(fence.owner, fence.run)
        .unwrap()
        .unwrap()
}

#[test]
fn decision_cold_header_restores_unopened_owner_without_opening_document() {
    let (mut h, _, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    assert!(
        h.orchestrator
            .header(fence.run)
            .unwrap_err()
            .publication_pending()
    );
    assert!(h.orchestrator.run_publication_pending(fence.run));
    assert!(h.orchestrator.runs[&fence.run].doc.document().is_err());
    assert!(h.orchestrator.runs[&fence.run].attempt.is_none());
    assert!(h.orchestrator.bot_budget_runs().contains(&fence.run));
}

#[test]
fn decision_cold_fresh_policy_refuses_before_provider_or_ordinary_open() {
    let (mut h, submission, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    let calls = h.sessions.lock().unwrap().calls.clone();
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&submission), 1, 1)
            .unwrap_err()
            .publication_pending()
    );
    assert_eq!(h.sessions.lock().unwrap().calls, calls);
}

#[test]
fn decision_stop_cold_retries_retained_cleanup_without_second_mutation() {
    let (mut h, submission, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .unwrap_err()
            .publication_pending()
    );
    let first = latest(&h, &fence);
    assert!(matches!(
        first.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::CleanupRequested {
                cause: DecisionClosureReason::Canceled,
                ..
            },
            ..
        }
    ));
    let creation = h.attempt(fence.run).provider_session.unwrap().idempotency;
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    h.orchestrator.runs.remove(&fence.run);
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .unwrap_err()
            .publication_pending()
    );
    assert_eq!(latest(&h, &fence), first);
    assert!(
        !h.attempt(fence.run).retired,
        "publication debt retains the admission"
    );
    assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
}

#[test]
fn decision_cleanup_unknown_write_keeps_exact_transition_and_stops_independently() {
    let (mut h, submission, fence) = waiting();
    h.ledger.lock().unwrap().fail = Some("decision_cleanup_before");
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    let transition = h.orchestrator.runs[&fence.run]
        .decision_cleanup
        .as_ref()
        .unwrap()
        .request
        .as_ref()
        .unwrap()
        .transition;
    let creation = h.attempt(fence.run).provider_session.unwrap().idempotency;
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert_eq!(latest(&h, &fence).identity.transition, transition);
}

#[test]
fn decision_cleanup_lost_commit_reply_recovers_same_transition_after_restart() {
    let (mut h, submission, fence) = waiting();
    h.ledger.lock().unwrap().fail = Some("decision_cleanup_after");
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    let first = latest(&h, &fence);
    h.orchestrator.runs.remove(&fence.run);
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert_eq!(latest(&h, &fence), first);
    assert!(
        h.orchestrator.runs[&fence.run]
            .decision_cleanup
            .as_ref()
            .unwrap()
            .request
            .is_none()
    );
}
