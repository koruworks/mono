// SPDX-License-Identifier: MPL-2.0
//! The workspace activity read: what is running and what needs the person,
//! from durable rows, and a plan that never walks a conversation's history.
use super::super::super::super::tests::{complete_turn, turn};
use super::*;

fn owner(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

/// A tool row on `attempt`; `approval_required` is an open approval or card.
fn tool(repo: &Repository, attempt: &PendingTurn, status: &str, at: i64) {
    let open = matches!(status, "proposed" | "approval_required" | "running");
    repo.connection
        .execute(
            "INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms,completed_at_ms) VALUES(?1,?2,?3,'agent',0,?4,?5,?6,?7)",
            params![
                ToolInvocationId::new().to_storage(),
                attempt.attempt.id.to_storage(),
                attempt.user_message.chat_id.to_storage(),
                status,
                if status == "approval_required" { "pending" } else { "not_required" },
                at,
                (!open).then_some(at),
            ],
        )
        .unwrap();
}

fn pending(repo: &mut Repository, chat: ChatId, offset: i64) -> PendingTurn {
    let pending = turn(chat, Uuid::new_v4(), offset);
    repo.begin_turn(&pending).unwrap();
    pending
}

fn sorted(mut entries: Vec<(BotId, ChatId, BotActivityState)>) -> Vec<BotActivity> {
    entries.sort_by_key(|(bot, chat, _)| (bot.to_storage(), chat.to_storage()));
    entries
        .into_iter()
        .map(|(bot_id, chat_id, state)| BotActivity {
            waiting: None,
            bot_id,
            chat_id,
            state,
        })
        .collect()
}

#[test]
fn running_and_needs_you_come_from_durable_rows_and_idle_trash_and_others_stay_out() {
    let (mut repo, bot, profile) = setup();
    // The main conversation has settled history only: idle.
    complete_turn(&mut repo, bot.main_chat, 0, "settled", 1);
    let running = additional(&mut repo, &bot, profile, 2);
    pending(&mut repo, running, 3);
    // A live approval on a running reply reads as Needs you, not Running.
    let approval = additional(&mut repo, &bot, profile, 4);
    let live = pending(&mut repo, approval, 5);
    tool(&repo, &live, "complete", 6);
    tool(&repo, &live, "approval_required", 7);
    // A question card the finished reply left open still needs the person.
    let card = additional(&mut repo, &bot, profile, 8);
    let asked = complete_turn(&mut repo, card, 0, "asked", 9);
    tool(&repo, &asked, "approval_required", 10);
    // A settled card is not activity.
    let answered = additional(&mut repo, &bot, profile, 12);
    let closed = complete_turn(&mut repo, answered, 0, "answered", 13);
    tool(&repo, &closed, "complete", 14);
    // Work in a conversation in Trash is out of every read.
    let trashed = additional(&mut repo, &bot, profile, 16);
    pending(&mut repo, trashed, 17);
    trash(&repo, &bot, trashed);

    // Another of the owner's Bots here, archived, with a running reply.
    let mut archived = fixture_bot();
    archived.user_scope = bot.user_scope;
    archived.owner_device = bot.owner_device;
    archived.home_workspace = bot.home_workspace;
    archived.config.role.workspace = bot.home_workspace;
    repo.save_bot(&archived, profile, BotRequestId::mint(), "archived", None)
        .unwrap();
    pending(&mut repo, archived.main_chat, 20);
    repo.fixture_set_bot_lifecycle(archived.id, BotLifecycle::Archived)
        .unwrap();
    // Someone else's Bot in the same workspace.
    let mut foreign = fixture_bot();
    foreign.home_workspace = bot.home_workspace;
    foreign.config.role.workspace = bot.home_workspace;
    repo.save_bot(&foreign, profile, BotRequestId::mint(), "foreign", None)
        .unwrap();
    pending(&mut repo, foreign.main_chat, 22);

    let page = repo
        .bot_activity(owner(&bot), bot.home_workspace, BOT_ACTIVITY_LIMIT)
        .unwrap();
    assert_eq!(
        page.entries,
        sorted(vec![
            (bot.id, running, BotActivityState::Running),
            (bot.id, approval, BotActivityState::NeedsYou),
            (bot.id, card, BotActivityState::NeedsYou),
        ])
    );
    assert!(!page.truncated);
    // The foreign owner sees only its own Bot, and another workspace nothing.
    let foreign_page = repo
        .bot_activity(owner(&foreign), bot.home_workspace, BOT_ACTIVITY_LIMIT)
        .unwrap();
    assert_eq!(
        foreign_page.entries,
        sorted(vec![(
            foreign.id,
            foreign.main_chat,
            BotActivityState::Running
        )])
    );
    assert!(
        repo.bot_activity(owner(&bot), WorkspaceId::new(), BOT_ACTIVITY_LIMIT)
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn activity_is_bounded_and_says_when_it_left_some_out() {
    let (mut repo, bot, profile) = setup();
    for n in 0..3 {
        let chat = additional(&mut repo, &bot, profile, n);
        pending(&mut repo, chat, 10 + n);
    }
    let cut = repo
        .bot_activity(owner(&bot), bot.home_workspace, 2)
        .unwrap();
    assert_eq!(cut.entries.len(), 2);
    assert!(cut.truncated);
    let whole = repo
        .bot_activity(owner(&bot), bot.home_workspace, 3)
        .unwrap();
    assert_eq!(whole.entries.len(), 3);
    assert!(!whole.truncated);
}

/// A conversation with a long settled history: every probe is a seek on
/// its index, so neither its attempts nor its tool rows are walked.
#[test]
fn activity_reads_one_index_probe_per_question_never_the_history() {
    let (mut repo, bot, profile) = setup();
    for n in 0..300u32 {
        let settled = complete_turn(
            &mut repo,
            bot.main_chat,
            n * 2,
            "settled",
            i64::from(n) * 10,
        );
        for k in 0..5 {
            tool(&repo, &settled, "complete", i64::from(n) * 10 + k);
        }
    }
    let live = additional(&mut repo, &bot, profile, 9000);
    let reply = pending(&mut repo, live, 9001);
    tool(&repo, &reply, "approval_required", 9002);

    let scope = bot.user_scope.to_storage();
    let device = bot.owner_device.to_storage();
    let workspace = bot.home_workspace.to_storage();
    let found = plan(
        &repo.connection,
        BOT_ACTIVITY_SQL,
        &[&scope, &device, &workspace],
    );
    for index in [
        "SEARCH m USING COVERING INDEX one_pending_assistant_per_chat",
        "SEARCH a USING INDEX inference_attempts_by_chat",
        "SEARCH t USING INDEX tool_invocations_by_attempt",
        "SEARCH p USING INDEX one_pending_assistant_per_chat",
        "SEARCH r USING INDEX",
        "SEARCH pt USING INDEX tool_invocations_by_attempt",
        "SEARCH b USING INDEX",
        "SEARCH tr USING",
    ] {
        assert!(found.contains(index), "{index} not used: {found}");
    }
    let scans: Vec<_> = found
        .split(" | ")
        .filter(|step| step.starts_with("SCAN "))
        .collect();
    assert!(
        scans.len() == 1 && scans[0].starts_with("SCAN bc"),
        "only the association table is scanned: {found}"
    );
    // Each probe is planned once, not again for an outer filter.
    assert_eq!(found.matches("SEARCH a USING").count(), 1, "{found}");
    let page = repo
        .bot_activity(owner(&bot), bot.home_workspace, BOT_ACTIVITY_LIMIT)
        .unwrap();
    assert_eq!(
        page.entries,
        vec![BotActivity {
            waiting: None,
            bot_id: bot.id,
            chat_id: live,
            state: BotActivityState::NeedsYou,
        }]
    );
}
