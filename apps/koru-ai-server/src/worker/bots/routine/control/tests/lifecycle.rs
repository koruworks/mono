// SPDX-License-Identifier: MPL-2.0
//! Archive, Trash and Restore of a Bot that owns a Routine, through the real
//! worker and its lifecycle driver. The Routine's Schedule document is real;
//! the coordinator that pauses it is the scripted peer, whose answer is the
//! only thing that may settle a Routine member.
use super::*;
use koru_ai_wire::bot_lifecycle_codes::{BOT_TRASHED, PENDING};

fn operation(h: &H, command: BotCommand) -> ApiResult<BotLifecycleProgress> {
    h.send(command).map(|response| match response {
        BotResponse::Lifecycle { operation } => operation,
        _ => panic!("lifecycle progress"),
    })
}
fn trash(h: &H, request: BotRequestId, revision: u64) -> ApiResult<BotLifecycleProgress> {
    operation(
        h,
        BotCommand::Trash {
            request_id: request,
            bot_id: h.bot(),
            expected_revision: revision,
        },
    )
}
fn archive(h: &H, request: BotRequestId, revision: u64) -> ApiResult<BotLifecycleProgress> {
    operation(
        h,
        BotCommand::Archive {
            request_id: request,
            bot_id: h.bot(),
            expected_revision: revision,
        },
    )
}
fn restore(h: &H, source: BotRequestId, revision: u64) -> ApiResult<BotLifecycleProgress> {
    operation(
        h,
        BotCommand::Restore {
            request_id: BotRequestId::mint(),
            bot_id: h.bot(),
            expected_revision: revision,
            source_request_id: source,
        },
    )
}
fn progress(h: &H, request: BotRequestId) -> BotLifecycleProgress {
    operation(
        h,
        BotCommand::ReadLifecycleOperation {
            request_id: request,
            bot_id: h.bot(),
        },
    )
    .unwrap()
}
/// The one Routine member of an operation.
fn routine(h: &H, operation: &BotLifecycleProgress) -> BotLifecycleMemberOutcome {
    let members: Vec<_> = operation
        .members
        .iter()
        .filter(|member| matches!(member.subject, BotLifecycleSubject::Routine { .. }))
        .cloned()
        .collect();
    assert_eq!(members.len(), 1, "one member per Routine of the Bot");
    assert_eq!(
        members[0].subject,
        BotLifecycleSubject::Routine {
            workspace_id: h.f.bot.home_workspace,
            document_id: koru_ai_core::DocumentId::from_uuid(h.schedule().as_uuid()),
        }
    );
    members[0].clone()
}
fn until_stage(h: &H, request: BotRequestId, stage: BotLifecycleStage) -> BotLifecycleProgress {
    let mut last = None;
    until(&format!("the operation is {stage:?}"), || {
        let now = progress(h, request);
        let done = now.stage == stage;
        last = Some(now);
        done
    });
    last.unwrap()
}
/// The Bot wherever it is: active or archived by `read`, trashed by the
/// Trash filter.
fn bot_now(h: &H) -> Bot {
    if let Ok(BotResponse::Bot { bot }) = h.send(BotCommand::Read { bot_id: h.bot() }) {
        return bot;
    }
    let Ok(BotResponse::List { bots }) = h.send(BotCommand::ListLifecycle {
        workspace: h.f.bot.home_workspace,
        lifecycle: BotLifecycle::Trashed,
    }) else {
        panic!("Trash listing")
    };
    bots.into_iter()
        .find(|bot| bot.id == h.bot())
        .expect("the Bot is in Trash")
}
fn position(calls: &[String], prefix: &str) -> usize {
    calls
        .iter()
        .position(|call| call.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
}

#[test]
fn lifecycle_routine_trash_pauses_routines_first_and_acknowledges_only_on_the_pause_answer() {
    let h = H::new();
    h.enable(1).unwrap();
    assert!(!h.owner.live().paused);
    // Hold the coordinator's next control: the Pause the Trash sends.
    let (held, entered, mut release) = gate();
    *h.owner.gate.lock().unwrap() = Some(held);
    let request = BotRequestId::mint();
    let captured = trash(&h, request, 1).unwrap();
    let member = routine(&h, &captured);
    assert_eq!(member.intended, BotLifecycleIntended::Hide);
    assert_eq!(member.stage, BotLifecycleMemberStage::Pending);
    entered.recv_timeout(Duration::from_secs(15)).unwrap();

    // The Pause is at the coordinator and not answered. The Bot's own pause
    // is already in the ledger. Nothing was assumed: the member is still
    // pending, the Bot is not in Trash, and no work was settled yet.
    let moving = progress(&h, request);
    assert_eq!(moving.stage, BotLifecycleStage::Settling);
    assert_eq!(routine(&h, &moving).stage, BotLifecycleMemberStage::Pending);
    assert!(h.owner.admission.lock().unwrap().as_ref().unwrap().paused);
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Active);
    let calls = h.owner.calls();
    assert_eq!(h.owner.named("pause"), ["pause@2"]);
    assert!(
        !calls.iter().any(|call| call == "unsettled_work"),
        "{calls:?}"
    );
    // A person's control of the Routine is held by the operation meanwhile.
    assert_eq!(code(h.enable(2)), PENDING);
    assert_eq!(code(h.pause(2)), PENDING);

    release.open();
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    // The coordinator paused it; the Schedule document was not moved.
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (3, true));
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Trashed);
    // The Bot's pause, then the Routine's, then the work.
    let calls = h.owner.calls();
    let barrier = position(&calls, "barrier:paused=true");
    let pause = position(&calls, "pause@2");
    let unsettled = position(&calls, "unsettled_work");
    assert!(barrier < pause && pause < unsettled, "{calls:?}");
    // In Trash every control answers the Bot's own code.
    assert_eq!(code(h.enable(3)), BOT_TRASHED);
    assert_eq!(code(h.pause(3)), BOT_TRASHED);
    assert_eq!(
        code(h.access(BotRequestId::mint(), 1, BotAccess::Workspace)),
        BOT_TRASHED
    );
    assert_eq!(h.owner.named("enable"), ["enable@1"]);
}

