// SPDX-License-Identifier: MPL-2.0
//! Actual writer/preparation/recovery with a no-path Store. Delegate Run truth
//! is an explicit finite script, not a provider or runtime qualification.
use super::*;
use crate::bots::routine::fixture::Fixture as StoreFixture;
use koru_delegate_core::{AttemptGeneration, RunContextItem, RunToolAuthority};
use std::sync::{Mutex, atomic::AtomicUsize};
mod fixture;
use fixture::*;

#[test]
fn routine_model_four_operations_use_actual_worker_and_same_retained_identity() {
    let f = Fixture::new();
    let args = f.create();
    let (worker, returned) = f.start();
    let client = worker.client();
    let created = fixture::created(
        call(
            &client,
            &f.subject,
            RunRoutineOperation::Create(args.clone()),
        )
        .unwrap(),
    );
    assert_eq!(created.bot_id, f.store.bot.id);
    assert_eq!(created.request_id, args.request_id);
    let status = status(
        call(
            &client,
            &f.subject,
            RunRoutineOperation::ReadCreation {
                request_id: args.request_id,
            },
        )
        .unwrap(),
    );
    assert_eq!(status.schedule_id, created.schedule_id);
    let retried = call(
        &client,
        &f.subject,
        RunRoutineOperation::RetryCreation {
            original_request_id: args.request_id,
            observed_token: status.observed_token,
        },
    );
    // Recovery can advance between status and retry; an exact-token conflict is
    // legitimate and never authorizes a replacement identity.
    match retried {
        Ok(value) => assert_eq!(fixture::status(value).schedule_id, created.schedule_id),
        Err(error) => assert_eq!(error.code, "request_conflict"),
    }
    let replay =
        fixture::created(call(&client, &f.subject, RunRoutineOperation::Create(args)).unwrap());
    assert_eq!(replay.schedule_id, created.schedule_id);
    let page = page(call(&client, &f.subject, list()).unwrap());
    assert!(
        page.items
            .iter()
            .any(|i| i.schedule_id == created.schedule_id)
    );
    let repo = finish(worker, returned);
    assert_eq!(
        repo.bot_routine_creation_by_request(f.store.owner, f.store.bot.id, created.request_id)
            .unwrap()
            .unwrap()
            .identity
            .schedule,
        created.schedule_id
    );
    assert!(f.delegate.resolves.load(Ordering::Acquire) >= 20);
}

#[test]
fn routine_model_list_observes_actual_paused_store_without_capture_or_catalog() {
    let f = Fixture::new();
    let (worker, returned) = f.start();
    let result = page(call(&worker.client(), &f.subject, list()).unwrap());
    assert_eq!(result.items.len(), 1);
    assert!(matches!(
        result.items[0].live,
        BotRoutineLiveObservation::Current { .. }
    ));
    assert_eq!(f.authority.overviews.load(Ordering::Acquire), 1);
    assert_eq!(f.authority.creations.load(Ordering::Acquire), 0);
    assert!(
        f.delegate
            .other
            .lock()
            .unwrap()
            .iter()
            .all(|v| *v == "hello" || *v == "pending")
    );
    finish(worker, returned);
}

#[test]
fn routine_model_workspace_read_accepts_document_outside_selected_write_pages() {
    let f = Fixture::new();
    let mut args = f.create();
    assert!(f.subject.captured.selected_pages.is_empty());
    args.documents.push(f.store.bot.config.role.document);
    let (worker, returned) = f.start();
    let result = created(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(args),
        )
        .unwrap(),
    );
    let repo = finish(worker, returned);
    let row = repo
        .bot_routine_creation_by_request(f.store.owner, f.store.bot.id, result.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.identity.seed.header.definition.template.context.len(),
        1
    );
}

#[test]
fn routine_model_fresh_create_refuses_absent_document_without_journal() {
    let f = Fixture::new();
    let mut args = f.create();
    args.documents.push(koru_ai_core::DocumentId::new());
    let request = args.request_id;
    let (worker, returned) = f.start();
    assert!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(args)
        )
        .is_err()
    );
    let repo = finish(worker, returned);
    assert!(
        repo.bot_routine_creation_by_request(f.store.owner, f.store.bot.id, request)
            .unwrap()
            .is_none()
    );
}

