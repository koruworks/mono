// SPDX-License-Identifier: MPL-2.0
//! A Routine as a member of a whole-Bot Archive or Trash (ai.db 23). For a
//! Routine, `hide` means paused, and only the delegate coordinator's answer
//! settles the member. The answer is supplied by hand here: no delegate runs.
use super::*;
use koru_delegate_core::{RoutineDigest32, ScheduleId};

/// One more Routine of the Bot, bound or left prepared.
fn routine(repo: &mut Repository, bot: &Bot, owner: BotOwner, bound: bool) -> ScheduleId {
    let current = repo.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let command: BotRoutineCreate = serde_json::from_value(serde_json::json!({
        "request":BotRequestId::mint(),"bot":bot.id,"expected_bot_revision":current.revision,
        "name":"Private routine","objective":"Private task","documents":[DocumentId::new()],
        "recurrence":{"type":"floating","cron":"0 9 * * *"}
    }))
    .unwrap();
    repo.prepare_bot_routine_creation(owner, &command, RoutineDigest32::from_bytes([7; 32]), 5)
        .unwrap();
    let creation = repo
        .bot_routine_creation_by_request(owner, bot.id, command.request)
        .unwrap()
        .unwrap();
    if bound {
        repo.transition_bot_routine_creation(
            &creation.identity,
            creation.observed(),
            BotRoutineCreationTransition::Confirmed(BotRoutineStoreConfirmation {
                identity: creation.identity.clone(),
                origin_edit_revision: vec![1, 2, 3],
            }),
            6,
        )
        .unwrap();
    }
    creation.identity.schedule
}

fn document(schedule: ScheduleId) -> DocumentId {
    DocumentId::from_uuid(schedule.as_uuid())
}

/// The members a driver captures for the Bot's Routines: a bound one is to
/// be paused; one whose creation is not bound is kept, ownership unproven.
fn routine_members(repo: &Repository, bot: &Bot) -> Vec<LifecycleMemberDraft> {
    repo.lifecycle_bot_routines(bot.id)
        .unwrap()
        .into_iter()
        .map(|(schedule, workspace_id, bound)| LifecycleMemberDraft {
            subject: BotLifecycleSubject::Routine {
                workspace_id,
                document_id: document(schedule),
            },
            reason: BotLifecycleReason::OwnedRoutine,
            prior_state: BotLifecyclePriorState::Live,
            intended: if bound {
                BotLifecycleIntended::Hide
            } else {
                BotLifecycleIntended::Retain
            },
            retained: (!bound).then_some(BotLifecycleRetained::OwnershipUnproven),
        })
        .collect()
}

fn with_routines(repo: &Repository, bot: &Bot, owner: BotOwner, kind: BotLifecycleKind) -> LifecycleCapture {
    let mut request = capture(bot, owner, kind);
    request.members.extend(routine_members(repo, bot));
    request
}

fn member(op: &LifecycleOperation, schedule: ScheduleId) -> LifecycleMember {
    op.members
        .iter()
        .find(|m| {
            matches!(&m.subject, BotLifecycleSubject::Routine { document_id, .. }
            if *document_id == document(schedule))
        })
        .cloned()
        .unwrap()
}

fn settling(repo: &mut Repository, scope: UserScopeId, operation: BotRequestId) {
    assert!(
        repo.advance_bot_lifecycle(
            scope,
            operation,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            true,
            25
        )
        .unwrap()
    );
}

