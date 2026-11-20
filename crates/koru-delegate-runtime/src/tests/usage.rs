// SPDX-License-Identifier: MPL-2.0

//! §17.2 — repeated, corrected and late usage reports — and §17.1's order:
//! the document first, then the queue, then the bus, then the
//! acknowledgement, with the same id on a retry.

use koru_delegate_core::{AmendmentRevision, Completeness, IncompleteReason, RunStatus};
use koru_session_protocol::{AgentUsage, CachedInputRelationship};

use super::*;

fn usage(input: u64, output: u64) -> AgentUsage {
    AgentUsage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_input_tokens: Some(0),
        cached_input: CachedInputRelationship::Included,
        model: Some("fixture-model".parse().unwrap()),
    }
}

#[test]
fn a_repeated_report_is_one_fact_and_a_correction_raises_the_revision() {
    let mut h = Harness::new("usage");
    let run = h.running(1);
    let t = turn(1);
    h.agent(run, AgentEvent::TurnStarted { turn_id: t });
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: t,
            usage: usage(100, 10),
        },
    );
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: t,
            usage: usage(100, 10),
        },
    );
    let facts = h.usage_facts(run);
    assert_eq!(facts.len(), 1, "a retry appends nothing");
    assert_eq!(facts[0].revision, AmendmentRevision::FIRST);
    assert_eq!(facts[0].completeness, Completeness::Complete);
    let published: Vec<_> = h
        .bus
        .lock()
        .unwrap()
        .published
        .iter()
        .filter(|(_, fact)| matches!(fact, RunFact::Usage { .. }))
        .map(|(event, _)| *event)
        .collect();
    assert_eq!(published.len(), 2);
    assert_eq!(published[0], published[1], "a retry reuses its EventId");
    // A correction: same identity, next revision, a new event.
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: t,
            usage: usage(140, 10),
        },
    );
    let facts = h.usage_facts(run);
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[1].identity, facts[0].identity);
    assert_eq!(facts[1].revision, AmendmentRevision(1));
    let published: Vec<_> = h
        .bus
        .lock()
        .unwrap()
        .published
        .iter()
        .filter(|(_, fact)| matches!(fact, RunFact::Usage { .. }))
        .map(|(event, _)| *event)
        .collect();
    assert_ne!(published[2], published[0]);
    // The header's summary is the highest revision, however it arrived.
    let header = h.orchestrator.header(run).unwrap();
    assert_eq!(header.economics.input.complete(), Some(140));
    assert_eq!(header.economics.turns_with_usage, 1);
    assert_eq!(header.economics.models.len(), 1);
    assert!(header.economics.complete);
}

#[test]
fn a_late_report_after_completion_is_applied_never_reopened() {
    let mut h = Harness::new("late");
    let run = h.running(2);
    let t = turn(1);
    h.agent(run, AgentEvent::TurnStarted { turn_id: t });
    h.registrations.lock().unwrap().next = vec![artifact(1)];
    h.agent(run, AgentEvent::TurnCompleted { turn_id: t });
    assert_eq!(h.status(run), RunStatus::Completed);
    let steps = h.steps(run).len();
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: t,
            usage: usage(90, 9),
        },
    );
    assert_eq!(h.status(run), RunStatus::Completed, "never reopened");
    assert_eq!(h.steps(run).len(), steps, "no step");
    let facts = h.usage_facts(run);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].input, Some(90));
    let header = h.orchestrator.header(run).unwrap();
    assert_eq!(header.economics.input.complete(), Some(90));
    // The economics summary was refreshed in place.
    assert_eq!(header.economics.turns_with_usage, 1);
}