#[test]
fn routine_model_wrong_retained_run_owner_and_policy_refuse_before_store() {
    let f = Fixture::new();
    let (worker, returned) = f.start();
    for which in 0..3 {
        let mut subject = f.subject.clone();
        match which {
            0 => subject.run = koru_delegate_core::RunId::mint(),
            1 => subject.provenance.owner.device = koru_delegate_core::DeviceId::new(),
            _ => subject.captured.workspace = WorkspaceId::new(),
        }
        assert!(call(&worker.client(), &subject, list()).is_err());
    }
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    assert_eq!(f.authority.overviews.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_unknown_or_changed_generation_never_uses_store() {
    for unknown in [false, true] {
        let f = Fixture::new();
        f.delegate.denied.store(!unknown, Ordering::Release);
        f.delegate.unknown.store(unknown, Ordering::Release);
        let (worker, returned) = f.start();
        assert!(call(&worker.client(), &f.subject, list()).is_err());
        assert_eq!(f.authority.overviews.load(Ordering::Acquire), 0);
        finish(worker, returned);
    }
}

#[test]
fn routine_model_late_generation_revocation_refuses_reply_after_store() {
    let f = Fixture::new();
    let (held, entered, mut release) = gate();
    *f.authority.gate.lock().unwrap() = Some(held);
    let (worker, returned) = f.start();
    let response = direct(&worker.client(), request(&f.subject, list()));
    entered.recv_timeout(WAIT).unwrap();
    f.delegate.denied.store(true, Ordering::Release);
    release.open();
    assert_eq!(
        response.recv_timeout(WAIT).unwrap().err().unwrap().code,
        "permission_denied"
    );
    assert_eq!(f.authority.overviews.load(Ordering::Acquire), 1);
    finish(worker, returned);
}

#[test]
fn routine_model_late_profile_and_owner_changes_refuse_whole_page() {
    for profile in [false, true] {
        let f = Fixture::new();
        let (held, entered, mut release) = gate();
        *f.authority.gate.lock().unwrap() = Some(held);
        let (worker, returned) = f.start();
        let response = direct(&worker.client(), request(&f.subject, list()));
        entered.recv_timeout(WAIT).unwrap();
        if profile {
            f.authority.wrong_profile.store(true, Ordering::Release);
        } else {
            f.authority.wrong_owner.store(true, Ordering::Release);
        }
        release.open();
        assert_eq!(
            response.recv_timeout(WAIT).unwrap().err().unwrap().code,
            "permission_denied"
        );
        finish(worker, returned);
    }
}

#[test]
fn routine_model_cancel_before_acceptance_allocates_no_creation() {
    let f = Fixture::new();
    let args = f.create();
    let id = args.request_id;
    let mut req = request(&f.subject, RunRoutineOperation::Create(args));
    req.cancellation.cancel();
    req.deadline = Instant::now() + WAIT;
    let (worker, returned) = f.start();
    assert_eq!(
        worker.client().routine_for_run(req).err().unwrap().code,
        "ai_worker_timeout"
    );
    let repo = finish(worker, returned);
    assert!(
        repo.bot_routine_creation_by_request(f.store.owner, f.store.bot.id, id)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
}

#[test]
fn routine_model_cancel_after_acceptance_retains_journal_and_recovery() {
    let f = Fixture::new();
    let args = f.create();
    let id = args.request_id;
    let req = request(&f.subject, RunRoutineOperation::Create(args));
    let cancel = req.cancellation.clone();
    // Caller consumes two exact Run proofs; the third begins the reply phase,
    // after the writer committed the immutable creation journal.
    let (held, entered, mut release) = gate();
    *f.delegate.gate.lock().unwrap() = Some((3, held));
    let (creation_hold, creation_entered, mut creation_release) = gate();
    *f.authority.creation_gate.lock().unwrap() = Some(creation_hold);
    let (worker, returned) = f.start();
    let response = direct(&worker.client(), req);
    entered.recv_timeout(WAIT).unwrap();
    creation_entered.recv_timeout(WAIT).unwrap();
    cancel.cancel();
    release.open();
    creation_release.open();
    assert_eq!(
        response.recv_timeout(WAIT).unwrap().err().unwrap().code,
        "save_status_unknown"
    );
    let end = Instant::now() + WAIT;
    let read = loop {
        let read = status(
            call(
                &worker.client(),
                &f.subject,
                RunRoutineOperation::ReadCreation { request_id: id },
            )
            .unwrap(),
        );
        if read.observed_token.state == BotRoutineCreationState::Bound {
            break read;
        }
        assert!(Instant::now() < end, "accepted recovery did not confirm");
        std::thread::sleep(Duration::from_millis(10));
    };
    let repo = finish(worker, returned);
    let retained = repo
        .bot_routine_creation_by_request(f.store.owner, f.store.bot.id, id)
        .unwrap()
        .unwrap();
    assert_eq!(retained.identity.schedule, read.schedule_id);
    assert_eq!(retained.state, BotRoutineCreationState::Bound);
    assert!(retained.origin_edit_revision.is_some());
    assert!(f.authority.creations.load(Ordering::Acquire) > 0);
    assert!(
        !f.authority
            .creation_cancellations
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(
        !f.authority
            .creation_cancellations
            .lock()
            .unwrap()
            .iter()
            .any(Cancellation::is_cancelled)
    );
}

#[test]
fn routine_model_adapter_panic_is_contained_and_next_request_progresses() {
    let f = Fixture::new();
    f.authority.panic_once.store(true, Ordering::Release);
    let (worker, returned) = f.start();
    assert!(call(&worker.client(), &f.subject, list()).is_err());
    assert_eq!(
        page(call(&worker.client(), &f.subject, list()).unwrap())
            .items
            .len(),
        1
    );
    finish(worker, returned);
}

#[test]
fn routine_model_cancelled_held_job_keeps_shared_capacity_until_completion() {
    let f = Fixture::new();
    let (held, entered, mut release) = gate();
    *f.authority.gate.lock().unwrap() = Some(held);
    let (worker, returned) = f.start();
    let first = request(&f.subject, list());
    let cancel = first.cancellation.clone();
    let mut responses = vec![direct(&worker.client(), first)];
    entered.recv_timeout(WAIT).unwrap();
    cancel.cancel();
    for _ in 0..8 {
        responses.push(direct(&worker.client(), request(&f.subject, list())));
    }
    let overflow = direct(&worker.client(), request(&f.subject, list()));
    assert_eq!(
        overflow.recv_timeout(WAIT).unwrap().err().unwrap().code,
        "ai_worker_saturated"
    );
    assert!(matches!(
        responses[0].try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release.open();
    for response in responses {
        let _ = response.recv_timeout(WAIT).unwrap();
    }
    assert_eq!(
        page(call(&worker.client(), &f.subject, list()).unwrap())
            .items
            .len(),
        1
    );
    finish(worker, returned);
}

#[test]
fn routine_model_deadline_is_not_restarted_after_queue_wait() {
    let f = Fixture::new();
    let (held, entered, mut release) = gate();
    *f.authority.gate.lock().unwrap() = Some(held);
    let (worker, returned) = f.start();
    let first = direct(&worker.client(), request(&f.subject, list()));
    entered.recv_timeout(WAIT).unwrap();
    let mut later = request(&f.subject, list());
    later.deadline = Instant::now() + Duration::from_millis(30);
    assert_eq!(
        worker.client().routine_for_run(later).err().unwrap().code,
        "ai_worker_timeout"
    );
    release.open();
    first.recv_timeout(WAIT).unwrap().unwrap();
    // A successful later full workflow is a real completion barrier. Only
    // these two listings may have entered the six Run-proof calls apiece.
    assert_eq!(
        page(call(&worker.client(), &f.subject, list()).unwrap())
            .items
            .len(),
        1
    );
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 12);
    assert_eq!(f.authority.overviews.load(Ordering::Acquire), 2);
    finish(worker, returned);
}

#[test]
fn routine_model_exact_replay_keeps_original_receipt_after_current_configuration_changes() {
    let f = Fixture::new();
    let args = f.create();
    let retained = f.prepare(&args, true);
    f.change_configuration();
    let (worker, returned) = f.start();
    let result = created(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(args.clone()),
        )
        .unwrap(),
    );
    assert_eq!(result.schedule_id, retained.identity.schedule);
    assert_eq!(result.state, BotRoutineCreationState::Bound);
    let mut changed = args;
    changed.objective = serde_json::from_value(serde_json::json!("different body")).unwrap();
    assert_eq!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(changed)
        )
        .err()
        .unwrap()
        .code,
        "request_conflict"
    );
    assert_eq!(f.authority.creations.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_bound_retry_does_not_wake_creation_and_stale_token_refuses() {
    let f = Fixture::new();
    let args = f.create();
    let row = f.prepare(&args, true);
    let (worker, returned) = f.start();
    let reply = status(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::RetryCreation {
                original_request_id: args.request_id,
                observed_token: row.observed(),
            },
        )
        .unwrap(),
    );
    assert_eq!(reply.schedule_id, row.identity.schedule);
    assert!(!reply.can_retry);
    let mut stale = row.observed();
    stale.updated_at_ms -= 1;
    assert_eq!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::RetryCreation {
                original_request_id: args.request_id,
                observed_token: stale
            }
        )
        .err()
        .unwrap()
        .code,
        "request_conflict"
    );
    assert_eq!(f.authority.creations.load(Ordering::Acquire), 0);
    assert!(!f.delegate.other.lock().unwrap().contains(&"register"));
    finish(worker, returned);
}

#[test]
fn routine_model_create_validation_rejects_duplicate_documents_and_invalid_revision() {
    let f = Fixture::new();
    let (worker, returned) = f.start();
    for revision in [0, u64::MAX] {
        let mut args = f.create();
        args.expected_bot_revision = revision;
        assert!(
            call(
                &worker.client(),
                &f.subject,
                RunRoutineOperation::Create(args)
            )
            .is_err()
        );
    }
    let mut args = f.create();
    args.documents = vec![f.store.bot.config.role.document; 2];
    assert!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(args)
        )
        .is_err()
    );
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_observational_replay_ignores_invalid_current_execution_config() {
    let f = Fixture::new();
    let args = f.create();
    let row = f.prepare(&args, true);
    f.damage("UPDATE bots SET working_folder=zeroblob(1048577),model=zeroblob(1048577)");
    let (worker, returned) = f.start();
    let result = created(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(args.clone()),
        )
        .unwrap(),
    );
    assert_eq!(result.schedule_id, row.identity.schedule);
    let status = status(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::ReadCreation {
                request_id: args.request_id,
            },
        )
        .unwrap(),
    );
    assert!(!status.can_retry);
    assert_eq!(
        page(call(&worker.client(), &f.subject, list()).unwrap())
            .items
            .len(),
        2
    );
    let fresh = f.create();
    assert!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::Create(fresh)
        )
        .is_err()
    );
    assert_eq!(f.authority.creations.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_optional_retry_hint_is_false_when_current_eligibility_is_unavailable() {
    let f = Fixture::new();
    let args = f.create();
    let row = f.prepare(&args, false);
    f.damage("UPDATE bots SET working_folder=zeroblob(1048577)");
    let (worker, returned) = f.start();
    let result = status(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::ReadCreation {
                request_id: args.request_id,
            },
        )
        .unwrap(),
    );
    assert_eq!(result.observed_token, row.observed());
    assert!(!result.can_retry);
    assert!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::RetryCreation {
                original_request_id: args.request_id,
                observed_token: row.observed()
            }
        )
        .is_err()
    );
    assert_eq!(f.authority.creations.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_retained_receipt_corruption_is_error_not_false_retry_hint() {
    let f = Fixture::new();
    let args = f.create();
    f.prepare(&args, false);
    f.damage(&format!(
        "UPDATE bot_command_receipts SET outcome_json='{{}}' WHERE request_id='{}'",
        args.request_id
    ));
    let (worker, returned) = f.start();
    assert!(
        call(
            &worker.client(),
            &f.subject,
            RunRoutineOperation::ReadCreation {
                request_id: args.request_id
            }
        )
        .is_err()
    );
    finish(worker, returned);
}

#[test]
fn routine_model_malformed_original_run_receipt_never_reaches_generation_or_store() {
    let f = Fixture::new();
    f.damage(&format!(
        "UPDATE bot_command_receipts SET submission_json='{{}}' WHERE planned_run_id='{}'",
        f.subject.run
    ));
    let (worker, returned) = f.start();
    assert!(call(&worker.client(), &f.subject, list()).is_err());
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    assert_eq!(f.authority.overviews.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_linked_attempt_is_checked_and_terminal_attempt_refuses_observation() {
    let f = Fixture::shared();
    let (worker, returned) = f.start();
    assert_eq!(
        page(call(&worker.client(), &f.subject, list()).unwrap())
            .items
            .len(),
        1
    );
    finish(worker, returned);
    let f = Fixture::shared();
    f.cancel_attempt();
    let (worker, returned) = f.start();
    assert!(call(&worker.client(), &f.subject, list()).is_err());
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_missing_shared_association_refuses_even_matching_delegate_subject() {
    let f = Fixture::shared();
    f.damage("DELETE FROM bot_chat_runs");
    let (worker, returned) = f.start();
    assert!(call(&worker.client(), &f.subject, list()).is_err());
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_client_refuses_full_existing_command_channel_without_waiting() {
    let f = Fixture::new();
    let (worker, returned) = f.start();
    let mut client = worker.client();
    let (sender, receiver) = sync_channel(WORKER_COMMAND_CAPACITY);
    client.sender = sender;
    for _ in 0..WORKER_COMMAND_CAPACITY {
        let (reply, _) = sync_channel(1);
        client
            .sender
            .try_send(Envelope::Command(WorkerCommand::Ping(reply)))
            .unwrap();
    }
    let req = request(&f.subject, list());
    let cancellation = req.cancellation.clone();
    assert_eq!(
        client.routine_for_run(req).err().unwrap().code,
        "ai_worker_saturated"
    );
    assert!(cancellation.is_cancelled());
    assert_eq!(receiver.try_iter().count(), WORKER_COMMAND_CAPACITY);
    assert_eq!(f.delegate.resolves.load(Ordering::Acquire), 0);
    finish(worker, returned);
}

#[test]
fn routine_model_generation_change_after_commit_returns_unknown_and_keeps_candidate() {
    let f = Fixture::new();
    let args = f.create();
    let id = args.request_id;
    let (held, entered, mut release) = gate();
    *f.delegate.gate.lock().unwrap() = Some((3, held));
    let (worker, returned) = f.start();
    let response = direct(
        &worker.client(),
        request(&f.subject, RunRoutineOperation::Create(args)),
    );
    entered.recv_timeout(WAIT).unwrap();
    f.delegate.denied.store(true, Ordering::Release);
    release.open();
    assert_eq!(
        response.recv_timeout(WAIT).unwrap().err().unwrap().code,
        "save_status_unknown"
    );
    let repo = finish(worker, returned);
    let row = repo
        .bot_routine_creation_by_request(f.store.owner, f.store.bot.id, id)
        .unwrap()
        .unwrap();
    assert_eq!(row.identity.creation_request, id);
    assert_eq!(row.identity.owner, f.store.owner);
    assert!(matches!(
        row.state,
        BotRoutineCreationState::Prepared
            | BotRoutineCreationState::Unconfirmed
            | BotRoutineCreationState::Bound
    ));
}

#[test]
fn routine_model_shutdown_joins_held_physical_job_without_detaching_it() {
    let f = Fixture::new();
    let (held, entered, mut release) = gate();
    *f.authority.gate.lock().unwrap() = Some(held);
    let (worker, returned) = f.start();
    let response = direct(&worker.client(), request(&f.subject, list()));
    entered.recv_timeout(WAIT).unwrap();
    worker.shutdown();
    let (done, joined) = sync_channel(1);
    let thread = std::thread::spawn(move || done.send(worker.join()).unwrap());
    assert!(matches!(
        joined.recv_timeout(Duration::from_millis(30)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    release.open();
    assert_eq!(joined.recv_timeout(WAIT).unwrap(), WorkerExit::Clean);
    thread.join().unwrap();
    let _ = response.recv_timeout(WAIT);
    returned.recv_timeout(WAIT).unwrap();
}