#[test]
fn lifecycle_routine_member_is_never_acknowledged_from_the_drivers_own_read() {
    let h = H::new();
    // The Routine is paused, and every read of its Schedule says so. The
    // coordinator does not answer.
    assert!(h.owner.live().paused);
    h.owner.silent.store(true, Ordering::SeqCst);
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let parked = until_stage(&h, request, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::OwnerUnavailable));
    // Not acknowledged, however paused it reads; and the Bot is not trashed.
    assert_eq!(
        routine(&h, &parked).stage,
        BotLifecycleMemberStage::Unavailable
    );
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Active);
    assert!(!h.owner.named("pause").is_empty());
    assert!(!h.owner.calls().iter().any(|call| call == "unsettled_work"));

    // The coordinator answers again (it finds the Routine already paused).
    // The same request takes the operation up, and only now is the member
    // acknowledged and the Bot trashed.
    h.owner.silent.store(false, Ordering::SeqCst);
    until("the operation completes", || {
        let _ = trash(&h, request, 1);
        progress(&h, request).stage == BotLifecycleStage::Completed
    });
    let done = progress(&h, request);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Trashed);
    assert!(h.owner.live().paused);
}

#[test]
fn lifecycle_routine_enable_racing_trash_leaves_no_routine_on() {
    // Enable first: its commit lands, and the Trash's own Pause turns it off.
    let h = H::new();
    h.enable(1).unwrap();
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    assert!(h.owner.live().paused);
    drop(h);

    // Enable second: admitted before the Trash was captured, and handled by
    // the coordinator only after it. The Bot's pause is in the ledger by
    // then, so the coordinator refuses it.
    let h = H::new();
    let (held, entered, mut release) = gate();
    *h.owner.gate.lock().unwrap() = Some(held);
    let client = h.client();
    let enable = h.lifecycle(BotRoutineLifecycleAction::Enable, 1);
    let racing = std::thread::spawn(move || client.bots(context(), enable));
    entered.recv_timeout(Duration::from_secs(15)).unwrap();
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    // The Trash does not wait for the person's control: it pauses the
    // Routine through the coordinator and completes.
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    release.open();
    let refused = racing.join().unwrap().unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY, "{refused:?}");
    assert_eq!(reasons(&refused), ["bot_paused"]);
    // Whatever the order, no Routine is on inside the trashed Bot.
    let live = h.owner.live();
    assert!(live.paused, "{live:?}");
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Trashed);
    assert_eq!(code(h.enable(live.revision)), BOT_TRASHED);
    assert!(h.owner.live().paused);
}