#[test]
fn lifecycle_routine_member_round_trips_through_trash_and_restore() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let on = routine(&mut repo, &bot, owner, true);
    let unbound = routine(&mut repo, &bot, owner, false);
    let mut listed = repo.lifecycle_bot_routines(bot.id).unwrap();
    listed.sort_by_key(|(_, _, bound)| !*bound);
    assert_eq!(
        listed,
        vec![
            (on, bot.home_workspace, true),
            (unbound, bot.home_workspace, false)
        ]
    );

    let request = with_routines(&repo, &bot, owner, BotLifecycleKind::Trash);
    let captured = repo.capture_bot_lifecycle(&request).unwrap();
    assert_eq!(captured.members.len(), 4);
    let paused = member(&captured, on);
    assert_eq!(
        paused.subject,
        BotLifecycleSubject::Routine {
            workspace_id: bot.home_workspace,
            document_id: document(on)
        }
    );
    assert_eq!(paused.subject.kind(), BotLifecycleMemberKind::Routine);
    assert_eq!(paused.subject.owner(), BotLifecycleOwner::Delegate);
    assert_eq!(paused.reason, BotLifecycleReason::OwnedRoutine);
    assert_eq!(paused.intended, BotLifecycleIntended::Hide);
    assert_eq!(paused.stage, BotLifecycleMemberStage::Pending);
    assert!(paused.child_request.is_some(), "a delegate member has a child request");
    let kept = member(&captured, unbound);
    assert_eq!(kept.stage, BotLifecycleMemberStage::Retained);
    assert_eq!(kept.retained, Some(BotLifecycleRetained::OwnershipUnproven));
    // The stored row, as written.
    let raw: (String, String, String, String, String, Option<String>, Option<String>, Option<String>) = repo
        .connection
        .query_row(
            "SELECT kind,owner,reason,workspace_id,document_id,chat_id,run_id,attachment_id FROM bot_lifecycle_members WHERE operation_id=?1 AND ordinal=?2",
            params![request.operation.to_storage(), paused.ordinal],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .unwrap();
    assert_eq!(
        raw,
        (
            "routine".into(),
            "delegate".into(),
            "owned_routine".into(),
            bot.home_workspace.to_storage(),
            on.to_storage(),
            None,
            None,
            None
        )
    );
    // An exact repeat returns the stored capture, member for member; the
    // read by id and the public progress agree with it.
    assert_eq!(repo.capture_bot_lifecycle(&request).unwrap(), captured);
    assert_eq!(
        repo.bot_lifecycle_operation(scope, request.operation)
            .unwrap(),
        Some(captured.clone())
    );
    let progress = captured.progress();
    assert_eq!(progress.members.len(), 4);
    assert_eq!(progress.members[paused.ordinal as usize].subject, paused.subject);
    assert_eq!(
        serde_json::to_value(&progress.members[paused.ordinal as usize].subject).unwrap(),
        serde_json::json!({"subject":"routine","workspace_id":bot.home_workspace,"document_id":document(on)})
    );

    // Trash completes only once the Routine is confirmed paused.
    settling(&mut repo, scope, request.operation);
    assert_eq!(
        repo.complete_bot_trash(scope, request.operation, 40),
        Err(BotError::Conflict)
    );
    let acknowledged = repo
        .settle_bot_lifecycle_routine(
            scope,
            request.operation,
            document(on),
            LifecycleRoutineAnswer::Paused,
            35,
        )
        .unwrap();
    assert_eq!(acknowledged.stage, BotLifecycleMemberStage::Acknowledged);
    let done = repo.complete_bot_trash(scope, request.operation, 40).unwrap();
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert_eq!(member(&done, on), acknowledged);
    assert_eq!(member(&done, unbound), kept);
    assert_eq!(
        repo.bot_any_lifecycle(bot.id).unwrap().unwrap().1,
        BotLifecycle::Trashed
    );

    // Restore reverses the Trash in one transaction and re-enables nothing:
    // the source keeps its Routine members exactly as Trash left them.
    let current = repo.bot_any_lifecycle(bot.id).unwrap().unwrap().0;
    let restore = LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind: BotLifecycleKind::Restore,
        chat: None,
        fingerprint: HASH.into(),
        expected_revision: current.revision,
        source: Some(request.operation),
        members: vec![],
        now: 50,
    };
    let restored = repo.restore_trashed_bot(&restore).unwrap();
    assert_eq!(restored.stage, BotLifecycleStage::Completed);
    assert!(restored.members.is_empty());
    let source = repo
        .bot_lifecycle_operation(scope, request.operation)
        .unwrap()
        .unwrap();
    assert_eq!(source.stage, BotLifecycleStage::Reversed);
    assert_eq!(member(&source, on), acknowledged);
    assert_eq!(member(&source, unbound), kept);
    let (bot_now, lifecycle) = repo.bot_any_lifecycle(bot.id).unwrap().unwrap();
    assert_eq!(lifecycle, BotLifecycle::Active);
    assert!(bot_now.paused);
    // A reversed operation accepts no late answer for an unsettled member,
    // and a settled member is returned as stored.
    assert_eq!(
        repo.settle_bot_lifecycle_routine(
            scope,
            request.operation,
            document(on),
            LifecycleRoutineAnswer::Unavailable,
            60
        )
        .unwrap(),
        acknowledged
    );
}

