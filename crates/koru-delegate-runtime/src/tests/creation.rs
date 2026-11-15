// SPDX-License-Identifier: MPL-2.0

//! §7.1's four steps, in order, with a failure injected at each.

use koru_delegate_core::{FailureStage, RunStatus, StepKind};
use koru_delegate_sqlite::IntentState;

use super::*;

/// §17.1, the half a slow disk exposes: the fact is queued under its id the
/// moment the document is committed, and it travels only once the host has
/// called that commit durable. The actor does not wait for that; it polls,
/// and `retry_publications` is what delivers.
#[test]
fn a_fact_waits_for_its_document_to_be_durable_and_then_travels() {
    let mut h = Harness::new("durable-first");
    let run = run_id(1);
    h.documents.lock().unwrap().not_durable = true;
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert!(
        h.facts().is_empty(),
        "nothing travels on a commit the host has not called durable"
    );
    assert_eq!(
        h.orchestrator
            .ports()
            .store
            .open_publications(8)
            .unwrap()
            .len(),
        1
    );
    assert!(h.orchestrator.has_unsettled());
    // Still in flight: the retry tick delivers nothing either.
    assert_eq!(h.orchestrator.retry_publications(8).unwrap(), 0);
    assert!(h.facts().is_empty());
    // Settled — the acknowledgement arrived — and the queued fact goes.
    h.orchestrator.settle(run).unwrap();
    assert!(!h.orchestrator.has_unsettled());
    assert_eq!(h.orchestrator.retry_publications(8).unwrap(), 1);
    assert!(matches!(h.facts().as_slice(), [RunFact::Created { .. }]));
    assert!(
        h.orchestrator
            .ports()
            .store
            .open_publications(8)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_new_fact_drains_earlier_facts_after_the_document_becomes_durable() {
    let mut h = Harness::without_folders();
    let run = run_id(1);
    h.documents.lock().unwrap().not_durable = true;
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    h.orchestrator.settle(run).unwrap();
    // No retry tick between the durable acknowledgement and the next fact.
    h.orchestrator.cancel(run).unwrap();
    assert!(matches!(
        h.facts().as_slice(),
        [RunFact::Created { .. }, RunFact::Cancelled { .. }]
    ));
}

#[test]
fn another_runs_backlog_does_not_delay_a_new_fact() {
    let mut h = Harness::without_folders();
    let run = run_id(1);
    h.documents.lock().unwrap().not_durable = true;
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    let first = h
        .orchestrator
        .ports()
        .store
        .open_publications(1)
        .unwrap()
        .remove(0);
    // More than one drain's capacity, all waiting behind the first save.
    for _ in 0..64 {
        h.orchestrator
            .ports_mut()
            .store
            .enqueue(EventId::mint(), run, first.kind, first.body.as_str())
            .unwrap();
    }
    h.orchestrator.settle(run).unwrap();
    h.orchestrator
        .create(request(run_id(2), h.scope()), workspace())
        .unwrap();
    assert!(matches!(h.facts().as_slice(), [RunFact::Created { .. }]));
    let pending = h.orchestrator.ports().store.open_publications(100).unwrap();
    assert_eq!(pending.len(), 65);
    assert!(pending.iter().all(|publication| publication.run == run));
}

#[test]
fn a_failed_fact_holds_later_facts_for_that_run_only() {
    let mut h = Harness::without_folders();
    let run = run_id(1);
    let other = run_id(2);
    h.documents.lock().unwrap().not_durable = true;
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    h.orchestrator.cancel(run).unwrap();
    h.orchestrator
        .create(request(other, h.scope()), workspace())
        .unwrap();
    h.orchestrator.settle(run).unwrap();
    h.orchestrator.settle(other).unwrap();
    h.bus.lock().unwrap().fail_next = Some(PublishFailure { retryable: true });
    // One Run per maintenance turn (the approved rotating selection, lowest
    // key first). The first turn serves `run`; its failed head holds its
    // later fact, and nothing is published out of order.
    h.orchestrator.retry_publications(8).unwrap();
    assert!(
        h.facts().is_empty(),
        "a failed head holds that Run's later facts"
    );
    let open = h.orchestrator.ports().store.open_publications(8).unwrap();
    assert_eq!(open.len(), 3);
    // Bounded progress, never an unbounded loop: the unrelated Run publishes
    // on the very next turn, and the held Run drains in FIFO order on the
    // turn after that. Two Runs with open work need at most two more turns.
    const TURNS: usize = 2;
    let mut other_published_at = None;
    for turn in 1..=TURNS {
        h.orchestrator.retry_publications(8).unwrap();
        if other_published_at.is_none() && h.facts().iter().any(|fact| fact.run() == other) {
            other_published_at = Some(turn);
        }
    }
    assert_eq!(
        other_published_at,
        Some(1),
        "another Run's failure delays an unrelated Run by at most one turn"
    );
    assert!(
        h.orchestrator
            .ports()
            .store
            .open_publications(8)
            .unwrap()
            .is_empty()
    );
    let held: Vec<_> = h
        .facts()
        .into_iter()
        .filter(|fact| fact.run() == run)
        .collect();
    assert!(matches!(
        held.as_slice(),
        [RunFact::Created { .. }, RunFact::Cancelled { .. }]
    ));
    assert_eq!(h.facts().len(), 3);
}

#[test]
fn creation_is_intent_then_capture_then_document_then_satisfied() {
    let mut h = Harness::new("order");
    let run = run_id(1);
    let created = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert_eq!(created.run, run);
    let ops = h.ops();
    let intent = ops.iter().position(|op| *op == "record_intent").unwrap();
    let satisfied = ops.iter().position(|op| *op == "satisfy_intent").unwrap();
    assert!(intent < satisfied, "{ops:?}");
    // The document exists between the two, and the fact was published
    // after the document was committed and before the intent was
    // satisfied.
    let record = h.orchestrator.ports().store.intent(run).unwrap().unwrap();
    assert_eq!(record.state, IntentState::Satisfied);
    assert_eq!(record.run_document, Some(created.document));
    assert_eq!(h.status(run), RunStatus::Created);
    assert_eq!(h.steps(run), vec![StepKind::RunCreated]);
    assert!(matches!(
        h.facts().as_slice(),
        [RunFact::Created {
            context_items: 1,
            ..
        }]
    ));
    // A retry is the same run.
    let again = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert_eq!(again, created);
    assert_eq!(h.documents.lock().unwrap().snapshots.len(), 1);
}

#[test]
fn satisfied_create_retry_is_validated_before_scope_preflight() {
    let mut h = Harness::new("create-idempotent-preflight");
    let run = run_id(0x71);
    let chosen = request(run, h.scope());
    let created = h.orchestrator.create(chosen.clone(), workspace()).unwrap();
    h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    let creates = h
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Create { .. }))
        .count();

    assert_eq!(
        h.orchestrator
            .existing_create(&chosen, workspace())
            .unwrap(),
        CreateIntentStatus::Satisfied(created)
    );
    assert_eq!(
        h.calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        creates,
        "an exact retry returns the durable answer without a second start"
    );

    let mut changed = chosen;
    changed.objective = koru_delegate_core::Objective::new("different payload").unwrap();
    let error = h
        .orchestrator
        .existing_create(&changed, workspace())
        .unwrap_err();
    assert!(matches!(error, CreateError::Port(ref port) if port.code == "intent_conflict"));
}

