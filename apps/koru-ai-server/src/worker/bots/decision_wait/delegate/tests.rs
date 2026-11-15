// SPDX-License-Identifier: MPL-2.0
//! In-memory scripted delegate port only; no IPC, Store, provider or disk.
use super::*;
use koru_delegate_api::Failure;
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_core as d;
use koru_delegate_core::decisions::*;
use std::sync::Mutex;
use std::time::Instant;

struct Scripted {
    reply: Mutex<Option<Result<BotReply, Failure>>>,
    delay: Duration,
    seen: Mutex<Vec<Duration>>,
}
impl BotDelegatePort for Scripted {
    fn execute(&self, _: BotCommand) -> Result<BotReply, Failure> {
        unreachable!("the adapter always passes its bound")
    }
    fn execute_within(&self, command: BotCommand, within: Duration) -> Result<BotReply, Failure> {
        assert!(matches!(command, BotCommand::ReadBotDecisionWait { .. }));
        self.seen.lock().unwrap().push(within);
        std::thread::sleep(self.delay);
        self.reply.lock().unwrap().take().expect("one call")
    }
}
fn adapter(
    reply: Result<BotReply, Failure>,
    delay: Duration,
) -> (BotDelegateDecisionWait, Arc<Scripted>) {
    let port = Arc::new(Scripted {
        reply: Mutex::new(Some(reply)),
        delay,
        seen: Mutex::new(Vec::new()),
    });
    (BotDelegateDecisionWait::new(port.clone()), port)
}

fn observation() -> BotDecisionWaitObservation {
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: d::BotOwner {
            user_scope: koru_ai_core::bots::UserScopeId::new(),
            device: d::DeviceId::new(),
        },
        bot: d::BotId::mint(),
        run: d::RunId::mint(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(3).unwrap(),
        wait_revision: DecisionRevision::new(2).unwrap(),
        origin_attempt: d::RunAttemptId::mint(),
        origin_generation: d::AttemptGeneration::from_raw(1).unwrap(),
        origin_session: d::SessionId::mint(),
        origin_turn: d::AgentTurnId::mint(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    let key = intent.key().unwrap();
    BotDecisionWaitObservation {
        current_revision: key.wait_revision,
        fresh_stop_eligibility: FreshWaitStopEligibility::EligibleOriginWaitStop,
        origin_stop: OriginWaitStop::Pending {},
        consumer_cleanup: ConsumerCleanup::NotRequested {},
        disposition: DecisionDisposition::Open {
            state: DecisionState {
                kind: DecisionKind::KoruEdit,
                authorization: DecisionAuthorization::Pending,
                phase: DecisionPhase::Holding,
                revision: key.decision_revision,
            },
        },
        publication: DecisionPublication::Pending {
            latest_sequence: DecisionRevision::new(1).unwrap(),
            target_digest: DecisionDigest::from_bytes([4; 32]),
        },
        key,
        cleanup_intent: intent,
    }
}

#[test]
fn decision_wait_adapter_serves_both_reads_from_one_answer_and_maps_missing_exactly() {
    let observation = observation();
    let subject = DecisionWaitSubject::of(&observation.key);
    let within = Duration::from_secs(5);
    let (found, port) = adapter(
        Ok(BotReply::DecisionWait {
            observation: observation.clone(),
            resume: None,
        }),
        Duration::ZERO,
    );
    assert_eq!(
        found.read_decision_wait(&subject, within).unwrap(),
        Some(observation.clone())
    );
    assert_eq!(port.seen.lock().unwrap().as_slice(), &[within]);
    let (resume, _) = adapter(
        Ok(BotReply::DecisionWait {
            observation: observation.clone(),
            resume: None,
        }),
        Duration::ZERO,
    );
    assert_eq!(resume.read_decision_resume(&subject, within).unwrap(), None);
    // Only the delegate's authenticated Missing is absence, for both reads.
    for missing in [true, false] {
        let (port, _) = adapter(Ok(BotReply::Missing), Duration::ZERO);
        if missing {
            assert_eq!(port.read_decision_wait(&subject, within).unwrap(), None);
        } else {
            assert_eq!(port.read_decision_resume(&subject, within).unwrap(), None);
        }
    }
}

#[test]
fn decision_wait_adapter_never_turns_failure_foreign_or_late_into_missing() {
    let observation = observation();
    let subject = DecisionWaitSubject::of(&observation.key);
    let within = Duration::from_secs(5);
    let mut foreign = subject;
    foreign.decision_id = DecisionId::mint();
    let cases: Vec<(
        Result<BotReply, Failure>,
        DecisionWaitSubject,
        DecisionWaitPortError,
    )> = vec![
        (
            Err(Failure::unavailable()),
            subject,
            DecisionWaitPortError::Unavailable,
        ),
        (
            Ok(BotReply::Applied),
            subject,
            DecisionWaitPortError::Unavailable,
        ),
        (
            Ok(BotReply::DecisionWait {
                observation: observation.clone(),
                resume: None,
            }),
            foreign,
            DecisionWaitPortError::Unavailable,
        ),
        (
            Err(Failure::new("bot_fenced", "refused", false)),
            subject,
            DecisionWaitPortError::Refused(BotWaitRefusal::Forbidden),
        ),
        (
            Err(Failure::new("bot_stale", "refused", false)),
            subject,
            DecisionWaitPortError::Refused(BotWaitRefusal::Stale),
        ),
        (
            Err(Failure::new("bot_stale", "retry", true)),
            subject,
            DecisionWaitPortError::Unavailable,
        ),
    ];
    for (reply, subject, expected) in cases {
        let (port, _) = adapter(reply, Duration::ZERO);
        assert_eq!(
            port.read_decision_wait(&subject, within).unwrap_err(),
            expected
        );
    }
    // A late answer past the caller's remaining budget is unavailable.
    let (late, _) = adapter(Ok(BotReply::Missing), Duration::from_millis(120));
    let started = Instant::now();
    assert_eq!(
        late.read_decision_wait(&subject, Duration::from_millis(50))
            .unwrap_err(),
        DecisionWaitPortError::Unavailable
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // No budget: no call at all.
    let (none, port) = adapter(Ok(BotReply::Missing), Duration::ZERO);
    assert_eq!(
        none.read_decision_resume(&subject, Duration::ZERO)
            .unwrap_err(),
        DecisionWaitPortError::Unavailable
    );
    assert!(port.seen.lock().unwrap().is_empty());
}
