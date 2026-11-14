// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn occurrence_worker_lost_prepare_reply_keeps_first_durable_identity_for_read_and_retry() {
    let mut h = Harness::new();
    let (held, entered, mut release) = gate();
    // Capture inspects before/after Store work. Its third inspection is the
    // next physical phase, after the writer has committed the draft.
    let at = h.authority.inspections.load(Ordering::Acquire) + 3;
    *h.authority.inspect_gate.lock().unwrap() = Some((at, held));
    let original = h
        .client()
        .prepare_routine_occurrence(context(), h.request.clone())
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(original);
    release.open();
    let ReadPreparationReply::Prepared {
        preparation: retained,
    } = wait(
        h.client()
            .read_routine_preparation(context(), h.read_request())
            .unwrap(),
    )
    .unwrap()
    else {
        panic!("committed draft must survive waiter loss")
    };
    assert_eq!(h.prepare(), retained);
    assert_eq!(h.authority.captures.load(Ordering::Acquire), 1);
    let repo = h.finish();
    assert_eq!(
        repo.read_routine_preparation(&retained.slot, retained.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared,
        retained
    );
}

#[test]
fn occurrence_worker_shutdown_cancels_but_joins_running_job_before_permit_and_waiter_teardown() {
    let mut h = Harness::new();
    let (held, entered, mut release) = gate();
    *h.authority.capture_gate.lock().unwrap() = Some(held);
    let context = context();
    let cancelled = context.cancellation.clone();
    let call = h
        .client()
        .prepare_routine_occurrence(context, h.request.clone())
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let gates = h.client().routine_calls.clone();
    h.worker.as_ref().unwrap().shutdown();
    let end = Instant::now() + Duration::from_secs(2);
    while !cancelled.is_cancelled() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
    let spare = gates.acquire(false).unwrap();
    assert!(gates.acquire(false).is_err());
    release.open();
    assert_eq!(wait(call), Err(RoutineBridgeRefusal::Unavailable));
    let repo = h.finish();
    assert!(gates.acquire(false).is_ok());
    drop(spare);
    assert!(
        repo.read_routine_preparation(&h.request.slot, h.read_request().semantic_hash)
            .unwrap()
            .is_none()
    );
}