#[test]
fn a_capture_failure_abandons_the_request_naming_the_item_and_no_document_exists() {
    let mut h = Harness::new("capture-fails");
    let run = run_id(2);
    h.capture.lock().unwrap().fail_capture = Some(CaptureFailure {
        item: 0,
        stage: CaptureStage::Commit,
    });
    let error = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap_err();
    assert_eq!(
        error,
        CreateError::Capture(CaptureFailure {
            item: 0,
            stage: CaptureStage::Commit
        })
    );
    let record = h.orchestrator.ports().store.intent(run).unwrap().unwrap();
    assert_eq!(record.state, IntentState::Abandoned);
    assert!(record.run_document.is_none());
    assert!(h.documents.lock().unwrap().snapshots.is_empty());
    assert!(h.facts().is_empty());
    assert!(!h.ops().contains(&"satisfy_intent"));
    // A request is not a run: it is listed under no workspace.
    assert!(h.orchestrator.list(workspace()).unwrap().is_empty());
}

#[test]
fn a_document_failure_leaves_the_intent_pending_for_a_restart() {
    let mut h = Harness::new("document-fails");
    let run = run_id(3);
    h.documents.lock().unwrap().fail_create = true;
    let mut chosen = request(run, h.scope());
    chosen.requested_model =
        Some(koru_delegate_core::ModelName::new("deepseek/deepseek-chat").unwrap());
    let error = h
        .orchestrator
        .create(chosen.clone(), workspace())
        .unwrap_err();
    assert!(matches!(error, CreateError::Port(_)), "{error:?}");
    let record = h.orchestrator.ports().store.intent(run).unwrap().unwrap();
    assert_eq!(record.state, IntentState::Pending);
    assert_eq!(
        h.orchestrator
            .existing_create(&chosen, workspace())
            .unwrap(),
        CreateIntentStatus::Pending,
        "the host must complete this durable request before gating its eventual start"
    );
    // The restart completes it forward from its own payload.
    h.documents.lock().unwrap().fail_create = false;
    let report = h.orchestrator.restart().unwrap();
    assert_eq!(report.completed_forward, vec![run]);
    assert_eq!(
        h.orchestrator.header(run).unwrap().requested_model,
        chosen.requested_model
    );
    let record = h.orchestrator.ports().store.intent(run).unwrap().unwrap();
    assert_eq!(record.state, IntentState::Satisfied);
    // And a run that exists and never started is started by the restart.
    assert_eq!(report.started.len(), 1);
    assert!(matches!(h.status(run), RunStatus::Running { .. }));
}

