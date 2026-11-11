// SPDX-License-Identifier: MPL-2.0
//! The per-Bot unsettled-work read (private IPC 12) that Archive and Trash
//! settle from: read from the ledger every time, with the current fence,
//! and with "a person holds this session" taken from the session owner as
//! well as the Run status, which forgets control once it is interrupted.
use super::*;
use koru_session_protocol::HandoverState;

fn held_by_a_person(h: &mut Harness, fence: &BotExecutionFence) -> IdempotencyKey {
    let native = h.attempt(fence.run).provider_session.unwrap().idempotency;
    let mut human = summary(ManagedState::Running, fence.turn);
    human.handover = HandoverState::Taken;
    let mut sessions = h.sessions.lock().unwrap();
    sessions
        .lookups
        .insert(native, (host(1), CreationLookup::Known { session: human }));
    sessions.owned_termination_refused = true;
    native
}

fn unsettled(h: &mut Harness, s: &BotSubmission, limit: u16) -> BotUnsettledWork {
    h.orchestrator
        .bot_unsettled(
            s.provenance.owner,
            s.provenance.bot,
            limit,
            s.provenance.owner.device,
        )
        .unwrap()
}

#[test]
fn unsettled_work_is_reread_from_the_ledger_with_its_fence_and_the_session_owners_hold() {
    let (mut h, s, fence, _, _) = active();
    let device = fence.owner.device;
    let queued = submission(2);
    h.orchestrator.bot_submit(queued.clone(), device).unwrap();
    let later = submission(3);
    h.orchestrator.bot_submit(later.clone(), device).unwrap();
    let work = unsettled(&mut h, &s, 64);
    assert!(!work.more);
    assert_eq!(
        work.admissions
            .iter()
            .map(|a| (a.admission.run, a.admission.state, a.held))
            .collect::<Vec<_>>(),
        vec![
            (s.request.run, BotAdmissionState::Executing, false),
            (queued.request.run, BotAdmissionState::Queued, false),
            (later.request.run, BotAdmissionState::Queued, false),
        ]
    );
    // The fence is the current one, exactly what Interrupt requires.
    assert_eq!(work.admissions[0].admission.fence.as_ref(), Some(&fence));
    assert!(work.admissions[1].admission.fence.is_none());
    // The queued page is bounded; the live admission always comes first.
    let page = unsettled(&mut h, &s, 1);
    assert!(page.more);
    assert_eq!(page.admissions.len(), 2);
    // Another owner device is refused, not answered with someone's work.
    assert!(
        h.orchestrator
            .bot_unsettled(
                s.provenance.owner,
                s.provenance.bot,
                64,
                DeviceId::from_uuid(v4(0x77))
            )
            .is_err()
    );

    // A person takes the session; an automatic interruption then records the
    // Run as interrupted, so its status no longer names any controller. The
    // read still says the session is held, and nothing ended it.
    let native = held_by_a_person(&mut h, &fence);
    assert!(
        h.orchestrator
            .bot_interrupt(&key(&s), device, Some(&fence))
            .is_err()
    );
    assert!(matches!(h.status(fence.run), RunStatus::Interrupted { .. }));
    assert!(h.status(fence.run).control().is_none());
    let work = unsettled(&mut h, &s, 64);
    assert!(work.admissions[0].held, "{work:?}");
    assert!(!work.admissions[1].held);
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert!(!h.attempt(fence.run).retired);

    // Queued Runs are interrupted without starting and leave the read.
    for waiting in [&queued, &later] {
        h.orchestrator
            .bot_interrupt(&key(waiting), device, None)
            .unwrap();
        assert!(
            h.orchestrator
                .bot_dispatch(&key(waiting), device)
                .unwrap()
                .is_none()
        );
    }
    let work = unsettled(&mut h, &s, 64);
    assert_eq!(
        work.admissions
            .iter()
            .map(|a| a.admission.run)
            .collect::<Vec<_>>(),
        vec![s.request.run]
    );

    // Handback: the owned cleanup proves the end, and the Bot reads settled.
    let mut ended = summary(ManagedState::Completed, None);
    ended.process = ProcessState::Exited { code: Some(0) };
    ended.handover = HandoverState::Returned;
    {
        let mut sessions = h.sessions.lock().unwrap();
        sessions.owned_termination_refused = false;
        sessions.lookups.insert(
            native,
            (host(1), CreationLookup::KnownEnded { session: ended }),
        );
    }
    h.orchestrator
        .attempt_mut(fence.run)
        .unwrap()
        .bot_cleanup
        .as_mut()
        .unwrap()
        .due = std::time::Instant::now();
    assert!(h.orchestrator.bot_budget_tick(fence.run).unwrap());
    h.orchestrator
        .bot_interrupt(&key(&s), device, Some(&fence))
        .unwrap();
    let work = unsettled(&mut h, &s, 64);
    assert!(work.is_settled(), "{work:?}");
    assert!(!h.calls().contains(&Call::Terminate(fence.session)));
    assert_eq!(
        h.calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}
