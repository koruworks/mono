// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
struct ScriptedAgent {
    history: Vec<Uuid>,
    work_seen: BTreeSet<Uuid>,
}
impl ScriptedAgent {
    fn receive(&mut self, bytes: &[u8]) -> Delivery {
        let delivery: Delivery = decode(bytes).unwrap();
        self.history.push(delivery.id);
        for message in &delivery.work {
            assert!(
                self.work_seen.insert(message.id),
                "known resumed work was sent twice"
            );
        }
        delivery
    }
}
struct FakeWorker {
    incarnation: Uuid,
    session: Uuid,
}
impl FakeWorker {
    fn new() -> Self {
        Self {
            incarnation: Uuid::new_v4(),
            session: Uuid::new_v4(),
        }
    }
    fn drive(
        &self,
        f: &mut Fixture,
        agent: &mut ScriptedAgent,
        drop_acceptance: bool,
        resume: Resume,
    ) -> u64 {
        let input =
            f.db.claim_messages(
                f.bot,
                f.thread,
                self.incarnation,
                self.session,
                resume,
                16384,
                2,
            )
            .unwrap()
            .unwrap();
        let bytes = encode(&input).unwrap();
        assert!(bytes.len() <= 16384);
        let received = agent.receive(&bytes);
        if !drop_acceptance {
            f.db.delivery_accepted(received.turn, received.id).unwrap();
        }
        f.db.publish_reply(&reply(&received), 3)
            .unwrap()
            .answer
            .answered
    }
}

#[test]
fn scripted_agent_recovers_lost_acceptance_without_resending_known_work() {
    let mut f = Fixture::new();
    for _ in 0..65 {
        f.send("work");
    }
    let mut agent = ScriptedAgent::default();
    let old = FakeWorker::new();
    assert_eq!(old.drive(&mut f, &mut agent, true, Resume::Rebuilt), 0);
    f.db.recover_interrupted_turns().unwrap();
    let new = FakeWorker::new();
    let found_deliveries = agent.history.clone();
    assert_eq!(
        new.drive(
            &mut f,
            &mut agent,
            false,
            Resume::Resumed { found_deliveries }
        ),
        32
    );
    assert_eq!(new.drive(&mut f, &mut agent, false, Resume::Rebuilt), 32);
    assert_eq!(new.drive(&mut f, &mut agent, false, Resume::Rebuilt), 1);
    assert_eq!(agent.work_seen.len(), 65);
    assert_eq!(f.count("inbox"), 0);
}