#[test]
fn a_satisfy_failure_after_the_document_is_retried_as_the_same_run() {
    let mut h = Harness::new("satisfy-fails");
    let run = run_id(4);
    h.ledger.lock().unwrap().fail = Some("satisfy_intent");
    let error = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap_err();
    assert!(matches!(error, CreateError::Port(_)));
    assert_eq!(h.documents.lock().unwrap().snapshots.len(), 1);
    let created = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert_eq!(
        h.documents.lock().unwrap().snapshots.len(),
        1,
        "one document, not two"
    );
    let record = h.orchestrator.ports().store.intent(run).unwrap().unwrap();
    assert_eq!(record.run_document, Some(created.document));
}

#[test]
fn a_start_binds_the_key_before_the_host_is_asked_and_submits_the_preamble_then_the_objective() {
    let mut h = Harness::new("start");
    let run = h.running(5);
    let ops = h.ops();
    let bound = ops.iter().position(|op| *op == "bind_session:key").unwrap();
    let running = ops.iter().position(|op| *op == "advance:running").unwrap();
    assert!(bound < running, "{ops:?}");
    let calls = h.calls();
    let create = calls
        .iter()
        .position(|call| matches!(call, Call::Create { .. }))
        .unwrap();
    let subscribe = calls
        .iter()
        .position(|call| matches!(call, Call::Subscribe(_)))
        .unwrap();
    let submit = calls
        .iter()
        .position(|call| matches!(call, Call::Submit { .. }))
        .unwrap();
    assert!(create < subscribe && subscribe < submit, "{calls:?}");
    // The ledger row and the document agree: attempt running, run running.
    let row = h.attempt(run);
    assert_eq!(row.phase, AttemptPhase::Running);
    assert_eq!(row.generation, AttemptGeneration::FIRST);
    let bound_key = row.provider_session.unwrap();
    assert_eq!(bound_key.session, Some(session(1)));
    assert!(matches!(
        calls[create],
        Call::Create { key, .. } if key == bound_key.idempotency
    ));
    assert_eq!(h.status(run), RunStatus::running());
    assert_eq!(h.steps(run), vec![StepKind::RunCreated, StepKind::Started]);
    let Call::Submit { text, .. } = &calls[submit] else {
        unreachable!()
    };
    let preamble = text.find(PREAMBLE_SENTINEL).unwrap();
    let run_line = text.find(&format!("This run is `{run}`")).unwrap();
    let objective = text.find(OBJECTIVE_SENTINEL).unwrap();
    assert!(preamble < run_line && run_line < objective);
    assert!(text.contains(".artifacts") && text.contains(".koru"));
    assert_eq!(h.capture.lock().unwrap().materialized.len(), 1);
}

