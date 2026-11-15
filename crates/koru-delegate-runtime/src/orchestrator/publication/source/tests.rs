// SPDX-License-Identifier: MPL-2.0

//! Pure frontier qualification. No ledger, document, session or Store.

use super::*;
use koru_delegate_core::{BotId, BotOwner, DeviceId, RunId};

fn sequence(value: u64) -> StreamSequence {
    StreamSequence::new(value).unwrap()
}

fn consumer() -> ConsumerIdentity {
    (
        RunAttemptId::mint(),
        AttemptGeneration::FIRST,
        SessionId::mint(),
    )
}

fn bookkeeping(identity: ConsumerIdentity, agent: u64, session: u64) -> DecisionSourceBookkeeping {
    DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: DecisionFence {
                owner: BotOwner {
                    user_scope: koru_store_core::UserScopeId::from_uuid(uuid::Uuid::new_v4()),
                    device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
                },
                bot: BotId::mint(),
                run: RunId::mint(),
                attempt: identity.0,
                generation: identity.1,
                session: identity.2,
                turn: AgentTurnId::mint(),
            },
            producer_epoch: uuid::Uuid::new_v4(),
            stream: DecisionSourceStream::Agent,
            sequence: sequence(agent),
            resulting: DecisionSourceFrontiers {
                agent_after: Some(sequence(agent)),
                session_after: Some(sequence(session)),
            },
        },
        unflushed: DecisionList::new(Vec::new()).unwrap(),
        usage: None,
        allocated_steps: DecisionList::new(Vec::new()).unwrap(),
    }
}

fn empty() -> DecisionSourceFrontiers {
    DecisionSourceFrontiers {
        agent_after: None,
        session_after: None,
    }
}

#[test]
fn decision_source_high_old_cursor_never_refuses_low_successor_cursor() {
    let origin = consumer();
    let successor = consumer();
    let retained = bookkeeping(origin, 100, 40);
    // The applied marker still names the origin's high cursor. The current
    // resume names a successor: its cursor starts empty, not at 100.
    let frontiers = resume_frontiers(Some(&retained), successor, Some(origin), &empty());
    assert_eq!(frontiers, empty());
    let accepted = advance_frontier(&frontiers, DecisionSourceStream::Agent, sequence(3))
        .expect("a successor's low sequence is its first accepted boundary");
    assert_eq!(accepted.agent_after, Some(sequence(3)));
    assert_eq!(accepted.session_after, None);
    // The same retained cursor still fences the origin consumer itself.
    let origin_frontiers = resume_frontiers(Some(&retained), origin, None, &empty());
    assert!(
        advance_frontier(&origin_frontiers, DecisionSourceStream::Agent, sequence(3)).is_none()
    );
    assert!(
        advance_frontier(
            &origin_frontiers,
            DecisionSourceStream::Agent,
            sequence(100)
        )
        .is_none()
    );
}

#[test]
fn decision_source_same_consumer_reconnect_is_monotonic_per_stream() {
    let current = consumer();
    let memory = DecisionSourceFrontiers {
        agent_after: Some(sequence(9)),
        session_after: Some(sequence(4)),
    };
    // First entry without a Run5 source boundary uses the ordinary accepted
    // pair of exactly this consumer, never an empty reset over its progress.
    let frontiers = resume_frontiers(None, current, Some(current), &memory);
    assert_eq!(frontiers, memory);
    // A retained boundary of the same consumer wins over working memory.
    let retained = bookkeeping(current, 12, 4);
    let frontiers = resume_frontiers(Some(&retained), current, Some(current), &memory);
    assert_eq!(frontiers.agent_after, Some(sequence(12)));
    // Session advances alone; Agent is untouched and cannot move backwards.
    let session = advance_frontier(&frontiers, DecisionSourceStream::Session, sequence(7)).unwrap();
    assert_eq!(session.agent_after, Some(sequence(12)));
    assert_eq!(session.session_after, Some(sequence(7)));
    assert!(advance_frontier(&session, DecisionSourceStream::Session, sequence(7)).is_none());
    // A noncontiguous later sequence is legal; only an explicit gap is loss.
    assert!(advance_frontier(&session, DecisionSourceStream::Agent, sequence(40)).is_some());
    // Another consumer's working memory never supplies this one's cursor.
    assert_eq!(
        resume_frontiers(None, current, Some(consumer()), &memory),
        empty()
    );
}
