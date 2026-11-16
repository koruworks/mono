// SPDX-License-Identifier: MPL-2.0
//! A Routine's occurrences after an owner control (P4.3), on the production
//! client/worker/Store path: the grant an occurrence captures after an
//! access edit, the unsettled-Run refusal of an access edit, and the one
//! paused preparation an owner Test admits.
use super::*;
use crate::worker::bots::routine::control::tests::Coordinator;
use koru_ai_wire::bot_routine_codes as codes;

fn owner_context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "routine owner fixture".into(),
            scopes: vec![AiScope::ManageBots],
        },
        trace_id: "routine-owner".into(),
        deadline_millis: None,
    }
}
/// The harness with the scripted schedule coordinator installed, and the
/// owner peer answering barrier installs.
fn controlled() -> (Harness, Arc<Coordinator>) {
    let h = Harness::new();
    let owner = Coordinator::new(&h.fixture);
    owner
        .minute
        .store(h.request.slot.occurrence_ms, Ordering::SeqCst);
    *h.delegate.routines.lock().unwrap() = Some(owner.clone());
    h.delegate.preclaim.enabled.store(true, Ordering::Release);
    (h, owner)
}
fn send(h: &Harness, command: BotCommand) -> ApiResult<BotResponse> {
    h.client().bots(owner_context(), command)
}
fn lifecycle(h: &Harness, action: BotRoutineLifecycleAction, revision: u32) -> BotRoutineControl {
    match send(
        h,
        BotCommand::SetRoutineLifecycle {
            bot_id: h.fixture.bot.id,
            schedule_id: h.fixture.creation.identity.schedule,
            expected_definition_revision: revision,
            action,
        },
    )
    .unwrap()
    {
        BotResponse::RoutineControl { control } => control,
        _ => panic!("routine_control"),
    }
}
fn edit_access(h: &Harness, request: BotRequestId, expected: u64) -> ApiResult<BotRoutineAccess> {
    send(
        h,
        BotCommand::UpdateRoutineAccess {
            request_id: request,
            bot_id: h.fixture.bot.id,
            schedule_id: h.fixture.creation.identity.schedule,
            expected_grant_revision: expected,
            access: BotAccess::Workspace,
            selected_pages: vec![],
        },
    )
    .map(|response| match response {
        BotResponse::RoutineAccess { access } => access,
        _ => panic!("routine_access"),
    })
}
fn prepare(h: &Harness) -> PrepareOccurrenceReply {
    wait(
        h.client()
            .prepare_routine_occurrence(context(), h.request.clone())
            .unwrap(),
    )
    .unwrap()
}
fn finalize(h: &Harness, prepared: &RoutinePrepared) -> RoutineClaimIdentity {
    let claim = h.delegate.claim(prepared);
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
        FinalizeOccurrenceReply::Linked {
            claim: claim.clone()
        }
    );
    claim
}

/// A Routine still fires after an access edit: the next occurrence captures
/// the edited grant, the repository accepts it, preclaim installs the
/// barrier of the edited revision, and the occurrence reaches a claim.
#[test]
fn routine_occurrence_after_access_edit_captures_edited_grant_and_is_claimed() {
    let (mut h, owner) = controlled();
    let schedule = h.fixture.creation.identity.schedule;
    // The fixture's Routine is on at revision 2. Access is edited only while
    // paused, so: pause, edit, enable.
    assert_eq!(
        lifecycle(&h, BotRoutineLifecycleAction::Pause, 2).definition_revision,
        3
    );
    let edit = BotRequestId::mint();
    let edited = edit_access(&h, edit, 1).unwrap();
    assert_eq!(
        (edited.grant_revision, edited.access),
        (2, BotAccess::Workspace)
    );
    assert_eq!(
        lifecycle(&h, BotRoutineLifecycleAction::Enable, 3).definition_revision,
        4
    );
    assert!(!owner.live().paused);

    // The next occurrence, offered with the live definition.
    h.request.schedule_definition = owner.definition();
    let prepared = h.prepare();
    // Keep recovery from reading the claim before this test makes it.
    h.delegate.unavailable.store(true, Ordering::Release);

    // Preclaim installs the Routine barrier of revision 2, under the edit's
    // own request, and reports it in the fresh proof.
    let preclaim = SynchronizeRoutinePreclaim {
        slot: prepared.slot,
        semantic_hash: prepared.semantic_hash,
        preparation_hash: prepared.preparation_hash,
    };
    let SynchronizeRoutinePreclaimReply::Ready { proof, .. } = wait(
        h.client()
            .synchronize_routine_preclaim(context(), preclaim.clone())
            .unwrap(),
    )
    .unwrap() else {
        panic!("preclaim must be ready after an access edit")
    };
    let barrier = proof.routine.expect("the Routine's own barrier");
    assert_eq!(barrier.grant_revision, 2);
    assert_eq!(barrier.request, edit.into());
    assert_eq!(barrier.schedule, schedule);
    assert_eq!(
        barrier.policy.access,
        koru_delegate_core::BotAccess::Workspace
    );

    // And it reaches a claim: the scheduler's claim is finalized and linked.
    h.delegate.unavailable.store(false, Ordering::Release);
    let claim = finalize(&h, &prepared);
    assert_eq!(claim.slot, prepared.slot);

    let repository = h.finish();
    let owner_id = h.fixture.owner;
    let current = repository
        .routine_current_grant(owner_id, h.fixture.bot.id, schedule)
        .unwrap();
    assert_eq!((current.grant.grant_revision, current.request), (2, edit));
    // The capture holds the edited grant at revision 2, not the setup grant.
    let row = repository
        .read_routine_preparation(&prepared.slot, prepared.semantic_hash)
        .unwrap()
        .expect("the repository accepted the preparation");
    assert_eq!(row.capture.grant, current.grant);
    assert_eq!(row.capture.grant.grant_revision, 2);
    assert_ne!(row.capture.grant, h.fixture.creation.identity.grant);
    assert_eq!(row.stage, BotRoutineStoredStage::Linked);
    assert_eq!(
        row.prepared
            .submission
            .provenance
            .routine
            .unwrap()
            .grant_revision,
        2
    );
}