/// **An edited preamble takes effect on the next run, and only a run.**
///
/// The file is resolved inside `start`, once per start, rather than read
/// into the orchestrator when the process came up — so a person who edits
/// `docs/prompts/delegated-agent.md` does not restart Koru, and a developer
/// editing the repository's copy does not rebuild. The resolver here
/// numbers its answers, which is the only way to tell *read again* from
/// *read once and remembered*.
#[test]
fn every_start_resolves_the_preamble_again_so_an_edit_needs_no_restart() {
    let mut h = Harness::new("preamble-per-start");
    h.running(80);
    h.running(81);
    assert_eq!(
        *h.preambles.lock().unwrap(),
        2,
        "one read per start, not one per process"
    );
    let submissions = h.submissions();
    assert_eq!(submissions.len(), 2, "{submissions:?}");
    assert!(submissions[0].contains("(read 1)"), "{}", submissions[0]);
    assert!(
        submissions[1].contains("(read 2)"),
        "the second run was handed the preamble as it stood at its own start"
    );
}

#[test]
fn a_busy_scope_warns_and_proceeds_and_records_the_holder() {
    let mut h = Harness::new("busy");
    let first = h.running(6);
    let first_attempt = h.attempt(first).attempt;
    let second = h.running(7);
    let steps = h.steps(second);
    assert!(
        steps.contains(&StepKind::ScopeBusy { holders: 1 }),
        "{steps:?}"
    );
    assert!(matches!(h.status(second), RunStatus::Running { .. }));
    let view = h.orchestrator.view(second, None, 0, usize::MAX).unwrap();
    let busy = view
        .timeline
        .items
        .iter()
        .find_map(|item| match item {
            WindowItem::Step(entry) if matches!(entry.kind, StepKind::ScopeBusy { .. }) => {
                Some(entry)
            }
            _ => None,
        })
        .unwrap();
    assert!(busy.refs.iter().any(|reference| matches!(
        reference,
        koru_delegate_core::TimelineRef::Attempt { attempt } if *attempt == first_attempt
    )));
    // One holder, so the fact names it and counts no others — what the
    // `delegate.scope.busy` row spells.
    assert!(h.facts().iter().any(|fact| matches!(
        fact,
        RunFact::ScopeBusy {
            holder_attempt,
            others: 0,
            ..
        } if *holder_attempt == first_attempt
    )));
    assert_eq!(h.orchestrator.claims().len(), 2);
}

#[test]
fn a_refused_scope_fails_the_start_with_its_stage_and_ends_the_attempt() {
    let mut h = Harness::new("refused");
    let run = run_id(8);
    h.orchestrator
        .create(
            request(
                run,
                WorkspaceScope::ProjectRoot {
                    project: project(9),
                },
            ),
            workspace(),
        )
        .unwrap();
    let started = h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    assert!(started.events.is_none());
    assert_eq!(
        started.status,
        RunStatus::Failed {
            stage: FailureStage::ScopeAdmission
        }
    );
    let row = h.attempt(run);
    assert_eq!(row.phase, AttemptPhase::Ended);
    assert!(row.retired);
    assert!(h.calls().is_empty(), "nothing asked the host");
    assert!(h.orchestrator.claims().is_empty());
}

