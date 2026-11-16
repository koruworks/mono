// SPDX-License-Identifier: MPL-2.0
//! The driver's scheduling arithmetic and the fail-closed policy override,
//! without a worker.
use super::*;
use koru_ai_core::{DocumentId, ModelSelection};

fn bot() -> Bot {
    let workspace = WorkspaceId::new();
    Bot {
        id: BotId::mint(),
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        home_workspace: workspace,
        config: BotConfig {
            name: "fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: if cfg!(windows) {
                "D:/isolated"
            } else {
                "/isolated"
            }
            .into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
            avatar: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        revision: 8,
        policy_revision: 3,
        main_chat: ChatId::new(),
        paused: true,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    }
}

fn archive(bot: &Bot) -> LifecycleOperation {
    LifecycleOperation {
        operation: BotRequestId::mint(),
        owner: BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        },
        bot: bot.id,
        kind: BotLifecycleKind::Archive,
        chat: None,
        fingerprint: "a".repeat(64),
        expected_revision: bot.revision - 1,
        barrier_revision: Some(bot.revision),
        source: None,
        prior_lifecycle: BotLifecycle::Active,
        pause_request: Some(BotRequestId::mint()),
        policy_request: Some(BotRequestId::mint().into()),
        policy_barrier: Some(koru_ai_core::bots::BotLifecycleBarrierState::Pending),
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::WorkRunning),
        requested_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
        members: vec![],
    }
}

/// Rounds put off for a reason outside the operation (a full pool, a
/// restart's recovery walk still paging) back off as waits do, and park the
/// operation after as many of them as waits do (the fourth lifecycle
/// review's F3): before, they never parked it, so a walk that could not
/// finish held it moving for good, and it could not be reversed.
#[test]
fn a_pool_that_stays_full_backs_off_to_the_cap_and_parks_the_operation_after_the_limit() {
    let mut drive = Drive::default();
    let now = Instant::now();
    let mut waits = Vec::new();
    for round in 1..PARK_AFTER {
        assert!(!drive.defer(now, ROUND_UNIT), "round {round}");
        waits.push(drive.due.unwrap() - now);
    }
    // Each refused send waits longer, from 500 ms up to the 32 s cap: the
    // rounds are spread over minutes, not 4 Hz.
    assert_eq!(waits[0], Duration::from_millis(500));
    assert_eq!(waits[1], Duration::from_secs(1));
    assert!(waits.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(*waits.last().unwrap(), Duration::from_secs(32));
    let total: Duration = waits.iter().sum();
    assert!(total > Duration::from_secs(2 * 60), "{total:?}");
    // Deferral is not counted as the operation's own failure.
    assert_eq!(drive.retries, 0);
    // The last one parks it, and still sets its next wait, so a park that
    // cannot be written is retried after a wait, never in a tight loop.
    assert!(drive.defer(now, ROUND_UNIT));
    assert_eq!(drive.due, Some(now + Duration::from_secs(32)));
    // Progress resets both counts.
    drive.progressed();
    assert_eq!((drive.retries, drive.deferrals, drive.due), (0, 0, None));
    assert!(!drive.wait(now, ROUND_UNIT));
}

#[test]
fn rounds_without_progress_park_after_the_limit_and_busy_is_never_due() {
    let mut drive = Drive::default();
    let now = Instant::now();
    for round in 1..PARK_AFTER {
        assert!(!drive.wait(now, ROUND_UNIT), "round {round}");
        assert_eq!(drive.due, Some(now + backoff(ROUND_UNIT, round)));
    }
    assert!(drive.wait(now, ROUND_UNIT));
    let mut busy = Drive {
        busy: true,
        ..Drive::default()
    };
    assert!(!busy.due(now));
    busy.busy = false;
    assert!(busy.due(now));
    let _ = busy.defer(now, ROUND_UNIT);
    assert!(!busy.due(now));
    assert!(busy.due(now + Duration::from_secs(1)));
}

#[test]
fn the_policy_override_fails_closed_when_the_journal_cannot_be_read() {
    let bot = bot();
    let op = archive(&bot);
    let key = (op.owner.user_scope, op.operation);
    // No open operation: the ordinary evidence applies (`None`).
    assert!(matches!(
        lifecycle_evidence(Ok(None), |_, _| unreachable!(), &bot),
        Ok(None)
    ));
    // An open Archive: its own effects-off barrier at the paused revision.
    let barrier = lifecycle_evidence(Ok(Some(key)), |_, _| Ok(Some(op.clone())), &bot)
        .unwrap()
        .unwrap();
    assert!(!barrier.effects_allowed);
    assert_eq!(barrier.config_revision, bot.revision);
    assert_eq!(barrier.key.request, op.policy_request.unwrap());
    // Any read that fails, or an operation that vanished or cannot name its
    // barrier, refuses. It is never read as "no operation", which would hand
    // out the ordinary effects-allowed evidence.
    assert!(lifecycle_evidence(Err(BotError::Storage), |_, _| unreachable!(), &bot).is_err());
    assert!(lifecycle_evidence(Ok(Some(key)), |_, _| Err(BotError::Storage), &bot).is_err());
    assert!(lifecycle_evidence(Ok(Some(key)), |_, _| Ok(None), &bot).is_err());
    let mut broken = op;
    broken.barrier_revision = None;
    assert!(lifecycle_evidence(Ok(Some(key)), |_, _| Ok(Some(broken.clone())), &bot).is_err());
}

#[test]
fn reads_are_the_only_bot_commands_an_archived_bot_answers() {
    let bot = BotId::mint();
    for read in [
        BotCommand::Read { bot_id: bot },
        BotCommand::ReadOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot,
        },
        BotCommand::ReadLifecycleOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot,
        },
        BotCommand::ListLifecycle {
            workspace: WorkspaceId::new(),
            lifecycle: BotLifecycle::Archived,
        },
        BotCommand::ReadRun {
            bot_id: bot,
            chat_id: ChatId::new(),
            message_id: Some(koru_ai_core::MessageId::new()),
            run_id: koru_delegate_core::RunId::mint(),
            from: 0,
            len: 1,
        },
    ] {
        assert!(bot_read_command(&read), "{read:?}");
    }
    for change in [
        BotCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: bot,
        },
        BotCommand::Pause {
            request_id: BotRequestId::mint(),
            bot_id: bot,
            expected_revision: 1,
        },
        BotCommand::ReconcileOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot,
        },
    ] {
        assert!(!bot_read_command(&change), "{change:?}");
    }
}