#[test]
fn lifecycle_routine_archive_pauses_routines_and_unarchive_leaves_them_paused() {
    let h = H::new();
    h.enable(1).unwrap();
    let request = BotRequestId::mint();
    let captured = archive(&h, request, 1).unwrap();
    assert_eq!(
        routine(&h, &captured).stage,
        BotLifecycleMemberStage::Pending
    );
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    assert!(h.owner.live().paused);
    assert_eq!(h.owner.named("pause"), ["pause@2"]);
    let archived = bot_now(&h);
    assert_eq!(archived.lifecycle, BotLifecycle::Archived);
    // Archived: Enable is not ready, an edit answers the Bot's code, and
    // Pause is still allowed (it only reduces work).
    let refused = h.enable(3).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["bot_paused", "bot_archived"]);
    assert_eq!(
        code(h.control(h.edit_command(3, "Routine", "Other", FLOATING))),
        "bot_archived"
    );
    assert_eq!(code(h.pause(3)), codes::STATE_CONFLICT);

    // Unarchive: one AI transaction, no owner call. Nothing is turned on.
    let before = h.owner.calls().len();
    let restored = restore(&h, request, archived.revision).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert!(restored.routines_still_on.is_empty());
    assert!(restored.routines_needing_access.is_empty());
    assert!(
        !h.owner.calls()[before..]
            .iter()
            .any(|call| call.starts_with("enable") || call.starts_with("pause")),
        "a Restore sends no control"
    );
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (3, true));
    let bot = bot_now(&h);
    assert_eq!((bot.lifecycle, bot.paused), (BotLifecycle::Active, true));
}

#[test]
fn lifecycle_routine_restore_of_a_trash_leaves_routines_paused_until_a_person_turns_them_on() {
    let h = H::new();
    h.enable(1).unwrap();
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    until_stage(&h, request, BotLifecycleStage::Completed);
    let trashed = bot_now(&h);
    let before = h.owner.calls().len();
    let restored = restore(&h, request, trashed.revision).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert!(restored.routines_still_on.is_empty());
    assert!(restored.routines_needing_access.is_empty());
    // Still paused, by no control of the Restore's.
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (3, true));
    assert!(
        !h.owner.calls()[before..]
            .iter()
            .any(|call| call.starts_with("enable") || call.starts_with("pause"))
    );
    // The Bot returned paused: the Routine stays off until the person
    // resumes the Bot and turns the Routine on.
    let bot = bot_now(&h);
    assert_eq!((bot.lifecycle, bot.paused), (BotLifecycle::Active, true));
    let refused = h.enable(3).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["bot_paused"]);
    assert!(h.owner.live().paused);
}