#[test]
fn a_manual_start_consumes_the_one_scope_token_admission_checked() {
    let mut h = Harness::new("manual-resolved-scope");
    let run = run_id(82);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    let admitted =
        koru_delegate_scope::ResolvedScope::of_directory(&h.dir.root.join("project")).unwrap();
    let expected = admitted.directory.clone();

    h.orchestrator
        .start_resolved(run, admitted, StartOrigin::Manual)
        .unwrap();
    assert_eq!(
        h.authority.lock().unwrap().issued[0].run_workspace,
        expected,
        "launch consumes the canonical directory paired with the checked identity"
    );
}

#[test]
fn a_changed_manual_scope_after_202_fails_visibly_before_materialization() {
    let mut h = Harness::new("manual-resolved-scope-changed");
    let run = run_id(83);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    let expected =
        koru_delegate_scope::ResolvedScope::of_directory(&h.dir.root.join("project")).unwrap();
    let changed = h.dir.root.join("changed");
    std::fs::create_dir_all(&changed).unwrap();

    let started = h
        .orchestrator
        .start_resolved(
            run,
            koru_delegate_scope::ResolvedScope {
                identity: expected.identity,
                directory: changed,
            },
            StartOrigin::Manual,
        )
        .unwrap();
    assert_eq!(
        started.status,
        RunStatus::Failed {
            stage: FailureStage::ScopeAdmission
        }
    );
    assert!(h.capture.lock().unwrap().materialized.is_empty());
    assert!(h.calls().is_empty());
}

#[test]
fn a_materialization_failure_fails_the_start_before_the_host_is_asked() {
    let mut h = Harness::new("materialize");
    let run = run_id(9);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    h.capture.lock().unwrap().fail_materialize = Some(MaterializeFailure {
        item: 0,
        stage: MaterializeStage::Write,
    });
    let started = h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    assert_eq!(
        started.status,
        RunStatus::Failed {
            stage: FailureStage::ContextMaterialization
        }
    );
    assert!(h.calls().is_empty());
    assert!(!h.ops().contains(&"bind_session:key"));
}

#[test]
fn a_session_creation_failure_is_failed_at_session_creation() {
    let mut h = Harness::new("create-fails");
    let run = run_id(10);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    h.sessions.lock().unwrap().create = None;
    let started = h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    assert_eq!(
        started.status,
        RunStatus::Failed {
            stage: FailureStage::SessionCreation
        }
    );
    // The key was bound before the ask, so a restart could have looked it
    // up; the attempt is ended and retired.
    let row = h.attempt(run);
    assert!(row.provider_session.is_some());
    assert!(row.retired);
    assert!(h.orchestrator.claims().is_empty());
}