#[test]
fn lifecycle_routine_member_is_settled_only_by_the_coordinator_answer() {
    let (mut repo, bot, owner) = setup();
    let scope = owner.user_scope;
    let first = routine(&mut repo, &bot, owner, true);
    let second = routine(&mut repo, &bot, owner, true);
    let request = with_routines(&repo, &bot, owner, BotLifecycleKind::Archive);
    let captured = repo.capture_bot_lifecycle(&request).unwrap();
    let operation = request.operation;
    // Not while only captured: the driver has not started settling.
    settling(&mut repo, scope, operation);

    // Completion never acknowledges a Routine by itself, and leaves the Bot
    // and every member as they were.
    let before = repo.bot_lifecycle_operation(scope, operation).unwrap().unwrap();
    assert_eq!(
        repo.complete_bot_archive(scope, operation, 40),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_lifecycle_operation(scope, operation).unwrap().unwrap(),
        before
    );
    assert_eq!(
        repo.bot_any_lifecycle(bot.id).unwrap().unwrap().1,
        BotLifecycle::Active
    );
    assert_eq!(member(&before, first).stage, BotLifecycleMemberStage::Pending);

    // No answer: the member is unavailable, and still blocks completion.
    let waiting = repo
        .settle_bot_lifecycle_routine(
            scope,
            operation,
            document(first),
            LifecycleRoutineAnswer::Unavailable,
            30,
        )
        .unwrap();
    assert_eq!(waiting.stage, BotLifecycleMemberStage::Unavailable);
    assert_eq!(waiting.retained, None);
    assert_eq!(
        repo.complete_bot_archive(scope, operation, 40),
        Err(BotError::Conflict)
    );
    // A parked operation takes no answer; resumed, the retry's answer lands.
    assert!(
        repo.park_bot_lifecycle(scope, operation, BotLifecycleRefusal::OwnerUnavailable, 31)
            .unwrap()
    );
    assert_eq!(
        repo.settle_bot_lifecycle_routine(
            scope,
            operation,
            document(first),
            LifecycleRoutineAnswer::Paused,
            32
        ),
        Err(BotError::Conflict)
    );
    assert!(repo.resume_bot_lifecycle(scope, operation, 33).unwrap());
    assert!(
        repo.advance_bot_lifecycle(
            scope,
            operation,
            BotLifecycleStage::Captured,
            BotLifecycleStage::Settling,
            true,
            34
        )
        .unwrap()
    );
    let paused = repo
        .settle_bot_lifecycle_routine(
            scope,
            operation,
            document(first),
            LifecycleRoutineAnswer::Paused,
            35,
        )
        .unwrap();
    assert_eq!(paused.stage, BotLifecycleMemberStage::Acknowledged);
    // One Routine confirmed is not all of them.
    assert_eq!(
        repo.complete_bot_archive(scope, operation, 40),
        Err(BotError::Conflict)
    );
    // The other was deleted meanwhile: kept, as already trashed.
    let gone = repo
        .settle_bot_lifecycle_routine(
            scope,
            operation,
            document(second),
            LifecycleRoutineAnswer::Gone,
            36,
        )
        .unwrap();
    assert_eq!(gone.stage, BotLifecycleMemberStage::Retained);
    assert_eq!(gone.retained, Some(BotLifecycleRetained::AlreadyTrashed));
    // A settled member is final: a late or repeated answer changes nothing.
    for answer in [
        LifecycleRoutineAnswer::Unavailable,
        LifecycleRoutineAnswer::Gone,
        LifecycleRoutineAnswer::Paused,
    ] {
        assert_eq!(
            repo.settle_bot_lifecycle_routine(scope, operation, document(first), answer, 37)
                .unwrap(),
            paused
        );
        assert_eq!(
            repo.settle_bot_lifecycle_routine(scope, operation, document(second), answer, 37)
                .unwrap(),
            gone
        );
    }
    assert_eq!(
        repo.settle_bot_lifecycle_routine(
            scope,
            operation,
            DocumentId::new(),
            LifecycleRoutineAnswer::Paused,
            37
        ),
        Err(BotError::NotFound)
    );
    assert_eq!(
        repo.settle_bot_lifecycle_routine(
            scope,
            BotRequestId::mint(),
            document(first),
            LifecycleRoutineAnswer::Paused,
            37
        ),
        Err(BotError::NotFound)
    );

    let done = repo.complete_bot_archive(scope, operation, 40).unwrap();
    assert_eq!(done.stage, BotLifecycleStage::Completed);
    assert_eq!(member(&done, first), paused);
    assert_eq!(member(&done, second), gone);
    // The members the AI owner settles itself are acknowledged as before.
    assert!(
        done.members
            .iter()
            .filter(|m| m.subject.kind() != BotLifecycleMemberKind::Routine)
            .all(|m| m.stage == BotLifecycleMemberStage::Acknowledged)
    );
    assert_eq!(done.members.len(), captured.members.len());
    assert_eq!(
        repo.bot_any_lifecycle(bot.id).unwrap().unwrap().1,
        BotLifecycle::Archived
    );
}