/// An access edit is refused while a Run of that Routine is unsettled, by
/// the preparation row's Run id, and accepted once the Run has settled.
#[test]
fn routine_access_edit_is_refused_while_a_run_of_the_routine_is_unsettled() {
    let (mut h, owner) = controlled();
    let prepared = h.prepare();
    finalize(&h, &prepared);
    // The owner still runs that occurrence. The Routine is paused for the
    // edit, which stops no Run.
    *owner.unsettled.lock().unwrap() = (vec![prepared.ids.run], false);
    lifecycle(&h, BotRoutineLifecycleAction::Pause, 2);
    let request = BotRequestId::mint();
    let refused = edit_access(&h, request, 1).unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.retryable),
        (codes::RUN_IN_PROGRESS, true)
    );
    // Not retained and nothing written: the same request succeeds once the
    // Run has settled.
    *owner.unsettled.lock().unwrap() = (vec![], false);
    assert_eq!(edit_access(&h, request, 1).unwrap().grant_revision, 2);
    let repository = h.finish();
    assert_eq!(
        repository
            .routine_current_grant(
                h.fixture.owner,
                h.fixture.bot.id,
                h.fixture.creation.identity.schedule
            )
            .unwrap()
            .grant
            .grant_revision,
        2
    );
    // The Run that was in progress keeps the grant it was captured under.
    let row = repository
        .read_routine_preparation(&prepared.slot, prepared.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(row.capture.grant.grant_revision, 1);
}

/// A paused preparation is admitted for exactly the owner Test this worker
/// asked for, and for nothing else.
#[test]
fn routine_test_admits_exactly_the_paused_preparation_it_expects() {
    let (mut h, owner) = controlled();
    let bot = h.fixture.bot.id;
    let schedule = h.fixture.creation.identity.schedule;
    let minute = h.request.slot.occurrence_ms;
    lifecycle(&h, BotRoutineLifecycleAction::Pause, 2);
    h.request.schedule_definition = owner.definition();

    // No Test was asked for: a paused definition is refused because it
    // merely arrived, and nothing is stored.
    assert_eq!(
        prepare(&h),
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Conflict
        }
    );

    // The person presses Test.
    let BotResponse::RoutineTest { test } = send(
        &h,
        BotCommand::TestRoutine {
            bot_id: bot,
            schedule_id: schedule,
            expected_definition_revision: 3,
        },
    )
    .unwrap() else {
        panic!("routine_test")
    };
    assert_eq!(test.occurrence_ms, minute);
    assert_eq!(*owner.tests.lock().unwrap(), [(minute, 3)]);
    assert!(owner.live().paused, "a Test does not turn the Routine on");

    // Another minute of the same Routine is still not expected.
    let mut other = h.request.clone();
    other.slot.occurrence_ms += 60_000;
    assert_eq!(
        wait(
            h.client()
                .prepare_routine_occurrence(context(), other)
                .unwrap()
        )
        .unwrap(),
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Conflict
        }
    );

    // The expected one is captured, under the paused definition.
    let prepared = h.prepare();
    assert_eq!(prepared.slot.occurrence_ms, minute);
    assert_eq!(prepared.definition.revision, 3);
    h.delegate.unavailable.store(true, Ordering::Release);
    // While the Test's minute is outstanding it holds the Routine's slot.
    assert_eq!(
        send(
            &h,
            BotCommand::SetRoutineLifecycle {
                bot_id: bot,
                schedule_id: schedule,
                expected_definition_revision: 3,
                action: BotRoutineLifecycleAction::Enable,
            },
        )
        .unwrap_err()
        .code,
        codes::CONTROL_PENDING
    );
    // Its claim is observed: the slot is free, and the Routine is still
    // paused until the person turns it on.
    h.delegate.unavailable.store(false, Ordering::Release);
    finalize(&h, &prepared);
    owner.fire(minute, "fired");
    assert!(owner.live().paused);

    // The history line of that minute is a Test, and opens its conversation.
    let BotResponse::RoutineDetail { reply } = send(
        &h,
        BotCommand::ReadRoutineDetail {
            bot_id: bot,
            schedule_id: schedule,
            before_occurrence_ms: None,
            history_limit: 16,
        },
    )
    .unwrap() else {
        panic!("routine_detail")
    };
    let RoutineOccurrenceHistoryObservation::Available { items, .. } = reply.detail.occurrences
    else {
        panic!("history")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].occurrence_ms, minute);
    assert_eq!(items[0].outcome, RoutineOccurrenceOutcome::Ran);
    assert!(items[0].test, "a paused capture can only be a Test");
    assert_eq!(
        items[0].conversation.as_ref().map(|c| c.run_id),
        Some(prepared.ids.run)
    );

    assert_eq!(
        lifecycle(&h, BotRoutineLifecycleAction::Enable, 3).definition_revision,
        4
    );
    let repository = h.finish();
    let row = repository
        .read_routine_preparation(&prepared.slot, prepared.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(
        BotRoutineCaptureLifecycle::of(&row.capture.schedule_definition.lifecycle),
        Some(BotRoutineCaptureLifecycle::TestPausedManual)
    );
    assert_eq!(row.stage, BotRoutineStoredStage::Linked);
}