#[test]
fn lifecycle_routine_restore_of_a_parked_trash_lists_the_routines_not_confirmed_paused() {
    let h = H::new();
    h.enable(1).unwrap();
    // The coordinator never answers the Trash's Pause: the operation parks,
    // and the Routine is still on.
    h.owner.silent.store(true, Ordering::SeqCst);
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let parked = until_stage(&h, request, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::OwnerUnavailable));
    assert_eq!(
        routine(&h, &parked).stage,
        BotLifecycleMemberStage::Unavailable
    );
    assert!(!h.owner.live().paused);

    // The person reverses it. The Routine keeps the state it has: the
    // Restore neither pauses it nor claims it is paused. It names it.
    let mut restored = None;
    until("the parked Trash is restored", || {
        let bot = bot_now(&h);
        match restore(&h, request, bot.revision) {
            Ok(done) => {
                restored = Some(done);
                true
            }
            // Taken up again for a moment by the owner probe: reversed once
            // it has parked again.
            Err(error) => {
                assert!(
                    error.code == PENDING || error.code == "revision_conflict",
                    "{error:?}"
                );
                false
            }
        }
    });
    let restored = restored.unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert_eq!(restored.routines_still_on, [h.schedule()]);
    assert!(restored.routines_needing_access.is_empty());
    assert_eq!(progress(&h, request).stage, BotLifecycleStage::Reversed);
    let live = h.owner.live();
    assert_eq!((live.revision, live.paused), (2, false));
    // The Bot comes back paused, so the Routine cannot run until the person
    // resumes the Bot; Pause is available at once.
    let bot = bot_now(&h);
    assert_eq!((bot.lifecycle, bot.paused), (BotLifecycle::Active, true));
    h.owner.silent.store(false, Ordering::SeqCst);
    until("the person pauses the Routine", || match h.pause(2) {
        Ok(_) => true,
        Err(error) => {
            assert!(
                error.code == "bot_operation_pending" || error.code == codes::CONTROL_PENDING,
                "{error:?}"
            );
            false
        }
    });
    assert!(h.owner.live().paused);
}

#[test]
fn lifecycle_routine_owned_elsewhere_parks_at_once_and_a_deleted_one_is_kept() {
    // Another computer owns the Schedule: asking again here cannot pause it.
    let h = H::new();
    h.owner
        .script
        .lock()
        .unwrap()
        .push_back("schedule_not_owner");
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let parked = until_stage(&h, request, BotLifecycleStage::Incomplete);
    assert_eq!(parked.refusal, Some(BotLifecycleRefusal::OwnerUnavailable));
    assert_eq!(
        routine(&h, &parked).stage,
        BotLifecycleMemberStage::Unavailable
    );
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Active);
    drop(h);

    // The Schedule was deleted for good by the ordinary path: the member is
    // kept as already trashed, and the Trash completes.
    let h = H::new();
    h.owner
        .script
        .lock()
        .unwrap()
        .push_back("schedule_not_found");
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    let member = routine(&h, &done);
    assert_eq!(member.stage, BotLifecycleMemberStage::Retained);
    assert_eq!(member.retained, Some(BotLifecycleRetained::AlreadyTrashed));
    assert_eq!(bot_now(&h).lifecycle, BotLifecycle::Trashed);
}

#[test]
fn lifecycle_routine_revision_that_moved_is_read_again_and_paused() {
    let h = H::new();
    h.enable(1).unwrap();
    // The first Pause names a revision the coordinator no longer holds.
    h.owner
        .script
        .lock()
        .unwrap()
        .push_back("schedule_revision_conflict");
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    let done = until_stage(&h, request, BotLifecycleStage::Completed);
    assert_eq!(
        routine(&h, &done).stage,
        BotLifecycleMemberStage::Acknowledged
    );
    assert_eq!(h.owner.named("pause"), ["pause@2", "pause@2"]);
    assert!(h.owner.live().paused);
}

#[test]
fn lifecycle_routine_restore_lists_a_routine_whose_access_no_longer_reads() {
    let mut h = H::new();
    let edit = BotRequestId::mint();
    h.access(edit, 1, BotAccess::Workspace).unwrap();
    let request = BotRequestId::mint();
    trash(&h, request, 1).unwrap();
    until_stage(&h, request, BotLifecycleStage::Completed);
    // Its edited grant row is damaged while the Bot is in Trash.
    let repository = h.finish();
    repository
        .fixture_execute_batch("UPDATE bot_routine_grants SET grant_digest=zeroblob(32);")
        .unwrap();
    h.restart(repository);
    let trashed = bot_now(&h);
    let restored = restore(&h, request, trashed.revision).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert_eq!(restored.routines_needing_access, [h.schedule()]);
    assert!(restored.routines_still_on.is_empty());
    // It stays paused, and turning it on is refused until its access is
    // chosen again.
    assert!(h.owner.live().paused);
    let refused = h.enable(1).unwrap_err();
    assert_eq!(refused.code, codes::NOT_READY);
    assert_eq!(reasons(&refused), ["bot_paused", "access_unavailable"]);
}
