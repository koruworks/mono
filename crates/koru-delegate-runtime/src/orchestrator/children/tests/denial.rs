// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_child_private_denial` (P6.4's "private output denial", the
//! delegate's half: a child never holds a connection grant its parent
//! lacks) and `m6_events_content_free` (the runtime's half: one fact per
//! committed revision, none on replay, ids and closed tokens only).

use super::clock::count;
use super::*;
use crate::test_support::{granted_child_chain, private_proposal};

#[test]
fn m6_child_private_denial_parent_without_the_grant_child_gets_none() {
    let mut chain = child_chain();
    assert!(chain.parent.submission.policy.connections.is_empty());
    // The child the AI owner builds holds no connection.
    let (request, _) = asked(&mut chain, &[1]);
    assert!(request.requests[0].submission.policy.connections.is_empty());
    let admitted = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_admission(request.requests[0].binding.child_run)
        .unwrap()
        .unwrap();
    assert!(admitted.submission.policy.connections.is_empty());
}

#[test]
fn m6_child_private_denial_a_child_keeping_an_ungranted_connection_is_refused() {
    let mut chain = child_chain();
    let grant = private_proposal().grant();
    let request = chain
        .parent
        .ask_with(&mut chain.orchestrator, &[1, 2], |submission| {
            if submission.provenance.bot == recipient_bot(2) {
                submission.policy.connections.insert(grant.clone());
            }
        });
    match send(&mut chain, &request) {
        ChildRequestOutcome::Refused(refused) => {
            assert_eq!(refused.code, ChildRefusal::ConnectionsNotSubset);
            assert_eq!(refused.code.code(), "bot_child_connections");
            assert!(!refused.code.retryable());
            assert_eq!(refused.ordinal, Some(1));
        }
        other => panic!("not refused: {other:?}"),
    }
    // All or nothing: the first recipient was not admitted either.
    assert!(requests(&mut chain).is_empty());
    assert!(execution(&mut chain).is_none());
}

#[test]
fn m6_child_private_denial_a_granted_parent_may_pass_its_grant_on() {
    let mut chain = granted_child_chain();
    let grant = private_proposal().grant();
    assert!(chain.parent.submission.policy.connections.contains(&grant));
    let request = chain
        .parent
        .ask_with(&mut chain.orchestrator, &[1], |submission| {
            submission.policy.connections.insert(grant.clone());
        });
    assert!(matches!(
        send(&mut chain, &request),
        ChildRequestOutcome::Created(_)
    ));
}

#[test]
fn m6_events_content_free_one_fact_per_revision_none_on_replay() {
    let mut chain = child_chain();
    let events = ChildEvents::install(&mut chain.orchestrator);
    chain.parent.script_resume(
        &mut chain.sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    let (request, requested) = asked(&mut chain, &[1, 2]);
    let first = events.published();
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|(_, fact)| fact.requested
        && fact.state == ChildRequestState::Open
        && fact.settled_by.is_none()));
    // A replayed command publishes nothing.
    assert!(matches!(
        send(&mut chain, &request),
        ChildRequestOutcome::AlreadyCreated(_)
    ));
    assert_eq!(events.published().len(), 2);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    let mut bridge = chain.parent.enrollment_bridge();
    advance(&mut chain, &mut bridge);
    advance(&mut chain, &mut bridge);
    count(&mut chain, BOT_CHILD_WAIT);
    count(&mut chain, 30_000);
    let all = events.published();
    // Each request: its ask, and its settlement, each exactly once.
    for receipt in &requested.receipts {
        let mine: Vec<_> = all
            .iter()
            .filter(|(_, fact)| fact.request == receipt.child_request)
            .collect();
        assert!(mine.iter().any(|(_, fact)| fact.requested));
        let settled: Vec<_> = mine
            .iter()
            .filter(|(_, fact)| fact.settled_by == Some(ChildSettledBy::TimedOut))
            .collect();
        assert_eq!(settled.len(), 1, "one settlement fact");
        let revisions: std::collections::BTreeSet<_> =
            mine.iter().map(|(_, fact)| fact.revision).collect();
        assert_eq!(revisions.len(), mine.len(), "one fact per revision");
    }
    let ids: std::collections::BTreeSet<_> = all.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), all.len());
    // Ids and closed tokens only: the task text never reaches a fact.
    let text = format!("{all:?}");
    assert!(!text.contains("OBJECTIVE-SENTINEL"));
    assert!(!text.contains("fixture/recipient"));
    // The event id is derived from the request and the revision.
    for (id, fact) in &all {
        assert_eq!(*id, fact.event_id());
    }
}