#[test]
fn occurrence_worker_restart_walks_past_unavailable_first_subject_and_sixteen_row_page() {
    let mut h = Harness::new();
    h.delegate.unavailable.store(true, Ordering::Release);
    let mut prepared = Vec::new();
    for _ in 0..17 {
        prepared.push(h.prepare());
        h.request.slot.occurrence_ms += 60_000;
    }
    let repo = h.finish();
    let claims = prepared
        .iter()
        .map(|p| h.delegate.claim(p))
        .collect::<Vec<_>>();
    *h.delegate.unavailable_slot.lock().unwrap() = Some(claims[0].slot.occurrence_ms);
    h.delegate.unavailable.store(false, Ordering::Release);
    let before = h.catalog.reads.load(Ordering::Acquire);
    h.restart(repo);
    let end = Instant::now() + Duration::from_secs(15);
    let last = claims.last().unwrap();
    loop {
        let answer = wait(
            h.client()
                .read_routine_occurrence_link(
                    context(),
                    ReadOccurrenceLink {
                        claim: last.clone(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
        if answer
            == (ReadOccurrenceLinkReply::Linked {
                claim: last.clone(),
            })
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "bounded recovery did not advance: {answer:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let repo = h.finish();
    assert_eq!(
        repo.read_routine_preparation(&prepared[0].slot, prepared[0].semantic_hash)
            .unwrap()
            .unwrap()
            .stage,
        BotRoutineStoredStage::Prepared
    );
    for p in prepared.iter().skip(1) {
        let row = repo
            .read_routine_preparation(&p.slot, p.semantic_hash)
            .unwrap()
            .unwrap();
        assert_eq!(row.stage, BotRoutineStoredStage::Linked);
        assert_eq!(&row.prepared, p);
        assert_eq!(
            repo.attempts_for_chat(ChatId::from_uuid(p.ids.chat))
                .unwrap()[0]
                .status,
            koru_ai_core::AttemptStatus::Pending
        );
    }
    assert_eq!(
        h.catalog.reads.load(Ordering::Acquire),
        before,
        "recovery never recaptures or rediscovers models"
    );
}

#[test]
fn occurrence_worker_actual_capture_replays_before_catalog_or_store_and_never_starts_provider() {
    let mut h = Harness::new();
    let prepared = h.prepare();
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
    assert_eq!(h.authority.captures.load(Ordering::Acquire), 1);
    assert_eq!(h.authority.verifies.load(Ordering::Acquire), 1);
    h.catalog.offered.store(false, Ordering::Release);
    assert_eq!(h.prepare(), prepared);
    assert_eq!(
        wait(
            h.client()
                .read_routine_preparation(context(), h.read_request())
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Prepared {
            preparation: prepared.clone()
        }
    );
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
    assert_eq!(h.authority.captures.load(Ordering::Acquire), 1);
    let repo = h.finish();
    let row = repo
        .read_routine_preparation(&prepared.slot, prepared.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(row.prepared, prepared);
    assert_eq!(row.stage, BotRoutineStoredStage::Prepared);
    assert!(
        repo.attempts_for_chat(ChatId::from_uuid(prepared.ids.chat))
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.messages(ChatId::from_uuid(prepared.ids.chat))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn occurrence_worker_unknown_ack_rereads_exact_claim_and_materializes_retained_ids_once() {
    let mut h = Harness::new();
    let prepared = h.prepare();
    h.delegate.unknown_ack.store(true, Ordering::Release);
    let claim = h.delegate.claim(&prepared);
    let request = FinalizeOccurrence {
        claim: claim.clone(),
    };
    assert_eq!(
        wait(
            h.client()
                .finalize_routine_occurrence(context(), request.clone())
                .unwrap()
        )
        .unwrap(),
        FinalizeOccurrenceReply::Linked {
            claim: claim.clone()
        }
    );
    assert_eq!(
        wait(
            h.client()
                .read_routine_occurrence_link(
                    context(),
                    ReadOccurrenceLink {
                        claim: claim.clone()
                    }
                )
                .unwrap()
        )
        .unwrap(),
        ReadOccurrenceLinkReply::Linked {
            claim: claim.clone()
        }
    );
    assert_eq!(
        wait(
            h.client()
                .read_routine_preparation(context(), h.read_request())
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Linked {
            slot: prepared.slot,
            semantic_hash: prepared.semantic_hash,
            claim: claim.clone()
        }
    );
    assert_eq!(
        wait(
            h.client()
                .finalize_routine_occurrence(context(), request)
                .unwrap()
        )
        .unwrap(),
        FinalizeOccurrenceReply::Linked {
            claim: claim.clone()
        }
    );
    let mut forged = claim.clone();
    forged.claimed_at_ms += 1;
    assert_eq!(
        wait(
            h.client()
                .finalize_routine_occurrence(context(), FinalizeOccurrence { claim: forged })
                .unwrap()
        )
        .unwrap(),
        FinalizeOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Conflict
        }
    );
    h.delegate.unavailable.store(true, Ordering::Release);
    assert_eq!(
        wait(
            h.client()
                .read_routine_preparation(context(), h.read_request())
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    assert_eq!(
        wait(
            h.client()
                .prepare_routine_occurrence(context(), h.request.clone())
                .unwrap()
        )
        .unwrap(),
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    let repo = h.finish();
    let row = repo
        .routine_occurrence_for_run(h.fixture.owner, prepared.slot.bot, prepared.ids.run)
        .unwrap()
        .unwrap();
    assert_eq!(row.prepared, prepared);
    let attempts = repo
        .attempts_for_chat(ChatId::from_uuid(prepared.ids.chat))
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id.as_uuid(), prepared.ids.attempt);
    assert_eq!(attempts[0].status, koru_ai_core::AttemptStatus::Pending);
    assert!(attempts[0].provider_started_at.is_none());
    assert!(attempts[0].context_started_at.is_none());
    assert_eq!(
        repo.messages(ChatId::from_uuid(prepared.ids.chat))
            .unwrap()
            .len(),
        2
    );
    let receipt = repo
        .bot_readable_submission_for_run(h.fixture.owner, prepared.slot.bot, prepared.ids.run)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.stage, BotReceiptStage::Completed);
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
}

#[test]
fn occurrence_worker_terminal_stop_after_materialization_never_resurrects_or_dispatches() {
    let mut h = Harness::new();
    let prepared = h.prepare();
    *h.delegate.terminal_on_ack.lock().unwrap() = Some(RoutineTerminalReason::Stopped);
    let claim = h.delegate.claim(&prepared);
    let request = FinalizeOccurrence {
        claim: claim.clone(),
    };
    for _ in 0..2 {
        assert_eq!(
            wait(
                h.client()
                    .finalize_routine_occurrence(context(), request.clone())
                    .unwrap()
            )
            .unwrap(),
            FinalizeOccurrenceReply::Terminal {
                claim: claim.clone(),
                reason: RoutineTerminalReason::Stopped
            }
        );
    }
    assert_eq!(
        wait(
            h.client()
                .read_routine_occurrence_link(
                    context(),
                    ReadOccurrenceLink {
                        claim: claim.clone()
                    }
                )
                .unwrap()
        )
        .unwrap(),
        ReadOccurrenceLinkReply::Terminal {
            claim: claim.clone(),
            reason: RoutineTerminalReason::Stopped
        }
    );
    let repo = h.finish();
    let attempts = repo
        .attempts_for_chat(ChatId::from_uuid(prepared.ids.chat))
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, koru_ai_core::AttemptStatus::Cancelled);
    assert!(attempts[0].provider_started_at.is_none());
    assert_eq!(
        repo.read_routine_preparation(&prepared.slot, prepared.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared,
        prepared
    );
}

#[test]
fn occurrence_worker_already_terminal_is_truthful_on_all_four_endpoints_and_replay() {
    let mut h = Harness::new();
    let prepared = h.prepare();
    let claim = h.delegate.claim_with_state(
        &prepared,
        RoutineLinkState::Terminal {
            reason: RoutineTerminalReason::AlreadyTerminal,
        },
    );
    for _ in 0..2 {
        assert_eq!(
            wait(
                h.client()
                    .finalize_routine_occurrence(
                        context(),
                        FinalizeOccurrence {
                            claim: claim.clone()
                        }
                    )
                    .unwrap()
            )
            .unwrap(),
            FinalizeOccurrenceReply::Terminal {
                claim: claim.clone(),
                reason: RoutineTerminalReason::AlreadyTerminal
            }
        );
        assert_eq!(
            wait(
                h.client()
                    .read_routine_occurrence_link(
                        context(),
                        ReadOccurrenceLink {
                            claim: claim.clone()
                        }
                    )
                    .unwrap()
            )
            .unwrap(),
            ReadOccurrenceLinkReply::Terminal {
                claim: claim.clone(),
                reason: RoutineTerminalReason::AlreadyTerminal
            }
        );
        assert_eq!(
            wait(
                h.client()
                    .read_routine_preparation(context(), h.read_request())
                    .unwrap()
            )
            .unwrap(),
            ReadPreparationReply::Refused {
                code: RoutineBridgeRefusal::Stale
            }
        );
        assert_eq!(
            wait(
                h.client()
                    .prepare_routine_occurrence(context(), h.request.clone())
                    .unwrap()
            )
            .unwrap(),
            PrepareOccurrenceReply::Refused {
                code: RoutineBridgeRefusal::Stale
            }
        );
    }
    let repo = h.finish();
    assert!(
        repo.attempts_for_chat(ChatId::from_uuid(prepared.ids.chat))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.read_routine_preparation(&prepared.slot, prepared.semantic_hash)
            .unwrap()
            .unwrap()
            .stage,
        BotRoutineStoredStage::Closed(BotRoutineStoredClosure::ClaimedTerminal {
            reason: RoutineTerminalReason::AlreadyTerminal
        })
    );
}

#[test]
fn occurrence_worker_dropped_capture_holds_capacity_until_actual_return_and_reads_stay_available() {
    let mut h = Harness::new();
    let (held, entered, mut release) = gate();
    *h.authority.capture_gate.lock().unwrap() = Some(held);
    let first = h
        .client()
        .prepare_routine_occurrence(context(), h.request.clone())
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(first);
    let mut later = h.request.clone();
    later.slot.occurrence_ms += 60_000;
    let second = h
        .client()
        .prepare_routine_occurrence(context(), later.clone())
        .unwrap();
    assert!(matches!(
        h.client()
            .prepare_routine_occurrence(context(), h.request.clone()),
        Err(RoutineBridgeRefusal::Capacity)
    ));
    h.worker.as_ref().unwrap().ping().unwrap();
    assert!(matches!(
        wait(
            h.client()
                .read_routine_preparation(context(), h.read_request())
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Missing { .. }
    ));
    release.open();
    let PrepareOccurrenceReply::Prepared { preparation } = wait(second).unwrap() else {
        panic!("queued operation was lost")
    };
    assert_eq!(preparation.slot, later.slot);
    let repo = h.finish();
    assert!(
        repo.read_routine_preparation(&h.request.slot, h.read_request().semantic_hash)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.read_routine_preparation(&later.slot, preparation.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared,
        preparation
    );
}

#[test]
fn occurrence_worker_deadline_does_not_release_running_capture_or_write_after_expiration() {
    let mut h = Harness::new();
    let (held, entered, mut release) = gate();
    *h.authority.capture_gate.lock().unwrap() = Some(held);
    let deadline = Instant::now() + Duration::from_secs(1);
    let call = h
        .client()
        .prepare_routine_occurrence(
            RoutineCallContext::new(Uuid::new_v4(), deadline, Cancellation::default()),
            h.request.clone(),
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    std::thread::sleep(
        deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
    );
    let queued = h
        .client()
        .prepare_routine_occurrence(context(), h.request.clone())
        .unwrap();
    assert!(matches!(
        h.client()
            .prepare_routine_occurrence(context(), h.request.clone()),
        Err(RoutineBridgeRefusal::Capacity)
    ));
    drop(queued);
    release.open();
    assert_eq!(
        wait(call).unwrap(),
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
    );
    let repo = h.finish();
    assert!(
        repo.read_routine_preparation(&h.request.slot, h.read_request().semantic_hash)
            .unwrap()
            .is_none()
    );
}

#[test]
fn occurrence_worker_current_authority_is_required_for_missing_and_immutable_replay() {
    let mut h = Harness::new();
    let prepared = h.prepare();
    h.authority.denied.store(true, Ordering::Release);
    assert_eq!(
        wait(
            h.client()
                .prepare_routine_occurrence(context(), h.request.clone())
                .unwrap()
        )
        .unwrap(),
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Forbidden
        }
    );
    let mut missing = h.read_request();
    missing.slot.occurrence_ms += 60_000;
    assert_eq!(
        wait(
            h.client()
                .read_routine_preparation(context(), missing)
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Forbidden
        }
    );
    let mut foreign = h.read_request();
    foreign.slot.owner.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        wait(
            h.client()
                .read_routine_preparation(context(), foreign)
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Forbidden
        }
    );
    let repo = h.finish();
    assert_eq!(
        repo.read_routine_preparation(&prepared.slot, prepared.semantic_hash)
            .unwrap()
            .unwrap()
            .prepared,
        prepared
    );
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
}