#[test]
fn an_undeclared_cached_input_relationship_is_recorded_incomplete_never_guessed() {
    let mut h = Harness::new("undeclared");
    let run = h.running(3);
    let t = turn(1);
    h.agent(run, AgentEvent::TurnStarted { turn_id: t });
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: t,
            usage: AgentUsage {
                input_tokens: Some(50),
                output_tokens: Some(5),
                cached_input_tokens: Some(20),
                cached_input: CachedInputRelationship::Undeclared,
                model: None,
            },
        },
    );
    let facts = h.usage_facts(run);
    assert_eq!(
        facts[0].completeness,
        Completeness::Incomplete {
            reason: IncompleteReason::CachedInputRelationshipUndeclared
        }
    );
    assert!(!h.orchestrator.header(run).unwrap().economics.complete);
    // Nothing reported at all is unavailable, not zero.
    h.agent(
        run,
        AgentEvent::Usage {
            turn_id: turn(2),
            usage: AgentUsage {
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                cached_input: CachedInputRelationship::Included,
                model: None,
            },
        },
    );
    let facts = h.usage_facts(run);
    assert_eq!(
        facts[1].completeness,
        Completeness::Incomplete {
            reason: IncompleteReason::NoUsageReported
        }
    );
    let header = h.orchestrator.header(run).unwrap();
    assert_eq!(header.economics.input.unavailable_turns, 1);
    assert_eq!(header.economics.input.known, 50);
    // Neither report named a model: one identity, an explicit unknown for
    // what was reported beside the model that was requested.
    assert_eq!(header.economics.models.len(), 1);
    let models = h
        .orchestrator
        .open(run)
        .unwrap()
        .doc
        .document()
        .models()
        .unwrap();
    let identity = models.values().next().unwrap();
    assert_eq!(identity.reported, koru_delegate_core::ModelReport::Unknown);
    assert!(matches!(
        identity.requested,
        koru_delegate_core::ModelReport::Named { .. }
    ));
}

#[test]
fn a_fact_is_committed_then_queued_then_published_then_acknowledged() {
    let mut h = Harness::new("order");
    let run = h.running(4);
    h.ledger.lock().unwrap().ops.clear();
    let commits_before = h.documents.lock().unwrap().commits;
    h.agent(run, AgentEvent::TurnStarted { turn_id: turn(1) });
    let ops = h.ops();
    // The document commit is not a ledger op; it happened before the
    // first ledger op of this event, which is the enqueue.
    assert_eq!(ops, vec!["enqueue", "acknowledge"]);
    assert!(h.documents.lock().unwrap().commits > commits_before);
    assert!(
        h.orchestrator
            .ports()
            .store
            .open_publications(10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_lost_publication_stays_queued_and_is_retried_under_the_same_id() {
    let mut h = Harness::new("retry");
    let run = h.running(5);
    h.bus.lock().unwrap().fail_next = Some(PublishFailure { retryable: true });
    h.agent(run, AgentEvent::TurnStarted { turn_id: turn(1) });
    let open = h.orchestrator.ports().store.open_publications(10).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].attempts, 1);
    let queued = open[0].event;
    // The step is in the document regardless: the document is the
    // authority, the bus is how facts travel.
    assert!(
        h.steps(run)
            .contains(&koru_delegate_core::StepKind::TurnStarted {
                originator: koru_delegate_core::TurnOriginator::Orchestrator
            })
    );
    assert_eq!(h.orchestrator.retry_publications(10).unwrap(), 1);
    assert!(
        h.orchestrator
            .ports()
            .store
            .open_publications(10)
            .unwrap()
            .is_empty()
    );
    let published = h.bus.lock().unwrap().published.clone();
    assert!(
        published.iter().any(|(event, fact)| {
            *event == queued && matches!(fact, RunFact::TurnStarted { .. })
        })
    );
    // A refusal the bus will never take is acknowledged unpublished.
    h.bus.lock().unwrap().fail_next = Some(PublishFailure { retryable: false });
    h.agent(run, AgentEvent::WaitingForInput { turn_id: turn(1) });
    assert!(
        h.orchestrator
            .ports()
            .store
            .open_publications(10)
            .unwrap()
            .is_empty()
    );
}