#[test]
fn lifecycle_routine_member_must_be_the_bots_own_routine_of_a_whole_bot_operation() {
    let (mut repo, bot, owner) = setup();
    let schedule = routine(&mut repo, &bot, owner, true);
    let own = routine_members(&repo, &bot).remove(0);
    let intents = |repo: &Repository| -> (i64, i64) {
        (
            repo.connection
                .query_row("SELECT count(*) FROM bot_lifecycle_intents", [], |r| {
                    r.get(0)
                })
                .unwrap(),
            repo.connection
                .query_row("SELECT count(*) FROM bot_lifecycle_members", [], |r| {
                    r.get(0)
                })
                .unwrap(),
        )
    };
    let with = |kind: BotLifecycleKind, member: LifecycleMemberDraft| {
        let mut request = capture(&bot, owner, kind);
        request.members.push(member);
        request
    };
    // A Schedule that is not this Bot's Routine, or not in its home.
    let mut unknown = own.clone();
    unknown.subject = BotLifecycleSubject::Routine {
        workspace_id: bot.home_workspace,
        document_id: DocumentId::new(),
    };
    let mut elsewhere = own.clone();
    elsewhere.subject = BotLifecycleSubject::Routine {
        workspace_id: WorkspaceId::new(),
        document_id: document(schedule),
    };
    // A reason that is not a Routine's, and a Routine's reason on a page.
    let mut misnamed = own.clone();
    misnamed.reason = BotLifecycleReason::RoleCreationReceipt;
    let role = LifecycleMemberDraft {
        subject: BotLifecycleSubject::OwnedRole {
            workspace_id: bot.config.role.workspace,
            document_id: bot.config.role.document,
        },
        reason: BotLifecycleReason::OwnedRoutine,
        prior_state: BotLifecyclePriorState::Live,
        intended: BotLifecycleIntended::Hide,
        retained: None,
    };
    for request in [
        with(BotLifecycleKind::Trash, unknown),
        with(BotLifecycleKind::Archive, elsewhere),
        with(BotLifecycleKind::Trash, misnamed),
        with(BotLifecycleKind::Trash, role),
        // A conversation's Trash captures no Routine.
        with(BotLifecycleKind::TrashChat, own.clone()),
    ] {
        assert_eq!(
            repo.capture_bot_lifecycle(&request),
            Err(BotError::Invalid)
        );
    }
    // The same Routine twice in one operation is refused by its index, and
    // the whole capture rolls back.
    let mut twice = with(BotLifecycleKind::Trash, own.clone());
    twice.members.push(own.clone());
    assert_eq!(repo.capture_bot_lifecycle(&twice), Err(BotError::Storage));
    assert_eq!(intents(&repo), (0, 0));
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    // Both whole-Bot operations accept it.
    let archive = with(BotLifecycleKind::Archive, own);
    assert_eq!(
        repo.capture_bot_lifecycle(&archive).unwrap().members.len(),
        3
    );
}
