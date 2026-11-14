// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn admission_read_runtime_is_current_exact_and_independent_of_unsettled_work() {
    let mut h = bot_harness();
    let s = submission(1);
    let owner = s.provenance.owner;
    let bot = s.provenance.bot;
    assert_eq!(
        h.orchestrator
            .bot_read_admission_barrier(owner, bot, owner.device)
            .unwrap(),
        BotBootAdmission::Missing {}
    );
    let mut barrier = BotAdmissionBarrier {
        owner,
        bot,
        request: BotRequestId::mint(),
        epoch: 1,
        paused: false,
    };
    h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
    for epoch in [1, 2] {
        if epoch == 2 {
            barrier.epoch = 2;
            barrier.paused = true;
            h.orchestrator.bot_barrier(&barrier, owner.device).unwrap();
        }
        h.ledger.lock().unwrap().ops.clear();
        assert_eq!(
            h.orchestrator
                .bot_read_admission_barrier(owner, bot, owner.device)
                .unwrap(),
            BotBootAdmission::Present {
                barrier: barrier.clone()
            }
        );
        assert_eq!(h.ledger.lock().unwrap().ops, vec!["bot_barrier_read"]);
    }
    assert_eq!(
        h.orchestrator
            .bot_read_admission_barrier(owner, BotId::mint(), owner.device)
            .unwrap(),
        BotBootAdmission::Missing {}
    );
    // Never submitting a Run proves this independent barrier is not derived
    // from unsettled inventory. No document/capture/session work is required.
}

#[test]
fn admission_read_runtime_authenticates_before_ledger_and_preserves_read_errors() {
    let mut h = bot_harness();
    let s = submission(1);
    let owner = s.provenance.owner;
    let bot = s.provenance.bot;
    h.ledger.lock().unwrap().ops.clear();
    for device in [DeviceId::new(), DeviceId::from_uuid(uuid::Uuid::nil())] {
        assert!(
            h.orchestrator
                .bot_read_admission_barrier(owner, bot, device)
                .is_err()
        );
    }
    assert!(
        h.orchestrator
            .bot_read_admission_barrier(owner, BotId::from_uuid(uuid::Uuid::nil()), owner.device)
            .is_err()
    );
    assert!(h.ledger.lock().unwrap().ops.is_empty());
    h.ledger.lock().unwrap().fail = Some("bot_barrier_read");
    assert!(
        h.orchestrator
            .bot_read_admission_barrier(owner, bot, owner.device)
            .is_err()
    );
    assert_eq!(h.ledger.lock().unwrap().ops, vec!["bot_barrier_read"]);
    // A later successful exact read may establish absence; the failed one did not.
    assert_eq!(
        h.orchestrator
            .bot_read_admission_barrier(owner, bot, owner.device)
            .unwrap(),
        BotBootAdmission::Missing {}
    );
}