#[test]
fn a_host_without_the_delegation_family_is_refused_before_anything_starts() {
    let mut h = Harness::new("not-delegatable");
    let run = run_id(11);
    let chosen = request(run, h.scope());
    h.orchestrator.create(chosen.clone(), workspace()).unwrap();
    h.sessions.lock().unwrap().supports_delegation = false;
    let error = h.orchestrator.start(run, StartOrigin::Manual).unwrap_err();
    assert_eq!(error, RunError::NotDelegatable);
    assert_eq!(h.status(run), RunStatus::Created);
    assert!(
        h.orchestrator
            .ports()
            .store
            .current_attempt(run)
            .unwrap()
            .is_none()
    );
    h.sessions.lock().unwrap().supports_delegation = true;
    assert!(matches!(
        h.orchestrator
            .existing_create(&chosen, workspace())
            .unwrap(),
        CreateIntentStatus::Satisfied(_)
    ));
    h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    assert!(matches!(h.status(run), RunStatus::Running { .. }));
    assert_eq!(
        h.calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn a_cancel_before_the_start_is_cancelled_without_a_host() {
    let mut h = Harness::new("cancel-created");
    let run = run_id(12);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    let header = h.orchestrator.cancel(run).unwrap();
    assert_eq!(header.status, RunStatus::Cancelled);
    assert!(h.calls().is_empty());
    assert!(h.orchestrator.start(run, StartOrigin::Manual).is_err());
    // Idempotent.
    assert_eq!(
        h.orchestrator.cancel(run).unwrap().status,
        RunStatus::Cancelled
    );
}

#[test]
fn refusal_after_session_creation_fails_start_and_releases_its_resources() {
    for refuse_attach in [false, true] {
        let mut h = Harness::new("post-create-refusal");
        let run = run_id(13);
        h.orchestrator
            .create(request(run, h.scope()), workspace())
            .unwrap();
        {
            let mut sessions = h.sessions.lock().unwrap();
            if refuse_attach {
                sessions.attach_fails = true;
            } else {
                sessions.turns.clear();
            }
        }
        let started = h.orchestrator.start(run, StartOrigin::Manual).unwrap();
        assert_eq!(
            started.status,
            RunStatus::Failed {
                stage: FailureStage::ProviderStart
            }
        );
        assert_eq!(h.status(run), started.status);
        assert!(started.events.is_none());
        let row = h.attempt(run);
        assert_eq!(row.phase, AttemptPhase::Ended);
        assert!(row.retired);
        assert!(h.orchestrator.claims().is_empty());
        assert!(h.calls().contains(&Call::Terminate(session(1))));
        assert_eq!(h.capture.lock().unwrap().cleaned.len(), 1);
        assert!(!h.ops().contains(&"advance:running"));
        assert!(!h.steps(run).contains(&StepKind::Started));
        assert_eq!(
            h.steps(run),
            vec![
                StepKind::RunCreated,
                StepKind::Failed {
                    stage: FailureStage::ProviderStart,
                    reason: Some(koru_delegate_core::RunFailureReason::Unknown),
                }
            ]
        );
        assert!(
            !h.facts()
                .iter()
                .any(|fact| matches!(fact, RunFact::Started { .. }))
        );
    }
}

#[test]
fn selected_model_capability_refusal_is_retained_atomically_without_provider_text() {
    use koru_delegate_core::RunFailureReason;
    let mut h = Harness::new("safe-creation-failure");
    let run = run_id(49);
    let created = h
        .orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    h.sessions.lock().unwrap().create_error = Some("capability_not_negotiated");
    h.orchestrator.start(run, StartOrigin::Manual).unwrap();
    let expected = StepKind::Failed {
        stage: FailureStage::SessionCreation,
        reason: Some(RunFailureReason::CapabilityNotNegotiated),
    };
    assert_eq!(
        h.steps(run)
            .iter()
            .filter(|step| matches!(step, StepKind::Failed { .. }))
            .count(),
        1
    );
    assert!(h.steps(run).contains(&expected));
    let before = h.documents.lock().unwrap().commits;
    assert!(
        h.orchestrator
            .apply_with_failure_reason(
                run,
                &koru_delegate_core::RunEvent::StartFailed {
                    stage: FailureStage::SessionCreation
                },
                None
            )
            .is_err()
    );
    assert_eq!(h.documents.lock().unwrap().commits, before);
    let documents = h.documents.lock().unwrap();
    for bytes in &documents.committed_snapshots {
        let doc = RunDocument::from_snapshot(bytes).unwrap();
        if matches!(doc.header().unwrap().status, RunStatus::Failed { .. }) {
            assert_eq!(
                doc.header().unwrap().failure_reason,
                Some(RunFailureReason::CapabilityNotNegotiated)
            );
            let deep = format!("{:?}", doc.doc().get_deep_value());
            assert!(deep.contains("capability_not_negotiated"));
            assert!(deep.contains("failed"));
            assert!(!deep.contains("SECRET"));
            assert_eq!(doc.timeline_len(), 2);
        }
    }
    let reopened = RunDocument::from_snapshot(&documents.snapshots[&created.document]).unwrap();
    assert_eq!(
        reopened.header().unwrap().failure_reason,
        Some(RunFailureReason::CapabilityNotNegotiated)
    );
}
